//! Cloud cleanup. One provider, speaking the OpenAI chat-completions wire format —
//! which Groq (the default), OpenRouter, Gemini's compatibility endpoint and OpenAI
//! itself all accept, so switching vendor is a base URL and not new code. It sends
//! the shared WhimprFlow system prompt plus the assembled context and returns cleaned
//! text. On failure the caller retries on the local model — cleanup is an
//! enhancement, never a gate.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use whimpr_core::cleanup::{
    build_messages, max_tokens_for, CleanupContext, CleanupProvider, ProviderId,
};
use whimpr_core::cloud::{retry_after_secs, KeyRing};

/// Default OpenAI Chat Completions endpoint.
const OPENAI_DEFAULT_URL: &str = "https://api.openai.com/v1/chat/completions";

/// Cleanup via the OpenAI Chat Completions API — or any OpenAI-compatible
/// endpoint (OpenRouter, a local server, etc.) when `base_url` is set.
/// OpenRouter in particular speaks this exact wire format at
/// `https://openrouter.ai/api/v1/chat/completions`.
pub struct OpenAiProvider {
    client: reqwest::blocking::Client,
    /// Every key the user gave for this endpoint, and which are rate limited. Owned
    /// here rather than shared with cloud ASR because a vendor's caps are per model:
    /// a key that has spent its cleanup quota can still transcribe.
    keys: Mutex<KeyRing>,
    model: String,
    /// Full chat-completions URL. Defaults to OpenAI's when empty.
    url: String,
}

impl OpenAiProvider {
    pub fn new(keys: Vec<String>, model: impl Into<String>) -> Self {
        Self::with_base_url(keys, model, None)
    }

    /// `base_url` is the API root (e.g. `https://openrouter.ai/api/v1`), without
    /// the `/chat/completions` suffix. `None` or empty uses OpenAI directly.
    pub fn with_base_url(
        keys: Vec<String>,
        model: impl Into<String>,
        base_url: Option<String>,
    ) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("failed to build HTTP client");
        let url = match base_url.map(|s| s.trim().trim_end_matches('/').to_string()) {
            Some(base) if !base.is_empty() => format!("{base}/chat/completions"),
            _ => OPENAI_DEFAULT_URL.to_string(),
        };
        Self {
            client,
            keys: Mutex::new(KeyRing::new(keys)),
            model: model.into(),
            url,
        }
    }

    /// The first key that is not rate limited, or the error the caller falls back on.
    ///
    /// The message carries "429" and a "try again in" hint on purpose: the caller's
    /// fallback chain and `cleanup_check`'s retry both read it the way they read the
    /// endpoint's own answer.
    fn pick(&self) -> anyhow::Result<(usize, String)> {
        let ring = self.keys.lock().unwrap();
        let now = unix_now();
        match ring.pick(now) {
            Some(i) => Ok((i, ring.keys()[i].clone())),
            None => anyhow::bail!(
                "OpenAI HTTP 429: every key ({}) is rate limited. Please try again in {}s",
                ring.len(),
                ring.wait_secs(now).unwrap_or(0)
            ),
        }
    }

    /// POST with the first usable key, moving to the next on a 429 until one answers
    /// or none is left. A key that is limited stays marked for as long as the
    /// endpoint asked, so the dictations that follow inside that window skip the
    /// dead key — and the network round trip — outright.
    fn post(
        &self,
        body: &mut serde_json::Value,
        mut low_reasoning: bool,
    ) -> anyhow::Result<reqwest::blocking::Response> {
        loop {
            let (index, key) = self.pick()?;
            let mut resp = self.client.post(&self.url).bearer_auth(&key).json(&*body).send()?;

            // A 400 is how an endpoint says it does not know a parameter, and the
            // allowlist in `takes_reasoning_effort` is a guess about vendors, not a
            // contract with them. Drop the optimization and retry rather than let it
            // cost the user cleanup entirely: on a cloud-only install there is no
            // local model to fall back to, so a rejected parameter would mean raw,
            // filler-ridden pastes forever. Remembered process-wide, so this costs
            // one wasted call and not one per dictation.
            if resp.status() == reqwest::StatusCode::BAD_REQUEST && low_reasoning {
                eprintln!(
                    "[whimpr] endpoint rejected reasoning_effort — retrying without it, \
                     and not sending it again this run"
                );
                REASONING_EFFORT_REFUSED.store(true, Ordering::Relaxed);
                body.as_object_mut().map(|b| b.remove("reasoning_effort"));
                low_reasoning = false;
                resp = self.client.post(&self.url).bearer_auth(&key).json(&*body).send()?;
            }

            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let header = resp
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                let detail = resp.text().unwrap_or_default();
                let secs = retry_after_secs(header.as_deref(), &detail);
                let mut ring = self.keys.lock().unwrap();
                ring.report_limited(index, unix_now(), secs);
                eprintln!(
                    "[whimpr] cloud key {} of {} is rate limited for {}s{}",
                    index + 1,
                    ring.len(),
                    secs.map(|s| s.ceil() as u64).unwrap_or(60),
                    if ring.pick(unix_now()).is_some() { " — trying the next" } else { "" }
                );
                continue;
            }
            return Ok(resp);
        }
    }
}

impl CleanupProvider for OpenAiProvider {
    fn id(&self) -> ProviderId {
        ProviderId::OpenAi
    }

    fn cleanup(&self, raw: &str, ctx: &CleanupContext) -> anyhow::Result<String> {
        // System prompt + few-shot demonstration turns + the real transcript.
        let messages: Vec<serde_json::Value> = build_messages(raw, ctx)
            .into_iter()
            .map(|m| serde_json::json!({ "role": m.role, "content": m.content }))
            .collect();
        let mut body = serde_json::json!({
            "model": self.model,
            // Greedy, matching the local worker. Cleanup is a mechanical rewrite with
            // one right answer, so sampling buys nothing and costs two things: the same
            // dictation can come back different twice, and `cleanup_check --cloud`
            // stops being an instrument — a borderline case flips between runs and a
            // prompt change gets credited or blamed for sampling noise. Measured at
            // 0.2: the quoted-cue case failed one run and passed the next with nothing
            // changed in between. Note this makes runs *repeatable*, not bit-identical
            // — batching and kernel nondeterminism upstream are not ours to control.
            "temperature": 0,
            // Scaled to the dictation, never fixed. See `max_tokens_for`: a fixed
            // ceiling does not fail, it truncates the paste mid-sentence.
            "max_tokens": max_tokens_for(raw),
            "messages": messages,
        });
        // Reasoning models think before they answer, and on Groq those hidden tokens
        // come out of the same `max_tokens` allowance *and* the same wall clock that
        // the user is waiting on with the paste blocked. Cleanup is a mechanical
        // rewrite, not a puzzle, so buy none of it.
        //
        // Sent only to the models that accept the parameter. It is not universally
        // ignored — a chat-completions endpoint given a parameter its model does not
        // support answers 400, which would take cleanup down for anyone on the
        // OpenAI or OpenRouter presets.
        let asked_for_low_reasoning =
            takes_reasoning_effort(&self.model) && !REASONING_EFFORT_REFUSED.load(Ordering::Relaxed);
        if asked_for_low_reasoning {
            body["reasoning_effort"] = serde_json::json!("low");
        }

        let resp = self.post(&mut body, asked_for_low_reasoning)?;

        let status = resp.status();
        if !status.is_success() {
            let detail = resp.text().unwrap_or_default();
            anyhow::bail!("OpenAI HTTP {status}: {detail}");
        }

        let v: serde_json::Value = resp.json()?;
        let text = v["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        if text.is_empty() {
            anyhow::bail!("OpenAI returned empty content");
        }
        // Where the completion stopped, not just what it said. `length` means the
        // budget ran out mid-sentence, and the text above is a fragment that would
        // otherwise be pasted as if it were the whole dictation — the gates read a
        // missing tail as a pass, since dropping the last few words is nowhere near
        // the over-deletion threshold. Failing here sends the caller down its
        // fallback chain, and a complete raw transcript beats a clean half of one.
        if v["choices"][0]["finish_reason"].as_str() == Some("length") {
            anyhow::bail!(
                "completion hit the token budget ({} tokens) and was truncated",
                max_tokens_for(raw)
            );
        }
        // Token counts, permanently. "Dictation feels slow" is unattributable
        // without them, and on a reasoning model the invisible half of the bill is
        // the half worth seeing.
        let usage = &v["usage"];
        if let Some(total) = usage["completion_tokens"].as_u64() {
            let reasoning = usage["completion_tokens_details"]["reasoning_tokens"]
                .as_u64()
                .unwrap_or(0);
            eprintln!(
                "[whimpr] cloud cleanup: {} prompt + {total} completion tokens ({reasoning} reasoning)",
                usage["prompt_tokens"].as_u64().unwrap_or(0),
            );
        }
        Ok(text)
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Set once an endpoint has answered 400 to `reasoning_effort`, so the parameter is
/// not sent again for the rest of the run. Deliberately process-wide rather than
/// per-provider: the answer is a property of the endpoint and model, both of which
/// only change when the provider is rebuilt.
static REASONING_EFFORT_REFUSED: AtomicBool = AtomicBool::new(false);

/// Whether this model id accepts `reasoning_effort` on a chat-completions call.
///
/// Deliberately a narrow allowlist rather than "send it and hope": an endpoint
/// given a parameter its model does not support returns 400, so a wrong guess here
/// does not degrade cleanup, it disables it.
fn takes_reasoning_effort(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.contains("gpt-oss") || m.contains("qwen3")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_effort_goes_to_the_models_that_take_it() {
        // The shipped default, and the same family however a vendor namespaces it.
        assert!(takes_reasoning_effort("openai/gpt-oss-20b"));
        assert!(takes_reasoning_effort("gpt-oss-120b"));
        assert!(takes_reasoning_effort("qwen/qwen3-32b"));
    }

    #[test]
    fn reasoning_effort_is_withheld_from_models_that_reject_it() {
        // A 400 from one of these would take cleanup down entirely for anyone on
        // the OpenAI or OpenRouter presets, so the allowlist must stay narrow.
        for m in ["gpt-4o-mini", "gpt-4.1", "claude-haiku-4-5", "llama-3.3-70b-versatile"] {
            assert!(!takes_reasoning_effort(m), "{m}");
        }
    }

    /// No keys is an error the caller can fall back on, not a panic, and the error
    /// reads like the endpoint's own 429 so the fallback chain treats it the same.
    #[test]
    fn no_usable_key_is_a_rate_limit_error() {
        let p = OpenAiProvider::new(vec![], "m");
        let err = p.pick().unwrap_err().to_string();
        assert!(err.contains("429"), "{err}");
        assert!(err.contains("try again in"), "{err}");
    }
}
