# WhimprFlow — architecture

What the code does today, as of the current commit. If something here disagrees
with the code, the code is right and this file is a bug.

Everything runs on the machine: audio never leaves it, and the transcript only
leaves it if you explicitly pick a cloud cleanup engine.

## The loop

Hold **Fn**, speak, release. Text lands at the cursor. (Or press once to start and
again to stop — see *The dictation key* below.)

```
Fn down ─ CGEventTap ─→ state machine ─→ StartCapture ─→ cpal mic (16 kHz mono)
                                      └─→ PlayPing      └─→ RMS ──→ pill waveform
Fn up / 2nd press / ■ → StopCaptureAndFinalize     ✕ → DiscardCapture (nothing pastes)
                            │
                            ├─ whisper.cpp (Metal) ──────────→ raw transcript
                            ├─ dictionary.prefilter(raw, 15) ─→ vocab entries
                            ├─ cleanup provider (local | OpenAI | Anthropic | raw)
                            ├─ gates: reject over-editing ────→ fall back to raw
                            └─ clipboard save → ⌘V → restore → paste
                                                  └─→ autolearn watches for a fix
```

The state machine (`crates/whimpr-core/src/state/`) is a pure reducer:
`step(input) -> Vec<Action>`. It owns hold-to-talk, double-tap-to-lock, explicit
stop/cancel, and the session cap. The shell enacts the actions; it never
re-derives the state. Timing lives in `state/timing.rs`: 200 ms minimum hold, 350 ms
double-tap window, 500 ms cooldown between sessions, 20 min session cap with a
warning at 19.

## The dictation key

`Settings::trigger_mode` picks how Fn starts and stops a session:

| Mode | Fn down | Fn up |
|---|---|---|
| `Hold` (default) | starts a push-to-talk session | finalizes (or, under the 200 ms minimum, arms double-tap-to-lock) |
| `Toggle` | starts a locked session, or ends the one running | ignored |

This lives entirely in the shell. `hotkey.rs` reports a press as the
`PushToTalk` binding in hold mode and the `HandsFree` binding in toggle mode; the
state machine already knew both, so `Toggle` reuses the exact locked-session path
that double-tap-to-lock drives, and the reducer has no idea a setting exists. The
mode is mirrored into an atomic (`TOGGLE_TRIGGER`) rather than read from
`SETTINGS`, because the tap callback must not allocate or block.

The key release is always reported, in both modes. It is a no-op in every state
toggle mode can produce, and sending it unconditionally means flipping the
setting mid-press still ends a hold-mode session instead of leaving it recording
until the cap.

Hold mode keeps its own hands-free path: tap Fn (under 200 ms), then press again
within 350 ms, and the session locks until the next press. Toggle mode has no
minimum hold — a press of any length starts recording.

### Esc cancels, and the tap that sees it

Esc discards a live dictation. Seeing it needs a key-down subscription — i.e.
every keystroke in every app — which is not a surface this app should carry while
it is doing nothing. So Esc gets its **own** tap, created at launch and left
disabled, switched on only while a dictation is live and off again the moment one
ends (same predicate as the pill's controls, in `emit_bar`). WhimprFlow has no
live keystroke tap at all except during the seconds you are dictating.

That is also what makes it affordable for this to be the app's one **consuming**
tap: it returns null for the Esc it acts on, so cancelling a dictation does not
also dismiss a dialog or clear a draft in the app behind the pill. Every other
key it sees is passed straight through, and the Fn tap stays listen-only.

Failing to create it costs Esc-to-cancel and nothing else, so it is logged and
skipped rather than treated as fatal.

### macOS has its own idea about Fn

The 🌐/Fn key carries a system action — on Apple keyboards, usually the emoji
picker — and it fires *in addition* to dictation, because our tap is listen-only.
It is not a bug to be fixed in code: a consuming tap would have to swallow the Fn
flag change, taking Fn+F1–F12, Fn+arrows and Fn+Delete with it. The fix is
System Settings → Keyboard → "Press 🌐 key to" → **Do Nothing** (the emoji picker
stays on ⌃⌘Space).

The app reads that setting so it can point at it, and only nags when there is
something to nag about: `src-tauri/src/fnkey.rs`, surfaced as
`StatusReport::fn_key_action`, shown as a step in the setup wizard and a row in
Settings → Permissions. It lives in the **`com.apple.HIToolbox`** domain under
`AppleFnUsageType` — *not* NSGlobalDomain, where the name suggests and where a
check returns "does not exist" on a machine that has it set. An absent key is
also not the same as `0`/Do Nothing; it means the macOS default, which is the
emoji picker.

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
| `src-tauri` | The app: tray, Hub window, overlay pill, hotkey tap, paste, auto-learn. The macOS-native parts live in `hotkey.rs` (CGEventTap), `paste.rs`, `autolearn.rs`, `appctx.rs`, `fnkey.rs`. |
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

**The two controls are live.** ■ (`stop_dictation`) ends the recording and pastes
what was said; ✕ (`cancel_dictation`) throws the dictation away, as does **Esc**.
All three feed `TriggerToken::Stop` / `Cancel` into the machine like any other
input — the pill is a view, not a second source of truth.

One predicate in `emit_bar` decides two things: `recording | locked |
transcribing` are the states where a dictation is live, so those are the states
where `ignore_cursor_events` is off (the pill's controls are clickable) *and*
where the Esc tap runs. The ✕ stays on the pill while the pipeline runs, because
that is most of the time a person spends wanting to cancel. Cancelling then is more than
stopping the mic — the pipeline thread already holds the audio. Enacting
`DiscardCapture` raises a high-water mark of cancelled session ids
(`CANCELLED_SESSION`), and the thread checks it before transcribing, before
cleanup, and before the paste. A high-water mark rather than the single cancelled
id, because a cancelled session's cleanup can still be running when the next
dictation starts, and anything that cleared the flag would let the abandoned one
paste after all. Best effort by construction: once the paste has been posted
there is nothing left to call off.

## Permissions (macOS)

**Accessibility is the one that matters.** Without it the CGEventTap is
frontmost-only, so Fn silently does nothing in every other app, and paste is
disabled. The app polls for it and starts working the moment it is granted, no
relaunch needed.

Microphone is prompted on first record. Input Monitoring is *not* required for a
CGEventTap — it is logged as diagnostics only.

The Hub's Permissions card carries one row that is not a permission at all: the
Fn key action (see *The dictation key*). It sits there because it is the other
macOS setting that decides whether pressing Fn does what the user expects.

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
