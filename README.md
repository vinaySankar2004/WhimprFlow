# WhimprFlow

> **This is Vinayak Sankaranarayanan's own continuation of WhimprFlow.** It began as
> the MIT-licensed proof of concept linked in [LICENSE](LICENSE), and is now developed
> independently here — this repository is where the project goes from here, and new
> features land in it directly. The original license and copyright are retained, as MIT
> requires.

A **local-first voice dictation app for macOS** — hold **Fn**, speak, release, and clean
text lands wherever your cursor is (or press **Fn** once to start and again to stop, if
you'd rather not hold a key). Speech is transcribed on-device with Whisper on the
GPU, then tidied up (fillers removed, spoken self-corrections resolved, punctuation,
lists and newlines) by a local LLM. Nothing leaves the machine unless you explicitly
choose a cloud cleanup engine — and even then, only the transcript, never the audio.

**macOS 14+ (Apple Silicon)**, build from source. There's no signed installer yet, so
`git clone` plus the steps below is how you run it.

## What's in it

- **On-device ASR** — Whisper via `whisper.cpp` on Metal. The best model present wins,
  so upgrading is just dropping a bigger file in the models folder.
- **Local LLM cleanup** — Qwen3-4B-Instruct via `llama.cpp`, in a separate worker
  process. Deterministic gates guard against over-editing and fall back to the raw
  transcript when the model gets creative.
- **Optional cloud cleanup** — OpenAI or Anthropic behind one trait, sharing the exact
  same prompt as the local path. Keys live in the macOS Keychain, **never in a file**.
- **Floating pill** — a non-activating panel showing idle / recording / processing that
  follows you across Spaces, including other apps' full-screen ones. ■ stops and pastes,
  ✕ or **Esc** discards — and cancelling still works while it's transcribing, before
  anything is pasted. Esc is watched only while a dictation is live, never otherwise.
- **Hold or toggle** — hold Fn while you speak, or switch to press-to-start /
  press-to-stop under Settings → Dictation Key.
- **Personal dictionary + auto-learn** — teach it names and jargon. A mis-hearing you
  list is applied verbatim rather than left to the cleanup model's judgement, so it
  lands even when the mis-heard spelling looks like a perfectly good name. A post-paste
  Accessibility observer notices one-word corrections and learns them.
- **Usage stats and history** — words, WPM, day streak, time saved, 7-day activity, plus
  a searchable, paged log of past dictations. All on this machine, and one button
  erases the text of every dictation while keeping the counts.

## Layout

```
crates/
  whimpr-core/       state machine, cleanup (prompts/gates/levels), dictionary, stats
  whimpr-asr/        Whisper ASR (Metal)
  whimpr-audio/      mic capture + resampling
  whimpr-cleanup/    OpenAI / Anthropic cloud providers
  whimpr-llm-worker/ local llama.cpp cleanup worker (separate process)
  whimpr-ipc/        sidecar wire protocol (built, not yet wired in)
  whimpr-sidecar/    out-of-process hotkey host (built, not yet wired in)
src-tauri/           Tauri shell: hotkey, paste, auto-learn, overlay, tray
ui/                  React Hub + overlay pill
docs/ARCHITECTURE.md how it actually works, and why the odd parts are odd
```

## Build

Requires Rust (stable), Node + pnpm, CMake, and the Xcode command-line tools.

```bash
cd ui && pnpm install && cd ..
./dev.sh                    # dev: Vite + the app, hot reload
./scripts/install-macos.sh  # build, install to /Applications, verify permissions
```

Use `install-macos.sh` rather than `tauri build` directly — it bundles the LLM worker,
which the Tauri bundler does not. An app missing the worker starts fine, transcribes
fine, and silently pastes raw uncleaned text.

## Models

Not committed — they're multi-GB. Put them in
`~/Library/Application Support/WhimprFlow/models/`:

- **Whisper** — `ggml-large-v3-turbo-q5_0.bin` (547 MB) from
  [huggingface.co/ggerganov/whisper.cpp](https://huggingface.co/ggerganov/whisper.cpp).
  Smaller models work and are far lighter — `ggml-base.en.bin` is 141 MB — but they
  mis-hear ordinary names, which is most of what a personal dictionary then has to
  repair.
- **Cleanup** — a Qwen GGUF, e.g. `qwen3-4b-instruct-2507-q4_k_m.gguf`

No local cleanup model? Set Cleanup Engine to **OpenAI** in Settings and point the base
URL at any OpenAI-compatible API — `https://openrouter.ai/api/v1` for
[OpenRouter](https://openrouter.ai), with that key in the "OpenAI API key" field.

Exact filenames and the full search order are in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Permissions

Grant **Accessibility** to WhimprFlow. Without it the Fn tap is limited to whenever
WhimprFlow itself is frontmost, so dictation silently does nothing in every other app.
The microphone is prompted on first use.

One non-permission setting matters just as much: macOS gives the 🌐/Fn key an action of
its own — usually the emoji picker — and it fires on top of dictation. Set **System
Settings → Keyboard → "Press 🌐 key to" → Do Nothing**; the emoji picker stays on
⌃⌘Space. The app checks this and says so in setup and in Settings → Permissions.

## Notes

- **Not affiliated with, endorsed by, or connected to Wispr Flow or any other product.**
  An independent, from-scratch implementation with its own name, branding, and code.
- **Still rough in places.** No notarization or release pipeline; error handling is thin.
  Known rough edges are listed at the end of [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).
- **Privacy.** ASR and default cleanup run on-device. Cloud cleanup is opt-in and sends
  only the transcript. API keys never touch disk in plaintext.

## License

MIT — see [LICENSE](LICENSE).
