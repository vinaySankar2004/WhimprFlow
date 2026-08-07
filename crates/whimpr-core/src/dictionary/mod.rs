//! Custom dictionary: user vocabulary plus a pre-filter that injects only the
//! entries relevant to a given utterance into the cleanup prompt (fewer distractors
//! → higher LLM precision). Manual entries and auto-learned (✨) entries share the
//! same store; the auto-learn diff engine (needs accessibility reads) layers on top.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::cleanup::VocabEntry;

/// How a dictionary entry was created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DictSource {
    Manual,
    Auto,
}

fn default_source() -> DictSource {
    DictSource::Manual
}

/// One vocabulary entry: the authoritative spelling and known mishears.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictionaryEntry {
    pub correct: String,
    #[serde(default)]
    pub mishears: Vec<String>,
    #[serde(default = "default_source")]
    pub source: DictSource,
}

/// The user's dictionary, persisted as JSON.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DictionaryStore {
    pub entries: Vec<DictionaryEntry>,
}

impl DictionaryStore {
    /// Load from `path`, returning an empty store if missing or unreadable.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Persist to `path` (creating parent dirs).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self).unwrap_or_default();
        std::fs::write(path, json)
    }

    /// Add or merge an entry, de-duplicating by spelling (case-insensitive).
    pub fn add(&mut self, correct: impl Into<String>, mishears: Vec<String>, source: DictSource) {
        let correct = correct.into();
        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|e| e.correct.eq_ignore_ascii_case(&correct))
        {
            for m in mishears {
                if !existing.mishears.iter().any(|x| x.eq_ignore_ascii_case(&m)) {
                    existing.mishears.push(m);
                }
            }
        } else {
            self.entries.push(DictionaryEntry {
                correct,
                mishears,
                source,
            });
        }
    }

    /// Remove an entry by its spelling (case-insensitive). Returns true if removed.
    pub fn remove(&mut self, correct: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| !e.correct.eq_ignore_ascii_case(correct));
        self.entries.len() != before
    }

    /// Select the entries relevant to `utterance` — those whose spelling or a known
    /// mishear is edit-close to a spoken token (or adjacent token pair, to catch
    /// split words like "charge bee" → "ChargeBee") — capped to `max`.
    pub fn prefilter(&self, utterance: &str, max: usize) -> Vec<VocabEntry> {
        let toks: Vec<String> = utterance
            .split_whitespace()
            .map(|t| {
                t.trim_matches(|c: char| c.is_ascii_punctuation())
                    .to_lowercase()
            })
            .filter(|t| !t.is_empty())
            .collect();

        // Adjacent pairs, glued. These are guesses we manufactured, not words anyone
        // said, so they are matched far more strictly below.
        let bigrams: Vec<String> = toks
            .windows(2)
            .map(|w| format!("{}{}", w[0], w[1]))
            .collect();

        let mut out = Vec::new();
        for e in &self.entries {
            // Targets are compared against single tokens and against concatenated
            // adjacent pairs, neither of which contains a space — so a multi-word
            // entry or mishear ("charge bee") has to lose its space too. Left in, it
            // can never match its own bigram exactly, and instead drifts close to
            // *unrelated* bigrams by exactly the one edit the space costs.
            let targets: Vec<String> = std::iter::once(e.correct.as_str())
                .chain(e.mishears.iter().map(|m| m.as_str()))
                .map(|t| t.split_whitespace().collect::<String>().to_lowercase())
                .filter(|t| !t.is_empty())
                .collect();
            let hit = toks
                .iter()
                .any(|g| targets.iter().any(|t| within(g, t, MAX_NORMALIZED_DISTANCE)))
                || bigrams
                    .iter()
                    .any(|g| targets.iter().any(|t| within(g, t, MAX_BIGRAM_DISTANCE)));
            if hit {
                out.push(VocabEntry {
                    correct: e.correct.clone(),
                    mishears: e.mishears.clone(),
                });
                if out.len() >= max {
                    break;
                }
            }
        }
        out
    }
}

/// How far off a spoken token may be and still pull its entry into the prompt,
/// as a fraction of the longer word.
///
/// 0.30 rather than a looser 0.34, for a reason worth keeping: at 0.34 a *one letter*
/// difference is enough for any word of three characters ("we" pulling in "Wei") and
/// a three-letter difference is enough at nine ("charge" pulling in "ChargeBee",
/// which then really did get substituted into "did you charge the battery"). Both of
/// those are one-third exactly, and both were false. The genuine catches do not
/// depend on that last bit of slack — a listed mishear matches itself at distance 0,
/// and a split name is caught by the bigram pass, also at distance 0. So the slack
/// bought nothing but false positives.
pub const MAX_NORMALIZED_DISTANCE: f32 = 0.30;

/// The same, for a glued adjacent pair — much stricter, and deliberately so.
///
/// A bigram is a token *we* invented by joining two words the speaker said
/// separately, purely to catch a name that recognition split in half. That job only
/// ever needs an (almost) exact match: "charge bee" glues to exactly "chargebee".
/// Give it the same slack as a real word and it becomes a noise generator — "charge
/// the" glues to "chargethe", which is two letters from "ChargeBee", and the model
/// duly rewrote "did you charge the battery" as "did you ChargeBee the battery". The
/// pair is a guess, so it has to be nearly right to count.
const MAX_BIGRAM_DISTANCE: f32 = 0.15;

/// Is `a` within `max` normalized edit distance of `b` (0 = identical, 1 = nothing
/// in common)?
fn within(a: &str, b: &str, max: f32) -> bool {
    if a == b {
        return true;
    }
    let maxlen = a.chars().count().max(b.chars().count());
    if maxlen == 0 {
        return false;
    }
    (strsim::levenshtein(a, b) as f32 / maxlen as f32) <= max
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> DictionaryStore {
        let mut s = DictionaryStore::default();
        s.add("Manvi", vec!["Monvi".into(), "Manvee".into()], DictSource::Manual);
        s.add("ChargeBee", vec!["charge bee".into()], DictSource::Manual);
        s
    }

    #[test]
    fn prefilter_selects_close_mishear() {
        // "monvi" is an exact mishear of Manvi.
        let v = store().prefilter("send the deck to monvi please", 15);
        assert!(v.iter().any(|e| e.correct == "Manvi"));
        assert!(!v.iter().any(|e| e.correct == "ChargeBee"));
    }

    #[test]
    fn prefilter_catches_split_word_via_bigram() {
        // "charge bee" spoken as two words → bigram "chargebee" matches.
        let v = store().prefilter("we should renew charge bee this month", 15);
        assert!(v.iter().any(|e| e.correct == "ChargeBee"));
    }

    #[test]
    fn prefilter_ignores_unrelated_utterance() {
        let v = store().prefilter("the weather is nice today", 15);
        assert!(v.is_empty());
    }

    /// Precision, which is the entire reason this pre-filter exists. An entry whose
    /// spelling contains an ordinary English word must not be dragged in every time
    /// that word is spoken — once in the prompt, the model really does substitute it
    /// ("did you charge the battery" -> "did you ChargeBee the battery").
    #[test]
    fn prefilter_ignores_an_entry_word_used_ordinarily() {
        let v = store().prefilter("did you charge the battery before the flight", 15);
        assert!(v.is_empty(), "selected {v:?}");
    }

    /// Short entries are the worst offenders: at a looser threshold a single letter
    /// separates a three-letter name from a very common word.
    #[test]
    fn prefilter_does_not_match_a_short_entry_on_one_letter() {
        let mut s = DictionaryStore::default();
        s.add("Wei", Vec::new(), DictSource::Manual);
        assert!(s.prefilter("we do not have that much left", 15).is_empty());
        // …but the name itself is still caught.
        assert_eq!(s.prefilter("wei is joining the call", 15).len(), 1);
    }

    /// Tightening the threshold must not cost the real catches: a listed mishear
    /// matches itself exactly, and a genuinely close unlisted one still lands.
    #[test]
    fn prefilter_still_catches_an_unlisted_near_mishear() {
        let v = store().prefilter("tell manvie about it", 15);
        assert!(v.iter().any(|e| e.correct == "Manvi"));
    }

    #[test]
    fn add_merges_mishears_case_insensitively() {
        let mut s = store();
        s.add("manvi", vec!["Manvie".into()], DictSource::Auto);
        let e = s.entries.iter().find(|e| e.correct == "Manvi").unwrap();
        assert!(e.mishears.iter().any(|m| m == "Manvie"));
        assert_eq!(s.entries.iter().filter(|e| e.correct.eq_ignore_ascii_case("manvi")).count(), 1);
    }
}
