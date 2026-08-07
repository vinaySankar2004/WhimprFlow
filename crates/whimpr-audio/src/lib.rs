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
/// Perceptual gain applied to raw RMS so speech fills the meter without clipping.
const LEVEL_GAIN: f32 = 14.0;

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
                let level = (rms * LEVEL_GAIN).clamp(0.0, 1.0);
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

#[cfg(test)]
mod tests {
    use super::*;

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
