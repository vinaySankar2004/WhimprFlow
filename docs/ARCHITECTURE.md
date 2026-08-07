# WhimprFlow — architecture

What the code does today, as of the current commit. If something here disagrees
with the code, the code is right and this file is a bug.

Everything runs on the machine: audio never leaves it, and the transcript only
leaves it if you explicitly pick a cloud cleanup engine.

## The loop

Hold **Fn**, speak, release. Text lands at the cursor.

```
Fn down ─ CGEventTap ─→ state machine ─→ StartCapture ─→ cpal mic (16 kHz mono)
                                      └─→ PlayPing      └─→ RMS ──→ pill waveform
Fn up   ─────────────→ StopCaptureAndFinalize
                            │
                            ├─ whisper.cpp (Metal) ──────────→ raw transcript
                            ├─ dictionary.prefilter(raw, 15) ─→ vocab entries
                            ├─ cleanup provider (local | OpenAI | Anthropic | raw)
                            ├─ gates: reject over-editing ────→ fall back to raw
                            └─ clipboard save → ⌘V → restore → paste
                                                  └─→ autolearn watches for a fix
```

The state machine (`crates/whimpr-core/src/state/`) is a pure reducer:
`step(input) -> Vec<Action>`. It owns hold-to-talk, double-tap-to-lock, Esc
cancel, and the session cap. The shell enacts the actions; it never re-derives
the state. Timing lives in `state/timing.rs`: 200 ms minimum hold, 350 ms
double-tap window, 500 ms cooldown between sessions, 20 min session cap with a
warning at 19.

## Crates

| Crate | Does |
|---|---|
| `whimpr-core` | State machine, cleanup prompts/levels/gates, dictionary, settings, stats. No I/O, no platform code — this is where the tests live. |
| `whimpr-asr` | Whisper via `whisper-rs`, on Metal. Implements the `AsrEngine` trait. |
| `whimpr-audio` | `cpal` mic capture, downmix, resample to 16 kHz, throttled RMS for the waveform. |
| `whimpr-cleanup` | OpenAI + Anthropic providers behind one trait. Keys come from the OS keychain, never a file. |
| `whimpr-llm-worker` | Separate binary running llama.cpp. Separate because llama.cpp's ggml and whisper.cpp's ggml cannot coexist in one process. Speaks one JSON request per line over stdio. |
| `whimpr-ipc` | Length-prefixed JSON wire protocol for a hotkey sidecar. **Built and tested, but not wired in** — the Fn tap currently runs in-process. |
| `whimpr-sidecar` | The sidecar binary for that protocol. Also **not currently used**. |
| `src-tauri` | The app: tray, Hub window, overlay pill, hotkey tap, paste, auto-learn. The macOS-native parts live in `hotkey.rs` (CGEventTap), `paste.rs`, `autolearn.rs`, `appctx.rs`. |
| `ui/` | React + TypeScript. Two Vite entry points: `index.html` (Hub) and `overlay.html` (pill). |

## Cleanup

Four modes (`CleanupMode`): `Raw`, `Local` (default), `OpenAi`, `Anthropic`.
All non-raw modes send the *same* prompt — system message, few-shot turns, then
the transcript — assembled once in `cleanup::build_messages`, so providers can't
drift apart.

Four aggressiveness levels: None, Light (default), Medium, High.

**Gates are the safety net.** An LLM asked to tidy a transcript will sometimes
rewrite it. `cleanup::gates::evaluate` rejects the output and pastes the raw
transcript instead when it sees:

- edit ratio above the level's ceiling
- a must-preserve token vanishing (number, URL, email, code-ish token)
- over-deletion (shrank >40%)
- hallucination (grew beyond punctuation)
- a banned pattern (added greeting/sign-off, or an assistant-style reply)

A wrong-but-clean paste is worse than an untidy-but-faithful one, so the gates
prefer the raw text whenever they are unsure.

## Dictionary

Teach it names and jargon it should always spell a particular way.

`DictionaryStore::prefilter(utterance, 15)` selects candidate entries by
normalized edit distance (≤0.34) against each spoken token *and* each adjacent
token pair — the bigram pass is what catches a name split into two words
("charge bee" → ChargeBee). Selected entries go into `CleanupContext.vocab` and
become a `<CUSTOM_VOCABULARY>` block in the prompt.

Auto-learn (macOS only, and deliberately conservative): after a paste,
`autolearn::watch_correction` watches via the Accessibility API for a one-word
fix and records it.

Prove the whole chain end to end against the real model:

```bash
cargo run -p whimpr-llm-worker --example dictionary_check --release
```

It runs each case twice — with and without the entry — because a pass only means
something if the transcript fails without the dictionary.

## The overlay pill

A transparent, always-on-top window that renders idle / recording / processing.
Three things make it actually visible, each fixing a real failure:

- Anchored to the monitor's **work area**, not its full rect, so it clears the Dock.
- **Window level 25** (NSStatusWindowLevel), above the Dock's 20. Tauri's
  `always_on_top` alone gives NSFloatingWindowLevel (3), which is *below* the Dock.
- Promoted to a **non-activating NSPanel** with `CanJoinAllSpaces` +
  `FullScreenAuxiliary`. A plain NSWindow cannot appear over another app's
  full-screen Space at any level, because it is not on that Space at all.
  Non-activating also means clicking the pill never steals focus from the app
  being dictated into — which the paste path depends on.

Idle renders nothing and sets `ignore_cursor_events`, so it is neither visible
nor in the way. State arrives as `whimpr://flowbar/state` events; mic level
arrives as `whimpr://audio/waveform`.

## Permissions (macOS)

**Accessibility is the one that matters.** Without it the CGEventTap is
frontmost-only, so Fn silently does nothing in every other app, and paste is
disabled. The app polls for it and starts working the moment it is granted, no
relaunch needed.

Microphone is prompted on first record. Input Monitoring is *not* required for a
CGEventTap — it is logged as diagnostics only.

On each launch the app writes `permissions.json` into its support directory. This
exists because `AXIsProcessTrusted()` answers for the *calling* process: run the
same check from a shell and you get the terminal's permissions, not the app's.
Only the app can answer honestly, so it writes the answer down and
`scripts/install-macos.sh` reads it.

## Models

Not committed — they are multi-GB. They live in
`~/Library/Application Support/WhimprFlow/models/`.

Both ladders take the best file present, so dropping a bigger model in is the
whole upgrade procedure. Exact filenames matter — these are the literal strings
the code searches for, in this order:

- **Whisper** (`hotkey.rs`): `ggml-large-v3-turbo.bin` → `ggml-medium.en.bin` →
  `ggml-small.en.bin` → `ggml-base.en.bin`
- **Cleanup** (`local_llm.rs`): `qwen3-4b-instruct-2507-q4_k_m.gguf` →
  `qwen2.5-1.5b-instruct-q4_k_m.gguf`

With no local GGUF, set the cleanup engine to OpenAI in Settings and point the
base URL at any OpenAI-compatible API.

## Build and install

```bash
./dev.sh                    # Vite + the app, hot reload
./scripts/install-macos.sh  # build, install to /Applications, verify permissions
cargo test -p whimpr-core -p whimpr-ipc
```

`dev.sh` runs Tauri from `src-tauri/`, not the repo root: from the root the CLI
resolves `ui/` as the app directory (the only `package.json`), so
`beforeBuildCommand`'s `pnpm --dir ui build` looks for `ui/ui` and fails.

`install-macos.sh` exists because `tauri build` does **not** bundle
`whimpr-llm-worker` (no `externalBin` in `tauri.conf.json`), and
`local_llm::worker_bin_path()` then falls back to a hardcoded path that will not
exist. An app missing the worker starts fine, transcribes fine, and silently
pastes raw uncleaned text. The script copies the worker in, re-signs, and
compares the code signature's designated requirement across the update — if that
changes, macOS treats it as a different app and every permission grant goes
stale.

`scripts/build-macos.sh` is the *distribution* path and refuses anything but a
Developer ID certificate. That guard is deliberate: a build signed with a
development certificate passes every local check and then will not open on
anyone else's Mac.

## macOS only

This is a macOS 14+ / Apple Silicon app and the code says so: no `cfg` branches,
no stub implementations, one path through every function. Whisper and llama.cpp
are both built with Metal unconditionally.

That is a deliberate narrowing — a previous version carried an unverified Windows
layer that doubled the platform surface while never being run. Porting later
means adding the branches back, not maintaining dead ones now.

## Known rough edges

- The Fn tap runs in-process rather than in a sidecar. Less alarming than it
  sounds, and measured rather than assumed: the tap callback only steps the state
  machine, every heavy stage (ASR, cleanup, paste) is dispatched to a spawned
  thread, and `kCGEventTapDisabledByTimeout` is caught and re-enabled on the spot.
  `whimpr-ipc` (protocol, tested) and `whimpr-sidecar` (still a standalone demo,
  does not speak that protocol) exist to move it out. Worth doing if stuck or
  missed Fn presses ever show up in practice; until then it is speculative work
  that would add a second binary needing its own TCC grant, bundling and signing.
- No notarization or installer pipeline. Local install only.
- The Hub's Insights pane and stats are lightly exercised compared to the
  dictation path.
