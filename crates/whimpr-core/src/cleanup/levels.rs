//! Auto Cleanup levels — how aggressively the LLM is allowed to edit, and in what
//! register.
//!
//! `None` bypasses the model entirely (raw ASR is pasted). The others append a
//! modifier to the shared system prompt. Light is WhimprFlow's default: research
//! found Wispr's more-aggressive default was the top "it changed what I said"
//! complaint, so we bias conservative. `Messaging` is not a *stronger* Light — it
//! does the same amount of editing in a casual register, all lowercase.
//!
//! There used to be a Medium and a High above Light. They were removed: an
//! aggressiveness dial past "fix the grammar" only ever produced text the speaker
//! did not say. Saved settings naming them deserialize as Light (see the aliases),
//! because an unknown value would fail the whole parse and reset every setting.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CleanupLevel {
    /// Transcribe exactly what was said, including mistakes. No model call.
    None,
    /// Casual register for chat apps: same edits as Light, but everything
    /// lowercased and punctuated only where the meaning needs it.
    Messaging,
    /// Remove fillers and fix grammar only; preserve the speaker's words. (Default.)
    #[default]
    #[serde(alias = "medium", alias = "high")]
    Light,
}

impl CleanupLevel {
    /// True when no model should be invoked and the raw transcript is used verbatim.
    pub fn bypasses_llm(self) -> bool {
        matches!(self, CleanupLevel::None)
    }

    /// True when the output is forced to lowercase after everything else has run.
    /// The prompt asks for it too, but the model is not the guarantee — see
    /// [`super::force_lowercase`].
    pub fn forces_lowercase(self) -> bool {
        matches!(self, CleanupLevel::Messaging)
    }

    /// Text appended to the shared system prompt for this level (empty for `None`).
    pub fn modifier(self) -> &'static str {
        match self {
            CleanupLevel::None => "",
            CleanupLevel::Messaging => {
                "REGISTER: this is a casual chat message. Write everything in lowercase, \
                 including the first word, the word \"i\", and every name and acronym. \
                 Punctuate only where the meaning needs it: keep question marks, drop the \
                 period ending a line, use commas sparingly.\n\
                 Casual means casual WORD CHOICE — keep contractions and slang exactly as \
                 spoken (\"gonna\" stays \"gonna\"), fix only clearly broken grammar, add \
                 nothing. It does NOT mean leaving the speech in. Fillers, hesitations and \
                 stutters come out here in full, exactly as at every other level: a chat \
                 message is still written, not transcribed. \"like\", \"you know\", \"I mean\" \
                 and \"basically\" are the ones that survive if you let them."
            }
            CleanupLevel::Light => {
                "Be conservative about word choice, meaning and tone: apply the allowed edits \
                 minimally, and when unsure whether to rephrase something, leave it as spoken. \
                 That conservatism does NOT extend to fillers, hesitations and stutters — \
                 remove those in full. Leaving them in is not the cautious choice, it is the \
                 failure the reader sees."
            }
        }
    }

    /// Ceiling on the *novelty ratio* (fraction of output words that were not
    /// spoken) the deterministic gate tolerates. Filler deletion and punctuation
    /// don't count as novelty; number/spoken-punctuation normalization introduces a
    /// little, so the ceiling leaves headroom for that while still catching full
    /// rewrites. Messaging edits no harder than Light, so it shares the ceiling —
    /// lowercasing and dropping punctuation are invisible to the gate, which
    /// normalizes case and strips punctuation before comparing.
    pub fn max_novelty_ratio(self) -> f32 {
        match self {
            CleanupLevel::None => 0.0,
            CleanupLevel::Messaging | CleanupLevel::Light => 0.34,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A settings.json written when Medium and High existed must still load, and
    /// must land on Light rather than failing the parse — a failed parse takes
    /// every other setting down with it.
    #[test]
    fn retired_levels_deserialize_as_light() {
        assert_eq!(
            serde_json::from_str::<CleanupLevel>("\"medium\"").unwrap(),
            CleanupLevel::Light
        );
        assert_eq!(
            serde_json::from_str::<CleanupLevel>("\"high\"").unwrap(),
            CleanupLevel::Light
        );
    }

    #[test]
    fn levels_round_trip_as_snake_case() {
        for level in [CleanupLevel::None, CleanupLevel::Messaging, CleanupLevel::Light] {
            let json = serde_json::to_string(&level).unwrap();
            assert_eq!(serde_json::from_str::<CleanupLevel>(&json).unwrap(), level);
        }
        assert_eq!(
            serde_json::to_string(&CleanupLevel::Messaging).unwrap(),
            "\"messaging\""
        );
    }

    #[test]
    fn only_messaging_forces_lowercase() {
        assert!(CleanupLevel::Messaging.forces_lowercase());
        assert!(!CleanupLevel::Light.forces_lowercase());
        assert!(!CleanupLevel::None.forces_lowercase());
    }
}
