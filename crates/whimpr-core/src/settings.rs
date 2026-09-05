//! User settings, persisted as JSON. Drives the cleanup engine (which provider,
//! how aggressive) and other behavior. Kept dependency-light so it lives in core.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::cleanup::CleanupLevel;

/// Which cleanup engine processes transcripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CleanupMode {
    /// Paste the raw transcript (no cleanup).
    Raw,
    /// Local on-device model (default — works offline, no API key).
    #[default]
    Local,
    /// A cloud endpoint speaking the OpenAI chat-completions wire format. Groq by
    /// default; `openai_base_url` repoints it at OpenAI, OpenRouter, Gemini's
    /// compatibility endpoint or anything else with the same shape. The variant is
    /// still named `OpenAi` because that string is in every saved `settings.json` —
    /// renaming it resets the file. It means "the OpenAI *protocol*", not the vendor.
    OpenAi,
}

/// Hand-written so an unrecognized mode degrades to `Local` instead of failing.
///
/// A derived impl rejects any string it does not know, and because `cleanup_mode` is
/// a required field that error fails the *whole* `Settings` parse — `load` then falls
/// back to `Default` and silently discards every other saved setting. The retired
/// `"anthropic"` is exactly such a string, so this is what stops removing that engine
/// from wiping the dictation key, level and endpoint of anyone who had it selected.
impl<'de> Deserialize<'de> for CleanupMode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match String::deserialize(d)?.as_str() {
            "raw" => Self::Raw,
            "open_ai" => Self::OpenAi,
            // "anthropic" and anything else unknown.
            _ => Self::Local,
        })
    }
}

/// Where speech recognition runs.
///
/// Separate from [`CleanupMode`] on purpose: these two stages send *different things*
/// off the machine. Cleanup sends a transcript — words you are about to paste into
/// someone's chat window anyway. ASR sends the recording itself. Folding them into one
/// "cloud" switch would mean agreeing to upload your voice in order to get a faster
/// full stop, so the privacy decision stays where the user can see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AsrMode {
    /// whisper.cpp on this machine's GPU. Audio never leaves the Mac.
    #[default]
    Local,
    /// Groq's hosted `whisper-large-v3-turbo` — the same model, on their hardware.
    /// Several times faster; the audio is uploaded.
    Cloud,
}

/// Groq's audio transcription endpoint (OpenAI-compatible, multipart upload).
pub const GROQ_ASR_URL: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
/// Groq's turbo Whisper — the same weights as the local `ggml-large-v3-turbo`, so
/// switching engines changes latency and not what the words come out as.
pub const GROQ_ASR_MODEL: &str = "whisper-large-v3-turbo";

/// How the dictation key starts and stops a recording.
///
/// This is a *shell* concern, not a state-machine one: the machine already knows
/// both a push-to-talk and a hands-free (locked) session. The setting only decides
/// which binding a press of the dictation key is reported as, so `Toggle` and
/// `DoubleTap` both reuse the exact same locked-session path that double-tap-to-lock
/// already drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TriggerMode {
    /// Hold the key while speaking; releasing it finalizes (default). A quick tap
    /// followed by a second press within the double-tap window still locks
    /// hands-free.
    #[default]
    Hold,
    /// Press once to start listening, press again to stop. Key release is ignored,
    /// so the key can be let go while speaking.
    Toggle,
    /// Double-tap to start, one press to stop — and a single press or a hold does
    /// **nothing**.
    ///
    /// That last part is the whole point: in the other two modes the dictation key
    /// is spent, so `Fn`+`Delete` (forward delete), `Fn`+arrows and the rest of
    /// macOS's Fn combinations either start a dictation or are shadowed by one. Here
    /// a lone press is left to the system, and dictation costs a deliberate gesture.
    DoubleTap,
}

/// Persisted user configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub cleanup_mode: CleanupMode,
    /// Where speech recognition runs. `#[serde(default)]` keeps every settings.json
    /// written before this field existed loading with the rest of its values intact.
    #[serde(default)]
    pub asr_mode: AsrMode,
    pub cleanup_level: CleanupLevel,
    /// Hold the dictation key, or press it once to start and again to stop.
    #[serde(default)]
    pub trigger_mode: TriggerMode,
    pub openai_model: String,
    /// API root for the OpenAI-compatible cleanup mode — Groq by default, or e.g.
    /// `https://openrouter.ai/api/v1`. An empty string means OpenAI's own endpoint,
    /// which is what a `settings.json` written before Groq became the default holds.
    #[serde(default)]
    pub openai_base_url: String,
    /// Play the record-start ping.
    pub sound_on_start: bool,
    /// Keep the raw pre-cleanup transcript alongside the cleaned text in `stats.json`.
    /// On by default: it never leaves the machine, and it is what the speaking
    /// insights are computed from — fillers and self-corrections only exist in the
    /// raw text, since cleanup's whole job is deleting them. Turning it off stops
    /// new raw text being written; it does not remove what is already stored (the
    /// "Clear transcripts" action does that).
    #[serde(default = "default_true")]
    pub store_raw_transcripts: bool,
}

fn default_true() -> bool {
    true
}

/// Groq's OpenAI-compatible API root. Free tier at the time of writing: 30 requests
/// per minute, 1,000 per day, no card required.
pub const GROQ_BASE_URL: &str = "https://api.groq.com/openai/v1";
/// Groq's fastest production model (~1000 tok/s, 131k context). Cleanup blocks the
/// paste, so speed is the selection criterion — the task itself is easy.
pub const GROQ_MODEL: &str = "openai/gpt-oss-20b";

impl Default for Settings {
    fn default() -> Self {
        Self {
            cleanup_mode: CleanupMode::default(),
            asr_mode: AsrMode::default(),
            cleanup_level: CleanupLevel::Light,
            trigger_mode: TriggerMode::default(),
            // Groq, not OpenAI: cleanup sits in the blocking paste path, so tokens per
            // second matters more here than model size, and gpt-oss-20b is the fastest
            // thing on the fastest free tier. Groq deprecated the llama-3.x ids in June
            // 2026 — do not "restore" one of those.
            openai_model: GROQ_MODEL.to_string(),
            openai_base_url: GROQ_BASE_URL.to_string(),
            sound_on_start: true,
            store_raw_transcripts: true,
        }
    }
}

impl Settings {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let s = Settings::default();
        assert_eq!(s.cleanup_mode, CleanupMode::Local);
        assert_eq!(s.cleanup_level, CleanupLevel::Light);
        assert_eq!(s.trigger_mode, TriggerMode::Hold);
    }

    /// A settings.json written before `trigger_mode` existed must still load with
    /// every other setting intact — without `#[serde(default)]` the whole parse
    /// fails and `Settings::load` silently falls back to defaults. Same hazard for
    /// the retired `"high"` cleanup level, which is aliased forward to Light rather
    /// than left to blow up the parse and take every other setting with it.
    #[test]
    fn older_settings_file_without_trigger_mode_still_loads() {
        let json = r#"{
            "cleanup_mode": "local",
            "cleanup_level": "high",
            "openai_model": "gpt-4o-mini",
            "openai_base_url": "",
            "sound_on_start": false
        }"#;
        let s: Settings = serde_json::from_str(json).expect("old file still parses");
        assert_eq!(s.cleanup_level, CleanupLevel::Light);
        assert!(!s.sound_on_start);
        assert_eq!(s.trigger_mode, TriggerMode::Hold);
        assert_eq!(s.asr_mode, AsrMode::Local);
    }

    /// The retired Anthropic engine must degrade to Local, taking nothing with it.
    /// A derived `Deserialize` would reject the string, fail the whole parse, and send
    /// `load` to `Default` — silently resetting the dictation key, cleanup level and
    /// endpoint of anyone who happened to have that engine selected.
    #[test]
    fn a_retired_cleanup_mode_degrades_without_resetting_everything_else() {
        let json = r#"{
            "cleanup_mode": "anthropic",
            "cleanup_level": "messaging",
            "trigger_mode": "toggle",
            "openai_model": "openai/gpt-oss-20b",
            "openai_base_url": "https://api.groq.com/openai/v1",
            "anthropic_model": "claude-haiku-4-5",
            "sound_on_start": false
        }"#;
        let s: Settings = serde_json::from_str(json).expect("retired mode must not fail the parse");
        assert_eq!(s.cleanup_mode, CleanupMode::Local);
        // Everything else survives — that is the point of the test.
        assert_eq!(s.cleanup_level, CleanupLevel::Messaging);
        assert_eq!(s.trigger_mode, TriggerMode::Toggle);
        assert_eq!(s.openai_base_url, GROQ_BASE_URL);
        assert!(!s.sound_on_start);
    }

    #[test]
    fn asr_mode_round_trips_and_defaults_to_local() {
        assert_eq!(Settings::default().asr_mode, AsrMode::Local);
        let s = Settings { asr_mode: AsrMode::Cloud, ..Default::default() };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"asr_mode\":\"cloud\""), "{json}");
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap().asr_mode, AsrMode::Cloud);
    }

    #[test]
    fn trigger_mode_round_trips_as_snake_case() {
        for (mode, wire) in [
            (TriggerMode::Toggle, "toggle"),
            (TriggerMode::DoubleTap, "double_tap"),
            (TriggerMode::Hold, "hold"),
        ] {
            let s = Settings { trigger_mode: mode, ..Default::default() };
            let json = serde_json::to_string(&s).unwrap();
            assert!(json.contains(&format!("\"trigger_mode\":\"{wire}\"")), "{json}");
            assert_eq!(serde_json::from_str::<Settings>(&json).unwrap().trigger_mode, mode);
        }
    }

    /// The cloud mode ships pointed at Groq, not at OpenAI. Cleanup blocks the paste,
    /// so the endpoint choice is a latency decision; a default of `""` (OpenAI) would
    /// also demand a paid key before cleanup worked at all.
    #[test]
    fn cloud_defaults_to_groq() {
        let s = Settings::default();
        assert_eq!(s.openai_base_url, GROQ_BASE_URL);
        assert_eq!(s.openai_model, GROQ_MODEL);
    }

    #[test]
    fn round_trips_json() {
        let s = Settings {
            cleanup_mode: CleanupMode::Local,
            ..Default::default()
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.cleanup_mode, CleanupMode::Local);
    }
}
