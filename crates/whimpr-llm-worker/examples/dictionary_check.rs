//! Does the personal dictionary actually change what gets pasted?
//!
//! The unit tests in `whimpr-core` prove `prefilter` SELECTS the right entries, and
//! that `assemble_user_message` puts them in the prompt. Neither proves the thing
//! that actually matters: that the local model, given that prompt, really replaces
//! the mis-heard spelling. Only running the model can show that — so this drives the
//! exact production chain end to end:
//!
//!   DictionaryStore::prefilter -> CleanupContext.vocab -> build_messages
//!     -> the real whimpr-llm-worker process -> cleaned text
//!
//! Each case is run twice, once WITH the dictionary and once WITHOUT, because a pass
//! only means something if the same transcript fails without the entry. A model that
//! already knew the spelling would otherwise look like a working dictionary.
//!
//! Usage (model path optional; defaults to the installed one):
//!   cargo run -p whimpr-llm-worker --example dictionary_check --release [-- <model.gguf>]

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use whimpr_core::cleanup::{build_messages, CleanupContext, CleanupLevel};
use whimpr_core::dictionary::{DictSource, DictionaryStore};

/// A transcript as Whisper would hand it over, plus the spelling we expect back.
struct Case {
    name: &'static str,
    /// (correct spelling, known mis-hearings) to put in the dictionary.
    entry: (&'static str, &'static [&'static str]),
    /// What the user said, as mis-transcribed.
    transcript: &'static str,
    /// The spelling that must appear in the cleaned output.
    expect: &'static str,
}

const CASES: &[Case] = &[
    Case {
        name: "name mis-heard as a common word",
        entry: ("Manvi", &["Monvi", "Manvee"]),
        transcript: "hey can you send the deck over to monvi before the standup",
        expect: "Manvi",
    },
    Case {
        name: "product name split into two words",
        entry: ("ChargeBee", &["charge bee"]),
        transcript: "we should renew charge bee this month before it lapses",
        expect: "ChargeBee",
    },
    Case {
        name: "surname with unusual spelling",
        entry: ("Sankaranarayanan", &["sankara narayanan", "shankar narayanan"]),
        transcript: "the report was written by vinayak sankara narayanan last week",
        expect: "Sankaranarayanan",
    },
];

fn main() -> anyhow::Result<()> {
    let model = std::env::args().nth(1).unwrap_or_else(default_model);
    if !std::path::Path::new(&model).exists() {
        anyhow::bail!("model not found: {model}");
    }
    let worker = worker_binary();
    println!("worker: {worker}\nmodel:  {model}\n");

    let mut child = Command::new(&worker)
        .arg(&model)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));

    let mut passed = 0usize;
    let mut failed = 0usize;

    for case in CASES {
        // WITH the dictionary: the entry must be selected, and the model must use it.
        let mut store = DictionaryStore::default();
        store.add(
            case.entry.0,
            case.entry.1.iter().map(|s| s.to_string()).collect(),
            DictSource::Manual,
        );
        let vocab = store.prefilter(case.transcript, 15);

        println!("── {} ──", case.name);
        println!("   said:      {}", case.transcript);
        if vocab.is_empty() {
            println!("   FAIL: prefilter selected nothing — the model never sees the entry");
            failed += 1;
            continue;
        }
        println!("   prefilter: {}", vocab.iter().map(|v| v.correct.as_str()).collect::<Vec<_>>().join(", "));

        let with = cleanup(&mut stdin, &mut stdout, case.transcript, vocab)?;
        let without = cleanup(&mut stdin, &mut stdout, case.transcript, Vec::new())?;

        println!("   with:      {with}");
        println!("   without:   {without}");

        let hit = with.contains(case.expect);
        // If the bare model already spells it right, the case proves nothing about
        // the dictionary — say so rather than banking an undeserved pass.
        let baseline = without.contains(case.expect);
        match (hit, baseline) {
            (true, false) => {
                println!("   PASS — dictionary supplied the spelling\n");
                passed += 1;
            }
            (true, true) => {
                println!("   PASS (weak) — model already knew it; case proves nothing\n");
                passed += 1;
            }
            (false, _) => {
                println!("   FAIL — expected {:?} in the cleaned output\n", case.expect);
                failed += 1;
            }
        }
    }

    drop(stdin);
    let _ = child.wait();

    println!("{passed} passed, {failed} failed");
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// One cleanup round-trip through the worker, using the production prompt builder.
fn cleanup(
    stdin: &mut std::process::ChildStdin,
    stdout: &mut BufReader<std::process::ChildStdout>,
    raw: &str,
    vocab: Vec<whimpr_core::cleanup::VocabEntry>,
) -> anyhow::Result<String> {
    let ctx = CleanupContext { level: CleanupLevel::Light, vocab, ..Default::default() };
    let messages: Vec<serde_json::Value> = build_messages(raw, &ctx)
        .into_iter()
        .map(|m| serde_json::json!({ "role": m.role, "content": m.content }))
        .collect();
    let req = serde_json::json!({ "messages": messages, "max_tokens": 400 });

    writeln!(stdin, "{req}")?;
    stdin.flush()?;

    let mut line = String::new();
    stdout.read_line(&mut line)?;
    let resp: serde_json::Value = serde_json::from_str(&line)?;
    if let Some(err) = resp.get("error").and_then(|e| e.as_str()) {
        anyhow::bail!("worker error: {err}");
    }
    Ok(resp.get("text").and_then(|t| t.as_str()).unwrap_or_default().trim().to_string())
}

fn default_model() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    format!("{home}/Library/Application Support/WhimprFlow/models/qwen3-4b-instruct-2507-q4_k_m.gguf")
}

/// Prefer the release build next to this example; fall back to debug.
fn worker_binary() -> String {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    for profile in ["release", "debug"] {
        let p = format!("{root}/target/{profile}/whimpr-llm-worker");
        if std::path::Path::new(&p).exists() {
            return p;
        }
    }
    "whimpr-llm-worker".to_string()
}
