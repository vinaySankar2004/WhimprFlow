//! What the mic actually does right now — which devices exist, what each one
//! advertises, and whether capture works this second.
//!
//! Run it while on a call. That is the case it exists for: a Bluetooth headset
//! switches to its HFP profile mid-call and stops offering the config it offered a
//! moment ago, and the question "is the mic unavailable, or did we just ask wrong"
//! is otherwise unanswerable from the outside.
//!
//!   cargo run -p whimpr-audio --example mic_check
//!
//! Caveat: run from a shell, this uses the TERMINAL's microphone permission, not
//! WhimprFlow's. A silent result here means "the terminal cannot hear", not "the app
//! cannot hear" — grant the terminal Microphone access and rerun before believing it.

use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait};

fn main() -> anyhow::Result<()> {
    println!("Input devices:");
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_else(|| "<none>".into());

    for device in host.input_devices()? {
        let name = device.name().unwrap_or_else(|_| "<unnamed>".into());
        let marker = if name == default_name { " (default)" } else { "" };
        println!("\n  {name}{marker}");
        match device.default_input_config() {
            Ok(c) => println!(
                "    default: {} ch, {} Hz, {}",
                c.channels(),
                c.sample_rate().0,
                c.sample_format()
            ),
            Err(e) => println!("    default: unavailable — {e}"),
        }
        match device.supported_input_configs() {
            Ok(ranges) => {
                for r in ranges {
                    println!(
                        "    also:    {} ch, {}–{} Hz, {}",
                        r.channels(),
                        r.min_sample_rate().0,
                        r.max_sample_rate().0,
                        r.sample_format()
                    );
                }
            }
            Err(e) => println!("    also:    unavailable — {e}"),
        }
    }

    println!("\nCapturing for 2 s through the production path…");
    let handle = whimpr_audio::start(|_: &[f32]| {})?;
    std::thread::sleep(Duration::from_secs(2));
    let Some(res) = handle.stop() else {
        anyhow::bail!("capture returned nothing");
    };

    let peak = res.samples.iter().fold(0f32, |m, &s| m.max(s.abs()));
    println!(
        "\n  device:  {}\n  got:     {} samples @ {} Hz (~{:.2}s)\n  peak:    {:.4}",
        res.device,
        res.samples.len(),
        res.sample_rate,
        res.duration_secs(),
        peak
    );
    if peak < 0.005 {
        println!(
            "\n  Silent. Either nothing was said, or this terminal has no Microphone\n  \
             grant — check System Settings → Privacy & Security → Microphone."
        );
    } else {
        println!("\n  Audio is arriving.");
    }
    Ok(())
}
