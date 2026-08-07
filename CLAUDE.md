# CLAUDE.md

Local-first voice dictation: hold Fn, speak, release, clean text lands at the
cursor. Rust + Tauri v2 core, React/TS webviews, Whisper on Metal for ASR, a
llama.cpp worker for cleanup. **macOS only** — there are no `cfg` branches or
platform stubs, and adding one back should be a deliberate decision, not a reflex.

Read `docs/ARCHITECTURE.md` first — it explains how the loop works and *why* the
odd parts are odd. Everything below is the working agreement on top of it.

## Commands

```bash
./dev.sh                                  # Vite + app, hot reload
./scripts/install-macos.sh                # build + install to /Applications + verify permissions
cargo test -p whimpr-core -p whimpr-ipc   # 38 tests, fast, no models needed
cd ui && node_modules/.bin/tsc --noEmit   # typecheck the UI
cargo run -p whimpr-llm-worker --example dictionary_check --release   # dictionary, end to end
```

## Documentation is a source of truth, not a snapshot

`docs/ARCHITECTURE.md` is meant to be trustworthy. Keep it that way:

- **Change behavior → update the doc in the same commit.** Not later.
- `crates/whimpr-core/tests/docs_are_current.rs` enforces the mechanical parts —
  crate list, timing constants, model filenames. Rename a crate or retune a
  constant without touching the doc and the suite goes red. That is the point;
  do not weaken the test to make it pass, fix the doc.
- The test only covers what is mechanically checkable. Prose accuracy is on you.
- If you discover the doc is wrong, fix the doc as part of the work — a stale
  line is a bug, and the next reader has no way to know it lied.

## Traps that have already cost time

These are not hypotheticals; each one bit during development.

- **`AXIsProcessTrusted()` answers for the calling process.** Run a permission
  check from a shell and you get the terminal's permissions, not the app's. The
  app writes `permissions.json` at launch precisely so tooling can read the truth.
  Never conclude "Accessibility is missing" from a shell-launched binary.
- **Without Accessibility the Fn tap is frontmost-only.** Symptom is "the pill
  only shows on the desktop" or "dictation does nothing in other apps" — it looks
  like a UI bug and is not.
- **`tauri build` does not bundle `whimpr-llm-worker`** (no `externalBin`). An app
  missing it starts fine, transcribes fine, and silently pastes *raw uncleaned
  text*. Always install via `scripts/install-macos.sh`.
- **Run Tauri from `src-tauri/`, never the repo root.** From the root the CLI picks
  `ui/` as the app dir (the only `package.json`) and `pnpm --dir ui build`
  resolves to `ui/ui`.
- **Do not lock `LOCAL` from a Tauri command.** `LocalWorker::cleanup` holds that
  mutex for the whole multi-second generation, so the Hub would freeze mid-
  dictation. Status lives in a separate short-lived static for this reason.
- **The overlay's oddities are load-bearing.** Work-area anchoring clears the Dock,
  level 25 beats it in z-order, and the non-activating NSPanel is what allows it
  onto another app's full-screen Space. Remove any one and it silently disappears
  in a specific situation. Non-activating also keeps focus in the app being
  dictated into, which paste depends on.
- **Re-signing can invalidate TCC grants.** The install script compares the
  designated requirement across updates and says so when it changes.

## Conventions

- **`whimpr-core` is pure.** State machine, prompts, gates, dictionary, settings,
  stats — no I/O, no platform code. This is where tests belong. Native code lives
  in `src-tauri`: `hotkey.rs` (CGEventTap), `paste.rs`, `autolearn.rs`, `appctx.rs`.
- **The state machine is a reducer**: `step(input) -> Vec<Action>`. The shell
  enacts actions; it does not re-derive state. Add behavior by emitting an
  `Action`, not by special-casing in the shell.
- Never emit a terminal bar state followed immediately by `Idle` in the same
  `Vec` — both drain microseconds apart and the terminal state never renders. Let
  the shell linger, as `apply_action` does.
- **Gates prefer raw.** When cleanup output looks over-edited, paste the raw
  transcript. A wrong-but-clean paste is worse than an untidy-but-faithful one.
- **API keys go in the OS keychain, never a file.** Audio never leaves the machine;
  only the transcript does, and only in an explicitly chosen cloud mode.
- **Both cleanup providers share one prompt** (`cleanup::build_messages`) so they
  cannot drift.
- Adding a field to `StatusReport` means updating `Status` **and** `EMPTY_STATUS`
  in `ui/src/hub/api.ts`. Adding one to `Settings` needs `#[serde(default)]` or
  `Settings::load` silently resets every saved setting.

## Verifying

The unit tests do not touch models, audio, or the GPU, so passing them is not
evidence the app works. For anything in the dictation path, run the real thing —
`./scripts/install-macos.sh` then hold Fn — or the `dictionary_check` example,
which drives the production chain against the real model.

Anything involving what is *visible on screen* (the pill, Spaces, full-screen
behavior) cannot be confirmed from a shell. Query the window server
(`CGWindowListCopyWindowInfo` reports layer and on-screen state) or ask the user.
Do not claim a visual fix works without one of those.
