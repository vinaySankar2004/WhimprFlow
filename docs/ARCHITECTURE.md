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
                            ├─ whisper again, vocab as initial_prompt   ┐ only when
                            ├─ accept_prompted: else keep pass 1        ┘ vocab hit
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

- novelty ratio above the level's ceiling (output words that were never spoken)
- a must-preserve token vanishing (number, URL, email, code-ish token)
- over-deletion (shrank >55%)
- hallucination (grew beyond punctuation)
- a banned pattern (added greeting/sign-off, or an assistant-style reply)

A wrong-but-clean paste is worse than an untidy-but-faithful one, so the gates
prefer the raw text whenever they are unsure.

`evaluate` takes the **utterance's vocab** — the same entries that went into the
prompt — and treats those spellings as expected rather than novel. This is not a
convenience parameter: a dictionary correction replaces a mis-heard token with a
spelling that by definition is not in the raw transcript, so without it every
custom-vocabulary fix reads as the model inventing a word. On a short dictation
("hey monvi" → "Hey Manvi.") that is a 0.5 novelty ratio against a 0.34 ceiling, so
the gate threw the fix away and pasted the mishear — the dictionary appeared to
work on long sentences and silently do nothing on short ones. Only the authorized
spellings are exempted, so a rewrite that happens to mention a dictionary name is
still caught on all its other novel words, and the entity and length gates are
untouched.

## Dictionary

Teach it names and jargon it should always spell a particular way.

`DictionaryStore::prefilter(utterance, 15)` selects candidate entries by normalized
edit distance against each spoken token *and* each adjacent token pair. Selected
entries go into `CleanupContext.vocab`, become a `<CUSTOM_VOCABULARY>` block in the
prompt, and are handed to the gates so the correction survives (see *Cleanup*).

**The two passes have different thresholds, and that asymmetry is the point.**

| Pass | Ceiling | Why |
|---|---|---|
| single token | 0.30 | a real word the speaker said |
| glued adjacent pair | 0.15 | a token *we* invented, so it must be nearly exact |

The bigram pass exists to catch a name recognition split in half — "charge bee"
glues to exactly "chargebee", distance 0. Given a real word's slack it instead
becomes a noise generator: "charge the" glues to "chargethe", two letters from
"ChargeBee", and the model duly rewrote "did you charge the battery" as "did you
ChargeBee the battery". Both thresholds tightened for the same reason — at 0.34 a
single wrong letter pulls in any three-letter entry ("we" → "Wei"). Nothing real was
lost: a listed mishear matches itself at distance 0, and a split name matches its
bigram at distance 0. Multi-word entries and mishears are compared with whitespace
stripped, since the tokens they are matched against never contain a space.

The vocabulary block also has to say when *not* to substitute. Given only "replace
close mistakes", a small model will happily put a product name where an ordinary
verb was; `assemble_user_message` spells out that entries are proper nouns and that
a word making sense as spoken should be left alone.

### Every utterance gets a second of silence appended

whisper.cpp will not begin a new segment within a second of the end of the audio
(`if (seek + 100 >= seek_end) break;`) and drops its prompt for short trailing
segments. A recording that stops the instant the speaker does can therefore lose its
final words — and push-to-talk produces exactly that shape, because the key comes up
on the last syllable. Upstream warns about this and recommends padding; `whimpr-asr`
appends `TAIL_PAD_SAMPLES` of zeros before every call.

Measured, not assumed: on a 5 s clip `large-v3-turbo` returned "…reviewed by Manvi, at
Charge" and, with a second of silence appended, the whole sentence. Bigger models
segment more finely and lose more this way, so this is a prerequisite for moving up
the model ladder rather than a nicety.

### Recognition is asked twice

A mis-heard name is best fixed where it was mis-heard, not repaired downstream — and
downstream repair does nothing at all in `Raw` mode or at cleanup level `None`, where
no model ever sees the transcript. Whisper takes an `initial_prompt` that conditions
decoding, so the dictionary goes in there too.

Prompting is not free: a word Whisper is primed for is a word it may emit from audio
that never contained it (whisper.cpp guards against the same thing internally, and
says so in a comment). So the shell transcribes **twice** — unprompted, then prompted
with a glossary of the entries `prefilter` matched — and `asr::prompt::accept_prompted`
picks between them. The prompted transcript is kept only when every word it introduced
is an authorized spelling *and* it introduced no more of them than it displaced. That
second clause is the load-bearing one: prompted with a glossary and handed near
silence, Whisper echoes the glossary back, and every echoed word is "authorized" — a
check that only asked whether the new words were in the dictionary would wave it
straight through.

"No more than it displaced" carries a slack of one, for a name Whisper missed
outright — and that slack applies **only to a word the unprompted pass did not have
at all**. An extra copy of a word already in the transcript is never a missed name;
it is the glossary echoing. That distinction is not academic: with a one-word
dictionary the echo costs exactly one addition, so it fits inside a flat slack.
Observed, and now a test — *"Hey, how's it going? My name is Vinayak."* came back as
*"Vinayak. Hey, how's it going? My name is Vinayak."* Whisper emits prompt echoes at
the **start**, so the symptom a user reports is the last word of the utterance turning
up as the first.

The second pass runs only when the pre-filter matched something, so the overwhelming
majority of dictations take the single-pass path unchanged. `set_no_context(true)` does
not cancel the prompt; whisper.cpp clears `prompt_past` first and then rotates the
initial prompt to the front of it (checked in the vendored source, not assumed).

Only the *correct* spellings go into the prompt, never the mishears — those are what
recognition is being steered away from.

### Auto-learn

After a paste, `autolearn::watch_correction` polls the focused element via the
Accessibility API for 20 s, taking the first clean one-word substitution it sees.
Polling rather than one snapshot at the end: a single late look is *worse* than an
early one for anyone who fixes the word and keeps typing, because by then the field has
moved on and the diff is no longer the clean swap auto-learn will accept.

`detect_correction` is deliberately hard to satisfy — exactly one word out and one in,
both ≥3 characters and alphabetic, neither on a ~70-word common list, the new one
Titlecase, and normalized distance in (0, 0.6]. A false positive poisons the dictionary
into mis-correcting you forever, so the bar is set where a miss is the cheaper mistake.

What gets recorded as the mishear is **what recognition wrote**, not what auto-learn
observed. The observed form comes from the *pasted* text, which is post-cleanup, so it
may be a spelling Whisper never produces; `dictionary::ground_truth_mishear` finds the
token in the raw transcript that the correction replaced and stores that alongside it.
This is the one place the raw transcript earns its keep in the dictation path rather
than in statistics.

Prove the whole chain end to end against the real model:

```bash
cargo run -p whimpr-llm-worker --example dictionary_check --release
cargo run -p whimpr-llm-worker --example dictionary_check --release -- --audit
```

Three things the harness insists on, each because leaving it out lets a green run
mean nothing:

- **It asserts on the pasted text, not the model's reply** — running `post_process`
  and the gates, because a cleanup the gates reject never reaches the cursor. The
  version that stopped at the model's reply could not see the gate bug above.
- **Every case runs twice, with and without the entry.** A model that already knew
  the spelling would otherwise look like a working dictionary; that outcome is
  reported as `PASS (weak)` rather than banked.
- **Negative cases.** Precision is the whole reason `prefilter` exists, so some
  cases assert the dictionary stays *out* of the way. Each case also loads ~36
  unrelated entries, because a one-entry store is not a dictionary.

Sampling in the worker is greedy, so re-running a case returns the same tokens —
repeats buy nothing and the cases vary phrasing instead. `--audit` skips the model
and reports on your real `dictionary.json`: which entries have no mishears listed
and how far off recognition can be before they stop being selected.

That harness starts from a *text* transcript, so it cannot see the recognition stage
at all. For the two-pass path, `whimpr-asr`'s example runs it against real audio:

```bash
cargo run -p whimpr-asr --example transcribe --release -- <model.bin> <clip.wav> Manvi,ChargeBee
```

It prints the unprompted transcript, the prompted one, and which would be kept.

## The Hub window

The Hub is the ordinary app window — settings, history, dictionary. Its red button
**hides** it rather than closing it: `CloseRequested` is intercepted, the close is
prevented, and the window is hidden. Letting the close through would destroy the
window while the app kept running (the overlay holds the process open), and a
destroyed window is unrecoverable — `get_webview_window("main")` returns `None`
from then on, so the tray's *Open WhimprFlow* item and the Dock icon would both
silently do nothing.

Two paths bring it back, and both go through `show_hub`: the tray menu item, and
`RunEvent::Reopen`, which is what a Dock click raises. `Reopen`'s
`has_visible_windows` flag is deliberately ignored — the overlay is a window and
counts as visible while the pill is up, so the flag says "true" with the Hub
nowhere on screen. `show_hub` unminimizes before showing and shows before
focusing, because `set_focus` is a no-op on a hidden or minimized window.

## The overlay pill

A transparent, always-on-top window that renders idle / recording / processing.
Five things make it actually visible, each fixing a real failure — and four of the
five produce the *same* symptom, "the pill only shows on the desktop":

- Anchored to the monitor's **work area**, not its full rect, so it clears the Dock.
- **Window level 25** (NSStatusWindowLevel), above the Dock's 20. Tauri's
  `always_on_top` alone gives NSFloatingWindowLevel (3), which is *below* the Dock.
- Promoted to a **non-activating NSPanel** with `CanJoinAllSpaces` +
  `FullScreenAuxiliary`. A plain NSWindow cannot appear over another app's
  full-screen Space at any level, because it is not on that Space at all.
  Non-activating also means clicking the pill never steals focus from the app
  being dictated into — which the paste path depends on.
- **`hidesOnDeactivate` forced off**, which is the bill that comes with being a
  panel. AppKit hides panels belonging to the inactive app, and `NSPanel` defaults
  the flag to true where `NSWindow` defaults it to false — so promoting the window
  is what breaks it. Correct for an inspector panel, fatal here: while dictating,
  WhimprFlow is *never* the active app. Left at the default the pill appears only
  when WhimprFlow itself is frontmost, which presents as "it only shows on the
  desktop" and is easily mistaken for the missing-Accessibility symptom below.

- **Ordered in LAST**, after the promotion and the flags. This is the one that is
  pure sequencing and therefore the easiest to reintroduce: macOS assigns a window
  to a Space when it is **first ordered in**, and setting `collectionBehavior`
  afterwards does not migrate it. A window built visible on the desktop is a
  desktop window for the rest of the process's life — `CanJoinAllSpaces` reads back
  correctly and means nothing. So the overlay is built with `visible(false)` and
  ordered in by the `orderFrontRegardless` at the end of `raise_overlay_level`,
  once it is already a panel with the right behavior.

None of this can be confirmed by reading the code back, because every one of these
failures leaves the flags looking right. Ask the window server instead:

- `CGWindowListCopyWindowInfo` gives each window's layer and `kCGWindowIsOnscreen`.
- `CGSCopySpacesForWindows` (private, diagnostic only) gives the Spaces a window is
  actually *on*, which is the only way to see the ordering bug. The pill's window
  should come back on every Space; `[1]` alone means it is pinned to the desktop.
  `CGSCopyManagedDisplaySpaces` lists the Spaces, where `type=4` is a full-screen
  app's own Space — worth checking before assuming a window is merely occluded.

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

## Where the data lives

Everything persistent is four flat JSON files in one directory
(`~/Library/Application Support/WhimprFlow/`, `hotkey::support_dir`). No database,
no migrations.

| File | Holds |
|---|---|
| `settings.json` | `Settings` — cleanup mode/level, trigger mode, model names, privacy |
| `dictionary.json` | `DictionaryStore` — manual and ✨ auto-learned entries |
| `stats.json` | `StatsStore` — one record per dictation, and the text |
| `permissions.json` | written each launch, read by the installer (see *Permissions*) |
| `models/` | the multi-GB weights, not committed (see *Models*) |

Every store follows the same shape: `load` returns `Default` on a missing or
unparseable file, `save` writes pretty JSON through on each mutation. That leniency
is why a new `Settings` or `SessionRecord` field needs `#[serde(default)]` — without
it one unknown shape fails the whole parse and silently resets everything saved.

Two things deliberately live elsewhere. **API keys** go in the OS keychain (service
`com.whimpr.whimprflow`), never a file. **Audio** is never persisted at all: samples
exist in memory from `StartCapture` until transcription and are then dropped.

### History and transcripts

`SessionRecord` keeps the cleaned text *and* the raw pre-cleanup transcript, because
everything interesting about how someone speaks is in the words cleanup removes —
fillers, stutters, self-corrections. The cleaned text alone cannot answer any of it.
`Settings::store_raw_transcripts` turns the raw copy off, and **Clear transcripts**
in Settings empties the text of every record while keeping the counts: words, WPM
and streak are derived from the numeric fields, so the control erases what was said
without resetting what was earned.

`StatsStore::query` does the Home list's search, date filtering and paging in Rust
rather than the webview. The log only ever grows: shipping every dictation ever made
so the UI can show ten of them gets slower every day, and a client-side filter over
a truncated list silently fails to find older matches. `HistoryQuery` carries a
`since_unix` the UI computes from its own clock, so day boundaries follow the user's
timezone without any timezone logic in core; `HistoryPage` returns the matching
total alongside the page so the UI can render "11–20 of 347".

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

- **Whisper** (`hotkey.rs`): `ggml-large-v3-turbo.bin` →
  `ggml-large-v3-turbo-q5_0.bin` → `ggml-medium.en.bin` → `ggml-small.en.bin` →
  `ggml-base.en.bin`
- **Cleanup** (`local_llm.rs`): `qwen3-4b-instruct-2507-q4_k_m.gguf` →
  `qwen2.5-1.5b-instruct-q4_k_m.gguf`

**Take the q5_0 build of turbo unless you have memory to burn.** It is second in the
ladder on paper, and the one to actually install: measured against the f16 build on
the same clips it was indistinguishable in accuracy — better, on the hardest surname —
at the same speed, for **716 MB** resident instead of **1755 MB**.

That gigabyte is paid around the clock. Whisper's weights live in a Metal buffer that
stays fully resident for as long as the app runs, whereas the llama worker mmaps its
GGUF and pages it in on demand — which is why the 2.5 GB cleanup model shows ~63 MB of
footprint while the smaller Whisper model shows all of its own. Neither costs CPU when
idle; the cost is memory, and it is continuous.

Recognition latency on an M-series machine, 2.8 s of audio: ~185 ms to load at startup,
~1.1 s per pass. A dictation the dictionary touches runs two passes. For comparison
`ggml-base.en.bin` transcribes in ~120 ms and mis-hears ordinary names ("Manvy" for
"Manvi"), which is the trade the ladder exists to let you make.

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
  dictation path. Its **Your Voice** tab is still a placeholder: the raw transcript
  it needs is now being stored (see *Where the data lives*), but nothing computes
  filler rates, self-correction frequency or pace from it yet.
- The harness covers the dictionary against *text* transcripts, so its mishears are
  written by hand rather than produced by Whisper. Recorded audio fixtures driven
  through the real ASR would close that gap, at the cost of committing audio.
