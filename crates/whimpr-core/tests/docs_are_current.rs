//! Keeps `docs/ARCHITECTURE.md` honest by testing it against the code.
//!
//! Documentation rots because nothing fails when it does. These tests fail. If you
//! rename a crate, retune a timing constant, or change which model files are looked
//! for, the suite goes red until the doc is updated to match — so the doc can be
//! trusted as a source of truth rather than a snapshot of what was true once.
//!
//! Deliberately narrow: only facts that are (a) mechanically checkable and (b) drift
//! silently. Prose is not tested; it is reviewed.

use std::collections::BTreeSet;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn architecture_doc() -> String {
    let p = repo_root().join("docs/ARCHITECTURE.md");
    std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Every crate is described, and every crate the doc describes exists.
#[test]
fn architecture_lists_every_crate() {
    let doc = architecture_doc();

    let on_disk: BTreeSet<String> = std::fs::read_dir(repo_root().join("crates"))
        .expect("crates/ is readable")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();

    let missing: Vec<&String> = on_disk.iter().filter(|c| !doc.contains(c.as_str())).collect();
    assert!(
        missing.is_empty(),
        "docs/ARCHITECTURE.md does not mention these crates: {missing:?}.\n\
         Add them to the crate table (or delete the crate)."
    );

    // And the reverse: a crate named in the doc that no longer exists is a stale claim.
    for token in doc.split(|c: char| !(c.is_alphanumeric() || c == '-')) {
        if token.starts_with("whimpr-") && !on_disk.contains(token) && token != "whimpr-tauri" {
            panic!(
                "docs/ARCHITECTURE.md refers to crate {token:?}, which is not in crates/.\n\
                 Either the crate was renamed or removed — update the doc."
            );
        }
    }
}

/// The timing numbers in the doc are the constants the state machine actually uses.
#[test]
fn architecture_quotes_real_timing_constants() {
    let doc = architecture_doc();
    let src = read("crates/whimpr-core/src/state/timing.rs");

    // (constant, how the doc is expected to phrase its value)
    let checks: &[(&str, fn(u64) -> String)] = &[
        ("HOLD_MIN_MS", |v| format!("{v} ms")),
        ("DOUBLE_TAP_MS", |v| format!("{v} ms")),
        ("COOLDOWN_MS", |v| format!("{v} ms")),
        ("SESSION_CAP_MS", |v| format!("{} min", v / 60_000)),
        ("WARN_AT_MS", |v| format!("{}", v / 60_000)),
    ];

    for (name, phrase) in checks {
        let value = const_value(&src, name)
            .unwrap_or_else(|| panic!("{name} not found in state/timing.rs"));
        let expected = phrase(value);
        assert!(
            doc.contains(&expected),
            "docs/ARCHITECTURE.md is stale: {name} is {value} (expected the doc to say {expected:?})."
        );
    }
}

/// Model filenames the app searches for are the ones the doc tells you to install.
/// Getting this wrong sends someone to download the wrong multi-GB file.
#[test]
fn architecture_quotes_real_model_filenames() {
    let doc = architecture_doc();

    for (source, marker) in [
        ("src-tauri/src/local_llm.rs", ".gguf"),
        ("src-tauri/src/hotkey.rs", "ggml-"),
    ] {
        for name in quoted_filenames(&read(source), marker) {
            // The doc may write whisper models without the .bin suffix; compare stems.
            let stem = name.trim_end_matches(".bin");
            assert!(
                doc.contains(stem),
                "docs/ARCHITECTURE.md does not mention model {name:?} from {source}.\n\
                 The install instructions would send someone after the wrong file."
            );
        }
    }
}

/// `pub const NAME: u64 = 12 * 34;` -> 408
fn const_value(src: &str, name: &str) -> Option<u64> {
    let line = src.lines().find(|l| l.contains(&format!("const {name}")))?;
    let rhs = line.split('=').nth(1)?.split(';').next()?;
    rhs.split('*')
        .map(|p| p.trim().parse::<u64>().ok())
        .try_fold(1u64, |acc, v| Some(acc * v?))
}

/// Double-quoted literals in `src` that look like model files.
fn quoted_filenames(src: &str, marker: &str) -> Vec<String> {
    src.split('"')
        .skip(1)
        .step_by(2)
        .filter(|s| s.contains(marker) && !s.contains('{'))
        .map(|s| s.to_string())
        .collect()
}
