//! The macOS dictation layer: Fn key → capture → ASR → cleanup → paste.
//!
//! An in-process CoreGraphics event tap feeds Fn key-down/key-up into the
//! [`whimpr_core`] state machine, and `apply_action` enacts what the machine
//! returns — start the mic, play the ping, drive the pill, transcribe on release,
//! run cleanup through the configured provider, and paste.
//!
//! A second tap handles Esc-to-cancel. It is deliberately NOT folded into the Fn
//! tap: seeing Esc means subscribing to key-down, i.e. seeing every keystroke in
//! every app, so it stays disabled until a dictation is actually live and goes off
//! again the moment one ends. Being off by default is also what makes it safe for
//! it to be the app's one consuming tap.
//!
//! `Settings::trigger_mode` decides how a press is reported: `Hold` sends the
//! push-to-talk binding (release finalizes), `Toggle` sends the hands-free one, so
//! the first press starts a locked session and the next press ends it. The state
//! machine is unchanged either way — both paths already existed.
//!
//! The tap is global only when Accessibility is granted; without it macOS silently
//! limits it to whenever this app is frontmost, which reads as "dictation does
//! nothing in other apps". The tap thread therefore waits for the grant and starts
//! working the moment it arrives, without a relaunch.
//!
//! Running the tap in-process is a known trade-off: heavy inference on a starved
//! machine can stall it. `whimpr-ipc` and `whimpr-sidecar` exist to move it into a
//! separate process but are not wired in yet.

/// Dictionary entry shape sent to the Hub UI (auto-learned entries flagged).
#[derive(Clone, serde::Serialize)]
pub struct DictEntryDto {
    pub correct: String,
    pub mishears: Vec<String>,
    pub auto: bool,
}

/// What the local cleanup model is doing, for the Hub's Cleanup Engine pane.
/// `state` is "loading" while the worker starts, then "ready" (with the GGUF
/// filename in `model`) or "missing" when no model / worker binary was found.
#[derive(Clone)]
pub struct LocalModelStatus {
    pub state: &'static str,
    pub model: Option<String>,
}

impl Default for LocalModelStatus {
    fn default() -> Self {
        Self { state: "loading", model: None }
    }
}

mod imp {
    use std::os::raw::c_void;
    use std::path::PathBuf;
    use super::DictEntryDto;
    use std::ptr::{null, null_mut};
    use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicU8, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use serde::Serialize;
    use tauri::{AppHandle, Emitter};
    use whimpr_core::state::{Action, BarState, TapOutcome};
    use whimpr_core::{
        AsrEngine, CleanupContext, CleanupMode, CleanupProvider, Input, PipelineEvent, StateMachine,
        TriggerToken,
    };
    use whimpr_ipc::BindingId;

    const OVERLAY_LABEL: &str = "whimpr_bar";

    // --- CoreGraphics / CoreFoundation FFI (listen-only Fn tap + Esc tap) ---
    type CFMachPortRef = *mut c_void;
    type CFRunLoopSourceRef = *mut c_void;
    type CFRunLoopRef = *mut c_void;
    type CFStringRef = *const c_void;
    type CFAllocatorRef = *const c_void;
    type CGEventRef = *mut c_void;
    type CGEventTapProxy = *mut c_void;
    type CGEventTapCallBack =
        extern "C" fn(CGEventTapProxy, u32, CGEventRef, *mut c_void) -> CGEventRef;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventTapCreate(
            tap: u32,
            place: u32,
            options: u32,
            events_of_interest: u64,
            callback: CGEventTapCallBack,
            user_info: *mut c_void,
        ) -> CFMachPortRef;
        fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
        fn CGEventGetFlags(event: CGEventRef) -> u64;
        fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFMachPortCreateRunLoopSource(
            allocator: CFAllocatorRef,
            port: CFMachPortRef,
            order: isize,
        ) -> CFRunLoopSourceRef;
        fn CFRunLoopGetCurrent() -> CFRunLoopRef;
        fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
        fn CFRunLoopRun();
        static kCFRunLoopDefaultMode: CFStringRef;
    }

    const K_CG_SESSION_EVENT_TAP: u32 = 1;
    const K_CG_HEAD_INSERT: u32 = 0;
    const K_CG_TAP_OPTION_LISTEN_ONLY: u32 = 1;
    /// A tap that may modify or drop the events it sees. Used ONLY by the Esc tap,
    /// and only while it is enabled — see [`set_esc_tap_enabled`].
    const K_CG_TAP_OPTION_DEFAULT: u32 = 0;
    const K_CG_EVENT_FLAGS_CHANGED: u32 = 12;
    const K_CG_EVENT_KEY_DOWN: u32 = 10;
    const EVENTS_OF_INTEREST: u64 = 1 << K_CG_EVENT_FLAGS_CHANGED;
    const ESC_EVENTS_OF_INTEREST: u64 = 1 << K_CG_EVENT_KEY_DOWN;
    const FLAG_SECONDARY_FN: u64 = 0x0080_0000;
    const K_CG_KEYBOARD_EVENT_KEYCODE: u32 = 9;
    const KEYCODE_FN: i64 = 63;
    const KEYCODE_ESC: i64 = 53;
    const K_CG_TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
    const K_CG_TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;

    static APP: OnceLock<AppHandle> = OnceLock::new();
    static MACHINE: OnceLock<Mutex<StateMachine>> = OnceLock::new();
    static CLOCK: OnceLock<Instant> = OnceLock::new();
    static FN_IS_DOWN: AtomicBool = AtomicBool::new(false);
    /// Mirror of `Settings::trigger_mode` for the tap callback, as the discriminants
    /// in [`trigger_mode_code`]. An atomic rather than a read of SETTINGS because the
    /// callback must stay allocation-free and never block: exceeding the hook's
    /// timeout gets the tap removed by macOS.
    static TRIGGER_MODE: AtomicU8 = AtomicU8::new(TRIGGER_HOLD);
    const TRIGGER_HOLD: u8 = 0;
    const TRIGGER_TOGGLE: u8 = 1;
    const TRIGGER_DOUBLE_TAP: u8 = 2;
    /// In `DoubleTap` mode, when the first tap of a possible pair was *released*, in
    /// `CLOCK` milliseconds. Zero means no tap is pending.
    ///
    /// Measured from the release, and only for a press shorter than `HOLD_MIN_MS`,
    /// because the whole reason this mode exists is to leave `Fn`+key combinations
    /// alone — and holding Fn to press Delete is a long press. Keying off the *down*
    /// instead would make two quick forward-deletes look exactly like a double-tap
    /// and start dictating into whatever was being edited.
    static PENDING_TAP_MS: AtomicU64 = AtomicU64::new(0);
    /// When the current Fn press began, so its release can be classified.
    static FN_DOWN_AT_MS: AtomicU64 = AtomicU64::new(0);
    static TAP_PORT: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
    /// The Esc-to-cancel tap. Separate from the Fn tap, and deliberately so.
    ///
    /// Seeing Esc means subscribing to key-down, which means seeing EVERY keystroke
    /// in every app — a privacy surface this app should not carry while it is idle.
    /// So this tap exists from launch but stays DISABLED, and is switched on only
    /// while a dictation is live. WhimprFlow therefore has no live keystroke tap at
    /// all except during the seconds you are actually dictating.
    ///
    /// Because it is only on then, it can also afford to be a consuming tap: the Esc
    /// that cancels a dictation is swallowed rather than passed on, so cancelling
    /// doesn't also dismiss a dialog or clear a draft in the app behind the pill.
    static ESC_TAP_PORT: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
    /// Whether the Esc tap *should* be live, so the disabled-by-timeout recovery
    /// re-enables it only when a session is still running.
    static ESC_TAP_WANTED: AtomicBool = AtomicBool::new(false);
    /// High-water mark of cancelled session ids (0 = none cancelled yet). Stopping
    /// the mic is not enough to cancel: once the pipeline thread is running it
    /// already holds the audio, so cancelling mid-transcription would otherwise
    /// still paste. The thread checks this before each expensive step.
    ///
    /// A high-water mark rather than "the cancelled id", and never reset: session
    /// ids only increase, so `mark >= id` stays true for the abandoned session for
    /// as long as its thread lives, and is false for every session started after.
    /// Clearing it at the next record-start looked equivalent and was not — a
    /// cancelled session whose cleanup runs for several seconds would have had its
    /// mark wiped by the next dictation and pasted after all.
    static CANCELLED_SESSION: AtomicU64 = AtomicU64::new(0);
    /// Bundle id of the app that was frontmost at record-start = the paste target.
    /// Cleanup uses it to format for the medium (email vs. text vs. chat).
    static TARGET_APP: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    static CAPTURE: OnceLock<Mutex<Option<whimpr_audio::CaptureHandle>>> = OnceLock::new();
    /// The local Whisper engine, loaded on demand rather than at launch.
    ///
    /// An `Option` behind a `Mutex` rather than a `OnceLock<Arc<_>>` because it has
    /// to be droppable: its weights live in a Metal buffer for as long as the app
    /// holds them (547 MB for the q5_0 turbo build), and a machine set to cloud ASR
    /// should not be paying that around the clock for a fallback. See `ensure_asr`
    /// and `reap_idle_engines`.
    static ASR: OnceLock<Mutex<Option<Arc<whimpr_asr::WhisperEngine>>>> = OnceLock::new();
    /// Cloud ASR, rebuilt alongside the cleanup provider whenever the key changes.
    /// Kept beside the local engine rather than replacing it, because `asr_mode` is a
    /// toggle the user can flip from the tray between one dictation and the next. It
    /// costs nothing to keep: it is an HTTP client, not a model.
    static CLOUD_ASR: OnceLock<Mutex<Option<Arc<whimpr_asr::CloudAsr>>>> = OnceLock::new();
    /// When each on-demand engine was last used, in `CLOCK` milliseconds, so the
    /// reaper can tell a warm engine from an abandoned one. Zero means never used.
    static ASR_LAST_USED: AtomicU64 = AtomicU64::new(0);
    static LOCAL_LAST_USED: AtomicU64 = AtomicU64::new(0);
    static OPENAI: OnceLock<Mutex<Option<whimpr_cleanup::OpenAiProvider>>> = OnceLock::new();
    static LOCAL: OnceLock<Mutex<Option<crate::local_llm::LocalWorker>>> = OnceLock::new();
    static SETTINGS: OnceLock<Mutex<whimpr_core::Settings>> = OnceLock::new();
    static DICTIONARY: OnceLock<Mutex<whimpr_core::DictionaryStore>> = OnceLock::new();
    static STATS: OnceLock<Mutex<whimpr_core::StatsStore>> = OnceLock::new();
    /// What the local cleanup worker is doing, for the Hub to display. Deliberately
    /// NOT read off `LOCAL`: `LocalWorker::cleanup` holds that mutex for the whole
    /// multi-second generation, so a status command that locked it would freeze the
    /// Hub mid-dictation. This one is held for microseconds.
    static LOCAL_STATUS: OnceLock<Mutex<super::LocalModelStatus>> = OnceLock::new();

    /// How long an on-demand engine may sit unused before it is dropped.
    ///
    /// Only ever applies to an engine that is *not* the selected one — it was loaded
    /// to rescue a dictation the cloud could not serve, and once that has passed
    /// there is no reason for gigabytes to stay resident until the app is quit. Long
    /// enough that a run of failures (a rate limit lasts minutes, an outage longer)
    /// reuses one warm load rather than reloading per dictation.
    const ENGINE_IDLE_TTL_MS: u64 = 5 * 60_000;

    /// Load the local Whisper model if it is not already loaded. Returns it either way.
    ///
    /// The `Mutex` is held across the load, so two dictations racing here cannot both
    /// pay for it. Nothing else takes this lock for longer than a clone, so unlike
    /// `LOCAL` it is safe to touch from anywhere.
    fn ensure_asr() -> Option<Arc<whimpr_asr::WhisperEngine>> {
        let slot = ASR.get_or_init(|| Mutex::new(None));
        let mut guard = slot.lock().unwrap();
        if guard.is_none() {
            let path = model_path();
            if !path.exists() {
                eprintln!("[whimpr] ASR model not found at {}", path.display());
                return None;
            }
            match whimpr_asr::WhisperEngine::load(&path) {
                Ok(engine) => {
                    eprintln!("[whimpr] ASR model loaded — ready to transcribe");
                    *guard = Some(Arc::new(engine));
                }
                Err(e) => eprintln!("[whimpr] ASR model load failed: {e}"),
            }
        }
        ASR_LAST_USED.store(now_ms().max(1), Ordering::SeqCst);
        guard.clone()
    }

    /// Drop an on-demand engine that is not the selected one and has gone unused.
    ///
    /// The point of the whole arrangement: a machine set to the cloud for both stages
    /// should sit at the app's own ~60 MB, not at three gigabytes of models it is not
    /// using. Loading one to serve a fallback is right; keeping it for the rest of the
    /// session afterwards is not, and "it only happens after an error" is exactly how
    /// a memory footprint becomes mysterious.
    ///
    /// The selected engine is never reaped, however long it idles — that one is warm
    /// on purpose, and reloading it mid-dictation is the cost this avoids.
    fn reap_idle_engines() {
        let settings = current_settings();
        let now = now_ms();
        let stale = |last: &AtomicU64| {
            let t = last.load(Ordering::SeqCst);
            t > 0 && now.saturating_sub(t) > ENGINE_IDLE_TTL_MS
        };

        if !matches!(settings.asr_mode, whimpr_core::AsrMode::Local) && stale(&ASR_LAST_USED) {
            if let Some(slot) = ASR.get() {
                // try_lock: a dictation in flight holds this, and waiting to free
                // memory is precisely the wrong trade. It will be stale next minute.
                if let Ok(mut guard) = slot.try_lock() {
                    if guard.take().is_some() {
                        ASR_LAST_USED.store(0, Ordering::SeqCst);
                        eprintln!("[whimpr] released the local ASR model — unused and not selected");
                    }
                }
            }
        }
        if !matches!(settings.cleanup_mode, CleanupMode::Local) && stale(&LOCAL_LAST_USED) {
            if let Some(slot) = LOCAL.get() {
                if let Ok(mut guard) = slot.try_lock() {
                    // Dropping the worker kills the child process (see `impl Drop`),
                    // which is what actually returns the model's memory.
                    if guard.take().is_some() {
                        LOCAL_LAST_USED.store(0, Ordering::SeqCst);
                        eprintln!(
                            "[whimpr] stopped the local cleanup worker — unused and not selected"
                        );
                    }
                }
            }
        }
    }

    /// Make sure the local cleanup worker is running, spawning it if it is not.
    ///
    /// Idempotent and safe to call on every local cleanup: once the worker is in the
    /// slot this is one lock and a check. It exists because the worker is no longer
    /// preloaded in a cloud mode — see the spawn site in `start` — so the first
    /// fallback cleanup is the one that pays for the model load, and it must not find
    /// an empty slot and give up.
    ///
    /// Takes the `LOCAL` lock, so it must never be called from a Tauri command: a
    /// cleanup in flight holds that lock for its whole multi-second generation, and
    /// the Hub would freeze mid-dictation. The status statics exist for that reason.
    fn ensure_local() {
        let Some(slot) = LOCAL.get() else { return };
        let mut guard = slot.lock().unwrap();
        if guard.is_none() {
            *guard = crate::local_llm::spawn_default();
        }
        LOCAL_LAST_USED.store(now_ms().max(1), Ordering::SeqCst);
    }

    fn set_local_status(state: &'static str, model: Option<String>) {
        let slot = LOCAL_STATUS.get_or_init(|| Mutex::new(super::LocalModelStatus::default()));
        *slot.lock().unwrap() = super::LocalModelStatus { state, model };
    }

    /// The local cleanup model's load state + filename, for `get_status`.
    pub fn local_model_status() -> super::LocalModelStatus {
        LOCAL_STATUS
            .get_or_init(|| Mutex::new(super::LocalModelStatus::default()))
            .lock()
            .unwrap()
            .clone()
    }

    /// Filename of the whisper model that was actually loaded, if any.
    pub fn asr_model_name() -> Option<String> {
        let p = model_path();
        p.exists()
            .then(|| p.file_name()?.to_str().map(|s| s.to_string()))
            .flatten()
    }

    #[derive(Clone, Serialize)]
    struct BarPayload {
        state: &'static str,
    }

    #[derive(Clone, Serialize)]
    struct WavePayload {
        bars: Vec<f32>,
    }

    #[derive(Clone, Serialize)]
    struct TranscriptPayload {
        text: String,
    }

    /// A specific reason to show on the error pill — see [`notify_error`].
    #[derive(Clone, Serialize)]
    struct NoticePayload {
        text: String,
    }

    /// The whisper ASR model to load: prefer the most accurate one present, in
    /// descending quality order, falling back to the small base model. Bigger
    /// English models mis-hear names/technical terms far less (and better ASR means
    /// less for cleanup and the dictionary to fix downstream).
    fn model_path() -> PathBuf {
        let dir = support_dir().join("models");
        // Best file present wins, so upgrading is just dropping a bigger one in.
        //
        // The q5_0 build of turbo sits second on paper but is the one worth having on
        // a 16 GB machine: measured against the f16 build on the same clips it was
        // indistinguishable in accuracy (better, on the hardest surname) at the same
        // speed, for 716 MB resident instead of 1755 MB. Whisper's weights stay fully
        // resident in a Metal buffer the whole time the app runs — unlike the llama
        // worker's, which are mmapped and paged in on demand — so that gigabyte is
        // paid around the clock, not just while dictating.
        for name in [
            "ggml-large-v3-turbo.bin",
            "ggml-large-v3-turbo-q5_0.bin",
            "ggml-medium.en.bin",
            "ggml-small.en.bin",
            "ggml-base.en.bin",
        ] {
            let p = dir.join(name);
            if p.exists() {
                return p;
            }
        }
        dir.join("ggml-base.en.bin")
    }

    fn support_dir() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_default();
        PathBuf::from(home).join("Library/Application Support/WhimprFlow")
    }

    /// Record which permissions this launch actually has, so the installer can tell
    /// the user whether an update kept its grants. It cannot ask macOS directly:
    /// AXIsProcessTrusted answers for the *calling* process, so the same check run
    /// from a shell reports the terminal's permissions, not the app's. Only the app
    /// itself can answer honestly, so it writes the answer down.
    fn write_permission_snapshot() {
        let dir = support_dir();
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let json = format!(
            "{{\"accessibility\":{},\"microphone\":{},\"input_monitoring\":{}}}\n",
            crate::paste::is_trusted(),
            crate::paste::microphone_granted(),
            crate::paste::input_monitoring_granted()
        );
        let _ = std::fs::write(dir.join("permissions.json"), json);
    }
    fn settings_path() -> PathBuf {
        support_dir().join("settings.json")
    }
    fn dict_path() -> PathBuf {
        support_dir().join("dictionary.json")
    }
    fn stats_path() -> PathBuf {
        support_dir().join("stats.json")
    }

    /// Seconds since the Unix epoch (UTC), or 0 if the clock is before the epoch.
    fn unix_now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Log one completed dictation to the stats store (words, speaking time, text,
    /// target app) and persist it. Powers both the Hub stats and the history list.
    /// `raw` is the pre-cleanup transcript, stored unless the user opted out.
    pub fn record_dictation(
        text: &str,
        raw: &str,
        duration_secs: f32,
        asr_ms: u32,
        cleanup_ms: u32,
    ) {
        let words = whimpr_core::stats::count_words(text);
        if words == 0 {
            return;
        }
        let app = TARGET_APP.get().and_then(|m| m.lock().unwrap().clone());
        let raw = if current_settings().store_raw_transcripts {
            raw.trim().to_string()
        } else {
            String::new()
        };
        if let Some(m) = STATS.get() {
            let mut store = m.lock().unwrap();
            store.push(whimpr_core::SessionRecord {
                ts_unix: unix_now(),
                words,
                duration_ms: (duration_secs.max(0.0) * 1000.0) as u32,
                chars: text.chars().count() as u32,
                text: text.to_string(),
                raw,
                app,
                asr_ms,
                cleanup_ms,
            });
            let _ = store.save(&stats_path());
        }
    }

    /// One filtered, paged slice of the history for the Hub Home list.
    pub fn history_page(query: whimpr_core::HistoryQuery) -> whimpr_core::HistoryPage {
        STATS
            .get()
            .map(|m| m.lock().unwrap().query(&query))
            .unwrap_or_default()
    }

    /// Erase the stored text of every dictation, keeping the counts, and persist.
    pub fn clear_transcripts() {
        if let Some(m) = STATS.get() {
            let mut store = m.lock().unwrap();
            store.forget_transcripts();
            let _ = store.save(&stats_path());
        }
    }

    /// The dictionary entries for the Hub Dictionary screen (auto-learned flagged).
    pub fn dictionary_entries() -> Vec<DictEntryDto> {
        DICTIONARY
            .get()
            .map(|m| {
                m.lock()
                    .unwrap()
                    .entries
                    .iter()
                    .map(|e| DictEntryDto {
                        correct: e.correct.clone(),
                        mishears: e.mishears.clone(),
                        auto: matches!(e.source, whimpr_core::DictSource::Auto),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Add a manual dictionary entry and persist.
    pub fn dictionary_add(correct: String, mishears: Vec<String>) {
        if let Some(m) = DICTIONARY.get() {
            let mut store = m.lock().unwrap();
            store.add(correct, mishears, whimpr_core::DictSource::Manual);
            let _ = store.save(&dict_path());
        }
    }

    /// Remove a dictionary entry by spelling and persist.
    pub fn dictionary_remove(correct: &str) {
        if let Some(m) = DICTIONARY.get() {
            let mut store = m.lock().unwrap();
            if store.remove(correct) {
                let _ = store.save(&dict_path());
            }
        }
    }

    /// Add an AUTO-learned entry (from the post-paste correction observer) and persist.
    /// Marked ✨ auto in the UI. No-op if it would duplicate an existing entry's data.
    pub fn dictionary_learn(correct: String, mishears: Vec<String>) {
        if let Some(m) = DICTIONARY.get() {
            let mut store = m.lock().unwrap();
            store.add(correct, mishears, whimpr_core::DictSource::Auto);
            let _ = store.save(&dict_path());
        }
    }

    /// Aggregated stats for the Hub. `tz_offset_minutes` is the UI's
    /// `Date.getTimezoneOffset()` so day math matches the user's local clock.
    pub fn stats_summary(tz_offset_minutes: i32) -> whimpr_core::StatsSummary {
        STATS
            .get()
            .map(|m| m.lock().unwrap().summary(tz_offset_minutes, unix_now()))
            .unwrap_or_else(|| {
                whimpr_core::StatsStore::default().summary(tz_offset_minutes, unix_now())
            })
    }

    /// Read an API key from an env var or the OS keychain (never a plaintext file).
    fn read_key(account: &str, env_var: &str) -> Option<String> {
        if let Ok(k) = std::env::var(env_var) {
            let k = k.trim().to_string();
            if !k.is_empty() {
                return Some(k);
            }
        }
        keyring::Entry::new("com.whimpr.whimprflow", account)
            .ok()
            .and_then(|e| e.get_password().ok())
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
    }
    /// The key for the OpenAI-*compatible* cleanup mode. One keychain entry serves
    /// whichever endpoint `openai_base_url` points at, so switching from Groq to
    /// OpenAI is a URL change, not a second credential store. `GROQ_API_KEY` is
    /// honoured alongside `OPENAI_API_KEY` because the default endpoint is Groq and
    /// looking for a variable named after the wrong vendor is a confusing dead end.
    fn read_openai_key() -> Option<String> {
        read_key("openai_api_key", "GROQ_API_KEY").or_else(|| read_key("openai_api_key", "OPENAI_API_KEY"))
    }

    /// A snapshot of the current settings.
    pub fn current_settings() -> whimpr_core::Settings {
        SETTINGS
            .get()
            .map(|m| m.lock().unwrap().clone())
            .unwrap_or_default()
    }
    /// Apply new settings and rebuild the cloud providers (picks up model changes).
    pub fn update_settings(new: whimpr_core::Settings) {
        if let Some(m) = SETTINGS.get() {
            *m.lock().unwrap() = new.clone();
        }
        cache_trigger_mode(&new);
        let _ = new.save(&settings_path());
        rebuild_providers();
    }

    /// Publish the trigger mode where the tap callback can read it cheaply.
    fn cache_trigger_mode(s: &whimpr_core::Settings) {
        TRIGGER_MODE.store(trigger_mode_code(s.trigger_mode), Ordering::SeqCst);
        // A mode change mid-gesture must not leave half a double-tap armed, waiting
        // to fire on the next unrelated press.
        PENDING_TAP_MS.store(0, Ordering::SeqCst);
    }

    /// Is a dictation running right now? Read from the Esc tap's own flag, which is
    /// set for exactly the live states (`recording | locked | transcribing`) by
    /// `emit_bar` — so this cannot drift from what the pill is showing.
    ///
    /// An atomic, deliberately: this is called from the tap callback, which must not
    /// take the machine's lock. Blocking there long enough to exceed the hook timeout
    /// gets the tap silently removed by macOS.
    fn session_is_live() -> bool {
        ESC_TAP_WANTED.load(Ordering::SeqCst)
    }

    fn trigger_mode_name(code: u8) -> &'static str {
        match code {
            TRIGGER_TOGGLE => "toggle",
            TRIGGER_DOUBLE_TAP => "double-tap",
            _ => "hold",
        }
    }

    fn trigger_mode_code(mode: whimpr_core::TriggerMode) -> u8 {
        match mode {
            whimpr_core::TriggerMode::Hold => TRIGGER_HOLD,
            whimpr_core::TriggerMode::Toggle => TRIGGER_TOGGLE,
            whimpr_core::TriggerMode::DoubleTap => TRIGGER_DOUBLE_TAP,
        }
    }

    /// (Re)build the cloud cleanup providers from the current keys + settings. Called
    /// at startup and whenever a key or model changes, so edits take effect live.
    pub fn rebuild_providers() {
        let settings = current_settings();
        let openai = read_openai_key().map(|k| {
            whimpr_cleanup::OpenAiProvider::with_base_url(
                k,
                settings.openai_model.clone(),
                Some(settings.openai_base_url.clone()),
            )
        });
        // The same key serves both stages: one Groq account, one credential.
        let cloud_asr = read_openai_key().map(|k| {
            Arc::new(whimpr_asr::CloudAsr::new(
                k,
                whimpr_core::GROQ_ASR_MODEL,
                whimpr_core::GROQ_ASR_URL,
            ))
        });
        eprintln!(
            "[whimpr] cloud key present: {} (asr mode: {:?})",
            openai.is_some(),
            settings.asr_mode
        );
        match CLOUD_ASR.get() {
            Some(m) => *m.lock().unwrap() = cloud_asr,
            None => {
                let _ = CLOUD_ASR.set(Mutex::new(cloud_asr));
            }
        }
        match OPENAI.get() {
            Some(m) => *m.lock().unwrap() = openai,
            None => {
                let _ = OPENAI.set(Mutex::new(openai));
            }
        }
        // Picking local cleanup warms the worker now rather than making the next
        // dictation wait for a 2.3 GB model load with the paste blocked. On a
        // background thread because this runs from a Tauri command and `ensure_local`
        // takes a lock a cleanup in flight holds for seconds.
        if matches!(settings.cleanup_mode, CleanupMode::Local) {
            std::thread::spawn(ensure_local);
        }
    }

    /// Transcribe a second time with the dictionary as Whisper's `initial_prompt`, and
    /// keep that result only if it is a better version of the same sentence.
    ///
    /// Recognition is where a mis-heard name should be fixed; cleanup repairing it
    /// afterwards is a workaround, and one that does nothing at all in `Raw` mode or
    /// at cleanup level `None`, where no model ever sees the transcript.
    ///
    /// Two passes rather than always prompting, for two reasons. Prompting biases
    /// every dictation, including the overwhelming majority with no dictionary word in
    /// them — and a prompt Whisper cannot hear in the audio is one it may simply emit
    /// anyway. Running unprompted first means the common case is untouched, and that
    /// the biased result has an unbiased one to be checked against; without that
    /// comparison a hallucinated name would go straight to the cursor. The cost is one
    /// extra pass over a few seconds of audio, and only when the pre-filter matched.
    fn biased_retranscribe(
        asr: &dyn AsrEngine,
        pcm: &[f32],
        unprompted: String,
        session: whimpr_core::SessionId,
    ) -> String {
        let vocab = DICTIONARY
            .get()
            .map(|d| d.lock().unwrap().prefilter(&unprompted, 15))
            .unwrap_or_default();
        let Some(prompt) = whimpr_core::asr::prompt::build_initial_prompt(&vocab) else {
            return unprompted; // Nothing to bias toward — one pass is the whole job.
        };
        // Cancelling during the first pass should not buy a second one.
        if is_cancelled(session) {
            return unprompted;
        }
        eprintln!("[whimpr] re-transcribing with {prompt:?}");
        let Ok(t) = asr.transcribe(pcm, Some(&prompt)) else {
            eprintln!("[whimpr] prompted pass failed — keeping the unprompted transcript");
            return unprompted;
        };
        if whimpr_core::asr::prompt::accept_prompted(&unprompted, &t.text, &vocab) {
            if t.text != unprompted {
                eprintln!("[whimpr] BIASED:     \"{}\"", t.text);
            }
            t.text
        } else {
            eprintln!(
                "[whimpr] prompted pass changed more than the dictionary allows — \
                 keeping the unprompted transcript: {:?}",
                t.text
            );
            unprompted
        }
    }

    /// Produce the text to paste: cleanup, then the dictionary's listed mishears
    /// enforced on whatever came out of it, then the Messaging level's lowercasing.
    ///
    /// The order matters and none of the steps are redundant. Cleanup already gets the
    /// vocabulary in its prompt and usually applies it, but "usually" is the problem:
    /// the model declines exactly when the mis-heard form is itself a plausible name,
    /// which is the case users add an entry for. Running the dictionary last also means
    /// a listed mishear is fixed on every path a prompt cannot reach — cleanup off,
    /// gates rejected the edit, provider down.
    ///
    /// Lowercasing comes after the dictionary for the same reason it exists at all:
    /// the dictionary writes the *authoritative* spelling, which is capitalized, so
    /// doing this earlier would let a corrected name arrive at the cursor as the one
    /// capitalized word in an all-lowercase message. It is skipped in `Raw` mode,
    /// where "paste exactly what you said" outranks a typing habit.
    fn clean_transcript(raw: &str) -> String {
        let settings = current_settings();
        let text = run_cleanup(raw, &settings);
        let text = match DICTIONARY.get() {
            Some(dict) => {
                let fixed = dict.lock().unwrap().apply_listed_mishears(&text);
                if fixed != text {
                    eprintln!("[whimpr] DICTIONARY: \"{fixed}\"");
                }
                fixed
            }
            None => text,
        };
        if settings.cleanup_level.forces_lowercase()
            && !matches!(settings.cleanup_mode, CleanupMode::Raw)
        {
            return whimpr_core::cleanup::messaging_style(&text);
        }
        text
    }

    /// Clean a raw transcript per the current settings (mode + level), feeding in the
    /// dictionary vocabulary relevant to this utterance. A cloud provider that is
    /// unavailable or errors falls back to the local model; raw is the last resort,
    /// used when cleanup is off, no engine at all is available, or the gates reject
    /// the edit.
    fn run_cleanup(raw: &str, settings: &whimpr_core::Settings) -> String {
        let level = settings.cleanup_level;
        if matches!(settings.cleanup_mode, CleanupMode::Raw) || level.bypasses_llm() {
            return raw.to_string();
        }
        // Turn explicit spoken layout cues ("new line", "new paragraph") into break
        // markers up front — the model passes an opaque marker through reliably but
        // mangles the literal cue words. The model sees `raw` (with markers); the gate
        // and any raw fallback use `raw_out` (markers restored to real breaks) so we
        // never paste a "[[NL]]" token or lose an explicit break.
        let raw_norm = whimpr_core::cleanup::pre_normalize_layout(raw);
        let raw = raw_norm.as_str();
        let raw_out = whimpr_core::cleanup::post_process(&raw_norm);
        // Selected again rather than handed down from `biased_retranscribe`: that pass
        // filtered against the *unprompted* transcript, and by now the text may have
        // been improved, layout markers inserted, or the prompted pass rejected. Each
        // stage picking vocab for the text it is actually about to send is cheaper than
        // reasoning about which earlier transcript the list came from.
        let vocab = DICTIONARY
            .get()
            .map(|d| d.lock().unwrap().prefilter(raw, 15))
            .unwrap_or_default();
        let app_bundle_id = TARGET_APP.get().and_then(|m| m.lock().unwrap().clone());
        if let Some(app) = app_bundle_id.as_deref() {
            eprintln!("[whimpr] cleanup target app: {app}");
        }
        let ctx = CleanupContext {
            level,
            vocab,
            app_bundle_id,
            ..Default::default()
        };
        // Run the on-device model with the same prompt + per-app formatting.
        let run_local = || -> Option<anyhow::Result<String>> {
            ensure_local();
            LOCAL.get().and_then(|m| {
                m.lock().unwrap().as_mut().map(|w| {
                    // System prompt + few-shot demonstration turns + the transcript,
                    // so the on-device model actually produces newlines/lists and
                    // resolves self-corrections instead of just being told to.
                    let messages = whimpr_core::cleanup::build_messages(raw, &ctx);
                    w.cleanup(&messages, whimpr_core::cleanup::max_tokens_for(raw))
                })
            })
        };
        // A cloud attempt that produces no text falls back to the on-device model
        // rather than to raw. Two different failures land here and both must survive
        // a dictation: no usable key (`None`), and a call that errored (`Some(Err)`) —
        // which on a free-tier endpoint means a 429 the moment the daily cap lands,
        // plus the ordinary offline/timeout cases. Pasting raw there drops the user
        // to unclean text with fillers intact, which reads as "cleanup is broken"
        // rather than "the free quota ran out", so local absorbs it silently.
        let or_local = |attempt: Option<anyhow::Result<String>>| match attempt {
            Some(Ok(text)) => {
                eprintln!("[whimpr] cleanup served by the cloud endpoint");
                Some(Ok(text))
            }
            Some(Err(e)) => {
                eprintln!("[whimpr] cloud cleanup failed ({e}) — retrying on the local model");
                run_local()
            }
            None => run_local(),
        };
        let run_cloud = || {
            OPENAI
                .get()
                .and_then(|m| m.lock().unwrap().as_ref().map(|p| p.cleanup(raw, &ctx)))
        };
        // Selected provider; Local mode uses the worker directly.
        let result: Option<anyhow::Result<String>> = match settings.cleanup_mode {
            CleanupMode::OpenAi => or_local(run_cloud()),
            // The mirror of `or_local`, for the install that has no local model to
            // fall back to. `CleanupMode` defaults to Local, so a cloud-only machine
            // whose settings.json never got written would otherwise paste raw,
            // filler-ridden text forever with a working key in the Keychain — and
            // read as cleanup being broken rather than unconfigured.
            CleanupMode::Local => run_local().or_else(|| {
                let cloud = run_cloud();
                if cloud.is_some() {
                    eprintln!(
                        "[whimpr] no local cleanup model — using the cloud endpoint, which is \
                         configured"
                    );
                }
                cloud
            }),
            CleanupMode::Raw => None,
        };
        match result {
            Some(Ok(cleaned)) => {
                // Deterministic safety net: convert any leftover spoken layout cue the
                // model missed into real line breaks, strip stray code fences, cap blank
                // lines. Guarantees no "new line"/"new paragraph" word reaches the cursor.
                let cleaned = whimpr_core::cleanup::post_process(&cleaned);
                // The prompt forbids em and en dashes; this is what makes it true. It
                // runs before the gate so what the gate judges is what gets pasted.
                let cleaned = whimpr_core::cleanup::de_dash(&cleaned);
                // The gate sees the same vocab the prompt did, so the spellings the
                // dictionary authorized don't read as the model inventing words.
                if whimpr_core::cleanup::evaluate_gates(&raw_out, &cleaned, level, &ctx.vocab)
                    .passed()
                {
                    cleaned
                } else {
                    eprintln!("[whimpr] cleanup gate rejected the edit — pasting raw");
                    raw_out
                }
            }
            Some(Err(e)) => {
                eprintln!("[whimpr] cleanup failed ({e}) — pasting raw");
                raw_out
            }
            None => {
                // In a cloud mode this means the cloud attempt already failed *and*
                // the local worker is missing too — every engine is out.
                eprintln!("[whimpr] no cleanup engine available — pasting raw");
                raw_out
            }
        }
    }

    fn now_ms() -> u64 {
        CLOCK.get().map(|c| c.elapsed().as_millis() as u64).unwrap_or(0)
    }

    fn bar_name(b: BarState) -> &'static str {
        match b {
            BarState::Idle => "idle",
            BarState::Recording => "recording",
            BarState::Locked => "locked",
            BarState::Transcribing => "transcribing",
            BarState::Done => "done",
            BarState::Cancelled => "cancelled",
            BarState::Error => "error",
        }
    }

    /// Show the error pill carrying a specific reason, then clear it.
    ///
    /// The machine's own `Failed` path returns the pill to idle without a word,
    /// which is right for an ordinary dud dictation (a stray Fn tap should not
    /// flash a warning) and wrong for a misconfiguration, which will happen again
    /// on every attempt until someone is told what it is. So this is a presentation
    /// concern the shell owns, alongside the linger timings in `apply_action` — the
    /// machine is still told the session failed, and still decides the state.
    ///
    /// The text has to survive being read on a 190px pill for under two seconds, so
    /// it names the fix rather than the fault.
    fn notify_error(app: &AppHandle, reason: &str) {
        let _ = app.emit_to(
            OVERLAY_LABEL,
            "whimpr://flowbar/notice",
            NoticePayload { text: reason.to_string() },
        );
        emit_bar(app, "error");
        let app2 = app.clone();
        std::thread::spawn(move || {
            // Longer than the generic error's 1800 ms: this one has words to read.
            std::thread::sleep(Duration::from_millis(3200));
            emit_bar(&app2, "idle");
        });
    }

    fn emit_bar(app: &AppHandle, state: &'static str) {
        eprintln!("[whimpr] pill -> {state}");
        let _ = app.emit_to(OVERLAY_LABEL, "whimpr://flowbar/state", BarPayload { state });
        // One predicate, two consequences. These are the states where a dictation is
        // live and can still be called off: the pill draws controls (so it must
        // accept clicks) and Esc means cancel (so that tap runs). Everything else
        // renders nothing or is a passive label — clicks fall through, and the
        // keystroke tap goes away entirely. Transcribing counts: the run can still
        // be abandoned before it pastes.
        let live = matches!(state, "recording" | "locked" | "transcribing");
        crate::sync_overlay_window(app, live);
        set_esc_tap_enabled(live);
    }

    /// Switch the Esc tap on for the duration of a dictation, off the rest of the
    /// time. Cheap (a mach-port message) and safe to call from any thread.
    fn set_esc_tap_enabled(on: bool) {
        // Already in the wanted state? Nothing to do — emit_bar fires on every
        // transition, including several that don't change this.
        if ESC_TAP_WANTED.swap(on, Ordering::SeqCst) == on {
            return;
        }
        let port = ESC_TAP_PORT.load(Ordering::SeqCst);
        if !port.is_null() {
            unsafe { CGEventTapEnable(port, on) };
        }
    }

    /// Esc while a dictation is live = discard it. Runs only while the tap is
    /// enabled, so there is no state check here beyond the keycode: if this callback
    /// is firing at all, a session is running.
    extern "C" fn esc_tap_callback(
        _proxy: CGEventTapProxy,
        etype: u32,
        event: CGEventRef,
        _info: *mut c_void,
    ) -> CGEventRef {
        if etype == K_CG_TAP_DISABLED_BY_TIMEOUT || etype == K_CG_TAP_DISABLED_BY_USER_INPUT {
            // Re-arm only if a session is still live; otherwise leave it off, which
            // is this tap's whole point.
            let port = ESC_TAP_PORT.load(Ordering::SeqCst);
            if !port.is_null() && ESC_TAP_WANTED.load(Ordering::SeqCst) {
                unsafe { CGEventTapEnable(port, true) };
            }
            return event;
        }
        if etype == K_CG_EVENT_KEY_DOWN {
            let keycode =
                unsafe { CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) };
            if keycode == KEYCODE_ESC {
                eprintln!("[whimpr] Esc — cancelling");
                handle_input(Input::Trigger(TriggerToken::Cancel { at_ms: now_ms() }));
                // Swallowed: this Esc meant "cancel the dictation", not "dismiss
                // whatever is on screen behind the pill".
                return null_mut();
            }
        }
        event
    }

    /// Whether this session was cancelled while its pipeline was running.
    fn is_cancelled(session: whimpr_core::SessionId) -> bool {
        CANCELLED_SESSION.load(Ordering::SeqCst) >= session.0
    }

    /// Stop the current recording now and paste what was said — the pill's ■.
    /// Ignored when nothing is recording.
    pub fn stop_now() {
        handle_input(Input::Trigger(TriggerToken::Stop { at_ms: now_ms() }));
    }

    /// Discard the current dictation entirely — the pill's ✕. Works while recording
    /// and while transcribing; after the paste has landed there is nothing to undo.
    pub fn cancel_now() {
        handle_input(Input::Trigger(TriggerToken::Cancel { at_ms: now_ms() }));
    }

    /// Feed one input into the shared state machine and enact its actions.
    fn handle_input(input: Input) {
        let (Some(app), Some(machine)) = (APP.get(), MACHINE.get()) else {
            return;
        };
        let actions = {
            let mut m = machine.lock().unwrap();
            m.step(input)
        };
        for action in actions {
            apply_action(app, action);
        }
    }

    fn apply_action(app: &AppHandle, action: Action) {
        match action {
            Action::ShowBar(bar) => {
                emit_bar(app, bar_name(bar));
                // Terminal states are messages, not steady states: show them long
                // enough to be read, then clear the pill off the screen. Errors get
                // longer because they're the ones worth actually reading.
                let linger_ms = match bar {
                    BarState::Done => 500,
                    BarState::Cancelled => 900,
                    BarState::Error => 1800,
                    _ => 0,
                };
                if linger_ms > 0 {
                    let app2 = app.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(linger_ms));
                        emit_bar(&app2, "idle");
                    });
                }
            }
            // Start the microphone; stream real RMS bars to the pill waveform.
            // Runs off the tap thread so the mic-permission prompt can't stall keys.
            Action::StartCapture { .. } => {
                let app_thread = app.clone();
                std::thread::spawn(move || {
                    let app_cb = app_thread.clone();
                    match whimpr_audio::start(move |bars| {
                        let _ = app_cb.emit_to(
                            OVERLAY_LABEL,
                            "whimpr://audio/waveform",
                            WavePayload { bars: bars.to_vec() },
                        );
                    }) {
                        Ok(handle) => {
                            *CAPTURE.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(handle);
                        }
                        Err(e) => eprintln!("[whimpr] mic capture failed to start: {e}"),
                    }
                });
            }
            // Stop the mic, transcribe the buffered audio, and advance the machine.
            Action::StopCaptureAndFinalize { session } => {
                let app2 = app.clone();
                let handle = CAPTURE.get().and_then(|slot| slot.lock().unwrap().take());
                std::thread::spawn(move || {
                    // Whatever happens, return the pill to idle (done -> idle).
                    let finish =
                        || handle_input(Input::Pipeline(PipelineEvent::Committed { session }));
                    let Some(res) = handle.and_then(|h| h.stop()) else {
                        eprintln!("[whimpr] no audio captured");
                        finish();
                        return;
                    };
                    let peak = res.samples.iter().fold(0f32, |m, &s| m.max(s.abs()));
                    eprintln!(
                        "[whimpr] captured {} samples @ {} Hz (~{:.2}s) from {}, peak {:.4}",
                        res.samples.len(),
                        res.sample_rate,
                        res.duration_secs(),
                        res.device,
                        peak
                    );
                    if peak < 0.005 {
                        eprintln!(
                            "[whimpr] ⚠ audio is silent — the mic isn't being captured. Grant \
                             Microphone access to your terminal (System Settings → Privacy & \
                             Security → Microphone), then fully quit + reopen it and rerun."
                        );
                    }
                    // Cloud ASR when asked for and usable, the local model otherwise.
                    // Falling back rather than failing keeps the same promise cleanup
                    // makes: a missing key or a dead network costs you speed, never the
                    // dictation you just spoke.
                    let local_asr = || ensure_asr().map(|a| a as Arc<dyn AsrEngine>);
                    let cloud_asr = || {
                        CLOUD_ASR
                            .get()
                            .and_then(|m| m.lock().unwrap().clone())
                            .map(|c| c as Arc<dyn AsrEngine>)
                    };
                    let asr: Option<Arc<dyn AsrEngine>> = match current_settings().asr_mode {
                        whimpr_core::AsrMode::Cloud => cloud_asr().or_else(|| {
                            eprintln!("[whimpr] cloud ASR has no key — using the local model");
                            local_asr()
                        }),
                        // Falling *forward* to the cloud, and only when there is no
                        // local model at all. A cloud-only install has none on disk,
                        // and `AsrMode` defaults to Local — so without this, an install
                        // whose settings.json did not get written (or got reset by an
                        // unparseable field) is an app that transcribes with nothing and
                        // says nothing, while a perfectly good key sits in the Keychain.
                        // Not a silent privacy change: it cannot fire on a machine that
                        // has a model, and reaching it at all requires a key the user
                        // entered themselves.
                        whimpr_core::AsrMode::Local => local_asr().or_else(|| {
                            let cloud = cloud_asr();
                            if cloud.is_some() {
                                eprintln!(
                                    "[whimpr] no local speech model — using cloud ASR, which is \
                                     configured"
                                );
                            }
                            cloud
                        }),
                    };
                    let Some(asr) = asr else {
                        // Every engine is out. This is a *configuration* failure, not a
                        // failed dictation, and it is the one an install can land in:
                        // a cloud-only machine has no model on disk, so the moment the
                        // key is missing or wrong there is nothing left to transcribe
                        // with. Reporting it as an ordinary failure returns the pill to
                        // idle and says nothing, which is indistinguishable from the Fn
                        // key not being detected at all — the exact wrong thing to send
                        // someone hunting for.
                        let settings = current_settings();
                        let reason = if matches!(settings.asr_mode, whimpr_core::AsrMode::Cloud) {
                            "No API key. Open WhimprFlow to add one."
                        } else if asr_model_name().is_some() {
                            "Still loading the model. Try again in a moment."
                        } else {
                            "No speech model. Open WhimprFlow to set one up."
                        };
                        eprintln!("[whimpr] no ASR engine available: {reason}");
                        notify_error(&app2, reason);
                        handle_input(Input::Pipeline(PipelineEvent::Failed { session }));
                        return;
                    };
                    // Cancelled while the mic was stopping — don't burn seconds of GPU
                    // on audio nobody wants. Checked again after ASR and before the
                    // paste, because each step is long enough to be cancelled during.
                    if is_cancelled(session) {
                        eprintln!("[whimpr] cancelled before transcribing — discarded");
                        return;
                    }
                    let mut pcm = whimpr_audio::resample_to_16k(&res.samples, res.sample_rate);
                    // Lift a quiet recording before the model sees it. Whisper drops
                    // softly-spoken words outright rather than mis-hearing them, so
                    // this shows up as "it ignored the end of my sentence", not as a
                    // wrong word. No-ops on audio that is already at a healthy level.
                    let gain = whimpr_audio::normalize_for_asr(&mut pcm);
                    if gain > 1.0 {
                        eprintln!("[whimpr] quiet recording — normalized by {gain:.1}x");
                    }
                    // Stage timings. Worth having permanently: "dictation feels slow" is
                    // otherwise unattributable, and the intuitive culprit — the cleanup
                    // model — is usually the cheap half. ASR runs twice whenever the
                    // dictionary hits, which costs more than any provider swap saves.
                    let t_asr = std::time::Instant::now();
                    match asr.transcribe(&pcm, None) {
                        Ok(t) => {
                            let raw = t.text;
                            eprintln!("[whimpr] TRANSCRIPT: \"{}\"", raw);
                            if is_cancelled(session) {
                                eprintln!("[whimpr] cancelled after transcribing — not pasted");
                                return;
                            }
                            // Give recognition a second look with the dictionary in
                            // hand, when this utterance looks like it needs one.
                            let raw = biased_retranscribe(asr.as_ref(), &pcm, raw, session);
                            let asr_ms = t_asr.elapsed().as_millis();
                            if is_cancelled(session) {
                                eprintln!("[whimpr] cancelled after re-transcribing — not pasted");
                                return;
                            }
                            // Clean the transcript (cloud LLM if configured), then paste.
                            let t_clean = std::time::Instant::now();
                            let text = clean_transcript(&raw);
                            let cleanup_ms = t_clean.elapsed().as_millis() as u32;
                            eprintln!(
                                "[whimpr] TIMING: asr {} ms + cleanup {} ms = {} ms for {:.1}s of audio",
                                asr_ms,
                                cleanup_ms,
                                t_asr.elapsed().as_millis(),
                                res.duration_secs()
                            );
                            if text != raw {
                                eprintln!("[whimpr] CLEANED:   \"{}\"", text);
                            }
                            // Last gate before the text becomes visible: cleanup can take
                            // seconds, which is most of the window a user cancels in.
                            if is_cancelled(session) {
                                eprintln!("[whimpr] cancelled during cleanup — not pasted");
                                return;
                            }
                            if !text.is_empty() {
                                if let Err(e) = crate::paste::paste_text(&text) {
                                    eprintln!("[whimpr] paste failed: {e}");
                                }
                                // Log words + speaking time for the Hub stats (WPM, streak…),
                                // keeping the raw transcript beside the cleaned text — the
                                // difference between them is the only record of how you
                                // actually speak, and cleanup's job is to delete it.
                                record_dictation(
                                    &text,
                                    &raw,
                                    res.duration_secs(),
                                    asr_ms as u32,
                                    cleanup_ms,
                                );
                                // Watch the field for a post-paste correction to learn (✨).
                                // The raw transcript goes along so the mishear recorded
                                // is what recognition wrote, not what cleanup wrote.
                                crate::autolearn::watch_correction(&text, &raw);
                            }
                            let _ = app2.emit_to(
                                OVERLAY_LABEL,
                                "whimpr://transcript",
                                TranscriptPayload { text },
                            );
                        }
                        Err(e) => eprintln!("[whimpr] ASR error: {e}"),
                    }
                    finish();
                });
            }
            // "Throw this session away" — both the audio still being captured and,
            // if the pipeline already took it, whatever that thread is about to
            // produce. Without the second half, cancelling mid-transcription still
            // pastes a few seconds later.
            Action::DiscardCapture { session } => {
                CANCELLED_SESSION.fetch_max(session.0, Ordering::SeqCst);
                if let Some(slot) = CAPTURE.get() {
                    if let Some(handle) = slot.lock().unwrap().take() {
                        let _ = handle.stop();
                    }
                }
            }
            // The ASR path (StopCaptureAndFinalize) now drives pipeline completion.
            Action::RunPipeline { .. } => {}
            Action::PlayPing => {
                if current_settings().sound_on_start {
                    play_ping(app);
                }
            }
            // A locked hands-free session auto-stops at the cap. Unhandled, the
            // recording simply ended mid-sentence with nothing on screen to explain
            // it, which reads as a crash rather than a limit.
            Action::WarnSessionCap => {
                eprintln!("[whimpr] session cap approaching — auto-stop in one minute");
                let _ = app.emit_to(OVERLAY_LABEL, "whimpr://session-cap", ());
            }
        }
    }

    /// The record-start ping. NSSound is AppKit and wants the main thread, while
    /// this runs on the CGEventTap callback thread — which must never block, or the
    /// hook exceeds its timeout and macOS silently removes the tap. So hop, and keep
    /// the closure trivial.
    fn play_ping(app: &AppHandle) {
        let _ = app.run_on_main_thread(|| {
            use objc2_app_kit::NSSound;
            use objc2_foundation::ns_string;
            // A system sound, so nothing has to ship in the bundle.
            if let Some(sound) = NSSound::soundNamed(ns_string!("Pop")) {
                sound.play();
            }
        });
    }

    extern "C" fn tap_callback(
        _proxy: CGEventTapProxy,
        etype: u32,
        event: CGEventRef,
        _info: *mut c_void,
    ) -> CGEventRef {
        if etype == K_CG_TAP_DISABLED_BY_TIMEOUT || etype == K_CG_TAP_DISABLED_BY_USER_INPUT {
            let port = TAP_PORT.load(Ordering::SeqCst);
            if !port.is_null() {
                unsafe { CGEventTapEnable(port, true) };
            }
            return event;
        }
        if etype == K_CG_EVENT_FLAGS_CHANGED {
            let keycode =
                unsafe { CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) };
            if keycode == KEYCODE_FN {
                let flags = unsafe { CGEventGetFlags(event) };
                let down = (flags & FLAG_SECONDARY_FN) != 0;
                let was_down = FN_IS_DOWN.swap(down, Ordering::SeqCst);
                let at_ms = now_ms();
                let mode = TRIGGER_MODE.load(Ordering::SeqCst);
                if down && !was_down {
                    FN_DOWN_AT_MS.store(at_ms, Ordering::SeqCst);
                    // In toggle mode the same key press is reported as the hands-free
                    // binding, which the machine already treats as "start a locked
                    // session, and end it on the next press". Nothing in the reducer
                    // needs to know a setting exists.
                    //
                    // Double-tap mode reports nothing on the way *down* unless a
                    // session is live, in which case this press is the stop. Starting
                    // is decided on release, where a tap can be told from a hold.
                    let binding = match mode {
                        TRIGGER_TOGGLE => Some(BindingId::HandsFree),
                        TRIGGER_DOUBLE_TAP if session_is_live() => Some(BindingId::HandsFree),
                        TRIGGER_DOUBLE_TAP => None,
                        _ => Some(BindingId::PushToTalk),
                    };
                    let Some(binding) = binding else {
                        return event;
                    };
                    eprintln!("[whimpr] Fn DOWN ({})", trigger_mode_name(mode));
                    // Snapshot the paste target now, while the user's app is focused.
                    let target = crate::appctx::frontmost_bundle_id();
                    *TARGET_APP.get_or_init(|| Mutex::new(None)).lock().unwrap() = target;
                    handle_input(Input::Trigger(TriggerToken::Down { binding, at_ms }));
                } else if !down && was_down {
                    // Double-tap mode: this release either completes a pair and starts
                    // a locked session, or becomes the pending first tap. A press long
                    // enough to be a hold is somebody using Fn as a modifier — Fn+Delete,
                    // Fn+arrow — so it arms nothing and clears anything armed.
                    if mode == TRIGGER_DOUBLE_TAP {
                        // A press during a live dictation was already handled as the
                        // stop on the way down; its release means nothing.
                        if session_is_live() {
                            PENDING_TAP_MS.store(0, Ordering::SeqCst);
                            return event;
                        }
                        let held = at_ms.saturating_sub(FN_DOWN_AT_MS.load(Ordering::SeqCst));
                        let pending = PENDING_TAP_MS.swap(0, Ordering::SeqCst);
                        match whimpr_core::state::classify_double_tap_release(
                            held,
                            (pending > 0).then_some(pending),
                            at_ms,
                        ) {
                            TapOutcome::StartLocked => {
                                eprintln!("[whimpr] Fn double-tap — starting a locked session");
                                let target = crate::appctx::frontmost_bundle_id();
                                *TARGET_APP.get_or_init(|| Mutex::new(None)).lock().unwrap() =
                                    target;
                                handle_input(Input::Trigger(TriggerToken::Down {
                                    binding: BindingId::HandsFree,
                                    at_ms,
                                }));
                            }
                            // `swap` above already cleared it; re-arm with this release.
                            TapOutcome::ArmFirstTap => {
                                PENDING_TAP_MS.store(at_ms.max(1), Ordering::SeqCst)
                            }
                            TapOutcome::Ignore => {}
                        }
                        return event;
                    }
                    eprintln!("[whimpr] Fn UP");
                    // Sent unconditionally: in toggle mode the session is Locked, and a
                    // push-to-talk release is a no-op in every state that can reach. That
                    // also rescues a hold-mode session if the setting is flipped mid-press,
                    // which suppressing the release would leave recording until the cap.
                    handle_input(Input::Trigger(TriggerToken::Up {
                        binding: BindingId::PushToTalk,
                        at_ms,
                    }));
                }
            }
        }
        event
    }

    pub fn install(app: AppHandle) {
        let _ = APP.set(app);
        let _ = MACHINE.set(Mutex::new(StateMachine::new()));
        let _ = CLOCK.set(Instant::now());

        // Load the speech-to-text model off the main thread, and only when it is the
        // engine that will be used — its weights sit in a Metal buffer for as long as
        // the app runs (547 MB for the q5_0 turbo build), so on a machine set to cloud
        // ASR that is half a gigabyte held permanently for a fallback. Same reasoning
        // as the cleanup worker below, and the same shape: `ensure_asr` loads on first
        // need, which costs the fallback one ~1s load and nothing after.
        //
        // Settings are read here rather than taken from the load below, because that
        // load has not happened yet and this wants to be off the main thread.
        std::thread::spawn(|| {
            if matches!(
                whimpr_core::Settings::load(&settings_path()).asr_mode,
                whimpr_core::AsrMode::Local
            ) {
                ensure_asr();
            }
        });

        // Load settings + dictionary, and build cloud providers from stored keys.
        let settings = whimpr_core::Settings::load(&settings_path());
        let dict = whimpr_core::DictionaryStore::load(&dict_path());
        eprintln!(
            "[whimpr] cleanup mode: {:?}, level: {:?}, trigger: {:?}",
            settings.cleanup_mode, settings.cleanup_level, settings.trigger_mode
        );
        cache_trigger_mode(&settings);
        let _ = SETTINGS.set(Mutex::new(settings));
        let _ = DICTIONARY.set(Mutex::new(dict));
        let _ = STATS.set(Mutex::new(whimpr_core::StatsStore::load(&stats_path())));
        rebuild_providers();

        // The local cleanup worker is only *preloaded* when local cleanup is the
        // selected engine. Loading it is loading a 2.3 GB model, and it stays paged
        // in for as long as the app runs — so on a machine set to cloud cleanup that
        // is a couple of gigabytes held around the clock for a fallback that fires
        // only when the endpoint 429s or the network drops. Measured on this
        // machine: 2.2 GB resident, with `cleanup_mode` set to the cloud.
        //
        // So in a cloud mode it is spawned on first need instead (see `ensure_local`),
        // which costs that one fallback cleanup a few seconds of model load and
        // nothing after. The default install, where local *is* the engine, keeps its
        // warm start exactly as before.
        let _ = LOCAL.set(Mutex::new(None));
        let preload = matches!(current_settings().cleanup_mode, CleanupMode::Local);
        std::thread::spawn(move || {
            // Status tracks whether local cleanup is *available*, which is a question
            // about the model on disk, not about whether the process is warm. A pane
            // that said "missing" next to a model sitting right there would be wrong.
            let model = crate::local_llm::model_path();
            if !model.exists() {
                set_local_status("missing", None);
                return;
            }
            let name = model.file_name().and_then(|n| n.to_str()).map(str::to_string);
            if preload {
                set_local_status("loading", None);
                ensure_local();
            }
            set_local_status("ready", name);
        });

        // Accessibility is the ONE permission that makes the Fn CGEventTap global AND
        // lets us post the Cmd+V paste into other apps. Without it, a keyboard tap is
        // silently limited to frontmost-only — the exact bug. Prompt for it up front.
        if crate::paste::is_trusted() {
            eprintln!("[whimpr] Accessibility granted — Fn works in every app, paste enabled");
        } else {
            eprintln!(
                "[whimpr] ⚠ Accessibility NOT granted — Fn only works while WhimprFlow is \
                 frontmost and paste is disabled. Prompting; grant WhimprFlow under System \
                 Settings → Privacy & Security → Accessibility (no relaunch needed)."
            );
            crate::paste::prompt_accessibility();
        }
        write_permission_snapshot();
        // Input Monitoring is NOT the gate for a CGEventTap — kept only as diagnostics.
        eprintln!(
            "[whimpr] (info) Input Monitoring: {}",
            crate::paste::input_monitoring_granted()
        );

        // Periodic tick drives the double-tap timeout / session cap.
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_millis(100));
            handle_input(Input::Tick { now_ms: now_ms() });
        });

        // Give back the memory of any model that was loaded to serve a fallback and
        // is no longer the engine in use. Its own thread rather than the 100 ms tick:
        // it takes locks a dictation holds, and it has nothing to do 599 times out of
        // 600. See `reap_idle_engines`.
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_secs(60));
            reap_idle_engines();
        });

        // The event tap runs on a thread with its own CFRunLoop. CRITICAL: create it
        // ONLY after the process is trusted for Accessibility. macOS fixes a keyboard
        // tap's privilege at CGEventTapCreate time — a tap born untrusted is
        // permanently frontmost-only and is NOT upgraded when the grant later arrives.
        // Polling here also means the Fn key starts working the moment the user grants
        // Accessibility, without a relaunch.
        std::thread::spawn(|| {
            while !crate::paste::is_trusted() {
                std::thread::sleep(Duration::from_millis(500));
            }
            eprintln!("[whimpr] Accessibility present — creating global Fn tap");
            // Trust just flipped (or was there all along) — refresh the snapshot so a
            // grant made after launch isn't reported as still missing.
            write_permission_snapshot();
            let port = unsafe {
                CGEventTapCreate(
                    K_CG_SESSION_EVENT_TAP,
                    K_CG_HEAD_INSERT,
                    K_CG_TAP_OPTION_LISTEN_ONLY,
                    EVENTS_OF_INTEREST,
                    tap_callback,
                    null_mut(),
                )
            };
            if port.is_null() {
                eprintln!(
                    "[whimpr] Fn tap null despite Accessibility — likely a stale TCC entry from \
                     an earlier build. Run: tccutil reset Accessibility com.whimpr.whimprflow, \
                     then re-grant and relaunch."
                );
                return;
            }
            TAP_PORT.store(port, Ordering::SeqCst);
            unsafe {
                let source = CFMachPortCreateRunLoopSource(null(), port, 0);
                CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopDefaultMode);
                CGEventTapEnable(port, true);
            }

            // The Esc tap shares this run loop but NOT its lifetime: created here,
            // left disabled, and switched on only while a dictation is live. It is
            // also the one consuming tap in the app, which is affordable precisely
            // because it is off the rest of the time. A failure to create it costs
            // Esc-to-cancel and nothing else, so it is not fatal.
            let esc_port = unsafe {
                CGEventTapCreate(
                    K_CG_SESSION_EVENT_TAP,
                    K_CG_HEAD_INSERT,
                    K_CG_TAP_OPTION_DEFAULT,
                    ESC_EVENTS_OF_INTEREST,
                    esc_tap_callback,
                    null_mut(),
                )
            };
            if esc_port.is_null() {
                eprintln!("[whimpr] Esc tap could not be created — Esc will not cancel");
            } else {
                ESC_TAP_PORT.store(esc_port, Ordering::SeqCst);
                unsafe {
                    let source = CFMachPortCreateRunLoopSource(null(), esc_port, 0);
                    CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopDefaultMode);
                    // Born disabled: no keystroke tap until a dictation starts.
                    CGEventTapEnable(esc_port, false);
                }
            }

            unsafe { CFRunLoopRun() };
        });
    }
}

pub use imp::{
    asr_model_name, cancel_now, clear_transcripts, current_settings, dictionary_add,
    dictionary_entries, dictionary_learn, dictionary_remove, history_page, install,
    local_model_status, rebuild_providers, stats_summary, stop_now, update_settings,
};
