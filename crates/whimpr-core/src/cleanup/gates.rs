//! Deterministic cleanup gates — the cheap, always-on guard against the LLM
//! over-editing or hallucinating. They run on every cleanup output before it is
//! committed; on any failure the caller falls back to the raw transcript (or,
//! optionally, an LLM verifier pass). This is the anti-over-editing safety net.

use std::collections::HashSet;

use super::levels::CleanupLevel;
use super::VocabEntry;

/// Why a cleanup output was rejected.
#[derive(Debug, Clone, PartialEq)]
pub enum GateReason {
    /// Token-level edit distance exceeded the level's ceiling.
    EditRatioTooHigh { ratio: f32, ceiling: f32 },
    /// A must-preserve token (number, URL, email, code-ish token) vanished.
    LostEntity(String),
    /// Output shrank past the over-deletion ceiling — likely dropped content.
    OverDeletion { shrink: f32 },
    /// Output grew beyond punctuation — likely added content.
    Hallucination,
    /// A banned pattern (added greeting/sign-off or an assistant-style reply) appeared.
    BannedPattern(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum GateVerdict {
    Pass,
    Fail(GateReason),
}

impl GateVerdict {
    pub fn passed(&self) -> bool {
        matches!(self, GateVerdict::Pass)
    }
}

/// Phrases that should never be *introduced* by cleanup (the model answering or
/// chatting instead of transcribing). Matched case-insensitively at output start.
const BANNED_PREFIXES: &[&str] = &[
    "sure,",
    "sure!",
    "here is",
    "here's",
    "i'm sorry",
    "i am sorry",
    "as an ai",
    "certainly",
    "of course",
    "i cannot",
    "i can't help",
];

/// Evaluate a cleanup output against the raw transcript for the given level.
///
/// `vocab` is the dictionary entries pre-filtered for *this* utterance — the same
/// list that went into the prompt. It must be passed, because a dictionary
/// correction replaces a mis-heard token with a spelling that by definition does
/// not appear in the raw transcript, which the novelty gate would otherwise read
/// as the model inventing a word. Pass `&[]` when no dictionary was in play.
pub fn evaluate(
    raw: &str,
    cleaned: &str,
    level: CleanupLevel,
    vocab: &[VocabEntry],
) -> GateVerdict {
    // None never invokes the model, so there is nothing to gate.
    if level.bypasses_llm() {
        return GateVerdict::Pass;
    }

    // 1) Introduced assistant-style / greeting prefixes.
    let cleaned_lc = cleaned.trim_start().to_lowercase();
    let raw_lc = raw.to_lowercase();
    for p in BANNED_PREFIXES {
        if cleaned_lc.starts_with(p) && !raw_lc.contains(p) {
            return GateVerdict::Fail(GateReason::BannedPattern((*p).to_string()));
        }
    }

    // 2) Must-preserve entities present in raw must survive in cleaned. Compared
    // case-insensitively: the Messaging register lowercases everything, and a real
    // "Amazon.com" was rejected as lost because it came back as "amazon.com".
    for ent in must_preserve_entities(raw) {
        if !cleaned_lc.contains(&ent.to_lowercase()) {
            return GateVerdict::Fail(GateReason::LostEntity(ent));
        }
    }

    // 3) Gross length changes. Thresholds are generous: self-corrections shorten
    // text and structural formatting (numbered lists, paragraph breaks, list
    // markers) lengthens it — both are legitimate, so only flag extreme changes.
    //
    // Filler is discounted from the raw length first, so the gate measures what is
    // left after the deletions rule 1 *authorized* rather than counting them as
    // content loss. Same widening as the vocab carve-out below, and needed for the
    // same reason: without it the gate punishes cleanup for doing its job. Measured
    // on a real 70-word dictation at speaking density, cleanup that correctly removed
    // every filler shrank the text 56% and was rejected at the then 55% line, so the
    // raw transcript — every filler intact — is what reached the cursor. Cleanup
    // looked switched off precisely when it had worked best, and the better the
    // model the more often it would happen.
    //
    // A spoken emoji request is discounted the same way: "laughing emoji" is fourteen
    // characters that rule 10 turns into one.
    let discounted = filler_mass(raw) + emoji_cue_mass(raw);
    let raw_len = (raw.chars().count() as f32 - discounted).max(1.0);
    let clean_len = cleaned.chars().count() as f32;
    let shrink = (raw_len - clean_len) / raw_len;
    let ceiling = if has_correction_cue(&raw_lc) {
        OVER_DELETION_CEILING_AFTER_CORRECTION
    } else {
        OVER_DELETION_CEILING
    };
    if shrink > ceiling {
        return GateVerdict::Fail(GateReason::OverDeletion { shrink });
    }
    // Growth is measured against the whole transcript, discounts and all: what was
    // authorized for deletion is still text the speaker produced, and on a short
    // message the discount alone can leave almost nothing to grow from ("thanks
    // laughing emoji" -> "Thanks 😂" is not a hallucination).
    if clean_len > raw.chars().count() as f32 * 1.6 {
        return GateVerdict::Fail(GateReason::Hallucination);
    }

    // 4) Novelty: how many output words were never spoken. Deletions (fillers) and
    // casing/punctuation don't count; a full rewrite does. Spellings the dictionary
    // authorized for this utterance are not novel — they were spoken, ASR just wrote
    // them down wrong. Neither is an emoji, when the speaker asked for one.
    let ratio = novelty_ratio(raw, cleaned, &authorized_spellings(vocab));
    let ceiling = level.max_novelty_ratio();
    if ratio > ceiling {
        return GateVerdict::Fail(GateReason::EditRatioTooHigh { ratio, ceiling });
    }

    GateVerdict::Pass
}

/// How much of the (filler-discounted) raw text cleanup may drop before the gate
/// reads it as content loss.
///
/// 0.65 rather than the original 0.55 because the gate exists to catch the model
/// *answering* or *summarizing* a dictation, and both land far past either line:
/// the 4B's reply to a request is ~9% of the input (a 0.91 shrink), and a genuine
/// summary halves the word count and then some. What lives between 0.55 and 0.65
/// is legitimate cleanup of a filler-dense or self-corrected dictation, which is
/// what the line was rejecting.
const OVER_DELETION_CEILING: f32 = 0.65;

/// The ceiling when the transcript contains an unambiguous self-correction cue.
///
/// Rule 3 tells the model to delete the abandoned wording, and the abandoned wording
/// is routinely most of the utterance: "Okay, so I want to talk about... actually,
/// um, scratch that. Let's talk about how life is." cleans correctly to the last
/// sentence, a 0.63 shrink, and a real "I noticed something that, you know, actually,
/// sorry, I noticed something that... actually, scratched that. I noticed that it
/// works well on light" cleans to its final clause at 0.58. Both were rejected, so
/// the raw text — "scratch that" and all — is what got pasted, and the speaker
/// concluded the app ignores the cue. Only the cues that cannot be an ordinary
/// word widen the gate: "actually", "sorry", "wait" and "I mean" are far too common
/// in speech that corrects nothing.
const OVER_DELETION_CEILING_AFTER_CORRECTION: f32 = 0.80;

/// Self-correction cues that are never anything else. Lowercase; matched as
/// substrings of the lowercased transcript, so "scratched that" hits "scratch".
const CORRECTION_CUES: &[&str] = &[
    "scratch that",
    "scratched that",
    "strike that",
    "no wait",
    "never mind",
    "nevermind",
    "make that",
    "i meant",
];

fn has_correction_cue(raw_lc: &str) -> bool {
    CORRECTION_CUES.iter().any(|c| raw_lc.contains(c))
}

/// The fillers rule 1 authorizes cleanup to delete, longest first so "you know" is
/// matched before "know" could be. Kept here rather than shared with
/// [`super::strip_parenthetical_fillers`]: that list is what a *deterministic* pass may
/// safely remove and is deliberately narrow, while this one is what the *prompt*
/// permits the model to remove — widening one must not silently widen the other.
const AUTHORIZED_FILLERS: &[&str] = &[
    "you know", "i mean", "sort of", "kind of", "basically", "like", "um", "uh", "er",
];

/// How many characters of `raw` are filler the prompt allows cleanup to delete.
///
/// Counted whole-word and case-insensitively, with the following space, since that is
/// what disappears along with the word. An over-count would widen the deletion gate
/// beyond what was authorized, so this deliberately never counts a filler inside
/// another word ("unlikely" is not a "like").
fn filler_mass(raw: &str) -> f32 {
    let lower = raw.to_lowercase();
    let bytes = lower.as_bytes();
    let mut total = 0usize;
    let mut i = 0;
    'scan: while i < lower.len() {
        if i == 0 || !bytes[i - 1].is_ascii_alphanumeric() {
            for f in AUTHORIZED_FILLERS {
                let end = i + f.len();
                if lower.get(i..end) != Some(*f) {
                    continue;
                }
                if lower[end..].chars().next().is_some_and(char::is_alphanumeric) {
                    continue;
                }
                // The trailing space goes with the word.
                let span = end - i + usize::from(lower[end..].starts_with(' '));
                total += span;
                i += span;
                continue 'scan;
            }
        }
        i += lower[i..].chars().next().map_or(1, char::len_utf8);
    }
    total as f32
}

/// The word that asks for an emoji, normalized.
fn is_emoji_cue(tok: &str) -> bool {
    matches!(normalize_tok(tok).as_str(), "emoji" | "emojis")
}

/// How many characters of `raw` are a spoken emoji request that rule 10 replaces
/// with a single glyph: the cue word and up to two words before it ("laughing
/// emoji", "thumbs up emoji", "a crying emoji"), each with its following space.
///
/// Two words rather than one because the common names are two words long, and a
/// short message is where the discount matters — "thumbs up emoji" on its own
/// becomes "👍", which counted at one word is an 83% shrink. Two words over-count
/// on "send me an emoji" by the length of "me an", which is harmless at the
/// ceiling this feeds.
fn emoji_cue_mass(raw: &str) -> f32 {
    let toks: Vec<&str> = raw.split_whitespace().collect();
    let mut counted = vec![false; toks.len()];
    for (i, t) in toks.iter().enumerate() {
        if is_emoji_cue(t) {
            for c in &mut counted[i.saturating_sub(2)..=i] {
                *c = true;
            }
        }
    }
    toks.iter()
        .zip(&counted)
        .filter(|(_, c)| **c)
        .map(|(t, _)| t.chars().count() + 1)
        .sum::<usize>() as f32
}

/// A token made only of non-ASCII symbols: an emoji, possibly with a variation
/// selector, skin tone or ZWJ sequence attached. Never a word in any script,
/// because letters are alphanumeric and this requires none.
fn is_emoji_token(tok: &str) -> bool {
    !tok.is_empty() && tok.chars().all(|c| !c.is_ascii() && !c.is_alphanumeric())
}

/// Tokens that must survive cleanup verbatim: URLs, emails, and *substantial*
/// digit strings (phone numbers, account/order ids, years, versions — 4+ digits).
/// Short numbers (1–3 digits) are deliberately NOT protected: they are routinely
/// and correctly dropped by self-corrections ("meet at 2, actually 3") and by
/// number normalization, and protecting them made the gate reject legitimate
/// cleanups and fall back to raw.
fn must_preserve_entities(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for tok in text.split_whitespace() {
        let trimmed = tok.trim_matches(|c: char| c.is_ascii_punctuation() && c != '@' && c != '#');
        if trimmed.is_empty() {
            continue;
        }
        let is_url = trimmed.contains("://") || trimmed.contains(".com") || trimmed.contains('@');
        let digit_count = trimmed.chars().filter(|c| c.is_ascii_digit()).count();
        if is_url || digit_count >= 4 {
            out.push(trimmed.to_string());
        }
    }
    out
}

/// Lowercase a token and strip surrounding punctuation, so "3." == "3" and
/// "So" == "so" don't read as changes.
fn normalize_tok(t: &str) -> String {
    t.trim_matches(|c: char| c.is_ascii_punctuation())
        .to_lowercase()
}

/// The exact spellings the dictionary authorized for this utterance, as normalized
/// tokens. A multi-word entry ("Vinayak Sankaranarayanan") contributes each word.
///
/// Only the authoritative spellings are allowed through — never the mishears (they
/// are already in the raw transcript) and never anything else the model felt like
/// adding. So this widens the gate by exactly the words the user asked for and not
/// one token more: a rewrite that happens to mention a dictionary name still gets
/// caught on all its other novel words.
fn authorized_spellings(vocab: &[VocabEntry]) -> HashSet<String> {
    vocab
        .iter()
        .flat_map(|v| v.correct.split_whitespace())
        .map(normalize_tok)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Fraction of output words that were never spoken *and* were not an authorized
/// dictionary spelling. Filler deletion and casing/punctuation contribute nothing;
/// a genuine rewrite or hallucination (new content words) drives this up. A couple
/// of legitimate normalizations ("seven" -> "7") add a little, which the per-level
/// ceiling leaves room for.
///
/// An emoji is exempt when the transcript asked for one: the glyph is by definition
/// not in the raw text, and in a short message it is a large fraction of the output
/// ("thanks 😂" is half novel). An emoji nobody asked for still counts.
fn novelty_ratio(raw: &str, cleaned: &str, authorized: &HashSet<String>) -> f32 {
    let emoji_requested = raw.split_whitespace().any(is_emoji_cue);
    let raw_set: HashSet<String> = raw
        .split_whitespace()
        .map(normalize_tok)
        .filter(|s| !s.is_empty())
        .collect();
    let clean_toks: Vec<String> = cleaned
        .split_whitespace()
        .map(normalize_tok)
        .filter(|s| !s.is_empty())
        .collect();
    if clean_toks.is_empty() {
        return 0.0;
    }
    let novel = clean_toks
        .iter()
        .filter(|t| !raw_set.contains(*t) && !authorized.contains(*t))
        .filter(|t| !(emoji_requested && is_emoji_token(t)))
        .count();
    novel as f32 / clean_toks.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No dictionary was in play for this utterance.
    const NO_VOCAB: &[VocabEntry] = &[];

    fn vocab(entries: &[&str]) -> Vec<VocabEntry> {
        entries
            .iter()
            .map(|c| VocabEntry {
                correct: (*c).to_string(),
                mishears: Vec::new(),
            })
            .collect()
    }

    #[test]
    fn light_cleanup_passes() {
        // Filler removal + punctuation — a legitimate Light edit.
        let raw = "um so i think we should uh meet at 3";
        let clean = "So I think we should meet at 3.";
        assert!(evaluate(raw, clean, CleanupLevel::Light, NO_VOCAB).passed());
    }

    #[test]
    fn dropping_a_number_fails() {
        let raw = "transfer 500 dollars to account 12345";
        let clean = "Transfer money to the account."; // lost 500 and 12345
        let v = evaluate(raw, clean, CleanupLevel::Light, NO_VOCAB);
        assert!(matches!(v, GateVerdict::Fail(GateReason::LostEntity(_))));
    }

    #[test]
    fn answering_a_question_is_banned() {
        let raw = "what time is the standup";
        let clean = "Here is the standup schedule: 9am."; // model answered instead of transcribing
        let v = evaluate(raw, clean, CleanupLevel::Light, NO_VOCAB);
        assert!(matches!(v, GateVerdict::Fail(GateReason::BannedPattern(_))));
    }

    #[test]
    fn heavy_rewrite_exceeds_the_novelty_ceiling() {
        let raw = "i went to the store and then i bought some milk and eggs and bread";
        let clean = "Purchased dairy and bakery goods."; // huge rewrite
        assert!(matches!(
            evaluate(raw, clean, CleanupLevel::Light, NO_VOCAB),
            GateVerdict::Fail(_)
        ));
        // Ensure the ratio logic is sane on a milder rewrite, which must pass:
        let clean_mild = "I went to the store and bought milk, eggs, and bread.";
        assert!(evaluate(raw, clean_mild, CleanupLevel::Light, NO_VOCAB).passed());
    }

    #[test]
    fn over_deletion_fails() {
        let raw = "the quarterly report is due on friday please review the budget section";
        let clean = "Report due Friday."; // a summary: dropped three quarters
        assert!(matches!(
            evaluate(raw, clean, CleanupLevel::Light, NO_VOCAB),
            GateVerdict::Fail(GateReason::OverDeletion { .. })
        ));
    }

    /// The two real rejections that loosened this gate. Each is a self-correction
    /// the model resolved exactly as rule 3 asks, and each shrank past the old 55%
    /// line — so the raw text, cue and all, was what got pasted.
    #[test]
    fn a_resolved_self_correction_may_drop_most_of_the_utterance() {
        let raw = "Okay, so I want to talk about... Actually, um, scratch that. Let's talk about how life is.";
        let clean = "Let's talk about how life is."; // 0.63 shrink
        assert!(
            evaluate(raw, clean, CleanupLevel::Light, NO_VOCAB).passed(),
            "a resolved correction is not over-deletion"
        );
        let raw = "Okay, so I just noticed something that, you know, actually, sorry, I noticed \
                   something that... Actually, scratched that. I noticed that it works well on \
                   light, but it does not work on messaging.";
        let clean = "I noticed that it works well on light, but it does not work on messaging.";
        assert!(evaluate(raw, clean, CleanupLevel::Light, NO_VOCAB).passed());
    }

    /// The wider ceiling a correction cue buys must still catch the failure the gate
    /// exists for: the model answering the dictation instead of writing it down.
    #[test]
    fn a_correction_cue_does_not_excuse_answering_the_dictation() {
        let raw = "can you remove the stuff from the speech recognition page scratch that \
                   ignore everything else just for speech recognition can you just say \
                   either on this mac or cloud";
        let clean = "On this Mac or cloud.";
        assert!(matches!(
            evaluate(raw, clean, CleanupLevel::Light, NO_VOCAB),
            GateVerdict::Fail(GateReason::OverDeletion { .. })
        ));
    }

    /// Only the unambiguous cues widen the gate. "actually" and "sorry" appear in
    /// speech that corrects nothing, and a summary that happens to sit near one must
    /// not slip through on their account.
    #[test]
    fn an_ambiguous_cue_word_does_not_widen_the_gate() {
        let raw = "sorry i actually think the quarterly report is due on friday so please \
                   review the budget section before the meeting tomorrow";
        let clean = "Report due Friday, review budget."; // 0.72 shrink
        assert!(matches!(
            evaluate(raw, clean, CleanupLevel::Light, NO_VOCAB),
            GateVerdict::Fail(GateReason::OverDeletion { .. })
        ));
    }

    /// A spoken emoji request comes back as one glyph: not novel, not deletion.
    #[test]
    fn a_requested_emoji_is_neither_novel_nor_deletion() {
        assert!(evaluate(
            "haha that was hilarious laughing emoji see you tomorrow",
            "Haha, that was hilarious 😂 See you tomorrow.",
            CleanupLevel::Messaging,
            NO_VOCAB
        )
        .passed());
        // The whole message is the request, and the name is two words long.
        assert!(evaluate("thumbs up emoji", "👍", CleanupLevel::Light, NO_VOCAB).passed());
        // A bare "emoji" with no name, mid-sentence.
        assert!(evaluate(
            "that was so much fun emoji thanks for having me",
            "That was so much fun 🎉 Thanks for having me.",
            CleanupLevel::Light,
            NO_VOCAB
        )
        .passed());
        // A ZWJ sequence is still one emoji token.
        assert!(evaluate("thanks facepalm emoji", "Thanks 🤦‍♂️", CleanupLevel::Light, NO_VOCAB).passed());
    }

    /// Nobody asked for one, so it is a word the model invented like any other.
    #[test]
    fn an_uninvited_emoji_is_novel() {
        assert!(matches!(
            evaluate("thanks", "Thanks 😊", CleanupLevel::Light, NO_VOCAB),
            GateVerdict::Fail(GateReason::EditRatioTooHigh { .. })
        ));
    }

    #[test]
    fn emoji_cue_mass_counts_the_name_and_the_cue() {
        // "laughing " + "emoji " = 9 + 6.
        assert_eq!(emoji_cue_mass("laughing emoji"), 15.0);
        // Two words back, so "hilarious " (10) is counted too.
        assert_eq!(emoji_cue_mass("hilarious laughing emoji"), 25.0);
        // "never " + "use " + "emoji " = 6 + 4 + 6: two words back, and no further.
        assert_eq!(emoji_cue_mass("i never use emoji in email"), 16.0);
        assert_eq!(emoji_cue_mass("no cue here"), 0.0);
    }

    /// The Messaging register lowercases the paste, and "Amazon.com" is the same
    /// address as "amazon.com".
    #[test]
    fn entity_check_ignores_case() {
        assert!(evaluate("Amazon.com", "amazon.com", CleanupLevel::Messaging, NO_VOCAB).passed());
        assert!(evaluate(
            "order 84213 shipped",
            "order 84213 shipped",
            CleanupLevel::Messaging,
            NO_VOCAB
        )
        .passed());
    }

    /// The measured regression. A real 70-word dictation at speaking density, cleaned
    /// correctly by the 120b, shrank 56% on raw length and was rejected at the 55%
    /// line — so the raw transcript with every filler intact is what got pasted.
    /// Cleanup looked switched off exactly when it had worked best.
    #[test]
    fn removing_dense_filler_is_not_over_deletion() {
        let raw = "so look at the way sometimes you know when i'm saying something i'll be \
                   like oh sorry i didn't do this like i'll just like i'll say it you know \
                   and it manages to get it so correct how like you know it'll just clean up \
                   the sentence in that way";
        let clean = "So look at the way sometimes when I'm saying something, I'll be like, \
                     oh sorry I didn't do this, I'll just say it, and it manages to get it \
                     so correct, how it'll just clean up the sentence in that way.";
        assert!(
            evaluate(raw, clean, CleanupLevel::Light, NO_VOCAB).passed(),
            "filler removal must not read as content loss"
        );
    }

    /// Discounting filler must not become a licence to drop content. The same
    /// filler-dense input, genuinely gutted, is still caught.
    #[test]
    fn the_filler_discount_does_not_excuse_real_deletion() {
        let raw = "so you know the quarterly report is basically due on friday and like \
                   please review the budget section before the meeting";
        let clean = "Report due Friday.";
        assert!(matches!(
            evaluate(raw, clean, CleanupLevel::Light, NO_VOCAB),
            GateVerdict::Fail(GateReason::OverDeletion { .. })
        ));
    }

    /// Whole-word only: a filler living inside another word is not filler, and
    /// counting it would widen the gate past what rule 1 authorized.
    #[test]
    fn filler_mass_ignores_lookalike_words() {
        assert_eq!(filler_mass("that is unlikely and likeable"), 0.0);
        // "like " with its trailing space is 5 characters.
        assert_eq!(filler_mass("it was like this"), 5.0);
    }

    #[test]
    fn none_level_always_passes() {
        assert!(evaluate("anything", "totally different", CleanupLevel::None, NO_VOCAB).passed());
    }

    /// The regression this gate was changed for. A dictionary fix in a SHORT
    /// dictation used to blow the Light novelty ceiling on its own — one corrected
    /// word out of two is a 0.5 ratio — so the gate rejected the cleanup and pasted
    /// the raw transcript with the mishear still in it. The dictionary appeared to
    /// work on long sentences and silently do nothing on short ones.
    #[test]
    fn a_dictionary_fix_survives_a_short_dictation() {
        let raw = "hey monvi";
        let clean = "Hey Manvi.";
        assert!(
            evaluate(raw, clean, CleanupLevel::Light, &vocab(&["Manvi"])).passed(),
            "an authorized spelling must not read as novelty"
        );
        // And without the entry it is genuinely an unexplained new word: still caught.
        assert!(matches!(
            evaluate(raw, clean, CleanupLevel::Light, NO_VOCAB),
            GateVerdict::Fail(GateReason::EditRatioTooHigh { .. })
        ));
    }

    /// Two corrections in one short sentence was the worst case: 2 novel tokens out
    /// of 4 (0.40) against a 0.34 ceiling, on a sentence a person would plausibly say.
    #[test]
    fn two_dictionary_fixes_in_one_short_sentence_survive() {
        let raw = "ask monvi about charge bee";
        let clean = "Ask Manvi about ChargeBee.";
        assert!(evaluate(raw, clean, CleanupLevel::Light, &vocab(&["Manvi", "ChargeBee"])).passed());
    }

    /// A multi-word entry contributes each of its words.
    #[test]
    fn multi_word_entry_is_authorized_word_by_word() {
        let raw = "email vinayak sankara narayanan";
        let clean = "Email Vinayak Sankaranarayanan.";
        assert!(evaluate(
            raw,
            clean,
            CleanupLevel::Light,
            &vocab(&["Vinayak Sankaranarayanan"])
        )
        .passed());
    }

    /// The dictionary widens the gate by its own spellings only. Having an entry in
    /// play must not license the model to rewrite everything else around it.
    #[test]
    fn vocab_does_not_license_an_unrelated_rewrite() {
        let raw = "monvi said the quarterly numbers looked fine to her";
        let clean = "Manvi confirmed the fiscal results appeared satisfactory overall.";
        assert!(matches!(
            evaluate(raw, clean, CleanupLevel::Light, &vocab(&["Manvi"])),
            GateVerdict::Fail(GateReason::EditRatioTooHigh { .. })
        ));
    }

    /// Vocab relaxes novelty and nothing else — the entity and length gates are
    /// untouched, so a dictionary entry can never smuggle out a dropped account number.
    #[test]
    fn vocab_does_not_relax_the_other_gates() {
        let raw = "tell monvi the order is 84213";
        let clean = "Tell Manvi about the order."; // lost 84213
        assert!(matches!(
            evaluate(raw, clean, CleanupLevel::Light, &vocab(&["Manvi"])),
            GateVerdict::Fail(GateReason::LostEntity(_))
        ));
    }
}
