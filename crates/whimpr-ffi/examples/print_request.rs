//! Print the exact chat-completions body the apps send, so it can be replayed with
//! curl without going through a build.
//!
//! Both shells send the same messages — that is the whole point of the shared core —
//! so a request reproduced here is byte-for-byte what iOS posts, minus the key.
//!
//! ```bash
//! cargo run -p whimpr-ffi --example print_request > /tmp/req.json
//! curl -s -w '\n%{http_code}\n' https://api.groq.com/openai/v1/chat/completions \
//!   -H "Authorization: Bearer $GROQ_KEY" -H 'Content-Type: application/json' \
//!   --data-binary @/tmp/req.json
//! ```
//!
//! Arguments: `[transcript] [level]`, defaulting to the string the app's connection
//! check uses and `light`.

use whimpr_core::cleanup::CleanupLevel;
use whimpr_core::dictionary::DictionaryStore;
use whimpr_core::pipeline;
use whimpr_core::settings::GROQ_MODEL;

fn main() {
    let mut args = std::env::args().skip(1);
    let raw = args.next().unwrap_or_else(|| "testing one two three".to_string());
    let level = match args.next().as_deref() {
        Some("messaging") => CleanupLevel::Messaging,
        Some("none") => CleanupLevel::None,
        _ => CleanupLevel::Light,
    };

    let prep = pipeline::prepare(&raw, level, &DictionaryStore::default(), None);
    let messages: Vec<serde_json::Value> = prep
        .messages
        .iter()
        .map(|m| serde_json::json!({ "role": m.role, "content": m.content }))
        .collect();

    // Mirrors `whimpr-cleanup`'s body and the iOS client's. `reasoning_effort` is
    // sent for gpt-oss models; drop it by hand when bisecting.
    let body = serde_json::json!({
        "model": GROQ_MODEL,
        "temperature": 0,
        "max_tokens": prep.max_tokens,
        "reasoning_effort": "low",
        "messages": messages,
    });

    println!("{}", serde_json::to_string(&body).unwrap());
    eprintln!(
        "-> {} messages, {} bytes, max_tokens {}",
        prep.messages.len(),
        serde_json::to_string(&body).unwrap().len(),
        prep.max_tokens
    );
}
