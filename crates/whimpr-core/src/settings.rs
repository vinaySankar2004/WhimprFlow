//! User settings, persisted as JSON. Drives the cleanup engine (which provider,
//! how aggressive) and other behavior. Kept dependency-light so it lives in core.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::cleanup::CleanupLevel;

/// Which cleanup engine processes transcripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CleanupMode {
    /// Paste the raw transcript (no cleanup).
    Raw,
    /// Local on-device model (default — works offline, no API key).
    #[default]
    Local,
    /// OpenAI cloud.
    OpenAi,
    /// Anthropic cloud.
    Anthropic,
}

/// How the dictation key starts and stops a recording.
///
/// This is a *shell* concern, not a state-machine one: the machine already knows
/// both a push-to-talk and a hands-free (locked) session. The setting only decides
/// which binding a press of the dictation key is reported as, so `Toggle` reuses
/// the exact same locked-session path that double-tap-to-lock already drives.
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
}

/// Persisted user configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub cleanup_mode: CleanupMode,
    pub cleanup_level: CleanupLevel,
    /// Hold the dictation key, or press it once to start and again to stop.
    #[serde(default)]
    pub trigger_mode: TriggerMode,
    pub openai_model: String,
    /// API root for the "OpenAI" cleanup mode, e.g. `https://openrouter.ai/api/v1`
    /// to route through OpenRouter instead of OpenAI directly (same wire format).
    /// Empty string (the default) means OpenAI's own endpoint.
    #[serde(default)]
    pub openai_base_url: String,
    pub anthropic_model: String,
    /// Play the record-start ping.
    pub sound_on_start: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            cleanup_mode: CleanupMode::default(),
            cleanup_level: CleanupLevel::Light,
            trigger_mode: TriggerMode::default(),
            openai_model: "gpt-4o-mini".to_string(),
            openai_base_url: String::new(),
            anthropic_model: "claude-haiku-4-5".to_string(),
            sound_on_start: true,
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
    /// fails and `Settings::load` silently falls back to defaults.
    #[test]
    fn older_settings_file_without_trigger_mode_still_loads() {
        let json = r#"{
            "cleanup_mode": "anthropic",
            "cleanup_level": "high",
            "openai_model": "gpt-4o-mini",
            "openai_base_url": "",
            "anthropic_model": "claude-haiku-4-5",
            "sound_on_start": false
        }"#;
        let s: Settings = serde_json::from_str(json).expect("old file still parses");
        assert_eq!(s.cleanup_mode, CleanupMode::Anthropic);
        assert_eq!(s.cleanup_level, CleanupLevel::High);
        assert!(!s.sound_on_start);
        assert_eq!(s.trigger_mode, TriggerMode::Hold);
    }

    #[test]
    fn trigger_mode_round_trips_as_snake_case() {
        let s = Settings { trigger_mode: TriggerMode::Toggle, ..Default::default() };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"trigger_mode\":\"toggle\""), "{json}");
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.trigger_mode, TriggerMode::Toggle);
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
