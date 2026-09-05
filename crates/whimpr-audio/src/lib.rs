//! Microphone capture for WhimprFlow.
//!
//! [`start`] opens the microphone and streams audio. While it runs it
//! downmixes to mono, accumulates the whole utterance, and invokes a throttled
//! callback with a small rolling window of RMS levels (0..1) for the pill's live
//! waveform. [`CaptureHandle::stop`] returns the accumulated mono samples plus the
//! device sample rate, ready for resampling to 16 kHz and handing to ASR.
//!
//! cpal's macOS `Stream` is not `Send`, so the stream is created and owned on a
//! dedicated thread; control flows over channels.
//!
//! Opening is a search, not a single attempt: every sample format is accepted, each
//! device is tried against its whole advertised config list, and the default input
//! device is only the *first* candidate. This is what makes dictation survive a call
//! — see [`start`].

use std::collections::VecDeque;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};

/// Number of bars in the rolling waveform window (matches the pill's bar count).
const WAVE_BARS: usize = 6;
/// Emit the waveform at ~30 fps.
const EMIT_INTERVAL: Duration = Duration::from_millis(33);
/// The meter's dynamic range, in dBFS. RMS is mapped across this window rather than
/// scaled linearly, because loudness is perceived logarithmically and a linear meter
/// is wrong at both ends: quiet speech (about -46 dBFS) produced a level of 0.07 —
/// under the pill's idle shimmer, so speaking looked identical to silence — while
/// anything above a normal speaking voice pinned at 1.0 and stopped moving.
///
/// The floor is set below a quiet room rather than at it: room tone should sit near
/// zero, and every voice above it should have somewhere to go.
const LEVEL_FLOOR_DB: f32 = -55.0;
const LEVEL_CEIL_DB: f32 = -12.0;

/// Map an RMS amplitude in `[0, 1]` onto meter height in `[0, 1]`.
fn meter_level(rms: f32) -> f32 {
    // Clamped before the log so digital silence gives the floor, not -infinity.
    let db = 20.0 * rms.max(1e-6).log10();
    ((db - LEVEL_FLOOR_DB) / (LEVEL_CEIL_DB - LEVEL_FLOOR_DB)).clamp(0.0, 1.0)
}

/// The captured audio for one utterance.
pub struct CaptureResult {
    /// Mono samples at `sample_rate` (device-native; resample before ASR).
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    /// The input device the audio actually came from. Worth logging: when the
    /// default device is unusable we fall back to another one, and "which mic did
    /// it use" is otherwise unanswerable after the fact.
    pub device: String,
}

impl CaptureResult {
    pub fn duration_secs(&self) -> f32 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.samples.len() as f32 / self.sample_rate as f32
        }
    }
}

/// A running capture. Drop or [`stop`](Self::stop) to end it.
pub struct CaptureHandle {
    stop_tx: Sender<()>,
    join: Option<JoinHandle<Option<CaptureResult>>>,
}

impl CaptureHandle {
    /// Stop capture and return the accumulated audio (None if the device failed).
    pub fn stop(mut self) -> Option<CaptureResult> {
        let _ = self.stop_tx.send(());
        self.join.take().and_then(|h| h.join().ok().flatten())
    }
}

impl Drop for CaptureHandle {
    fn drop(&mut self) {
        // If dropped without an explicit stop, still end the capture thread.
        let _ = self.stop_tx.send(());
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
    }
}

/// Start capturing from the microphone.
///
/// `on_bars` is called ~30x/second with `WAVE_BARS` RMS levels in `[0, 1]`
/// (oldest→newest), from the audio thread. Returns once the stream is playing (so
/// a microphone-permission failure surfaces here, not silently).
///
/// Opening tries, in order, every config the default input device advertises, then
/// every other input device the same way, and takes the first that plays. Being
/// stubborn here is the whole point: CoreAudio input is *shared*, so a call app
/// holding the mic never locks us out, but a Bluetooth headset on a call switches
/// to its HFP profile — mono, low rate, a different sample format — and one attempt
/// at one config on one device fails. That failure is what "dictation is dead while
/// I'm on a call" actually is; the built-in mic was available the whole time.
pub fn start<F>(on_bars: F) -> anyhow::Result<CaptureHandle>
where
    F: Fn(&[f32]) + Send + Sync + 'static,
{
    let (stop_tx, stop_rx) = channel::<()>();
    let (ready_tx, ready_rx) = channel::<anyhow::Result<()>>();

    let join = std::thread::spawn(move || -> Option<CaptureResult> {
        let buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
        let on_bars = Arc::new(on_bars);

        let mut last_err = None;
        let mut opened = None;
        for device in input_devices() {
            let name = device.name().unwrap_or_else(|_| "<unnamed>".into());
            match open_stream(&device, &buffer, &on_bars) {
                Ok((stream, sample_rate)) => {
                    opened = Some((stream, sample_rate, name));
                    break;
                }
                Err(e) => {
                    eprintln!("[whimpr-audio] {name}: {e}");
                    last_err = Some(e);
                }
            }
        }
        let Some((stream, sample_rate, device)) = opened else {
            let _ = ready_tx.send(Err(last_err
                .unwrap_or_else(|| anyhow::anyhow!("no usable input device"))));
            return None;
        };

        if let Err(e) = stream.play() {
            let _ = ready_tx.send(Err(anyhow::anyhow!("failed to start stream: {e}")));
            return None;
        }
        eprintln!("[whimpr-audio] capturing from {device} @ {sample_rate} Hz");
        let _ = ready_tx.send(Ok(()));

        // Keep the stream alive on this thread until asked to stop.
        let _ = stop_rx.recv();
        drop(stream);

        let samples = std::mem::take(&mut *buffer.lock().unwrap());
        Some(CaptureResult {
            samples,
            sample_rate,
            device,
        })
    });

    match ready_rx.recv() {
        Ok(Ok(())) => Ok(CaptureHandle {
            stop_tx,
            join: Some(join),
        }),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(anyhow::anyhow!("capture thread exited before starting")),
    }
}

/// Input devices to try, default first, deduped by name.
fn input_devices() -> Vec<cpal::Device> {
    let host = cpal::default_host();
    let mut devices: Vec<cpal::Device> = host.default_input_device().into_iter().collect();
    if let Ok(rest) = host.input_devices() {
        for d in rest {
            let name = d.name().ok();
            if name.is_some() && devices.iter().any(|c| c.name().ok() == name) {
                continue;
            }
            devices.push(d);
        }
    }
    devices
}

/// Build a playable input stream on `device`, trying its default config first and
/// then everything else it advertises. The device's *default* config is the one that
/// goes stale when the device reconfigures underneath us, so a rejection there says
/// nothing about the rest of the list.
fn open_stream<F>(
    device: &cpal::Device,
    buffer: &Arc<Mutex<Vec<f32>>>,
    on_bars: &Arc<F>,
) -> anyhow::Result<(cpal::Stream, u32)>
where
    F: Fn(&[f32]) + Send + Sync + 'static,
{
    let mut configs: Vec<cpal::SupportedStreamConfig> =
        device.default_input_config().into_iter().collect();
    if let Ok(ranges) = device.supported_input_configs() {
        for range in ranges {
            let c = range.with_max_sample_rate();
            let dup = configs.iter().any(|e| {
                e.sample_format() == c.sample_format()
                    && e.channels() == c.channels()
                    && e.sample_rate() == c.sample_rate()
            });
            if !dup {
                configs.push(c);
            }
        }
    }
    if configs.is_empty() {
        anyhow::bail!("no input configs advertised");
    }

    let mut last_err = None;
    for supported in configs {
        let sample_rate = supported.sample_rate().0;
        let channels = supported.channels().max(1) as usize;
        let config = supported.config();
        let built = match supported.sample_format() {
            SampleFormat::F32 => build::<f32, F>(device, &config, channels, buffer, on_bars),
            SampleFormat::F64 => build::<f64, F>(device, &config, channels, buffer, on_bars),
            SampleFormat::I8 => build::<i8, F>(device, &config, channels, buffer, on_bars),
            SampleFormat::I16 => build::<i16, F>(device, &config, channels, buffer, on_bars),
            SampleFormat::I32 => build::<i32, F>(device, &config, channels, buffer, on_bars),
            SampleFormat::U8 => build::<u8, F>(device, &config, channels, buffer, on_bars),
            SampleFormat::U16 => build::<u16, F>(device, &config, channels, buffer, on_bars),
            SampleFormat::U32 => build::<u32, F>(device, &config, channels, buffer, on_bars),
            other => Err(anyhow::anyhow!("unsupported sample format {other}")),
        };
        match built {
            Ok(stream) => return Ok((stream, sample_rate)),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no input config could be opened")))
}

/// Build the input stream for one concrete sample type: downmix to mono into
/// `buffer`, and feed the throttled RMS window to `on_bars`.
fn build<T, F>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    buffer: &Arc<Mutex<Vec<f32>>>,
    on_bars: &Arc<F>,
) -> anyhow::Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
    F: Fn(&[f32]) + Send + Sync + 'static,
{
    let buf_cb = buffer.clone();
    let on_bars = on_bars.clone();
    let mut ring: VecDeque<f32> = VecDeque::from(vec![0.0f32; WAVE_BARS]);
    let mut last_emit = Instant::now();

    let stream = device.build_input_stream(
        config,
        move |data: &[T], _| {
            let frames = data.len() / channels;
            let mut sumsq = 0.0f32;
            {
                let mut buf = buf_cb.lock().unwrap();
                buf.reserve(frames);
                for f in 0..frames {
                    let mut acc = 0.0f32;
                    for c in 0..channels {
                        acc += f32::from_sample(data[f * channels + c]);
                    }
                    let mono = acc / channels as f32;
                    buf.push(mono);
                    sumsq += mono * mono;
                }
            }
            if last_emit.elapsed() >= EMIT_INTERVAL {
                last_emit = Instant::now();
                let rms = if frames > 0 {
                    (sumsq / frames as f32).sqrt()
                } else {
                    0.0
                };
                let level = meter_level(rms);
                ring.pop_front();
                ring.push_back(level);
                let bars: Vec<f32> = ring.iter().copied().collect();
                on_bars(&bars);
            }
        },
        |e| eprintln!("[whimpr-audio] stream error: {e}"),
        None,
    )?;
    Ok(stream)
}

/// Resample mono `input` from `src_rate` to 16 kHz (what ASR models expect) using
/// linear interpolation. Adequate for speech recognition; a polyphase resampler is
/// a later refinement. Returns `input` unchanged when already at 16 kHz.
pub fn resample_to_16k(input: &[f32], src_rate: u32) -> Vec<f32> {
    const DST: u32 = 16_000;
    if src_rate == DST || src_rate == 0 || input.is_empty() {
        return input.to_vec();
    }
    let ratio = DST as f64 / src_rate as f64;
    let out_len = ((input.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_pos = i as f64 / ratio;
        let idx = src_pos.floor() as usize;
        let frac = (src_pos - idx as f64) as f32;
        let a = input.get(idx).copied().unwrap_or(0.0);
        let b = input.get(idx + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
}

/// Peak-normalize a quiet utterance up to a healthy level before ASR.
///
/// Whisper transcribes low-amplitude audio noticeably worse — softly-spoken words
/// get dropped or truncated rather than mis-heard, which reads as "it ignored me"
/// instead of "it misunderstood me". A built-in mic at arm's length, or a headset
/// that has switched to its HFP profile mid-call, routinely peaks around 0.05.
///
/// Two things keep this from making matters worse. Gain is capped, so a recording
/// of an empty room is not amplified into something the model will hallucinate over
/// — the ceiling is the whole reason this is not a plain divide-by-peak. And audio
/// already at a healthy level is returned untouched, so the normal case is a scan
/// and a move, and nothing that was fine gets touched.
pub fn normalize_for_asr(samples: &mut [f32]) -> f32 {
    /// Above this the recording is already fine; leave it alone.
    const HEALTHY_PEAK: f32 = 0.5;
    /// What a quiet recording is lifted to. Short of 1.0 so the linear resampler's
    /// interpolation between two near-peak samples cannot overshoot into clipping.
    const TARGET_PEAK: f32 = 0.7;
    /// Ceiling on the boost. Past roughly this much, what is being amplified is the
    /// noise floor rather than a voice.
    const MAX_GAIN: f32 = 8.0;
    /// Below this there is no signal to rescue, only room tone.
    const NOISE_FLOOR: f32 = 0.002;

    let peak = samples.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    if peak < NOISE_FLOOR {
        return 1.0; // room tone, not a voice — nothing to rescue
    }
    if peak >= HEALTHY_PEAK {
        return 1.0; // already fine; rescaling would only risk clipping
    }
    let gain = (TARGET_PEAK / peak).min(MAX_GAIN);
    for s in samples.iter_mut() {
        *s *= gain;
    }
    gain
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug this exists for: quiet speech must come out loud enough for Whisper.
    #[test]
    fn normalize_lifts_a_quiet_recording() {
        let mut s = vec![0.15f32, -0.12, 0.02];
        let gain = normalize_for_asr(&mut s);
        assert!(gain > 1.0, "expected a boost, got {gain}");
        let peak = s.iter().fold(0.0f32, |m, &v| m.max(v.abs()));
        assert!((0.65..=0.75).contains(&peak), "peak {peak}");
    }

    /// A very quiet recording is lifted by the full 8x rather than all the way to
    /// the target — 0.05 would need 14x, which is into noise-amplifying territory.
    /// The result is short of `TARGET_PEAK` on purpose; it is still 8x better than
    /// what Whisper was being handed.
    #[test]
    fn normalize_lifts_very_quiet_audio_as_far_as_the_cap_allows() {
        let mut s = vec![0.05f32, -0.04];
        assert_eq!(normalize_for_asr(&mut s), 8.0);
        let peak = s.iter().fold(0.0f32, |m, &v| m.max(v.abs()));
        assert!((0.35..0.5).contains(&peak), "peak {peak}");
    }

    /// A healthy recording is not touched — normalizing it would only add clipping
    /// risk for no benefit.
    #[test]
    fn normalize_leaves_healthy_audio_alone() {
        let mut s = vec![0.8f32, -0.6, 0.1];
        let before = s.clone();
        assert_eq!(normalize_for_asr(&mut s), 1.0);
        assert_eq!(s, before);
    }

    /// Silence must not be amplified into something the model will hallucinate over.
    /// This is the case the gain cap and the noise floor both exist for.
    #[test]
    fn normalize_refuses_to_amplify_room_tone() {
        let mut s = vec![0.0004f32, -0.0003, 0.0001];
        let before = s.clone();
        assert_eq!(normalize_for_asr(&mut s), 1.0);
        assert_eq!(s, before);
    }

    #[test]
    fn normalize_caps_its_gain() {
        // Peak 0.01 would need 70x to reach the target; the cap holds it to 8x.
        let mut s = vec![0.01f32, -0.008];
        assert_eq!(normalize_for_asr(&mut s), 8.0);
    }

    /// Quiet speech used to sit below the pill's idle shimmer (0.12), so speaking
    /// looked exactly like silence. That is the symptom the dB curve fixes.
    #[test]
    fn meter_shows_quiet_speech_above_the_idle_shimmer() {
        let quiet = meter_level(0.005);
        assert!(quiet > 0.15, "quiet speech reads {quiet}, still lost in the shimmer");
        assert!(quiet < 0.4, "quiet speech reads {quiet}, too hot to leave headroom");
    }

    /// A normal voice must not pin the meter, or the waveform stops responding at
    /// exactly the level most speech sits at.
    #[test]
    fn meter_has_headroom_at_a_normal_speaking_level() {
        let normal = meter_level(0.02);
        assert!(normal > 0.4 && normal < 0.8, "normal speech reads {normal}");
        assert!(meter_level(0.02) < meter_level(0.08), "louder must read higher");
    }

    #[test]
    fn meter_bottoms_out_on_silence() {
        assert_eq!(meter_level(0.0), 0.0);
        assert_eq!(meter_level(1.0), 1.0);
    }

    #[test]
    fn resample_48k_to_16k_thirds_the_length() {
        let input = vec![0.0f32; 48_000];
        let out = resample_to_16k(&input, 48_000);
        assert!((out.len() as i64 - 16_000).abs() <= 1);
    }

    #[test]
    fn resample_noop_at_16k() {
        let input = vec![0.1f32, 0.2, 0.3];
        assert_eq!(resample_to_16k(&input, 16_000), input);
    }
}
