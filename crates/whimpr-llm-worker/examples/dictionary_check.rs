//! Does the personal dictionary actually change what gets pasted?
//!
//! The unit tests in `whimpr-core` prove `prefilter` SELECTS the right entries and
//! that `assemble_user_message` puts them in the prompt. Neither proves the thing
//! that matters: that the local model, given that prompt, really replaces the
//! mis-heard spelling — and that the result then survives the gates. So this drives
//! the exact production chain end to end:
//!
//!   DictionaryStore::prefilter -> CleanupContext.vocab -> build_messages
//!     -> the real whimpr-llm-worker process -> post_process -> de_dash
//!     -> gates::evaluate -> apply_listed_mishears -> force_lowercase (Messaging)
//!     -> the text that would actually be pasted
//!
//! Three things this harness insists on, each because leaving it out lets a green
//! run mean nothing:
//!
//! 1. **It asserts on the PASTED text, not the model's reply.** A cleanup the gates
//!    reject is discarded and the raw transcript is pasted instead, mishear intact.
//!    An earlier version of this file stopped at the model's reply and so could not
//!    see that a dictionary fix in a short dictation was being thrown away.
//! 2. **Every case runs twice, with and without the entry.** A pass only means
//!    something if the same transcript fails without the dictionary; a model that
//!    already knew the spelling would otherwise look like a working dictionary.
//! 3. **Negative cases.** Precision is the whole reason `prefilter` exists, so some
//!    cases assert the dictionary stays out of the way: a similar-sounding ordinary
//!    word must not be "corrected" into a name.
//!
//! Sampling is greedy (see `LlamaSampler::greedy` in the worker), so re-running one
//! case is pointless — it returns the same tokens. Variance lives in the phrasing
//! instead, which is why cases vary sentence position and length rather than repeat.
//!
//! Usage (model path optional; defaults to the installed one):
//!   cargo run -p whimpr-llm-worker --example dictionary_check --release [-- <model.gguf>]
//!   cargo run -p whimpr-llm-worker --example dictionary_check --release -- --audit
//!   cargo run -p whimpr-llm-worker --example dictionary_check --release -- --messaging
//!
//! `--audit` skips the model entirely and reports on *your* real dictionary.json:
//! which entries can never be selected, and which are risky enough to over-fire.
//!
//! `--messaging` runs every case at `CleanupLevel::Messaging` instead of Light. The
//! dictionary must keep working there: the level lowercases its authoritative
//! spelling, so the expectation is matched case-insensitively, but a dropped
//! correction is still a failure.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use whimpr_core::cleanup::{
    build_messages, de_dash, evaluate_gates, messaging_style, post_process, CleanupContext,
    CleanupLevel, GateVerdict, VocabEntry,
};
use whimpr_core::dictionary::{DictSource, DictionaryStore, MAX_NORMALIZED_DISTANCE};

/// What a case is trying to prove.
#[derive(PartialEq, Clone, Copy)]
enum Kind {
    /// The dictionary spelling must appear in the pasted text.
    Corrects,
    /// The dictionary must stay out of the way: `expect` must NOT appear, because
    /// the speaker said an ordinary word that merely sounds close to an entry.
    LeavesAlone,
}

/// A transcript as Whisper would hand it over, plus what must happen to it.
struct Case {
    name: &'static str,
    /// (correct spelling, known mis-hearings) to put in the dictionary. More than one
    /// when the sentence needs more than one fix.
    entries: &'static [(&'static str, &'static [&'static str])],
    /// What the user said, as mis-transcribed.
    transcript: &'static str,
    /// The spelling that must appear (Corrects) or must not appear (LeavesAlone).
    expect: &'static str,
    kind: Kind,
}

const MANVI: (&str, &[&str]) = ("Manvi", &["Monvi", "Manvee"]);
const CHARGEBEE: (&str, &[&str]) = ("ChargeBee", &["charge bee"]);
const GEETHA: (&str, &[&str]) = ("Geetha", &["Gita", "Geeta"]);
const ABISHEK: (&str, &[&str]) = ("Abishek", &["Abhishek"]);

const CASES: &[Case] = &[
    Case {
        name: "name mis-heard as a common word",
        entries: &[MANVI],
        transcript: "hey can you send the deck over to monvi before the standup",
        expect: "Manvi",
        kind: Kind::Corrects,
    },
    Case {
        name: "product name split into two words",
        entries: &[CHARGEBEE],
        transcript: "we should renew charge bee this month before it lapses",
        expect: "ChargeBee",
        kind: Kind::Corrects,
    },
    Case {
        name: "surname with unusual spelling",
        entries: &[("Sankaranarayanan", &["sankara narayanan", "shankar narayanan"])],
        transcript: "the report was written by vinayak sankara narayanan last week",
        expect: "Sankaranarayanan",
        kind: Kind::Corrects,
    },
    // Short dictations are where a correction is most likely to be discarded by the
    // novelty gate, because one changed word out of two is a huge edit ratio. This is
    // the shape that used to silently paste raw.
    Case {
        name: "short dictation — one fix is most of the sentence",
        entries: &[MANVI],
        transcript: "thanks monvi",
        expect: "Manvi",
        kind: Kind::Corrects,
    },
    Case {
        name: "two fixes in one short sentence",
        entries: &[MANVI, CHARGEBEE],
        transcript: "ask monvi about charge bee",
        expect: "ChargeBee",
        kind: Kind::Corrects,
    },
    // The model's blind spot, and why `apply_listed_mishears` exists. It substitutes a
    // mishear that looks like a mistake and refuses one that looks like a real name —
    // "Geeta" is a perfectly good spelling, so the precision guard in the vocabulary
    // block tells it to leave the word alone. Strengthening that block does not move
    // it. Only the deterministic pass makes this case land.
    Case {
        name: "listed mishear that is itself a plausible spelling",
        entries: &[GEETHA],
        transcript: "hey geeta how's it going",
        expect: "Geetha",
        kind: Kind::Corrects,
    },
    Case {
        name: "two plausible-looking mishears in one sentence",
        entries: &[GEETHA, ABISHEK],
        transcript: "my brother's name is Abhishek and my mom's name is Geeta",
        expect: "Geetha",
        kind: Kind::Corrects,
    },
    // ── Precision: the dictionary must not invent corrections ──────────────────
    Case {
        name: "ordinary word that sounds like an entry",
        entries: &[MANVI],
        transcript: "we do not have that much money left in the budget",
        expect: "Manvi",
        kind: Kind::LeavesAlone,
    },
    Case {
        name: "entry's words used in their ordinary sense",
        entries: &[CHARGEBEE],
        transcript: "did you charge the battery before the flight",
        expect: "ChargeBee",
        kind: Kind::LeavesAlone,
    },
];

/// Unrelated entries loaded alongside the case's own, so the model is choosing from
/// a realistic dictionary rather than a single unambiguous hint. `prefilter` should
/// keep most of these out of the prompt; the ones it lets through are exactly the
/// distractors a real user's dictionary would produce.
const DISTRACTORS: &[&str] = &[
    "Kubernetes", "Vercel", "Datadog", "Grafana", "Postgres", "Redis", "Anthropic",
    "Snowflake", "Databricks", "Terraform", "Cloudflare", "Segment", "Amplitude",
    "Mixpanel", "Retool", "Linear", "Notion", "Figma", "Sentry", "PagerDuty",
    "Aarav", "Priya", "Rohan", "Ishaan", "Ananya", "Kavya", "Nikhil", "Meera",
    "Siobhan", "Caoimhe", "Bjorn", "Anaïs", "Mateus", "Yuki", "Wei", "Zhang",
];

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--audit") {
        return audit_real_dictionary();
    }

    // Messaging is the level where the chain has the most to get wrong: the
    // dictionary writes a capital and the level takes it away again.
    let level = if args.iter().any(|a| a == "--messaging") {
        CleanupLevel::Messaging
    } else {
        CleanupLevel::Light
    };
    let model = args
        .into_iter()
        .find(|a| !a.starts_with("--"))
        .unwrap_or_else(default_model);
    if !std::path::Path::new(&model).exists() {
        anyhow::bail!("model not found: {model}");
    }
    let worker = worker_binary();
    println!("worker: {worker}\nmodel:  {model}\nlevel:  {level:?}\n");

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
    let mut weak = 0usize;

    for case in CASES {
        // A realistic store: the case's entries plus a pile of unrelated ones.
        let mut store = DictionaryStore::default();
        for (correct, mishears) in case.entries {
            store.add(
                *correct,
                mishears.iter().map(|s| s.to_string()).collect(),
                DictSource::Manual,
            );
        }
        for d in DISTRACTORS {
            store.add(*d, Vec::new(), DictSource::Manual);
        }
        let vocab = store.prefilter(case.transcript, 15);

        println!("── {} ──", case.name);
        println!("   said:      {}", case.transcript);
        println!(
            "   prefilter: {}",
            if vocab.is_empty() {
                "(nothing selected)".to_string()
            } else {
                vocab.iter().map(|v| v.correct.as_str()).collect::<Vec<_>>().join(", ")
            }
        );

        if case.kind == Kind::Corrects && vocab.is_empty() {
            println!("   FAIL: prefilter selected nothing — the model never sees the entry\n");
            failed += 1;
            continue;
        }

        // "without" gets no dictionary at all — no prompt entries and no deterministic
        // pass — so a pass only counts when the same transcript fails bare.
        let with = run(&mut stdin, &mut stdout, case.transcript, vocab.clone(), &store, level)?;
        let without = run(
            &mut stdin,
            &mut stdout,
            case.transcript,
            Vec::new(),
            &DictionaryStore::default(),
            level,
        )?;

        println!("   with:      {}{}", with.pasted, gate_note(&with));
        println!("   without:   {}{}", without.pasted, gate_note(&without));

        match case.kind {
            Kind::Corrects => {
                let hit = shows(&with.pasted, case.expect, level);
                let baseline = shows(&without.pasted, case.expect, level);
                let kept = keeps_the_sentence(case.transcript, &with.pasted, case);
                if !hit {
                    println!("   FAIL — expected {:?} in the pasted text\n", case.expect);
                    failed += 1;
                } else if !kept {
                    // Containing the name is not enough: a model that replied with the
                    // bare name and dropped the sentence would otherwise pass.
                    println!("   FAIL — {:?} is there but the rest of the sentence is not\n", case.expect);
                    failed += 1;
                } else if baseline {
                    println!("   PASS (weak) — model already knew it; case proves nothing\n");
                    passed += 1;
                    weak += 1;
                } else {
                    println!("   PASS — dictionary supplied the spelling\n");
                    passed += 1;
                }
            }
            Kind::LeavesAlone => {
                if shows(&with.pasted, case.expect, level) {
                    println!(
                        "   FAIL — dictionary over-fired: {:?} appeared in ordinary speech\n",
                        case.expect
                    );
                    failed += 1;
                } else {
                    println!("   PASS — left alone, as it should be\n");
                    passed += 1;
                }
            }
        }
    }

    drop(stdin);
    let _ = child.wait();

    println!("{passed} passed, {failed} failed{}", if weak > 0 { format!(" ({weak} weak)") } else { String::new() });
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// What the app would actually have pasted, and why.
struct Outcome {
    pasted: String,
    /// Set when the gates rejected the cleanup, so the raw transcript was pasted.
    rejected: Option<String>,
    /// True when the deterministic listed-mishear pass, not the model, made the fix.
    /// Worth surfacing: it means the prompt alone would not have produced this.
    deterministic: bool,
}

fn gate_note(o: &Outcome) -> String {
    let mut s = match &o.rejected {
        Some(why) => format!("   ⟵ GATE REJECTED ({why}), raw pasted"),
        None => String::new(),
    };
    if o.deterministic {
        s.push_str("   ⟵ fixed by the listed-mishear pass, not the model");
    }
    s
}

/// One cleanup round-trip through the worker, followed by the same post-processing
/// and gating the app applies — so `pasted` is the string that would reach the cursor.
fn run(
    stdin: &mut std::process::ChildStdin,
    stdout: &mut BufReader<std::process::ChildStdout>,
    raw: &str,
    vocab: Vec<VocabEntry>,
    dict: &DictionaryStore,
    level: CleanupLevel,
) -> anyhow::Result<Outcome> {
    let ctx = CleanupContext { level, vocab: vocab.clone(), ..Default::default() };
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
    let cleaned = de_dash(&post_process(
        resp.get("text").and_then(|t| t.as_str()).unwrap_or_default().trim(),
    ));
    let raw_out = post_process(raw);

    let (gated, rejected) = match evaluate_gates(&raw_out, &cleaned, level, &vocab) {
        GateVerdict::Pass => (cleaned, None),
        GateVerdict::Fail(reason) => (raw_out, Some(format!("{reason:?}"))),
    };
    // Same last two steps as the app: whatever survived the gates, the mishears the
    // user listed are enforced on it, and only then does Messaging lowercase it —
    // the dictionary writes a capitalized spelling, so the order is load-bearing.
    let fixed = dict.apply_listed_mishears(&gated);
    let pasted = if level.forces_lowercase() { messaging_style(&fixed) } else { fixed.clone() };
    Ok(Outcome {
        deterministic: fixed != gated,
        pasted,
        rejected,
    })
}

/// Did the output keep the sentence, or just the name? Containing the expected
/// spelling is not enough on its own: a model that replied with the bare name and
/// threw the sentence away would pass a `contains` check.
///
/// Words the dictionary was *meant* to change are excluded from both sides of the
/// ratio — the mishears, and the words of the correct spelling itself. Counting a
/// mishear as a word that must survive asks the fix not to happen.
/// Did the authoritative spelling reach the cursor? Messaging lowercases the whole
/// paste, names included, so at that level "manvi" is the fix landing, not missing.
fn shows(pasted: &str, expect: &str, level: CleanupLevel) -> bool {
    if level.forces_lowercase() {
        pasted.to_lowercase().contains(&expect.to_lowercase())
    } else {
        pasted.contains(expect)
    }
}

fn keeps_the_sentence(raw: &str, out: &str, case: &Case) -> bool {
    const FILLERS: &[&str] = &["um", "uh", "er", "like", "you", "know", "i", "mean", "so"];

    // Everything the dictionary is entitled to rewrite, as lowercase word fragments.
    let mut rewritable: Vec<String> = Vec::new();
    for (correct, mishears) in case.entries {
        rewritable.push(correct.to_lowercase());
        rewritable.extend(mishears.iter().map(|m| m.to_lowercase()));
    }
    let is_rewritable =
        |w: &str| rewritable.iter().any(|r| r.contains(w) || r.split_whitespace().any(|p| p == w));

    let out_lc = out.to_lowercase();
    let checkable: Vec<String> = raw
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| c.is_ascii_punctuation()).to_lowercase())
        .filter(|w| w.len() > 2 && !FILLERS.contains(&w.as_str()))
        .filter(|w| !is_rewritable(w))
        .collect();
    if checkable.is_empty() {
        return true;
    }
    let kept = checkable.iter().filter(|w| out_lc.contains(w.as_str())).count();
    // Cleanup legitimately drops the odd word; losing a quarter of them is a rewrite.
    kept * 4 >= checkable.len() * 3
}

/// Report on the user's real dictionary without touching the model. Pure `prefilter`
/// logic, so it is instant — and it answers the question people actually have, which
/// is "why didn't it catch my name?".
fn audit_real_dictionary() -> anyhow::Result<()> {
    let path = support_dir().join("dictionary.json");
    let store = DictionaryStore::load(&path);
    println!("dictionary: {}\n", path.display());
    if store.entries.is_empty() {
        println!("No entries yet — add one in the Hub, or let auto-learn find one.");
        return Ok(());
    }

    let mut at_risk = 0usize;
    for e in &store.entries {
        // A listed mishear always matches itself, so listed spellings are covered by
        // construction. The interesting question is what happens to an UNLISTED one:
        // only mishears within the prefilter's edit-distance radius of the correct
        // spelling will be selected, and a short or unusual name has a tiny radius.
        let len = e.correct.chars().count();
        let radius = (len as f32 * MAX_NORMALIZED_DISTANCE).floor() as usize;
        let tag = if e.mishears.is_empty() && radius < 2 {
            at_risk += 1;
            "  ⚠ no mishears listed and too short to match a loose guess"
        } else if e.mishears.is_empty() {
            at_risk += 1;
            "  ⚠ no mishears listed — only close guesses will be caught"
        } else {
            ""
        };
        println!(
            "  {:<28} {} mishear(s), tolerates ~{radius} wrong letter(s){tag}",
            e.correct,
            e.mishears.len()
        );
    }

    println!("\n{} entries, {at_risk} worth adding a mishear to.", store.entries.len());
    println!(
        "Tip: the surest fix is to add what Whisper actually wrote. Dictate the word, look at\n\
         what landed, and paste that in as a mishear — guessing the spelling is the hard way.\n\
         A listed mishear is then applied verbatim; only unlisted guesses depend on the model."
    );
    Ok(())
}

fn support_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::PathBuf::from(home).join("Library/Application Support/WhimprFlow")
}

fn default_model() -> String {
    support_dir()
        .join("models/qwen3-4b-instruct-2507-q4_k_m.gguf")
        .to_string_lossy()
        .into_owned()
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
