//! The ASR seam. One backend implements [`AsrEngine`] today — whisper.cpp on Metal,
//! in `whimpr-asr`. The trait exists so a lower-latency engine can replace it without
//! the core knowing; the ids below name the candidates, not shipped code.

pub mod prompt;

use serde::{Deserialize, Serialize};

/// Identifies which backend produced a transcript (for diagnostics + UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AsrEngineId {
    FluidAudioAne,
    OnnxParakeet,
    WhisperCpp,
}

/// A finalized transcription result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    pub text: String,
    pub confidence: Option<f32>,
}

/// Static capabilities of an engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsrCaps {
    pub supports_streaming: bool,
}

/// Batch, finalize-on-release speech recognition over 16 kHz mono f32 samples in
/// [-1, 1]. Push-to-talk endpoints on key release, so a batch API is sufficient;
/// streaming preview is an optional per-engine capability.
pub trait AsrEngine: Send + Sync {
    fn id(&self) -> AsrEngineId;

    fn caps(&self) -> AsrCaps {
        AsrCaps {
            supports_streaming: false,
        }
    }

    /// Load the model and run a throwaway inference so the first real call is warm.
    fn warmup(&self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Transcribe one complete utterance.
    ///
    /// `initial_prompt` conditions decoding toward particular spellings — the
    /// dictionary, rendered by [`prompt::build_initial_prompt`]. It is a `Some`/`None`
    /// argument rather than a separate method because a caller must decide, every
    /// time, whether this run is biased: a prompted transcript is only trustworthy
    /// next to an unprompted one to check it against (see [`prompt::accept_prompted`]).
    fn transcribe(&self, pcm16k: &[f32], initial_prompt: Option<&str>)
        -> anyhow::Result<Transcript>;
}
