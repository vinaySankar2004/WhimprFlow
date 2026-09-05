//! Dictation usage stats — words dictated, speaking time, words-per-minute,
//! streaks, and estimated time saved vs typing. One small record is appended per
//! completed dictation and persisted as JSON (same dependency-light pattern as
//! [`crate::settings`] and [`crate::dictionary`]).
//!
//! All "today"/"streak" bucketing is done against a timezone offset the UI passes
//! in (minutes to add to local time to reach UTC, i.e. JS `getTimezoneOffset()`),
//! so day boundaries line up with the user's own clock without pulling in a
//! timezone crate.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Average typing speed (words/min) we compare speaking against for "time saved".
/// 45 wpm matches Wispr Flow's own typed baseline (they cite 45 typed vs ~220 spoken).
const TYPING_WPM_BASELINE: f64 = 45.0;

const DAY_SECS: i64 = 86_400;

/// One completed dictation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    /// Seconds since the Unix epoch (UTC) when the dictation was committed.
    pub ts_unix: u64,
    /// Word count of the final inserted text.
    pub words: u32,
    /// Speaking duration in milliseconds.
    pub duration_ms: u32,
    /// Character count of the final inserted text.
    pub chars: u32,
    /// The cleaned/inserted text (for the Home history list). Older records may
    /// predate this field.
    #[serde(default)]
    pub text: String,
    /// The raw ASR transcript, before cleanup — empty when the record predates this
    /// field or when the user turned transcript storage off.
    ///
    /// Kept because everything interesting about *how someone speaks* is in the words
    /// cleanup removes: fillers, stutters, self-corrections, spoken punctuation. The
    /// cleaned text alone cannot answer any of it. This is also the only honest
    /// measure of what cleanup actually did for a given dictation.
    #[serde(default)]
    pub raw: String,
    /// Bundle id of the app the text was inserted into, if known.
    #[serde(default)]
    pub app: Option<String>,
    /// Milliseconds spent in recognition, and in cleanup, for this dictation.
    ///
    /// Recorded because "dictation feels slow" is otherwise unattributable after the
    /// fact, and the intuitive culprit — the cleanup model — is often the cheaper
    /// half. Both stages block the paste and they are wildly different costs
    /// depending on which engine each is set to, so the only way to know where the
    /// wait went is to have written it down at the time. Zero on records that
    /// predate the fields.
    #[serde(default)]
    pub asr_ms: u32,
    #[serde(default)]
    pub cleanup_ms: u32,
    /// Which engine actually served each stage: `"local"`, `"cloud"`, or — for
    /// cleanup only — `"raw"`, meaning nothing cleaned it.
    ///
    /// The *setting* is not the answer. Both stages fall back and fall forward, so on
    /// any given dictation the engine that ran may not be the one selected, and that
    /// difference is the interesting part: it is how "cleanup feels worse today"
    /// turns into "the cloud 429'd at 11:14 and the local model has been serving
    /// since". Empty on records written before these fields.
    #[serde(default)]
    pub asr_engine: String,
    #[serde(default)]
    pub cleanup_engine: String,
    /// Why this dictation did not take the intended path, as a short stable key with
    /// optional detail (`"cloud_error: HTTP 429"`, `"gate_rejected: OverDeletion"`,
    /// `"no_local_model"`). `None` when the selected engines served it.
    ///
    /// This is the field worth having. Every degradation in this app is deliberately
    /// silent — the point of falling back is that the dictation survives — so without
    /// writing the reason down at the time, a run of raw pastes has no explanation
    /// after the fact.
    #[serde(default)]
    pub degraded: Option<String>,
}

/// A history row for the Hub Home list (newest first). Trimmed view of a record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryItem {
    pub ts_unix: u64,
    pub text: String,
    pub app: Option<String>,
    pub words: u32,
}

/// One page of history: which records to search, and which slice of the matches.
///
/// Filtering and paging happen here rather than in the webview because the log
/// grows without bound — handing the UI every dictation ever made so it can show
/// ten of them gets slower every day someone uses the app, and a search that only
/// covers the most recent N silently fails to find older text.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HistoryQuery {
    /// Case-insensitive substring over the dictated text. Empty matches everything.
    #[serde(default)]
    pub search: String,
    /// Only records at or after this Unix time. 0 means no lower bound. The caller
    /// computes it, so day boundaries follow the user's own clock and no timezone
    /// logic is needed here.
    #[serde(default)]
    pub since_unix: u64,
    /// How many matches to skip (page number × page size).
    #[serde(default)]
    pub offset: usize,
    /// Page size. 0 returns no items — useful for a count-only probe.
    #[serde(default)]
    pub limit: usize,
}

/// A page of matches plus the total that matched, so the UI can render "11–20 of
/// 347" and know whether a next page exists without asking again.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HistoryPage {
    pub items: Vec<HistoryItem>,
    pub total: usize,
}

/// The persisted stats log.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StatsStore {
    #[serde(default)]
    pub sessions: Vec<SessionRecord>,
}

/// Aggregated stats for the Hub. Everything the UI needs to draw the dashboard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatsSummary {
    pub total_words: u64,
    pub total_sessions: u64,
    pub total_speaking_secs: f64,
    /// Lifetime average speaking speed (words/min).
    pub avg_wpm: u32,
    /// Fastest single dictation (words/min), ignoring trivially short ones.
    pub best_wpm: u32,
    pub words_today: u64,
    pub wpm_today: u32,
    /// Consecutive days (up to today) with at least one dictation.
    pub day_streak: u32,
    /// Estimated seconds saved vs typing the same words at [`TYPING_WPM_BASELINE`].
    pub time_saved_secs: f64,
    /// Words per local day, oldest first; index 6 is today, 0 is six days ago.
    pub last7_words: [u64; 7],
}

/// Count whitespace-delimited words. Matches how the cleanup layer thinks of words.
pub fn count_words(text: &str) -> u32 {
    text.split_whitespace().count() as u32
}

/// The local calendar day index for a UTC timestamp, given the UI's tz offset
/// (minutes to add to local to get UTC, per JS `Date.getTimezoneOffset()`).
fn local_day(ts_unix: u64, tz_offset_minutes: i32) -> i64 {
    let local = ts_unix as i64 - (tz_offset_minutes as i64) * 60;
    local.div_euclid(DAY_SECS)
}

/// Words/min from words and a duration, rounded; 0 for empty/instant sessions.
fn wpm(words: u64, secs: f64) -> u32 {
    if secs <= 0.0 || words == 0 {
        return 0;
    }
    (words as f64 / (secs / 60.0)).round() as u32
}

impl StatsStore {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self).unwrap_or_default())
    }

    /// Append one completed dictation. `raw` is the pre-cleanup transcript; pass an
    /// empty string when transcript storage is off.
    #[allow(clippy::too_many_arguments)]
    /// Append a fully-built record. The shell uses this; `record` below is the
    /// shorthand the tests are written against. New fields go on the struct and
    /// stay off this signature — a `record` that grew a positional argument per
    /// field is how a caller ends up silently passing `cleanup_ms` as `asr_ms`.
    pub fn push(&mut self, record: SessionRecord) {
        self.sessions.push(record);
    }

    /// Test-only shorthand for [`push`](Self::push), kept because the stats tests
    /// are written against it and read better for it. Not available to the shell:
    /// a positional argument per field is precisely how a caller ends up passing
    /// `cleanup_ms` where `asr_ms` was meant, which is why `push` takes the struct.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &mut self,
        words: u32,
        duration_ms: u32,
        chars: u32,
        ts_unix: u64,
        text: String,
        raw: String,
        app: Option<String>,
    ) {
        self.push(SessionRecord {
            ts_unix,
            words,
            duration_ms,
            chars,
            text,
            raw,
            app,
            asr_ms: 0,
            cleanup_ms: 0,
            asr_engine: String::new(),
            cleanup_engine: String::new(),
            degraded: None,
        });
    }

    /// Drop the stored text of every dictation, keeping the counts. The stats — words,
    /// speaking time, WPM, streak — are derived from the numeric fields, so this
    /// clears what was *said* without resetting what was *earned*. That is what makes
    /// it a usable privacy control rather than a factory reset nobody will press.
    pub fn forget_transcripts(&mut self) {
        for s in &mut self.sessions {
            s.text.clear();
            s.raw.clear();
        }
    }

    /// One page of history, newest first, filtered by search text and start time.
    pub fn query(&self, q: &HistoryQuery) -> HistoryPage {
        let needle = q.search.trim().to_lowercase();
        let matches = self
            .sessions
            .iter()
            .rev()
            .filter(|s| !s.text.is_empty())
            .filter(|s| s.ts_unix >= q.since_unix)
            .filter(|s| needle.is_empty() || s.text.to_lowercase().contains(&needle));

        // Count and slice in one pass over the same predicate chain, so `total` can
        // never disagree with what the page actually contains.
        let mut total = 0usize;
        let mut items = Vec::new();
        for s in matches {
            total += 1;
            let idx = total - 1;
            if idx >= q.offset && items.len() < q.limit {
                items.push(HistoryItem {
                    ts_unix: s.ts_unix,
                    text: s.text.clone(),
                    app: s.app.clone(),
                    words: s.words,
                });
            }
        }
        HistoryPage { items, total }
    }

    /// Aggregate everything the dashboard shows. `now_unix` and `tz_offset_minutes`
    /// come from the caller so day math matches the user's local clock (and so the
    /// aggregation stays pure/testable).
    pub fn summary(&self, tz_offset_minutes: i32, now_unix: u64) -> StatsSummary {
        let total_words: u64 = self.sessions.iter().map(|s| s.words as u64).sum();
        let total_sessions = self.sessions.len() as u64;
        let total_speaking_secs: f64 =
            self.sessions.iter().map(|s| s.duration_ms as f64 / 1000.0).sum();

        let avg_wpm = wpm(total_words, total_speaking_secs);

        // Best WPM, ignoring blips that inflate the number (need real words + time).
        let best_wpm = self
            .sessions
            .iter()
            .filter(|s| s.words >= 3 && s.duration_ms >= 1000)
            .map(|s| wpm(s.words as u64, s.duration_ms as f64 / 1000.0))
            .max()
            .unwrap_or(0);

        let today = local_day(now_unix, tz_offset_minutes);

        let mut words_today: u64 = 0;
        let mut secs_today: f64 = 0.0;
        let mut last7_words = [0u64; 7];
        for s in &self.sessions {
            let day = local_day(s.ts_unix, tz_offset_minutes);
            if day == today {
                words_today += s.words as u64;
                secs_today += s.duration_ms as f64 / 1000.0;
            }
            let ago = today - day; // 0 = today, 6 = six days ago
            if (0..7).contains(&ago) {
                last7_words[(6 - ago) as usize] += s.words as u64;
            }
        }
        let wpm_today = wpm(words_today, secs_today);

        // Streak: consecutive days with activity, up to today. A day with no
        // dictations yet doesn't break the streak until it's fully past, so if
        // today is still empty we start counting from yesterday.
        use std::collections::HashSet;
        let active: HashSet<i64> = self
            .sessions
            .iter()
            .map(|s| local_day(s.ts_unix, tz_offset_minutes))
            .collect();
        let mut day_streak = 0u32;
        let mut d = if active.contains(&today) { today } else { today - 1 };
        while active.contains(&d) {
            day_streak += 1;
            d -= 1;
        }

        // Time saved: how long these words would take to type at the baseline,
        // minus the time actually spent speaking. Never negative.
        let typed_secs = total_words as f64 / TYPING_WPM_BASELINE * 60.0;
        let time_saved_secs = (typed_secs - total_speaking_secs).max(0.0);

        StatsSummary {
            total_words,
            total_sessions,
            total_speaking_secs,
            avg_wpm,
            best_wpm,
            words_today,
            wpm_today,
            day_streak,
            time_saved_secs,
            last7_words,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A fixed "now": 2021-01-08 12:00:00 UTC.
    const NOW: u64 = 1_610_107_200;
    const DAY: u64 = 86_400;

    #[test]
    fn counts_words() {
        assert_eq!(count_words("  hello   there  world "), 3);
        assert_eq!(count_words(""), 0);
    }

    #[test]
    fn aggregates_totals_and_wpm() {
        let mut s = StatsStore::default();
        // 60 words in 60s -> 60 wpm.
        s.record(60, 60_000, 300, NOW, String::new(), String::new(), None);
        // 30 words in 15s -> 120 wpm.
        s.record(30, 15_000, 150, NOW, String::new(), String::new(), None);
        let sum = s.summary(0, NOW);
        assert_eq!(sum.total_words, 90);
        assert_eq!(sum.total_sessions, 2);
        // 90 words / 75s = 72 wpm.
        assert_eq!(sum.avg_wpm, 72);
        assert_eq!(sum.best_wpm, 120);
        assert_eq!(sum.words_today, 90);
    }

    #[test]
    fn streak_counts_consecutive_days_including_gap_today() {
        let mut s = StatsStore::default();
        // Activity yesterday, day-before, and three days ago (but NOT today).
        s.record(10, 5_000, 50, NOW - DAY, String::new(), String::new(), None);
        s.record(10, 5_000, 50, NOW - 2 * DAY, String::new(), String::new(), None);
        s.record(10, 5_000, 50, NOW - 3 * DAY, String::new(), String::new(), None);
        // Gap at 4 days ago, then one more.
        s.record(10, 5_000, 50, NOW - 5 * DAY, String::new(), String::new(), None);
        let sum = s.summary(0, NOW);
        // Today empty -> start at yesterday; 3 consecutive days back, then a gap.
        assert_eq!(sum.day_streak, 3);
        assert_eq!(sum.words_today, 0);
    }

    #[test]
    fn last7_buckets_by_local_day() {
        let mut s = StatsStore::default();
        s.record(5, 3_000, 25, NOW, String::new(), String::new(), None); // today
        s.record(7, 3_000, 35, NOW - 2 * DAY, String::new(), String::new(), None); // 2 days ago
        let sum = s.summary(0, NOW);
        assert_eq!(sum.last7_words[6], 5); // today
        assert_eq!(sum.last7_words[4], 7); // two days ago
        assert_eq!(sum.last7_words[5], 0);
    }

    /// 25 dictations, oldest first, one per hour going back from NOW.
    fn logged(n: usize) -> StatsStore {
        let mut s = StatsStore::default();
        for i in (0..n).rev() {
            s.record(
                3,
                2_000,
                20,
                NOW - (i as u64) * 3_600,
                format!("dictation number {i}"),
                format!("um dictation number {i}"),
                Some("com.apple.Notes".into()),
            );
        }
        s
    }

    #[test]
    fn query_pages_newest_first_and_reports_the_full_total() {
        let s = logged(25);
        let page = s.query(&HistoryQuery { limit: 10, ..Default::default() });
        assert_eq!(page.items.len(), 10);
        assert_eq!(page.total, 25, "total is every match, not the page size");
        // Newest first: index 0 was recorded last.
        assert_eq!(page.items[0].text, "dictation number 0");

        let page2 = s.query(&HistoryQuery { offset: 10, limit: 10, ..Default::default() });
        assert_eq!(page2.items[0].text, "dictation number 10");
        assert_eq!(page2.total, 25);

        // Last page is short, and paging past the end is empty rather than an error.
        assert_eq!(s.query(&HistoryQuery { offset: 20, limit: 10, ..Default::default() }).items.len(), 5);
        assert!(s.query(&HistoryQuery { offset: 99, limit: 10, ..Default::default() }).items.is_empty());
    }

    #[test]
    fn query_search_is_case_insensitive_and_totals_only_matches() {
        let mut s = logged(3);
        s.record(2, 1_000, 10, NOW, "Ship the RELEASE notes".into(), String::new(), None);
        let page = s.query(&HistoryQuery { search: "release".into(), limit: 10, ..Default::default() });
        assert_eq!(page.total, 1);
        assert_eq!(page.items.len(), 1);
    }

    /// Search must reach the whole log, not just the page — the bug in a client-side
    /// filter over a capped list is that old matches silently do not exist.
    #[test]
    fn query_search_reaches_past_the_first_page() {
        let mut s = logged(30);
        s.record(2, 1_000, 10, NOW - 100 * 3_600, "the oldest one".into(), String::new(), None);
        let page = s.query(&HistoryQuery { search: "oldest".into(), limit: 10, ..Default::default() });
        assert_eq!(page.total, 1);
    }

    #[test]
    fn query_since_bounds_the_window() {
        let s = logged(25);
        // Only the last 5 hours.
        let page = s.query(&HistoryQuery { since_unix: NOW - 5 * 3_600, limit: 100, ..Default::default() });
        assert_eq!(page.total, 6, "inclusive lower bound");
    }

    #[test]
    fn forgetting_transcripts_keeps_the_numbers() {
        let mut s = logged(4);
        let before = s.summary(0, NOW);
        s.forget_transcripts();
        assert!(s.sessions.iter().all(|r| r.text.is_empty() && r.raw.is_empty()));
        assert_eq!(s.summary(0, NOW), before, "stats are derived from counts, not text");
        // …and an emptied record drops out of history rather than showing as a blank row.
        assert_eq!(s.query(&HistoryQuery { limit: 10, ..Default::default() }).total, 0);
    }

    /// A stats.json written before `raw` existed must still load with every other
    /// field intact — without `#[serde(default)]` the parse fails and the whole log
    /// is silently replaced by an empty one.
    #[test]
    fn older_stats_file_without_raw_still_loads() {
        let json = r#"{"sessions":[
            {"ts_unix":1610107200,"words":5,"duration_ms":2000,"chars":25,
             "text":"hello there","app":null}
        ]}"#;
        let s: StatsStore = serde_json::from_str(json).expect("old file still parses");
        assert_eq!(s.sessions[0].text, "hello there");
        assert_eq!(s.sessions[0].raw, "");
        assert_eq!(s.summary(0, NOW).total_words, 5);
        // The usage fields added later must also default rather than fail the parse.
        // Without `#[serde(default)]` on each, one unknown shape takes the whole
        // stats log down and `load` silently returns an empty store — every word
        // ever dictated, gone, with no error anywhere.
        assert_eq!(s.sessions[0].asr_engine, "");
        assert_eq!(s.sessions[0].cleanup_engine, "");
        assert_eq!(s.sessions[0].degraded, None);
        assert_eq!(s.sessions[0].asr_ms, 0);
    }

    /// A degraded dictation round-trips with its reason, because the reason is the
    /// only record of why a paste came out raw — the app is silent about it by design.
    #[test]
    fn usage_attribution_round_trips() {
        let mut s = StatsStore::default();
        s.push(SessionRecord {
            ts_unix: NOW,
            words: 4,
            duration_ms: 2_000,
            chars: 20,
            text: "the demo went well".into(),
            raw: "um the demo went well".into(),
            app: None,
            asr_ms: 430,
            cleanup_ms: 1_900,
            asr_engine: "cloud".into(),
            cleanup_engine: "local".into(),
            degraded: Some("cloud_error: HTTP 429".into()),
        });
        let json = serde_json::to_string(&s).unwrap();
        let back: StatsStore = serde_json::from_str(&json).unwrap();
        let r = &back.sessions[0];
        assert_eq!(r.asr_engine, "cloud");
        assert_eq!(r.cleanup_engine, "local");
        assert_eq!(r.degraded.as_deref(), Some("cloud_error: HTTP 429"));
        assert_eq!((r.asr_ms, r.cleanup_ms), (430, 1_900));
    }
}
