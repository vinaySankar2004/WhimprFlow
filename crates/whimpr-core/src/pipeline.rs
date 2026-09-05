//! The deterministic passes that run *around* a cleanup provider, in the one order
//! that is correct.
//!
//! This used to live in `src-tauri/src/hotkey.rs`, which was fine while macOS was
//! the only shell. It is not fine with a second one: the sequence below is
//! load-bearing in several places where getting it wrong is silent, and a second
//! platform re-deriving it from the same docs is exactly how the two drift.
//!
//! The order, and why each step sits where it does:
//!
//! **Before the model** — [`prepare`]
//! 1. `pre_normalize_layout` turns spoken layout cues ("new paragraph") into opaque
//!    markers. The model passes a marker through reliably and mangles the literal
//!    words, so it must see the marked-up text.
//! 2. `post_process` on that same text produces `raw_fallback`: markers restored to
//!    real breaks. This — not the caller's original string — is what the gate
//!    compares against and what gets pasted when anything fails, so no `[[NL]]`
//!    token can ever reach the cursor and no explicit break is lost.
//! 3. The dictionary is prefiltered against the *marked-up* text, and the resulting
//!    vocab is carried in [`Prepared`] so the gate later sees the same list the
//!    prompt did.
//!
//! **After the model** — [`finish`]
//! 4. `post_process` again, catching any cue the model missed.
//! 5. `de_dash`, then 6. `strip_parenthetical_fillers` — both *before* the gate, so
//!    what the gate judges is byte-for-byte what gets pasted.
//! 7. The gate. Failing it pastes `raw_fallback`.
//! 8. `apply_listed_mishears` — after the gate, and on the raw path too.
//! 9. `messaging_style` — last, and only when the level forces lowercase and the
//!    user did not explicitly ask for Raw. It must follow step 8: the dictionary
//!    writes the capitalized authoritative spelling, so lowercasing earlier leaves a
//!    corrected name as the one capital in the message.
//!
//! Steps 8 and 9 run on whatever text is about to be pasted, cleaned or raw. Note
//! the asymmetry in 9: a *gate-rejected* dictation is still styled, because the user
//! chose the Messaging register and only cleanup failed. Raw *mode* is not styled,
//! because there the user asked for verbatim.

use serde::{Deserialize, Serialize};

use crate::cleanup::{
    self, build_messages, evaluate_gates, max_tokens_for, CleanupContext, CleanupLevel, CleanupMsg,
    GateVerdict, VocabEntry,
};
use crate::dictionary::DictionaryStore;

/// Which engine produced the text that is about to be pasted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    Cloud,
    Local,
    Raw,
}

impl Engine {
    /// The label used in logs and in `SessionRecord`.
    pub fn as_str(self) -> &'static str {
        match self {
            Engine::Cloud => "cloud",
            Engine::Local => "local",
            Engine::Raw => "raw",
        }
    }
}

/// Everything computed from the raw transcript before a provider is called.
///
/// Serializable in full because a non-Rust shell (the iOS app) makes the HTTP call
/// itself: it receives this, sends `messages`, and hands the result back to
/// [`finish`] along with this same value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Prepared {
    /// The transcript with layout markers inserted — what the model must see.
    pub model_input: String,
    /// Markers restored to real breaks — the gate baseline and the raw fallback.
    pub raw_fallback: String,
    /// The context the prompt was built from, carried whole so [`finish`] cannot be
    /// told a different level than the prompt used, and so the gate is handed the
    /// same vocab the model saw. Also what a Rust shell passes straight to
    /// [`crate::cleanup::CleanupProvider::cleanup`].
    pub ctx: CleanupContext,
    /// The chat turns to send verbatim.
    pub messages: Vec<CleanupMsg>,
    /// Token budget, scaled to the input — a fixed budget truncates the paste.
    pub max_tokens: u32,
}

impl Prepared {
    pub fn level(&self) -> CleanupLevel {
        self.ctx.level
    }

    pub fn vocab(&self) -> &[VocabEntry] {
        &self.ctx.vocab
    }
}

/// The text to paste, plus which engine produced it and why that was not the one
/// the user selected.
///
/// The attribution is not decoration. Every degradation in this app is deliberately
/// silent — falling back is what keeps the dictation alive — so a run of raw or
/// oddly-slow pastes has no explanation unless the reason was written down as it
/// happened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finished {
    pub text: String,
    pub engine: Engine,
    pub degraded: Option<String>,
}

/// Steps 1-3: everything the provider needs, computed from the raw transcript.
///
/// `app_bundle_id` is the focused app, for light tone adaptation; `None` on shells
/// that cannot see it.
///
/// The dictionary is prefiltered here rather than handed down from an earlier
/// ASR-side pass: that one filtered against the *unprompted* transcript, and by now
/// the text may have been improved, layout markers inserted, or the prompted pass
/// rejected. Each stage picking vocab for the text it is actually about to send is
/// cheaper than reasoning about which earlier transcript the list came from.
pub fn prepare(
    raw: &str,
    level: CleanupLevel,
    dict: &DictionaryStore,
    app_bundle_id: Option<String>,
) -> Prepared {
    let model_input = cleanup::pre_normalize_layout(raw);
    let raw_fallback = cleanup::post_process(&model_input);
    let ctx = CleanupContext {
        level,
        vocab: dict.prefilter(&model_input, 15),
        app_bundle_id,
        ..Default::default()
    };
    let messages = build_messages(&model_input, &ctx);
    let max_tokens = max_tokens_for(&model_input);
    Prepared {
        model_input,
        raw_fallback,
        ctx,
        messages,
        max_tokens,
    }
}

/// Steps 4-9 on a provider's output: the deterministic passes, the gate, and the
/// trailing dictionary + register passes.
///
/// A gate rejection is not an error — it returns [`Engine::Raw`] with the reason
/// recorded, because a wrong-but-clean paste is worse than an untidy-but-faithful
/// one.
pub fn finish(
    prep: &Prepared,
    model_output: &str,
    engine: Engine,
    dict: &DictionaryStore,
    raw_mode: bool,
) -> Finished {
    let cleaned = cleanup::post_process(model_output);
    let cleaned = cleanup::de_dash(&cleaned);
    let cleaned = cleanup::strip_parenthetical_fillers(&cleaned);
    match evaluate_gates(&prep.raw_fallback, &cleaned, prep.level(), prep.vocab()) {
        GateVerdict::Pass => Finished {
            text: finalize(&cleaned, prep.level(), dict, raw_mode),
            engine,
            degraded: None,
        },
        // Which gate fired is the difference between "the model rewrote it" and
        // "the model answered the question instead of transcribing it", so the
        // reason is recorded, not just the fact.
        GateVerdict::Fail(reason) => Finished {
            text: finalize(&prep.raw_fallback, prep.level(), dict, raw_mode),
            engine: Engine::Raw,
            degraded: Some(format!("gate_rejected: {reason:?}")),
        },
    }
}

/// The raw path: cleanup is off by request, every engine was unavailable, or the
/// provider errored. Still runs steps 8-9 — the dictionary and the register are the
/// user's settings, not part of cleanup.
pub fn raw_only(
    prep: &Prepared,
    degraded: Option<String>,
    dict: &DictionaryStore,
    raw_mode: bool,
) -> Finished {
    Finished {
        text: finalize(&prep.raw_fallback, prep.level(), dict, raw_mode),
        engine: Engine::Raw,
        degraded,
    }
}

/// Steps 8-9, applied to whatever text is about to be pasted.
///
/// Private on purpose: the two orderings it encodes (dictionary before register,
/// register only when not Raw mode) are the ones a caller most easily gets wrong,
/// and there is no reason to run them separately.
fn finalize(text: &str, level: CleanupLevel, dict: &DictionaryStore, raw_mode: bool) -> String {
    let fixed = dict.apply_listed_mishears(text);
    if level.forces_lowercase() && !raw_mode {
        cleanup::messaging_style(&fixed)
    } else {
        fixed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dictionary::DictSource;

    fn dict_with(correct: &str, mishears: &[&str]) -> DictionaryStore {
        let mut d = DictionaryStore::default();
        d.add(
            correct,
            mishears.iter().map(|s| s.to_string()).collect(),
            DictSource::Manual,
        );
        d
    }

    /// The gate must compare against the *post-processed* transcript, not the caller's
    /// original: otherwise an explicit "new paragraph" reaches it as a literal marker
    /// and reads as a lost entity.
    #[test]
    fn raw_fallback_has_markers_restored() {
        let prep = prepare(
            "first thought new paragraph second thought",
            CleanupLevel::Light,
            &DictionaryStore::default(),
            None,
        );
        assert!(!prep.raw_fallback.contains("[["), "marker leaked to the paste");
        assert!(prep.raw_fallback.contains('\n'), "explicit break was lost");
        // The model, by contrast, must see the marker.
        assert_ne!(prep.model_input, prep.raw_fallback);
    }

    /// The vocab that went into the prompt must be the vocab the gate sees, or an
    /// authorized spelling reads as the model inventing a word.
    #[test]
    fn prepared_vocab_reaches_the_gate() {
        let dict = dict_with("Manvi", &["monvi"]);
        let prep = prepare("thanks monvi for the help", CleanupLevel::Light, &dict, None);
        assert!(
            prep.vocab().iter().any(|v| v.correct == "Manvi"),
            "prefilter did not select the relevant entry"
        );
        // A cleanup that uses the authorized spelling must pass, not trip novelty.
        let out = finish(&prep, "Thanks Manvi for the help.", Engine::Cloud, &dict, false);
        assert_eq!(out.engine, Engine::Cloud, "authorized spelling was gated: {out:?}");
    }

    /// Step 8 before step 9: the dictionary writes a capitalized spelling, so
    /// lowercasing first would leave it as the one capital in the message.
    #[test]
    fn messaging_lowercases_the_dictionary_fix() {
        let dict = dict_with("Manvi", &["monvi"]);
        let prep = prepare("thanks monvi", CleanupLevel::Messaging, &dict, None);
        let out = finish(&prep, "thanks monvi", Engine::Cloud, &dict, false);
        assert!(
            !out.text.contains("Manvi"),
            "register pass ran before the dictionary: {:?}",
            out.text
        );
        assert!(out.text.contains("manvi"), "dictionary fix was lost: {:?}", out.text);
    }

    /// A gate rejection still gets the user's register — only cleanup failed, the
    /// Messaging choice stands.
    #[test]
    fn gate_rejection_is_still_styled() {
        let prep = prepare(
            "so i was thinking we should ship the thing on friday",
            CleanupLevel::Messaging,
            &DictionaryStore::default(),
            None,
        );
        // An assistant-style reply trips the banned-prefix gate.
        let out = finish(&prep, "Sure, here is your text.", Engine::Cloud, &DictionaryStore::default(), false);
        assert_eq!(out.engine, Engine::Raw);
        assert!(out.degraded.unwrap().starts_with("gate_rejected:"));
        assert_eq!(out.text, out.text.to_lowercase(), "raw paste skipped the register");
    }

    /// Raw *mode* is verbatim by request, so the register pass must not run — but
    /// the dictionary still does.
    #[test]
    fn raw_mode_skips_the_register_but_not_the_dictionary() {
        let dict = dict_with("Manvi", &["monvi"]);
        let prep = prepare("Thanks monvi", CleanupLevel::Messaging, &dict, None);
        let out = raw_only(&prep, None, &dict, true);
        assert!(out.text.contains("Manvi"), "dictionary did not run in raw mode");
    }

    /// `Prepared` crosses an FFI boundary as JSON on iOS; a field that does not
    /// survive the round trip breaks the gate silently.
    #[test]
    fn prepared_round_trips_as_json() {
        let dict = dict_with("Manvi", &["monvi"]);
        let prep = prepare("thanks monvi for today", CleanupLevel::Messaging, &dict, None);
        let json = serde_json::to_string(&prep).unwrap();
        let back: Prepared = serde_json::from_str(&json).unwrap();
        assert_eq!(prep, back);
    }
}
