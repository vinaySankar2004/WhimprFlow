//! Speech-to-text. Two engines behind [`whimpr_core::AsrEngine`], both taking 16 kHz
//! mono f32 samples: whisper.cpp on Metal (default, audio stays on the machine) and
//! [`cloud::CloudAsr`], the same Whisper model hosted on Groq.

pub mod cloud;

pub use cloud::CloudAsr;

use std::path::Path;

use whimpr_core::asr::{AsrCaps, AsrEngine, AsrEngineId, Transcript};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// Silence appended to every utterance before recognition, in samples at 16 kHz.
///
/// Not cosmetic. whisper.cpp will not begin a new segment within a second of the end
/// of the audio (`if (seek + 100 >= seek_end) break;`) and drops its prompt for short
/// trailing segments — so a recording that stops the instant the speaker does can lose
/// its final words. Upstream warns about exactly this and recommends padding.
///
/// Push-to-talk produces precisely that shape: the key comes up on the last syllable.
/// Measured on a 5 s clip, `large-v3-turbo` returned "...reviewed by Manvi, at Charge"
/// and, with a second of silence appended, the whole sentence. Larger models segment
/// more finely and so are hit harder, which makes this a prerequisite for upgrading
/// the model rather than a nicety.
const TAIL_PAD_SAMPLES: usize = 16_000;

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

        // See TAIL_PAD_SAMPLES: without a tail of silence the last words of an
        // abruptly-ended recording can be dropped, which is the shape push-to-talk
        // always produces.
        let mut padded = Vec::with_capacity(pcm16k.len() + TAIL_PAD_SAMPLES);
        padded.extend_from_slice(pcm16k);
        padded.resize(pcm16k.len() + TAIL_PAD_SAMPLES, 0.0);

        state
            .full(params, &padded)
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
