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

    /// Rewrite every **listed** mishear in `text` to its entry's spelling, verbatim.
    ///
    /// The cleanup model is asked to do this too, and cannot be relied on to. It
    /// applies the substitution readily when the mis-heard form looks like a mistake
    /// ("monvi" -> "Manvi") and refuses when it looks like a perfectly good word —
    /// which is exactly the case a user adds an entry for. Observed against the
    /// shipped 4B model: "Gita" alone became "Geetha", while "Hey Geeta, how's it
    /// going?" came back untouched, and no amount of instruction in the vocabulary
    /// block changed it. To the model, "Geeta" is a correct spelling of a name and the
    /// precision guard ("if the sentence still makes sense with the word the speaker
    /// used, leave it alone") tells it to stop. It is right in general and wrong here,
    /// because the user already answered the question by typing the mishear.
    ///
    /// So a listed mishear is not a judgment call: the user stated "when you hear this
    /// exact string, write that one", and this enacts it. Unlisted near-misses stay the
    /// model's job — that is where the judgment actually lives, and where
    /// [`Self::prefilter`]'s precision work matters. Same division as
    /// [`crate::cleanup::post_process`]: the model does the smart part, this guarantees
    /// the mechanical one.
    ///
    /// Runs on the text about to be pasted, whatever produced it — so it also works
    /// when cleanup is off, when the gates rejected the edit, and when the provider
    /// failed, none of which a prompt can reach.
    pub fn apply_listed_mishears(&self, text: &str) -> String {
        let mut rules: Vec<(Vec<char>, &str)> = Vec::new();
        for e in &self.entries {
            for m in &e.mishears {
                // Users add a mishear by pasting what landed in the field, so it arrives
                // with the sentence's punctuation still attached ("Vinayk."). Left in, the
                // phrase can never match anything.
                let m = m.trim_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace());
                // A mishear that IS the spelling is a no-op that would still cost a scan.
                if m.is_empty() || m.eq_ignore_ascii_case(&e.correct) {
                    continue;
                }
                rules.push((m.chars().collect(), e.correct.as_str()));
            }
        }
        if rules.is_empty() {
            return text.to_string();
        }
        // Longest first, so a multi-word mishear wins over one of its own words.
        rules.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

        let chars: Vec<char> = text.chars().collect();
        let n = chars.len();
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        'scan: while i < n {
            // Only at a word boundary — "Geeta" must not fire inside "Geetanjali".
            if i == 0 || !chars[i - 1].is_alphanumeric() {
                for (phrase, correct) in &rules {
                    if let Some(end) = match_phrase(&chars, i, phrase) {
                        if end == n || !chars[end].is_alphanumeric() {
                            out.push_str(correct);
                            i = end;
                            continue 'scan;
                        }
                    }
                }
            }
            out.push(chars[i]);
            i += 1;
        }
        out
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

/// What speech recognition *actually wrote* where the user corrected a word.
///
/// Auto-learn works by diffing the pasted text against the field afterwards, so the
/// mishear it observes is the **cleaned** form — whatever cleanup made of what Whisper
/// heard, which is not necessarily what Whisper heard. Recording that as the mishear
/// teaches the dictionary a spelling recognition may never actually produce.
///
/// The raw transcript is the ground truth, so this looks up the token in it that the
/// correction replaced: the closest one to the observed mishear. Both forms are worth
/// keeping — the raw one is what will need matching next time, the cleaned one is what
/// the pre-filter will see if cleanup mangles it the same way again.
///
/// Returns `None` when the raw transcript has nothing recognizably similar, which is
/// the honest answer when cleanup rewrote the region beyond recognition.
pub fn ground_truth_mishear(raw_transcript: &str, observed: &str) -> Option<String> {
    let observed_lc = observed.to_lowercase();
    raw_transcript
        .split_whitespace()
        .map(|t| t.trim_matches(|c: char| c.is_ascii_punctuation()))
        .filter(|t| !t.is_empty())
        .filter(|t| !t.eq_ignore_ascii_case(observed))
        .map(|t| (score(&t.to_lowercase(), &observed_lc), t))
        // Ties go to the earlier token, which `min_by` gives us for free.
        .filter(|(d, _)| *d <= MAX_GROUND_TRUTH_DISTANCE)
        .min_by(|(a, _), (b, _)| a.total_cmp(b))
        .map(|(_, t)| t.to_string())
}

/// How far the raw token may sit from the observed mishear and still be taken as the
/// same word.
///
/// Deliberately looser than [`MAX_NORMALIZED_DISTANCE`], because the question is a
/// different one. Selection scans every entry in the dictionary against every spoken
/// word, so a wide net there means false corrections. Here we already know a
/// correction happened, and we are picking the best of a handful of tokens in one
/// short sentence — cleanup is entitled to have reshaped the word a fair bit on its
/// way to the screen ("monvee" -> "Monvi"), and the fallback if nothing matches is
/// simply to keep the observed form.
const MAX_GROUND_TRUTH_DISTANCE: f32 = 0.55;

/// Does `phrase` occur in `chars` starting at `start`, ignoring case? Returns the
/// index just past the match.
///
/// A space in the phrase matches any run of whitespace, so a two-word mishear still
/// matches across the line break or double space a transcript may have put there.
fn match_phrase(chars: &[char], start: usize, phrase: &[char]) -> Option<usize> {
    let mut i = start;
    let mut p = 0;
    while p < phrase.len() {
        if phrase[p] == ' ' {
            let before = i;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            if i == before {
                return None;
            }
            p += 1;
            continue;
        }
        if i >= chars.len() || !same_char(chars[i], phrase[p]) {
            return None;
        }
        i += 1;
        p += 1;
    }
    Some(i)
}

/// Case-insensitive char comparison that also folds non-ASCII letters.
fn same_char(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

/// Normalized edit distance, 0 = identical.
fn score(a: &str, b: &str) -> f32 {
    let maxlen = a.chars().count().max(b.chars().count());
    if maxlen == 0 {
        return 1.0;
    }
    strsim::levenshtein(a, b) as f32 / maxlen as f32
}

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

    /// Cleanup capitalized and punctuated what Whisper wrote, so the mishear auto-learn
    /// observes is "Monvi." while recognition actually produced "monvee". The dictionary
    /// needs the latter — that is the string it will have to match next time.
    #[test]
    fn ground_truth_prefers_what_recognition_wrote() {
        let raw = "hey can you send this to monvee before the standup";
        assert_eq!(ground_truth_mishear(raw, "Monvi"), Some("monvee".to_string()));
    }

    #[test]
    fn ground_truth_ignores_punctuation_and_case() {
        assert_eq!(ground_truth_mishear("tell monvee, please", "monvi"), Some("monvee".to_string()));
    }

    /// Nothing similar in the raw transcript means cleanup rewrote the region; say so
    /// rather than learning an unrelated word.
    #[test]
    fn ground_truth_is_none_when_nothing_resembles_it() {
        assert_eq!(ground_truth_mishear("the weather is nice today", "Manvi"), None);
    }

    /// When recognition already wrote the observed form, there is no better truth to
    /// find and the caller should keep what it had.
    #[test]
    fn ground_truth_skips_an_exact_match() {
        assert_eq!(ground_truth_mishear("tell monvi about it", "monvi"), None);
    }

    /// The regression this pass exists for, verbatim from the user's dictionary and
    /// the transcript that failed: the model leaves a mis-heard name alone when the
    /// mishear is itself a plausible spelling, so the substitution cannot be left to it.
    #[test]
    fn listed_mishears_are_applied_even_when_the_model_would_not() {
        let mut s = DictionaryStore::default();
        s.add("Abishek", vec!["Abhishek".into()], DictSource::Manual);
        s.add("Geetha", vec!["Gita".into(), "Geeta".into()], DictSource::Manual);
        assert_eq!(
            s.apply_listed_mishears("My brother's name is Abhishek and my mom's name is Geeta."),
            "My brother's name is Abishek and my mom's name is Geetha."
        );
    }

    /// Whole words only. A mishear that is a prefix of a longer name must not fire
    /// inside it, or "Geeta" quietly mangles "Geetanjali".
    #[test]
    fn listed_mishears_only_match_whole_words() {
        let mut s = DictionaryStore::default();
        s.add("Geetha", vec!["Geeta".into()], DictSource::Manual);
        assert_eq!(s.apply_listed_mishears("Geetanjali"), "Geetanjali");
        assert_eq!(s.apply_listed_mishears("ageeta"), "ageeta");
        // A possessive is a boundary, so the name inside it is still fixed.
        assert_eq!(s.apply_listed_mishears("Geeta's car"), "Geetha's car");
    }

    /// Case is recognition's to get wrong; the entry's spelling is what gets written.
    #[test]
    fn listed_mishears_match_regardless_of_case() {
        let mut s = DictionaryStore::default();
        s.add("Manvi", vec!["Monvi".into()], DictSource::Manual);
        assert_eq!(s.apply_listed_mishears("monvi and MONVI"), "Manvi and Manvi");
    }

    /// Users add a mishear by pasting what landed in the field, punctuation and all.
    /// Stored as "Vinayk." it would otherwise never match the token "Vinayk".
    #[test]
    fn a_mishear_stored_with_punctuation_still_matches() {
        let mut s = DictionaryStore::default();
        s.add("Vinayak", vec!["Vinayk.".into()], DictSource::Manual);
        assert_eq!(s.apply_listed_mishears("hey Vinayk, hi"), "hey Vinayak, hi");
    }

    /// A multi-word mishear is one phrase, and beats a shorter rule that overlaps it.
    #[test]
    fn multi_word_mishears_are_matched_as_a_phrase() {
        let mut s = DictionaryStore::default();
        s.add("ChargeBee", vec!["charge bee".into()], DictSource::Manual);
        s.add("Sankaranarayanan", vec!["Shankar Narayanan.".into()], DictSource::Manual);
        assert_eq!(s.apply_listed_mishears("renew charge bee soon"), "renew ChargeBee soon");
        assert_eq!(
            s.apply_listed_mishears("Vinayak Shankar Narayanan speaking"),
            "Vinayak Sankaranarayanan speaking"
        );
    }

    /// This pass enacts what the user listed and nothing else. An unlisted near-miss
    /// stays the model's judgment call, and text with no listed mishear is untouched.
    #[test]
    fn unlisted_near_misses_are_left_to_the_model() {
        let s = store();
        assert_eq!(s.apply_listed_mishears("tell manvie about it"), "tell manvie about it");
        assert_eq!(s.apply_listed_mishears("the weather is nice"), "the weather is nice");
    }

    /// An entry with no mishears listed asks for nothing, and a mishear equal to the
    /// spelling is a no-op — neither may disturb the text.
    #[test]
    fn entries_with_nothing_to_apply_are_inert() {
        let mut s = DictionaryStore::default();
        s.add("Wei", Vec::new(), DictSource::Manual);
        s.add("Manvi", vec!["manvi".into()], DictSource::Manual);
        let text = "Wei asked Manvi about it";
        assert_eq!(s.apply_listed_mishears(text), text);
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
