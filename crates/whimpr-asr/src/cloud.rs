//! Cloud speech-to-text via Groq's OpenAI-compatible transcription endpoint.
//!
//! Same `AsrEngine` trait as the local whisper.cpp engine, and deliberately the same
//! *model* — `whisper-large-v3-turbo` are the weights `ggml-large-v3-turbo-q5_0.bin`
//! is a quantization of. Switching engines is meant to change how long a dictation
//! takes and nothing about which words come out, so the dictionary, the gates and the
//! two-pass prompted retranscribe all behave identically on either side.
//!
//! The trade this makes is the one the rest of the app avoids: the audio leaves the
//! machine. That is why it is a separate setting from cloud *cleanup*, which only ever
//! sends a transcript.

use std::io::Cursor;
use std::sync::Mutex;
use std::time::Duration;

use whimpr_core::asr::{AsrCaps, AsrEngine, AsrEngineId, Transcript};
use whimpr_core::cloud::{retry_after_secs, KeyRing};

/// Groq transcribes far faster than realtime, so this is a stall guard, not a budget.
/// It is generous because the whole clip is uploaded first and a phone tether is a
/// realistic place to be dictating from.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Speech recognition over HTTP against a Groq-hosted Whisper.
pub struct CloudAsr {
    client: reqwest::blocking::Client,
    /// The same keys cleanup holds, with their own clocks: the vendor's caps are per
    /// model, so a key limited for cleanup may still transcribe and vice versa.
    keys: Mutex<KeyRing>,
    model: String,
    url: String,
}

impl CloudAsr {
    pub fn new(keys: Vec<String>, model: impl Into<String>, url: impl Into<String>) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .expect("failed to build HTTP client");
        Self {
            client,
            keys: Mutex::new(KeyRing::new(keys)),
            model: model.into(),
            url: url.into(),
        }
    }

    fn pick(&self) -> anyhow::Result<(usize, String)> {
        let ring = self.keys.lock().unwrap();
        let now = unix_now();
        match ring.pick(now) {
            Some(i) => Ok((i, ring.keys()[i].clone())),
            None => anyhow::bail!(
                "cloud ASR HTTP 429: every key ({}) is rate limited. Please try again in {}s",
                ring.len(),
                ring.wait_secs(now).unwrap_or(0)
            ),
        }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl AsrEngine for CloudAsr {
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
        if pcm16k.is_empty() {
            anyhow::bail!("nothing to transcribe");
        }
        let wav = encode_wav_16k_mono(pcm16k)?;

        // One attempt per usable key: a 429 marks that key for as long as the
        // endpoint asked and moves on, and `pick` is the exit when none is left.
        loop {
            let (index, key) = self.pick()?;
            let mut form = reqwest::blocking::multipart::Form::new()
                .text("model", self.model.clone())
                // Plain text back: no segments, no timestamps, nothing to parse around.
                .text("response_format", "text")
                // Pinned rather than auto-detected. Whisper's language ID is unreliable
                // on short push-to-talk clips and a wrong guess does not mis-spell a
                // word, it silently *translates* the whole utterance.
                .text("language", "en")
                .text("temperature", "0")
                .part(
                    "file",
                    reqwest::blocking::multipart::Part::reader(Cursor::new(wav.clone()))
                        .file_name("utterance.wav")
                        .mime_str("audio/wav")?,
                );
            // The same conditioning the local engine gets, so `accept_prompted` is
            // still comparing like with like across the two passes. Without this the
            // dictionary would quietly stop working the moment cloud ASR was selected.
            if let Some(p) = initial_prompt {
                form = form.text("prompt", p.to_string());
            }

            let resp = self.client.post(&self.url).bearer_auth(&key).multipart(form).send()?;

            let status = resp.status();
            let header = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let body = resp.text().unwrap_or_default();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let secs = retry_after_secs(header.as_deref(), &body);
                let mut ring = self.keys.lock().unwrap();
                ring.report_limited(index, unix_now(), secs);
                eprintln!(
                    "[whimpr] cloud ASR key {} of {} is rate limited for {}s",
                    index + 1,
                    ring.len(),
                    secs.map(|s| s.ceil() as u64).unwrap_or(60)
                );
                continue;
            }
            if !status.is_success() {
                anyhow::bail!("cloud ASR HTTP {status}: {body}");
            }
            return Ok(Transcript {
                text: body.trim().to_string(),
                confidence: None,
            });
        }
    }
}

/// Encode 16 kHz mono f32 samples as a 16-bit PCM WAV.
///
/// Written out by hand rather than pulled in as a dependency: it is a 44-byte header
/// and a cast, and `hound` is already a dev-dependency only. 16-bit is what halves the
/// upload against f32 with no accuracy cost — Whisper's own front end quantizes well
/// below that before it sees anything.
fn encode_wav_16k_mono(pcm: &[f32]) -> anyhow::Result<Vec<u8>> {
    const RATE: u32 = 16_000;
    const BITS: u16 = 16;
    let data_len = (pcm.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);

    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // format = PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // channels = mono
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&BITS.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for &s in pcm {
        // Clamp before scaling: a sample slightly past 1.0 (the normalizer targets 0.7
        // precisely to avoid this, but resampling can still overshoot) would otherwise
        // wrap to a large negative and paste as a click.
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_is_well_formed() {
        let wav = encode_wav_16k_mono(&[0.0, 0.5, -0.5]).unwrap();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(wav.len(), 44 + 3 * 2, "44-byte header plus 16-bit samples");
        // Declared data length must match what actually follows it.
        let declared = u32::from_le_bytes(wav[40..44].try_into().unwrap());
        assert_eq!(declared as usize, wav.len() - 44);
    }

    /// A sample past full scale must clamp, not wrap. Wrapping turns the loudest
    /// moment of an utterance into a click, which is worse than clipping it.
    #[test]
    fn samples_past_full_scale_clamp_instead_of_wrapping() {
        let wav = encode_wav_16k_mono(&[1.4, -1.4]).unwrap();
        let a = i16::from_le_bytes(wav[44..46].try_into().unwrap());
        let b = i16::from_le_bytes(wav[46..48].try_into().unwrap());
        assert!(a > 32_000, "positive overshoot became {a}");
        assert!(b < -32_000, "negative overshoot became {b}");
    }
}
