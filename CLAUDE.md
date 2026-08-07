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
cargo test -p whimpr-core -p whimpr-ipc   # 84 tests, fast, no models needed
cd ui && node_modules/.bin/tsc --noEmit   # typecheck the UI
cargo run -p whimpr-llm-worker --example dictionary_check --release            # dictionary, end to end
cargo run -p whimpr-llm-worker --example dictionary_check --release -- --audit # your own dictionary, no model
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
  level 25 beats it in z-order, the non-activating NSPanel is what allows it onto
  another app's full-screen Space, and `hidesOnDeactivate` must be forced off
  because NSPanel defaults it to true — the pill would then show only while
  WhimprFlow is frontmost, i.e. never, since dictating means being in another app.
  Remove any one and it silently disappears in a specific situation. Non-activating
  also keeps focus in the app being dictated into, which paste depends on.
- **The overlay must be ordered in AFTER it is a panel with its collection behavior
  set.** macOS assigns a window to a Space on its FIRST order-in and never migrates
  it; set `CanJoinAllSpaces` afterwards and the flag reads back correct while the
  window stays pinned to the Space it was born on. That is why it is built
  `visible(false)` and ordered in by `raise_overlay_level`. Flip it back to
  `visible(true)` and the pill works perfectly on the desktop and is absent from
  every full-screen app — which is where dictation actually happens.
- **"The pill only shows on the desktop" has several unrelated causes** — missing
  Accessibility (below), the panel hiding on deactivate, and the Space-assignment
  order above — and they are indistinguishable by reading the code, because all of
  them leave the flags looking right. Do not guess; each wrong guess costs a build
  and install. Ask the window server: `CGWindowListCopyWindowInfo` for
  `kCGWindowIsOnscreen`, and `CGSCopySpacesForWindows` for the Spaces the window is
  really on (`[1]` alone = pinned to the desktop). `CGSCopyManagedDisplaySpaces`
  says whether the current Space is a full-screen app's (`type=4`) — a maximized
  window and a full-screen one look alike in a screenshot and behave nothing alike.
- **macOS runs its own action on the Fn key**, usually the emoji picker, on top of
  dictation. The setting is `AppleFnUsageType` in the **`com.apple.HIToolbox`**
  domain — reading it from NSGlobalDomain (`defaults read -g`) says "does not
  exist" on a machine that has it set, and an absent key means the macOS default
  (emoji), not "Do Nothing". `fnkey.rs` handles both; do not simplify it to an
  integer read that defaults to 0, which reports the exact opposite of the truth.
- **Re-signing can invalidate TCC grants.** The install script compares the
  designated requirement across updates and says so when it changes.
- **The Esc tap is a separate tap, and off by default on purpose.** Folding it into
  the Fn tap would mean a permanent key-down subscription — every keystroke in every
  app — for a feature that only matters while dictating. It is enabled by `emit_bar`
  for the live states and disabled otherwise, which is also the only reason it can
  safely be the app's one *consuming* tap. Do not merge the two taps.
- **Closing the Hub must hide it, never close it.** The app survives a real close —
  the overlay keeps the process alive — but the window is *destroyed*, and
  `get_webview_window("main")` returns `None` forever after, so the tray's Open item
  and the Dock icon both go dead with no error anywhere. `CloseRequested` is
  intercepted for exactly this reason. It reads as "the tray menu is broken".
- **Do not "fix" the in-process Fn tap on principle.** The callback is cheap, heavy
  work already runs on spawned threads, and tap-disabled-by-timeout is caught and
  re-enabled. Move it to the sidecar when a real symptom appears, not before.
- **The cleanup model will not apply a listed mishear that looks like a real name.**
  It substitutes what reads as a mistake ("monvi" → "Manvi") and refuses what reads as
  correct — "Hey Geeta, how's it going?" came back untouched with `Geetha (mis-heard
  as: Geeta)` right there in the prompt, because the precision guard says a word that
  makes sense as spoken should be left alone. Rewording the vocabulary block does not
  move it; that was measured, not assumed. `apply_listed_mishears` enacts listed
  mishears after cleanup for this reason. Do not fold it back into the prompt.
- **The gates must see the utterance's vocab.** `gates::evaluate` takes the
  prefiltered entries, because a dictionary fix is a word that is *not* in the raw
  transcript and otherwise reads as a hallucination. Pass `&[]` only when there
  genuinely was no dictionary. Getting this wrong is invisible in tests and shows up
  as "the dictionary works in long sentences but not short ones".
- **Prefilter's bigram pass is stricter than its unigram pass** (0.15 vs 0.30), and
  levelling them re-introduces false corrections — a glued pair is a token the code
  invented, not a word anyone said. Both numbers are load-bearing; the harness has a
  negative case for each.
- **Do not remove the tail padding in `whimpr-asr`.** whisper.cpp refuses to start a
  segment within a second of the end of the audio, so an utterance that stops the
  instant the speaker does loses its last words — which is every push-to-talk
  recording. It looks like a model problem and is not; larger models lose more.
- **Never prompt Whisper without keeping the unprompted transcript.** `initial_prompt`
  makes it emit words it was primed for from audio that lacked them, and the only
  defence is `accept_prompted` comparing the two. Collapsing the two passes into one
  "to save a pass" removes the comparison and there is nothing left to catch it.
  Note the count check, not just the membership check: an echoed glossary is *all*
  authorized words.

## Conventions

- **`whimpr-core` is pure.** State machine, prompts, gates, dictionary, settings,
  stats — no I/O, no platform code. This is where tests belong. Native code lives
  in `src-tauri`: `hotkey.rs` (CGEventTap), `paste.rs`, `autolearn.rs`, `appctx.rs`,
  `fnkey.rs`.
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
  in `ui/src/hub/api.ts`. Adding one to `Settings` or `SessionRecord` needs
  `#[serde(default)]`, or `load` silently resets every saved setting / discards the
  whole stats log.
- **History is filtered and paged in Rust** (`StatsStore::query`), not in the
  webview. The log only grows; a client-side filter over a truncated list cannot
  find old matches, and it gets slower every day. Keep new list features on that
  side of the boundary.

## Verifying

The unit tests do not touch models, audio, or the GPU, so passing them is not
evidence the app works. For anything in the dictation path, run the real thing —
`./scripts/install-macos.sh` then hold Fn — or the `dictionary_check` example,
which drives the production chain against the real model.

Anything involving what is *visible on screen* (the pill, Spaces, full-screen
behavior) cannot be confirmed from a shell. Query the window server
(`CGWindowListCopyWindowInfo` reports layer and on-screen state) or ask the user.
Do not claim a visual fix works without one of those.
