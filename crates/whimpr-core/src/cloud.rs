//! Several API keys for one endpoint, and which of them to use right now.
//!
//! A free-tier key has a per-minute and a per-day token cap, and either one arriving
//! mid-sentence used to mean a raw paste (iOS) or a silent hop to the local model
//! (Mac) until the window rolled over. With more than one key the answer is to use
//! the next one and come back when the first is free again. This module is the
//! policy; the providers (Mac) and `GroqClient` (iOS) supply the HTTP and the clock.
//!
//! Pure on purpose: `now` is passed in, so the same policy runs — and is tested — on
//! both platforms, and the bridge carries the ring as JSON.

use serde::{Deserialize, Serialize};

/// A limit with no usable hint is assumed to be a per-minute cap.
const DEFAULT_LIMIT_SECS: f64 = 60.0;
/// A hint past this is a wrong parse, not a wait anyone should honour.
const MAX_LIMIT_SECS: f64 = 24.0 * 3600.0;

/// The keys, in the order the user added them, and until when each is rate limited.
///
/// Order is preference: the first key is tried whenever it is free, so the account the
/// user thinks of as "theirs" carries the load and the others only fill the gaps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct KeyRing {
    keys: Vec<String>,
    /// Unix seconds until which the key at the same index is limited; 0 means free.
    /// Sized to `keys` on every access, because a ring round-tripped through the
    /// bridge may come back with the field missing.
    #[serde(default)]
    limited_until: Vec<u64>,
}

impl KeyRing {
    /// Trims, drops empties, and keeps the first occurrence of a duplicate.
    pub fn new(keys: impl IntoIterator<Item = String>) -> Self {
        let mut ring = KeyRing::default();
        for k in keys {
            ring.add(&k);
        }
        ring
    }

    /// The Keychain form: one key per line. A single key stored before there was a
    /// list is a one-line file and parses as a ring of one.
    pub fn from_stored(text: &str) -> Self {
        Self::new(text.lines().map(str::to_string))
    }

    pub fn to_stored(&self) -> String {
        self.keys.join("\n")
    }

    pub fn keys(&self) -> &[String] {
        &self.keys
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Append a key. `false` when it was blank or already present.
    pub fn add(&mut self, key: &str) -> bool {
        let key = key.trim();
        if key.is_empty() || self.keys.iter().any(|k| k == key) {
            return false;
        }
        self.keys.push(key.to_string());
        self.limited_until.push(0);
        true
    }

    /// Remove the key at `index`. `false` when there is none.
    pub fn remove(&mut self, index: usize) -> bool {
        if index >= self.keys.len() {
            return false;
        }
        self.keys.remove(index);
        if index < self.limited_until.len() {
            self.limited_until.remove(index);
        }
        true
    }

    /// The keys as a settings screen may show them: enough to tell two apart, not
    /// enough to use.
    pub fn masked(&self) -> Vec<String> {
        self.keys.iter().map(|k| mask(k)).collect()
    }

    /// The first key that is not rate limited at `now`, by index.
    pub fn pick(&self, now: u64) -> Option<usize> {
        (0..self.keys.len()).find(|&i| self.limited(i) <= now)
    }

    /// How long until *some* key is usable, when [`pick`](Self::pick) found none.
    /// `None` when there are no keys at all.
    pub fn wait_secs(&self, now: u64) -> Option<u64> {
        (0..self.keys.len())
            .map(|i| self.limited(i).saturating_sub(now))
            .min()
    }

    /// The endpoint answered 429 for the key at `index`. `retry_after_secs` is what
    /// [`retry_after_secs`] made of the response; `None` assumes a per-minute cap.
    pub fn report_limited(&mut self, index: usize, now: u64, retry_after_secs: Option<f64>) {
        if index >= self.keys.len() {
            return;
        }
        if self.limited_until.len() < self.keys.len() {
            self.limited_until.resize(self.keys.len(), 0);
        }
        let secs = retry_after_secs
            .filter(|s| s.is_finite())
            .unwrap_or(DEFAULT_LIMIT_SECS)
            .clamp(1.0, MAX_LIMIT_SECS);
        self.limited_until[index] = now + secs.ceil() as u64;
    }

    fn limited(&self, index: usize) -> u64 {
        self.limited_until.get(index).copied().unwrap_or(0)
    }
}

fn mask(key: &str) -> String {
    let n = key.chars().count();
    if n < 12 {
        return "••••".to_string();
    }
    let head: String = key.chars().take(4).collect();
    let tail: String = key.chars().skip(n - 4).collect();
    format!("{head}…{tail}")
}

/// How long a 429 asks us to wait, in seconds, from the `Retry-After` header when
/// there is one and otherwise from the body.
///
/// Groq puts the useful number in the body, as prose: `"Please try again in
/// 16m58.224s"`, `"try again in 9.6825s"`, and it is the *daily* cap that produces
/// the minutes form. A parser that stops at the first `s` reads `"5m56.832s"` as
/// nothing and falls back to a default, which is how the harness used to wait twelve
/// seconds eight times against a limit that wanted six minutes.
pub fn retry_after_secs(header: Option<&str>, body: &str) -> Option<f64> {
    if let Some(secs) = header.and_then(|h| h.trim().parse::<f64>().ok()) {
        if secs > 0.0 {
            return Some(secs);
        }
    }
    let lower = body.to_ascii_lowercase();
    let tail = lower.split("try again in ").nth(1)?;
    parse_duration(tail.trim_start())
}

/// `"1h2m3.5s"`, `"16m58.224s"`, `"9.68s"`, `"2m"` → seconds. Stops at the first
/// character that is not part of a duration, so trailing prose is fine.
fn parse_duration(s: &str) -> Option<f64> {
    let mut total = 0.0;
    let mut number = String::new();
    let mut any = false;
    for c in s.chars() {
        if c.is_ascii_digit() || c == '.' {
            number.push(c);
            continue;
        }
        let unit = match c {
            'h' => 3600.0,
            'm' => 60.0,
            's' => 1.0,
            _ => break,
        };
        let value: f64 = number.parse().ok()?;
        total += value * unit;
        number.clear();
        any = true;
    }
    any.then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring(keys: &[&str]) -> KeyRing {
        KeyRing::new(keys.iter().map(|k| k.to_string()))
    }

    #[test]
    fn a_single_stored_key_is_a_ring_of_one() {
        let r = KeyRing::from_stored("gsk_abc\n");
        assert_eq!(r.keys(), ["gsk_abc"]);
        assert_eq!(r.to_stored(), "gsk_abc");
    }

    #[test]
    fn stored_form_round_trips_and_drops_blanks_and_duplicates() {
        let r = KeyRing::from_stored("a\n\n b \na\n");
        assert_eq!(r.keys(), ["a", "b"]);
        assert_eq!(KeyRing::from_stored(&r.to_stored()), r);
    }

    /// The first key is the preferred one and comes back the moment it is free.
    #[test]
    fn picks_the_first_free_key_and_returns_to_it() {
        let mut r = ring(&["a", "b"]);
        assert_eq!(r.pick(100), Some(0));
        r.report_limited(0, 100, Some(30.0));
        assert_eq!(r.pick(100), Some(1));
        assert_eq!(r.pick(129), Some(1));
        assert_eq!(r.pick(130), Some(0), "the limit has passed");
    }

    #[test]
    fn every_key_limited_reports_the_shortest_wait() {
        let mut r = ring(&["a", "b"]);
        r.report_limited(0, 100, Some(600.0));
        r.report_limited(1, 100, Some(45.0));
        assert_eq!(r.pick(100), None);
        assert_eq!(r.wait_secs(100), Some(45));
        assert_eq!(ring(&[]).wait_secs(100), None);
    }

    #[test]
    fn a_limit_with_no_hint_is_a_minute() {
        let mut r = ring(&["a"]);
        r.report_limited(0, 0, None);
        assert_eq!(r.pick(59), None);
        assert_eq!(r.pick(60), Some(0));
    }

    /// The daily-cap wording. This is the one the old harness parser misread.
    #[test]
    fn parses_groqs_prose_retry_hint() {
        let daily = r#"{"error":{"message":"Rate limit reached for model `x` on tokens per day (TPD): Limit 200000, Used 199533, Requested 2824. Please try again in 16m58.224s. Need more tokens?"}}"#;
        assert_eq!(retry_after_secs(None, daily), Some(16.0 * 60.0 + 58.224));
        assert_eq!(retry_after_secs(None, "Please try again in 9.6825s."), Some(9.6825));
        assert_eq!(retry_after_secs(None, "try again in 1h2m3s"), Some(3723.0));
        assert_eq!(retry_after_secs(None, "no hint here"), None);
    }

    #[test]
    fn the_header_wins_when_present_and_numeric() {
        assert_eq!(retry_after_secs(Some("120"), "try again in 5s"), Some(120.0));
        // An HTTP-date header is not a number; fall through to the body.
        assert_eq!(
            retry_after_secs(Some("Wed, 21 Oct 2026 07:28:00 GMT"), "try again in 5s"),
            Some(5.0)
        );
    }

    /// A ring that crossed the bridge without its clocks must still work.
    #[test]
    fn deserializes_without_the_limit_field() {
        let mut r: KeyRing = serde_json::from_str(r#"{"keys":["a","b"]}"#).unwrap();
        assert_eq!(r.pick(0), Some(0));
        r.report_limited(1, 0, Some(10.0));
        assert_eq!(r.pick(0), Some(0));
        r.report_limited(0, 0, Some(10.0));
        assert_eq!(r.pick(0), None);
        assert_eq!(r.pick(10), Some(0));
    }

    #[test]
    fn masks_show_the_ends_only() {
        assert_eq!(ring(&["gsk_1234567890abcdef"]).masked(), ["gsk_…cdef"]);
        assert_eq!(ring(&["short"]).masked(), ["••••"]);
    }

    #[test]
    fn remove_keeps_the_clocks_aligned() {
        let mut r = ring(&["a", "b", "c"]);
        r.report_limited(1, 0, Some(100.0));
        assert!(r.remove(0));
        assert_eq!(r.keys(), ["b", "c"]);
        assert_eq!(r.pick(0), Some(1), "b is still limited after the shift");
        assert!(!r.remove(5));
    }
}
