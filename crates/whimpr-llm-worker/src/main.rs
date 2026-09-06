//! Local-LLM cleanup worker.
//!
//! Loads a GGUF instruction model once, then serves one request per line of stdin:
//! `{"system": "...", "user": "..."}` → `{"text": "..."}` on stdout. The WhimprFlow
//! app spawns this and keeps it warm so cleanup is fast and fully offline.
//!
//! Usage: `whimpr-llm-worker <model.gguf>` (or WHIMPR_LLM_MODEL env var).

use std::io::{BufRead, Write};
use std::num::NonZeroU32;

use anyhow::Context as _;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::TokenToStringError;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct Msg {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct Request {
    /// Full multi-turn message list (system + few-shot + user). Preferred.
    #[serde(default)]
    messages: Vec<Msg>,
    /// Back-compat single-turn form, used only when `messages` is empty.
    #[serde(default)]
    system: String,
    #[serde(default)]
    user: String,
    #[serde(default = "default_max")]
    max_tokens: i32,
}
/// Only reached by a request that omits the field — the app always sends one, sized
/// to the dictation by `whimpr_core::cleanup::max_tokens_for`. Matches that
/// function's floor so a hand-written request behaves like a real one.
fn default_max() -> i32 {
    768
}

#[derive(Serialize)]
struct Response {
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let model_path = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("WHIMPR_LLM_MODEL").ok())
        .context("model path required (argv[1] or WHIMPR_LLM_MODEL)")?;

    let backend = LlamaBackend::init()?;
    // Offload everything to the Apple GPU (Metal) — capped by what fits.
    let model_params = LlamaModelParams::default().with_n_gpu_layers(999);
    let model = LlamaModel::load_from_file(&backend, &model_path, &model_params)
        .with_context(|| format!("failed to load model {model_path}"))?;
    eprintln!("[llm-worker] model loaded, ready");

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Request>(&line) {
            Ok(req) => match generate(&backend, &model, &req) {
                Ok(text) => Response { text, error: None },
                Err(e) => Response {
                    text: String::new(),
                    error: Some(e.to_string()),
                },
            },
            Err(e) => Response {
                text: String::new(),
                error: Some(format!("bad request: {e}")),
            },
        };
        serde_json::to_writer(&mut stdout, &resp)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }
    Ok(())
}

fn generate(backend: &LlamaBackend, model: &LlamaModel, req: &Request) -> anyhow::Result<String> {
    // Qwen2.5 ChatML template. Prefer the full multi-turn message list (few-shot
    // demonstrations drive the newline/list/self-correction behavior); fall back
    // to the legacy single system+user pair.
    let mut prompt = String::new();
    if req.messages.is_empty() {
        prompt.push_str(&format!(
            "<|im_start|>system\n{}<|im_end|>\n<|im_start|>user\n{}<|im_end|>\n",
            req.system, req.user
        ));
    } else {
        for m in &req.messages {
            prompt.push_str(&format!("<|im_start|>{}\n{}<|im_end|>\n", m.role, m.content));
        }
    }
    prompt.push_str("<|im_start|>assistant\n");

    let tokens = model.str_to_token(&prompt, AddBos::Always)?;
    let n_prompt = tokens.len() as i32;

    // Size the context to this request rather than pinning it at 4096. The prompt is
    // a fixed ~1.5k tokens of system prompt and few-shot turns plus the dictation,
    // and the completion budget now scales with the dictation — so a long utterance
    // can want more than 4096 between them. Too small a context does not error, it
    // quietly evicts the beginning of the prompt, which loses the instructions and
    // the demonstrations while still returning fluent-looking text.
    let n_ctx = (n_prompt + req.max_tokens + 64).clamp(4096, 16_384) as u32;
    // The batch has to hold the whole prompt too: it is submitted in one `decode`,
    // and llama.cpp *aborts the process* — GGML_ASSERT, not an error — when the
    // batch exceeds `n_batch`, which defaults to 2048. The prompt sat just under
    // that until one more rule and one more demonstration pushed a 70-word dictation
    // over it, and every long dictation on the local path then killed the worker
    // mid-request, which the app reads as a dead engine and pastes raw.
    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(n_ctx))
        .with_n_batch(n_ctx);
    let mut ctx = model.new_context(backend, ctx_params)?;

    let mut batch = LlamaBatch::new(n_ctx as usize, 1);
    let last = tokens.len() - 1;
    for (i, tok) in tokens.iter().enumerate() {
        batch.add(*tok, i as i32, &[0], i == last)?;
    }
    ctx.decode(&mut batch)?;

    let mut sampler = LlamaSampler::greedy();
    let mut n_cur = batch.n_tokens();
    let mut out = String::new();
    let limit = n_prompt + req.max_tokens;

    // ONE decoder for the whole generation, not one per token. A multi-byte UTF-8
    // character can straddle two tokens, and only a decoder that carries the partial
    // bytes across the boundary can reassemble it.
    //
    // The bytes are decoded here rather than through the library's `token_to_piece`,
    // which is that same decoder behind a bug: it decodes each token into a String
    // sized to that token's bytes, on the assumption that a byte yields at most one
    // character. A byte that COMPLETES a character the decoder was holding from
    // earlier tokens yields the whole character, which does not fit, and encoding_rs
    // reports the output as full and drops it. Qwen3-4B emits a 4-byte emoji as
    // separate byte tokens, so every one of them vanished — measured: "hilarious 😂
    // see you 🎉 thanks ❤️ ok 🤦‍♂️" came back as "hilarious  see you  thanks ❤️ ok ♂️",
    // every astral-plane character gone and the BMP ones intact. The earlier note
    // here claiming emoji round-tripped cleanly was measured on characters that
    // happen to fit in one token. Reserving what the decoder says it may need is
    // the whole fix.
    let mut decoder = encoding_rs::UTF_8.new_decoder();

    while n_cur <= limit {
        let token = sampler.sample(&ctx, batch.n_tokens() - 1);
        sampler.accept(token);
        if model.is_eog_token(token) {
            break;
        }
        // `true` = render special tokens, matching the old Special::Tokenize.
        let bytes = match model.token_to_piece_bytes(token, 8, true, None) {
            // A negative size is the size that would have been needed.
            Err(TokenToStringError::InsufficientBufferSpace(need)) => {
                model.token_to_piece_bytes(token, need.unsigned_abs() as usize, true, None)?
            }
            other => other?,
        };
        let need = decoder
            .max_utf8_buffer_length(bytes.len())
            .unwrap_or(bytes.len() * 4 + 16);
        out.reserve(need);
        // With the reservation above the result can only be InputEmpty.
        let _ = decoder.decode_to_string(&bytes, &mut out, false);
        batch.clear();
        batch.add(token, n_cur, &[0], true)?;
        n_cur += 1;
        ctx.decode(&mut batch)?;
    }
    Ok(out.trim().to_string())
}
