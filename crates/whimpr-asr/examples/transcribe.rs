//! Transcribe a 16 kHz mono WAV with the local whisper engine.
//!
//! Usage:
//!   cargo run -p whimpr-asr --example transcribe -- <model.bin> <audio.wav>
//!   cargo run -p whimpr-asr --example transcribe -- <model.bin> <audio.wav> Manvi,ChargeBee
//!
//! With a comma-separated glossary it runs the app's real two-pass path — unprompted,
//! then prompted with those spellings — and prints which one would be kept and why.
//! This is the only way to exercise dictionary biasing against real audio; the
//! `dictionary_check` harness in whimpr-llm-worker starts from a text transcript and
//! so cannot see the recognition stage at all.

use std::path::Path;

use whimpr_core::asr::prompt::{accept_prompted, build_initial_prompt};
use whimpr_core::AsrEngine;
use whimpr_core::VocabEntry;
use whimpr_asr::WhisperEngine;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: transcribe <model.bin> <audio.wav> [Word,Word,...]");
        std::process::exit(2);
    }
    let model = &args[1];
    let wav = &args[2];
    let vocab: Vec<VocabEntry> = args
        .get(3)
        .map(|s| {
            s.split(',')
                .map(|w| w.trim())
                .filter(|w| !w.is_empty())
                .map(|w| VocabEntry { correct: w.to_string(), mishears: Vec::new() })
                .collect()
        })
        .unwrap_or_default();

    let mut reader = hound::WavReader::open(wav)?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    // Read i16 PCM and downmix to mono f32 in [-1, 1].
    let raw: Vec<i16> = reader.samples::<i16>().collect::<Result<_, _>>()?;
    let mut mono: Vec<f32> = Vec::with_capacity(raw.len() / channels);
    for frame in raw.chunks(channels) {
        let sum: i32 = frame.iter().map(|s| *s as i32).sum();
        mono.push(sum as f32 / channels as f32 / 32768.0);
    }
    let pcm = whimpr_audio_resample(&mono, spec.sample_rate);

    // Load and inference are timed apart because they are paid at different moments:
    // the model is loaded once at app start, while every dictation pays the transcribe
    // cost — twice over, when the dictionary triggers the prompted second pass.
    let t0 = std::time::Instant::now();
    let engine = WhisperEngine::load(Path::new(model))?;
    let load_ms = t0.elapsed().as_millis();
    let audio_secs = pcm.len() as f32 / 16_000.0;

    let t1 = std::time::Instant::now();
    let unprompted = engine.transcribe(&pcm, None)?.text;
    let pass1_ms = t1.elapsed().as_millis();
    println!("audio:      {audio_secs:.1}s   load {load_ms} ms   pass 1 {pass1_ms} ms");
    println!("unprompted: {unprompted}");

    let Some(prompt) = build_initial_prompt(&vocab) else {
        return Ok(());
    };
    println!("prompt:     {prompt}");
    let t2 = std::time::Instant::now();
    let prompted = engine.transcribe(&pcm, Some(&prompt))?.text;
    println!("            pass 2 {} ms", t2.elapsed().as_millis());
    println!("prompted:   {prompted}");

    if accept_prompted(&unprompted, &prompted, &vocab) {
        println!("\nKEPT the prompted transcript.");
        if prompted.trim() == unprompted.trim() {
            println!("(identical — the glossary changed nothing here)");
        }
    } else {
        println!("\nREJECTED — it changed more than the dictionary allows; unprompted stands.");
    }
    Ok(())
}

/// Minimal inline 16 kHz resample so this example needn't depend on whimpr-audio.
fn whimpr_audio_resample(input: &[f32], src_rate: u32) -> Vec<f32> {
    const DST: u32 = 16_000;
    if src_rate == DST || src_rate == 0 || input.is_empty() {
        return input.to_vec();
    }
    let ratio = DST as f64 / src_rate as f64;
    let out_len = ((input.len() as f64) * ratio).round() as usize;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 / ratio;
            let idx = pos.floor() as usize;
            let frac = (pos - idx as f64) as f32;
            let a = input.get(idx).copied().unwrap_or(0.0);
            let b = input.get(idx + 1).copied().unwrap_or(a);
            a + (b - a) * frac
        })
        .collect()
}
