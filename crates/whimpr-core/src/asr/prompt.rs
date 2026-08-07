//! Biasing recognition itself with the user's dictionary, and deciding whether to
//! trust the result.
//!
//! Whisper accepts an `initial_prompt` that conditions decoding, which is the
//! standard way to make it spell names and jargon correctly. That fixes mis-hearings
//! at the source rather than repairing them downstream in cleanup — and it is the
//! only way the dictionary can help at all in `Raw` mode or at cleanup level `None`,
//! where no model ever sees the transcript.
//!
//! The catch is that conditioning cuts both ways: prompt Whisper with a word and it
//! will sometimes emit that word for audio that never contained it. So the shell
//! transcribes **twice** — once unprompted, once prompted — and [`accept_prompted`]
//! decides between them. Because the unprompted transcript is right there to compare
//! against, a prompt-induced hallucination cannot reach the cursor: the prompted
//! result is only taken when the *only* words it introduced are spellings the
//! dictionary authorized. Same shape as [`crate::cleanup::gates`], one layer earlier.
//!
//! The second pass only runs when the pre-filter matched something, so a dictation
//! with no dictionary relevance takes the single-pass path unchanged.

use std::collections::HashMap;

use crate::cleanup::VocabEntry;

/// How much prompt text to send. whisper.cpp truncates the prompt to half the text
/// context (224 tokens for the standard models); this stays well inside that, since
/// a prompt long enough to be truncated is also long enough to start steering the
/// transcript's style, which is not what it is here for.
const MAX_PROMPT_CHARS: usize = 200;

/// Render the vocabulary as an `initial_prompt`, or `None` when there is nothing to
/// bias toward.
///
/// A plain comma-separated glossary line: Whisper conditions on this as if it were
/// text preceding the utterance, so it reads as context rather than instructions.
/// Only the authoritative spellings go in — the mishears are what we are trying to
/// steer *away* from, and listing them would bias toward the wrong ones.
pub fn build_initial_prompt(vocab: &[VocabEntry]) -> Option<String> {
    let mut out = String::from("Glossary: ");
    let mut n = 0usize;
    for v in vocab {
        let word = v.correct.trim();
        if word.is_empty() {
            continue;
        }
        let sep = if n == 0 { "" } else { ", " };
        if out.len() + sep.len() + word.len() + 1 > MAX_PROMPT_CHARS {
            break;
        }
        out.push_str(sep);
        out.push_str(word);
        n += 1;
    }
    if n == 0 {
        return None;
    }
    out.push('.');
    Some(out)
}

/// Should the prompted transcript replace the unprompted one?
///
/// Yes only when every word the prompt introduced is an authorized spelling, and it
/// introduced no more of them than it displaced. The second condition is the one that
/// matters: prompting Whisper with a glossary and handing it near-silence makes it
/// echo the glossary back, and every one of those echoed words *is* an authorized
/// spelling — so a check that only asked "are the new words in the dictionary?" would
/// wave the failure straight through.
pub fn accept_prompted(unprompted: &str, prompted: &str, vocab: &[VocabEntry]) -> bool {
    let before = counts(unprompted);
    let after = counts(prompted);
    if after.is_empty() {
        // The prompted pass produced nothing usable; keep what we had.
        return false;
    }

    let authorized = authorized_spellings(vocab);

    // Multiset difference in both directions: a word repeated four times where it was
    // said once is three additions, not zero.
    let mut added = 0usize;
    for (tok, n_after) in &after {
        let n_before = before.get(tok).copied().unwrap_or(0);
        if n_after > &n_before {
            if !authorized.contains(tok.as_str()) {
                // A word that was neither spoken nor in the dictionary. The prompt is
                // not allowed to invent vocabulary of its own.
                return false;
            }
            added += n_after - n_before;
        }
    }
    let mut removed = 0usize;
    for (tok, n_before) in &before {
        let n_after = after.get(tok).copied().unwrap_or(0);
        if n_before > &n_after {
            removed += n_before - n_after;
        }
    }

    // A correction swaps words: one out, one in ("monvi" -> "Manvi"), or two out and
    // one in when recognition split a name ("charge bee" -> "ChargeBee"). Slack of one
    // covers a name Whisper simply missed; beyond that it is inserting, not correcting.
    added <= removed + 1
}

/// Lowercased alphanumeric word counts, splitting on every non-alphanumeric character
/// and dropping apostrophes entirely.
///
/// Deliberately blunter than the tokenizer the cleanup gates use, because this is
/// comparing two transcripts *of the same audio*. Whisper re-punctuates freely between
/// runs — the same clip came back as "stand up" unprompted and "stand-up" prompted —
/// and treating that as an invented word rejected an otherwise perfect correction.
/// Dropping apostrophes rather than splitting on them keeps "it's" and "its" the same
/// token instead of turning one into two.
fn counts(text: &str) -> HashMap<String, usize> {
    let mut m = HashMap::new();
    let cleaned: String = text.chars().filter(|c| *c != '\'' && *c != '\u{2019}').collect();
    for tok in cleaned.split(|c: char| !c.is_alphanumeric()) {
        if !tok.is_empty() {
            *m.entry(tok.to_lowercase()).or_insert(0) += 1;
        }
    }
    m
}

/// The exact spellings the dictionary authorized, as normalized tokens. A multi-word
/// entry contributes each of its words.
fn authorized_spellings(vocab: &[VocabEntry]) -> std::collections::HashSet<String> {
    vocab
        .iter()
        .flat_map(|v| v.correct.split_whitespace())
        .map(|w| {
            w.trim_matches(|c: char| c.is_ascii_punctuation())
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vocab(words: &[&str]) -> Vec<VocabEntry> {
        words
            .iter()
            .map(|w| VocabEntry {
                correct: (*w).to_string(),
                mishears: Vec::new(),
            })
            .collect()
    }

    #[test]
    fn prompt_lists_only_the_correct_spellings() {
        let v = vec![VocabEntry {
            correct: "ChargeBee".into(),
            mishears: vec!["charge bee".into()],
        }];
        let p = build_initial_prompt(&v).unwrap();
        assert!(p.contains("ChargeBee"));
        // Listing the mishear would bias toward the spelling we are trying to fix.
        assert!(!p.contains("charge bee"));
    }

    #[test]
    fn no_vocab_means_no_prompt() {
        assert_eq!(build_initial_prompt(&[]), None);
    }

    #[test]
    fn prompt_is_capped() {
        let many: Vec<VocabEntry> = (0..200)
            .map(|i| VocabEntry {
                correct: format!("Averyveryverylongname{i}"),
                mishears: Vec::new(),
            })
            .collect();
        let p = build_initial_prompt(&many).unwrap();
        assert!(p.len() <= MAX_PROMPT_CHARS, "{} chars", p.len());
    }

    #[test]
    fn accepts_a_straight_substitution() {
        assert!(accept_prompted(
            "hey can you ask monvi about it",
            "hey can you ask Manvi about it",
            &vocab(&["Manvi"])
        ));
    }

    #[test]
    fn accepts_a_split_name_being_joined() {
        assert!(accept_prompted(
            "we should renew charge bee this month",
            "we should renew ChargeBee this month",
            &vocab(&["ChargeBee"])
        ));
    }

    #[test]
    fn identical_output_is_accepted() {
        assert!(accept_prompted("nothing changed here", "nothing changed here", &vocab(&["Manvi"])));
    }

    /// The failure the second pass exists to catch: prompted with a glossary and given
    /// audio that did not contain it, Whisper echoes the glossary back. Every inserted
    /// word is "authorized", so only the count check catches this.
    #[test]
    fn rejects_the_prompt_being_echoed_back() {
        assert!(!accept_prompted(
            "thanks",
            "thanks Manvi ChargeBee Manvi ChargeBee",
            &vocab(&["Manvi", "ChargeBee"])
        ));
    }

    /// Repetition of a single authorized word is the same failure in a narrower form.
    #[test]
    fn rejects_a_repeated_authorized_word() {
        assert!(!accept_prompted(
            "call monvi",
            "call Manvi Manvi Manvi",
            &vocab(&["Manvi"])
        ));
    }

    #[test]
    fn rejects_a_word_that_is_neither_spoken_nor_in_the_dictionary() {
        assert!(!accept_prompted(
            "call monvi tomorrow",
            "call Manvi tomorrow about the invoice",
            &vocab(&["Manvi"])
        ));
    }

    #[test]
    fn rejects_an_empty_prompted_result() {
        assert!(!accept_prompted("call monvi", "", &vocab(&["Manvi"])));
    }

    /// Case and trailing punctuation are not changes.
    #[test]
    fn punctuation_and_case_do_not_count_as_edits() {
        assert!(accept_prompted("call monvi", "Call Manvi.", &vocab(&["Manvi"])));
    }

    /// Observed against real audio: the prompted pass fixed the name *and* re-hyphenated
    /// "stand up" as "stand-up". Counting that as an invented word rejected an otherwise
    /// perfect correction, which is the whole feature failing on a punctuation mark.
    #[test]
    fn rehyphenation_between_passes_is_not_an_invented_word() {
        assert!(accept_prompted(
            "Hey can you send the deck over to Manvy before the stand up?",
            "Hey, can you send the deck over to Manvi before the stand-up?",
            &vocab(&["Manvi"])
        ));
    }

    #[test]
    fn apostrophes_do_not_split_a_word_in_two() {
        assert!(accept_prompted(
            "its monvi calling",
            "It's Manvi calling.",
            &vocab(&["Manvi"])
        ));
    }
}
