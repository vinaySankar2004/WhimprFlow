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
    use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use serde::Serialize;
    use tauri::{AppHandle, Emitter};
    use whimpr_core::state::{Action, BarState};
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
    /// Mirror of `Settings::trigger_mode` for the tap callback. An atomic rather than
    /// a read of SETTINGS because the callback must stay allocation-free and never
    /// block: exceeding the hook's timeout gets the tap removed by macOS.
    static TOGGLE_TRIGGER: AtomicBool = AtomicBool::new(false);
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
    static ASR: OnceLock<Arc<whimpr_asr::WhisperEngine>> = OnceLock::new();
    static OPENAI: OnceLock<Mutex<Option<whimpr_cleanup::OpenAiProvider>>> = OnceLock::new();
    static ANTHROPIC: OnceLock<Mutex<Option<whimpr_cleanup::AnthropicProvider>>> = OnceLock::new();
    static LOCAL: OnceLock<Mutex<Option<crate::local_llm::LocalWorker>>> = OnceLock::new();
    static SETTINGS: OnceLock<Mutex<whimpr_core::Settings>> = OnceLock::new();
    static DICTIONARY: OnceLock<Mutex<whimpr_core::DictionaryStore>> = OnceLock::new();
    static STATS: OnceLock<Mutex<whimpr_core::StatsStore>> = OnceLock::new();
    /// What the local cleanup worker is doing, for the Hub to display. Deliberately
    /// NOT read off `LOCAL`: `LocalWorker::cleanup` holds that mutex for the whole
    /// multi-second generation, so a status command that locked it would freeze the
    /// Hub mid-dictation. This one is held for microseconds.
    static LOCAL_STATUS: OnceLock<Mutex<super::LocalModelStatus>> = OnceLock::new();

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

    /// The whisper ASR model to load: prefer the most accurate one present, in
    /// descending quality order, falling back to the small base model. Bigger
    /// English models mis-hear names/technical terms far less (and better ASR means
    /// less for cleanup and the dictionary to fix downstream).
    fn model_path() -> PathBuf {
        let dir = support_dir().join("models");
        for name in [
            "ggml-large-v3-turbo.bin",
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
    pub fn record_dictation(text: &str, duration_secs: f32) {
        let words = whimpr_core::stats::count_words(text);
        if words == 0 {
            return;
        }
        let app = TARGET_APP.get().and_then(|m| m.lock().unwrap().clone());
        if let Some(m) = STATS.get() {
            let mut store = m.lock().unwrap();
            let duration_ms = (duration_secs.max(0.0) * 1000.0) as u32;
            let chars = text.chars().count() as u32;
            store.record(words, duration_ms, chars, unix_now(), text.to_string(), app);
            let _ = store.save(&stats_path());
        }
    }

    /// The most recent dictations for the Hub Home history list.
    pub fn history(limit: usize) -> Vec<whimpr_core::HistoryItem> {
        STATS
            .get()
            .map(|m| m.lock().unwrap().history(limit))
            .unwrap_or_default()
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
    fn read_openai_key() -> Option<String> {
        read_key("openai_api_key", "OPENAI_API_KEY")
    }
    fn read_anthropic_key() -> Option<String> {
        read_key("anthropic_api_key", "ANTHROPIC_API_KEY")
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
        let toggle = matches!(s.trigger_mode, whimpr_core::TriggerMode::Toggle);
        TOGGLE_TRIGGER.store(toggle, Ordering::SeqCst);
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
        let anthropic = read_anthropic_key()
            .map(|k| whimpr_cleanup::AnthropicProvider::new(k, settings.anthropic_model.clone()));
        eprintln!(
            "[whimpr] cleanup providers: openai={}, anthropic={}",
            openai.is_some(),
            anthropic.is_some()
        );
        match OPENAI.get() {
            Some(m) => *m.lock().unwrap() = openai,
            None => {
                let _ = OPENAI.set(Mutex::new(openai));
            }
        }
        match ANTHROPIC.get() {
            Some(m) => *m.lock().unwrap() = anthropic,
            None => {
                let _ = ANTHROPIC.set(Mutex::new(anthropic));
            }
        }
    }

    /// Clean a raw transcript per the current settings (mode + level), feeding in the
    /// dictionary vocabulary relevant to this utterance. Falls back to raw whenever
    /// cleanup is off, the provider is unavailable, it errors, or the gates reject it.
    fn clean_transcript(raw: &str) -> String {
        let settings = current_settings();
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
            LOCAL.get().and_then(|m| {
                m.lock().unwrap().as_mut().map(|w| {
                    // System prompt + few-shot demonstration turns + the transcript,
                    // so the on-device model actually produces newlines/lists and
                    // resolves self-corrections instead of just being told to.
                    let messages = whimpr_core::cleanup::build_messages(raw, &ctx);
                    w.cleanup(&messages)
                })
            })
        };
        // Selected provider, falling back to local when a cloud key can't be read
        // (so cleanup still runs) — and Local mode uses the worker directly.
        let result: Option<anyhow::Result<String>> = match settings.cleanup_mode {
            CleanupMode::OpenAi => OPENAI
                .get()
                .and_then(|m| m.lock().unwrap().as_ref().map(|p| p.cleanup(raw, &ctx)))
                .or_else(run_local),
            CleanupMode::Anthropic => ANTHROPIC
                .get()
                .and_then(|m| m.lock().unwrap().as_ref().map(|p| p.cleanup(raw, &ctx)))
                .or_else(run_local),
            CleanupMode::Local => run_local(),
            CleanupMode::Raw => None,
        };
        match result {
            Some(Ok(cleaned)) => {
                // Deterministic safety net: convert any leftover spoken layout cue the
                // model missed into real line breaks, strip stray code fences, cap blank
                // lines. Guarantees no "new line"/"new paragraph" word reaches the cursor.
                let cleaned = whimpr_core::cleanup::post_process(&cleaned);
                if whimpr_core::cleanup::evaluate_gates(&raw_out, &cleaned, level).passed() {
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
                if matches!(settings.cleanup_mode, CleanupMode::Local) {
                    eprintln!("[whimpr] local cleanup model not wired yet — pasting raw");
                } else {
                    eprintln!("[whimpr] cleanup provider has no API key — pasting raw");
                }
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
                        "[whimpr] captured {} samples @ {} Hz (~{:.2}s), peak {:.4}",
                        res.samples.len(),
                        res.sample_rate,
                        res.duration_secs(),
                        peak
                    );
                    if peak < 0.005 {
                        eprintln!(
                            "[whimpr] ⚠ audio is silent — the mic isn't being captured. Grant \
                             Microphone access to your terminal (System Settings → Privacy & \
                             Security → Microphone), then fully quit + reopen it and rerun."
                        );
                    }
                    let Some(asr) = ASR.get().cloned() else {
                        eprintln!("[whimpr] ASR not ready (model still loading or missing)");
                        finish();
                        return;
                    };
                    // Cancelled while the mic was stopping — don't burn seconds of GPU
                    // on audio nobody wants. Checked again after ASR and before the
                    // paste, because each step is long enough to be cancelled during.
                    if is_cancelled(session) {
                        eprintln!("[whimpr] cancelled before transcribing — discarded");
                        return;
                    }
                    let pcm = whimpr_audio::resample_to_16k(&res.samples, res.sample_rate);
                    match asr.transcribe(&pcm) {
                        Ok(t) => {
                            let raw = t.text;
                            eprintln!("[whimpr] TRANSCRIPT: \"{}\"", raw);
                            if is_cancelled(session) {
                                eprintln!("[whimpr] cancelled after transcribing — not pasted");
                                return;
                            }
                            // Clean the transcript (cloud LLM if configured), then paste.
                            let text = clean_transcript(&raw);
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
                                // Log words + speaking time for the Hub stats (WPM, streak…).
                                record_dictation(&text, res.duration_secs());
                                // Watch the field for a post-paste correction to learn (✨).
                                crate::autolearn::watch_correction(&text);
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
            _ => {}
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
                if down && !was_down {
                    // In toggle mode the same key press is reported as the hands-free
                    // binding, which the machine already treats as "start a locked
                    // session, and end it on the next press". Nothing in the reducer
                    // needs to know a setting exists.
                    let toggle = TOGGLE_TRIGGER.load(Ordering::SeqCst);
                    let binding = if toggle {
                        BindingId::HandsFree
                    } else {
                        BindingId::PushToTalk
                    };
                    eprintln!("[whimpr] Fn DOWN ({})", if toggle { "toggle" } else { "hold" });
                    // Snapshot the paste target now, while the user's app is focused.
                    let target = crate::appctx::frontmost_bundle_id();
                    *TARGET_APP.get_or_init(|| Mutex::new(None)).lock().unwrap() = target;
                    handle_input(Input::Trigger(TriggerToken::Down { binding, at_ms }));
                } else if !down && was_down {
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

        // Load the speech-to-text model off the main thread (it takes ~1s).
        std::thread::spawn(|| {
            let path = model_path();
            if !path.exists() {
                eprintln!("[whimpr] ASR model not found at {}", path.display());
                return;
            }
            match whimpr_asr::WhisperEngine::load(&path) {
                Ok(engine) => {
                    let _ = ASR.set(Arc::new(engine));
                    eprintln!("[whimpr] ASR model loaded — ready to transcribe");
                }
                Err(e) => eprintln!("[whimpr] ASR model load failed: {e}"),
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

        // Start the local cleanup worker in the background (model load takes a few
        // seconds; the first local cleanup waits for it, subsequent ones are fast).
        std::thread::spawn(|| {
            set_local_status("loading", None);
            let model = crate::local_llm::model_path();
            let worker = crate::local_llm::spawn_default();
            if worker.is_some() {
                let name = model
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string());
                set_local_status("ready", name);
            } else {
                set_local_status("missing", None);
            }
            let _ = LOCAL.set(Mutex::new(worker));
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
    asr_model_name, cancel_now, current_settings, dictionary_add, dictionary_entries,
    dictionary_learn, dictionary_remove, history, install, local_model_status, rebuild_providers,
    stats_summary, stop_now, update_settings,
};
