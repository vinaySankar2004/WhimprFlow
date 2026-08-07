//! Local speech-to-text via whisper.cpp (whisper-rs), implementing
//! [`whimpr_core::AsrEngine`]. Expects 16 kHz mono f32 samples.

use std::path::Path;

use whimpr_core::asr::{AsrCaps, AsrEngine, AsrEngineId, Transcript};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// A loaded whisper model ready to transcribe utterances.
pub struct WhisperEngine {
    ctx: WhisperContext,
}

impl WhisperEngine {
    /// Load a GGML/GGUF whisper model from `model_path`.
    pub fn load(model_path: &Path) -> anyhow::Result<Self> {
        let path = model_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("model path is not valid UTF-8"))?;
        let ctx = WhisperContext::new_with_params(path, WhisperContextParameters::default())
            .map_err(|e| anyhow::anyhow!("failed to load whisper model: {e}"))?;
        Ok(Self { ctx })
    }
}

impl AsrEngine for WhisperEngine {
    fn id(&self) -> AsrEngineId {
        AsrEngineId::WhisperCpp
    }

    fn caps(&self) -> AsrCaps {
        AsrCaps {
            supports_streaming: false,
        }
    }

    fn transcribe(
        &self,
        pcm16k: &[f32],
        initial_prompt: Option<&str>,
    ) -> anyhow::Result<Transcript> {
        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| anyhow::anyhow!("whisper create_state: {e}"))?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some("en"));
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_suppress_blank(true);
        // Push-to-talk utterances are always one short clip, not long-form audio.
        // Without this, whisper.cpp can split it into multiple internal segments
        // that repeat the same words — which then get concatenated below,
        // producing the sentence twice. Single-segment mode avoids that.
        params.set_single_segment(true);
        // No carry-over between dictations: each is its own utterance, and letting the
        // last one condition the next produces stray repeats.
        //
        // This does NOT cancel the initial_prompt below, which is the obvious worry.
        // Checked against the vendored whisper.cpp rather than assumed: no_context
        // clears `prompt_past` first, and the initial prompt is tokenized and rotated
        // to the front of it afterwards. The two settings compose.
        params.set_no_context(true);
        // Bias decoding toward the dictionary's spellings, when the caller asked for
        // it. Whisper treats this as text preceding the utterance, so it is the one
        // place a mis-hearing can be fixed rather than repaired afterwards.
        if let Some(p) = initial_prompt {
            params.set_initial_prompt(p);
        }

        state
            .full(params, pcm16k)
            .map_err(|e| anyhow::anyhow!("whisper full: {e}"))?;

        let n = state
            .full_n_segments()
            .map_err(|e| anyhow::anyhow!("whisper n_segments: {e}"))?;
        let mut text = String::new();
        for i in 0..n {
            if let Ok(seg) = state.full_get_segment_text(i) {
                text.push_str(&seg);
            }
        }

        Ok(Transcript {
            text: text.trim().to_string(),
            confidence: None,
        })
    }
}
