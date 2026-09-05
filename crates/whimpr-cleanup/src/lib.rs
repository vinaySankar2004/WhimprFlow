//! Cloud cleanup. One provider, speaking the OpenAI chat-completions wire format —
//! which Groq (the default), OpenRouter, Gemini's compatibility endpoint and OpenAI
//! itself all accept, so switching vendor is a base URL and not new code. It sends
//! the shared WhimprFlow system prompt plus the assembled context and returns cleaned
//! text. On failure the caller retries on the local model — cleanup is an
//! enhancement, never a gate.

use std::time::Duration;

use whimpr_core::cleanup::{build_messages, CleanupContext, CleanupProvider, ProviderId};

/// Default OpenAI Chat Completions endpoint.
const OPENAI_DEFAULT_URL: &str = "https://api.openai.com/v1/chat/completions";

/// Cleanup via the OpenAI Chat Completions API — or any OpenAI-compatible
/// endpoint (OpenRouter, a local server, etc.) when `base_url` is set.
/// OpenRouter in particular speaks this exact wire format at
/// `https://openrouter.ai/api/v1/chat/completions`.
pub struct OpenAiProvider {
    client: reqwest::blocking::Client,
    api_key: String,
    model: String,
    /// Full chat-completions URL. Defaults to OpenAI's when empty.
    url: String,
}

impl OpenAiProvider {
    pub fn new(api_key: String, model: impl Into<String>) -> Self {
        Self::with_base_url(api_key, model, None)
    }

    /// `base_url` is the API root (e.g. `https://openrouter.ai/api/v1`), without
    /// the `/chat/completions` suffix. `None` or empty uses OpenAI directly.
    pub fn with_base_url(
        api_key: String,
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
            api_key,
            model: model.into(),
            url,
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
        let body = serde_json::json!({
            "model": self.model,
            "temperature": 0.2,
            "max_tokens": 512,
            "messages": messages,
        });

        let resp = self
            .client
            .post(&self.url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()?;

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
        Ok(text)
    }
}
