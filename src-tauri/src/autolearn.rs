//! Auto-learn: after WhimprFlow pastes dictated text, watch the focused text field
//! for the next twenty seconds. If the user corrects a single distinctive word
//! (typically a mis-heard name), diff it out and add it to the dictionary — so next
//! time ASR and cleanup spell it right. This is where ✨ entries come from.
//!
//! What gets stored as the mishear is what *recognition* wrote, not what was on
//! screen: the pasted text has been through cleanup, so the word the user corrected
//! may be a spelling Whisper never actually produces. The raw transcript settles it —
//! see `whimpr_core::dictionary::ground_truth_mishear`.
//!
//! It is deliberately conservative: it only learns on a clean one-word substitution
//! into an otherwise-unchanged field, where the new word looks like a proper noun
//! and is phonetically close to the word it replaced. That avoids poisoning the
//! dictionary with common-word edits. Reads use the Accessibility API and only run
//! when Accessibility is granted.

mod imp {
    use std::os::raw::{c_char, c_void};
    use std::ptr;
    use std::time::Duration;

    type CFTypeRef = *const c_void;
    type CFStringRef = *const c_void;
    type AXUIElementRef = *const c_void;

    const KCF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    /// How long after a paste to keep watching the field for a correction.
    ///
    /// Noticing a mis-heard name, selecting it and retyping it takes longer than it
    /// sounds — especially the first time, when you also have to decide whether it is
    /// worth fixing.
    const OBSERVE_WINDOW: Duration = Duration::from_secs(20);
    /// How often to look during that window.
    ///
    /// Polling rather than one snapshot at the end, because a single late look is
    /// *worse* than an early one for anyone who fixes the word and then keeps typing:
    /// by then the field has moved on and the diff is no longer the clean one-word swap
    /// that auto-learn will accept. Checking repeatedly and taking the first clean swap
    /// catches the correction at the moment it is still legible. An accessibility read
    /// every few seconds costs nothing.
    const OBSERVE_EVERY: Duration = Duration::from_secs(2);

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(cf: CFTypeRef);
        fn CFStringCreateWithCString(
            alloc: CFTypeRef,
            cstr: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
        fn CFStringGetLength(s: CFStringRef) -> isize;
        fn CFStringGetCString(s: CFStringRef, buf: *mut c_char, size: isize, encoding: u32) -> bool;
        fn CFStringGetMaximumSizeForEncoding(len: isize, encoding: u32) -> isize;
        fn CFGetTypeID(cf: CFTypeRef) -> usize;
        fn CFStringGetTypeID() -> usize;
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementCreateSystemWide() -> AXUIElementRef;
        fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: *mut CFTypeRef,
        ) -> i32;
    }

    fn make_cfstring(s: &str) -> CFStringRef {
        let Ok(c) = std::ffi::CString::new(s) else {
            return ptr::null();
        };
        unsafe { CFStringCreateWithCString(ptr::null(), c.as_ptr(), KCF_STRING_ENCODING_UTF8) }
    }

    /// Convert a CFStringRef to a Rust String (None if it isn't actually a string).
    unsafe fn cfstring_to_string(s: CFStringRef) -> Option<String> {
        if s.is_null() || CFGetTypeID(s) != CFStringGetTypeID() {
            return None;
        }
        let len = CFStringGetLength(s);
        let max = CFStringGetMaximumSizeForEncoding(len, KCF_STRING_ENCODING_UTF8) + 1;
        if max <= 0 {
            return Some(String::new());
        }
        let mut buf = vec![0i8; max as usize];
        if CFStringGetCString(s, buf.as_mut_ptr(), max, KCF_STRING_ENCODING_UTF8) {
            std::ffi::CStr::from_ptr(buf.as_ptr())
                .to_str()
                .ok()
                .map(|x| x.to_string())
        } else {
            None
        }
    }

    /// Copy the system-wide focused UI element (retained — caller CFReleases it).
    unsafe fn copy_focused_element() -> AXUIElementRef {
        let system = AXUIElementCreateSystemWide();
        if system.is_null() {
            return ptr::null();
        }
        let attr = make_cfstring("AXFocusedUIElement");
        let mut focused: CFTypeRef = ptr::null();
        let err = AXUIElementCopyAttributeValue(system, attr, &mut focused);
        if !attr.is_null() {
            CFRelease(attr);
        }
        CFRelease(system);
        if err != 0 {
            return ptr::null();
        }
        focused as AXUIElementRef
    }

    /// Read a text element's AXValue as a string.
    unsafe fn element_value(element: AXUIElementRef) -> Option<String> {
        if element.is_null() {
            return None;
        }
        let attr = make_cfstring("AXValue");
        let mut value: CFTypeRef = ptr::null();
        let err = AXUIElementCopyAttributeValue(element, attr, &mut value);
        if !attr.is_null() {
            CFRelease(attr);
        }
        if err != 0 || value.is_null() {
            return None;
        }
        let s = cfstring_to_string(value);
        CFRelease(value);
        s
    }

    /// A raw AX pointer we deliberately move to the observer thread. Safe because
    /// CF/AX types are internally thread-safe and we retain it before sending.
    struct SendPtr(AXUIElementRef);
    unsafe impl Send for SendPtr {}

    /// Right after paste, hold on to the focused field and watch it for a one-word
    /// correction to learn. `raw` is the pre-cleanup transcript, used to record what
    /// recognition actually wrote rather than what cleanup made of it.
    pub fn watch_correction(inserted: &str, raw: &str) {
        // Reads require Accessibility; also skip trivial dictations.
        if !crate::paste::is_trusted() || crate::autolearn::word_tokens(inserted).len() < 2 {
            return;
        }
        let inserted = inserted.to_string();
        let raw = raw.to_string();
        let focused = unsafe { copy_focused_element() };
        if focused.is_null() {
            return;
        }
        let holder = SendPtr(focused);
        std::thread::spawn(move || {
            // Force whole-struct capture (2021 disjoint captures would otherwise grab
            // the raw pointer field and lose the `Send` impl on `SendPtr`).
            let holder = holder;
            let deadline = std::time::Instant::now() + OBSERVE_WINDOW;
            let found = loop {
                std::thread::sleep(OBSERVE_EVERY);
                if let Some(after) = unsafe { element_value(holder.0) } {
                    // First clean one-word swap wins — later looks only get muddier as
                    // the user keeps typing around it.
                    if let Some(pair) = super::detect_correction(&inserted, &after) {
                        break Some(pair);
                    }
                }
                if std::time::Instant::now() >= deadline {
                    break None;
                }
            };
            unsafe { CFRelease(holder.0) };
            let Some((observed, correct)) = found else { return };

            // Prefer what recognition wrote over what cleanup wrote — that is the
            // string the pre-filter will have to match next time. Keep both when they
            // differ, since cleanup may mangle it the same way again.
            let mut mishears = vec![observed.clone()];
            if let Some(truth) = whimpr_core::dictionary::ground_truth_mishear(&raw, &observed) {
                eprintln!("[whimpr] ✨ recognition actually wrote \"{truth}\"");
                mishears.push(truth);
            }
            eprintln!("[whimpr] ✨ auto-learned: {mishears:?} -> \"{correct}\"");
            crate::hotkey::dictionary_learn(correct, mishears);
        });
    }
}

pub use imp::watch_correction;

/// Split into alphanumeric word tokens (punctuation stripped), original case kept.
pub fn word_tokens(s: &str) -> Vec<String> {
    s.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_string()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Very common words we never learn as a "correction" — avoids dictionary poisoning
/// from ordinary edits (their/there, your/you're, then/than, sentence rewording…).
const COMMON: &[&str] = &[
    "the", "and", "for", "are", "but", "not", "you", "your", "youre", "with", "this", "that",
    "have", "from", "they", "theyre", "their", "there", "would", "could", "should", "about",
    "then", "than", "them", "these", "those", "here", "were", "well", "will", "what", "when",
    "where", "which", "while", "your", "into", "just", "like", "make", "made", "want", "some",
    "time", "know", "take", "come", "back", "good", "much", "also", "been", "over", "only",
    "more", "most", "very", "even", "such", "many", "does", "done", "same", "sure", "okay",
    "yeah", "hey", "hello", "please", "thanks", "thank", "message", "email", "text", "call",
];

/// Detect a single clean one-word correction: exactly one word removed from the
/// inserted text and one word added in the field, both distinctive and phonetically
/// close, with the new word looking like a proper noun. Returns `(mishear, correct)`.
pub fn detect_correction(inserted: &str, after: &str) -> Option<(String, String)> {
    use std::collections::HashSet;
    let ins = word_tokens(inserted);
    let aft = word_tokens(after);
    if ins.is_empty() || aft.is_empty() {
        return None;
    }
    let ins_lc: HashSet<String> = ins.iter().map(|w| w.to_lowercase()).collect();
    let aft_lc: HashSet<String> = aft.iter().map(|w| w.to_lowercase()).collect();

    let removed: Vec<&String> = ins.iter().filter(|w| !aft_lc.contains(&w.to_lowercase())).collect();
    let added: Vec<&String> = aft.iter().filter(|w| !ins_lc.contains(&w.to_lowercase())).collect();
    if removed.len() != 1 || added.len() != 1 {
        return None; // only learn on a clean 1-for-1 swap
    }
    let mishear = removed[0].clone();
    let correct = added[0].clone();

    let alpha = |w: &str| w.chars().all(|c| c.is_alphabetic());
    if mishear.chars().count() < 3 || correct.chars().count() < 3 {
        return None;
    }
    if !alpha(&mishear) || !alpha(&correct) {
        return None;
    }
    if correct.eq_ignore_ascii_case(&mishear) {
        return None;
    }
    if is_common(&correct) || is_common(&mishear) {
        return None;
    }
    // The correction should look like a name (Titlecase) and be phonetically close
    // to what it replaced (a real mishear, not an unrelated rewrite).
    let titled = correct.chars().next().is_some_and(|c| c.is_uppercase());
    let d = norm_levenshtein(&mishear, &correct);
    if titled && d > 0.0 && d <= 0.6 {
        Some((mishear, correct))
    } else {
        None
    }
}

fn is_common(w: &str) -> bool {
    let lc = w.to_lowercase();
    COMMON.contains(&lc.as_str())
}

/// Levenshtein distance normalized by the longer length (0 = identical, 1 = totally
/// different).
fn norm_levenshtein(a: &str, b: &str) -> f32 {
    let a = a.to_lowercase();
    let b = b.to_lowercase();
    let m = a.chars().count().max(b.chars().count());
    if m == 0 {
        return 1.0;
    }
    strsim::levenshtein(&a, &b) as f32 / m as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn learns_a_name_correction() {
        // We inserted "monvi"; the user fixed it to "Manvi".
        let got = detect_correction("send the deck to monvi please", "send the deck to Manvi please");
        assert_eq!(got, Some(("monvi".to_string(), "Manvi".to_string())));
    }

    #[test]
    fn ignores_common_word_edits() {
        // "there" -> "their" is a common-word edit, never learned.
        assert_eq!(detect_correction("i left there bag", "i left their bag"), None);
    }

    #[test]
    fn ignores_multi_word_changes() {
        // More than one word changed → too ambiguous, skip.
        assert_eq!(detect_correction("meet at noon monvi", "see you later Manvi"), None);
    }

    #[test]
    fn ignores_unrelated_replacement() {
        // Not phonetically close → not a mishear.
        assert_eq!(detect_correction("ping the server foo", "ping the server Xylophone"), None);
    }

    #[test]
    fn no_change_learns_nothing() {
        assert_eq!(detect_correction("hello there world", "hello there world"), None);
    }
}
