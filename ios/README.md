# WhimprFlow for iOS and iPadOS

The same dictation loop as the Mac — record, recognize, clean, insert — inside the
only shape iOS permits. Cloud-only: Groq for both recognition and cleanup, no local
models.

The prompt, the levels, the gates, the dictionary and the order the deterministic
passes run in are **not reimplemented here**. They are `whimpr-core`, compiled for
iOS and linked in. See [Sharing the core](#sharing-the-core).

## The constraint everything else follows from

**A keyboard extension cannot record audio.** Not "needs a permission" — iOS refuses
outright. Apple's Technical Q&A [QA1872][qa1872] states it plainly ("App extensions
in iOS 8 are not allowed to record audio"), lists the AVFoundation and AudioToolbox
entry points that fail, and offers no workaround. Developers hitting it today get
`AVAudioSessionErrorCodeCannotStartRecording` (error 561145187) with the message that
extensions lack the entitlement to record. `RequestsOpenAccess` does not change this;
it buys network access and the shared container, nothing more.

So the keyboard asks and the app records:

```
       keyboard                          app
  ┌──────────────────┐            ┌──────────────────┐
  │  mic key tapped  │            │                  │
  └────────┬─────────┘            │                  │
           │  app alive?          │                  │
     ┌─────┴─────┐                │                  │
   yes│         │no               │                  │
     ▼           ▼                │                  │
 Darwin      open                 │                  │
 .start      whimprflow://   ───► │  record          │
                                  │  Groq Whisper    │
                                  │  Groq cleanup    │
                                  │  core: finish    │
           ┌──────────────────────┤  App Group write │
           ▼                      └──────────────────┘
   insertText(at cursor)
```

Two transports, and both are needed. The **App Group container** carries the data —
it is the only memory the two processes share. **Darwin notifications** carry the
timing — the only cross-process wake-up an extension can receive — but they cannot
carry a payload, so each one means only "look in the container".

### Does the app have to be open?

**No — but it has to be alive, and "alive" means audio is actually running.**
`UIBackgroundModes: audio` keeps an app resident only while audio is *active*;
declaring the mode and idling gets the app suspended seconds after it backgrounds. So
the app plays **silence in standby** — a `.playback` session rendering zeros — and
that is what keeps it reachable. The microphone is opened only for a dictation, so
the orange indicator shows only then. The first version stayed alive by *recording*
and discarding, which lit the indicator all day and, under `.measurement` mode, made
every other app quieter; both were user-visible enough to be reported within an hour
of use. The cost of the silent version is the capture engine spinning up on each mic
tap, a few dozen milliseconds before the first sample is kept.

Standby survives a phone call, Siri, AirPods connecting and a media-services reset:
the recorder tracks whether it *should* be up separately from whether it is, and
rebuilds on each. Without that, the first call of the day ended standby for good and
every later mic tap opened the app.

What nothing survives is a force-quit. A terminated app cannot be woken by a Darwin
notification and no extension can launch its container in the background, so the
keyboard opens `whimprflow://dictate` — you see the app, then tap the back arrow.
Wispr Flow's keyboard [documents the same limit][wispr]. Launching your *own*
container app is the one exception to the rule that a keyboard "must not launch other
apps", [confirmed by Apple DTS][dts]; since iOS 26 it also requires Full Access.

The keyboard tells the two cases apart with a heartbeat (`Handoff.markAlive`, every
~4s) rather than a flag: a killed app never gets to clear a flag, and a stale "alive"
leaves the mic key silently doing nothing.

## Sharing the core

`crates/whimpr-ffi` is a C ABI over `whimpr-core`: **one** function taking a JSON
request and returning a JSON response, plus a free. Not a `repr(C)` struct ABI —
every type crossing it already derives `Serialize`/`Deserialize` because the core
persists them, so a hand-written mirror of each would be pure surface area for the
two definitions to drift apart on.

The ops are deliberately coarse. `prepare` and `finish` each do several things a
caller could in principle do separately, because the order of those steps is
load-bearing in ways that fail *silently* — so the bridge does not offer a way to get
it wrong. Swift decides what to send to the provider and when; it never decides what
runs before or after.

```
Swift                     bridge                    core
  │                          │                        │
  │  {"op":"prepare",…}      │                        │
  ├─────────────────────────►│  pipeline::prepare     │
  │◄─────────────────────────┤  messages, max_tokens  │
  │                                                   │
  │  POST /chat/completions  (Swift's only job)       │
  │                                                   │
  │  {"op":"finish",…}       │                        │
  ├─────────────────────────►│  post_process,de_dash, │
  │                          │  strip_fillers, GATE,  │
  │                          │  dictionary, register  │
  │◄─────────────────────────┤  text to insert        │
```

`Prepared` is **opaque to Swift**: it is passed back verbatim, and only `messages`
and `max_tokens` are read out of it. Adding a field on the Rust side therefore needs
no Swift change and cannot be lost in transit — which matters most for `ctx.vocab`,
whose loss makes every dictionary correction look to the gate like the model
inventing a word.

### Parity is tested, not assumed

`crates/whimpr-ffi/tests/parity.rs` runs the same inputs down both paths — macOS
calling `pipeline` directly, iOS going out to JSON and back — and asserts
byte-identical text, engine and degradation reason across every level.

The case list includes short utterances carrying a dictionary word (`"call monvi"`),
and that is not decoration: in a long sentence an authorized spelling is a small
fraction of the output and the novelty ratio absorbs it, so losing the vocabulary
changes nothing and a long-cases-only test passes while the bug is real. Verified by
mutation — clearing `ctx.vocab` in the bridge leaves every long case green and fails
only the short ones.

### What is *not* shared

Two constants are genuine ports, because audio buffers cannot sensibly cross a JSON
bridge. Both are commented as copies at both ends; retune one, retune the other.

| iOS (`Recorder.swift`) | Rust | What it is |
|---|---|---|
| `normalizeForASR` | `whimpr_audio::normalize_for_asr` | Lifts quiet recordings, gain capped at 8× |
| `tailPadSamples` | `whimpr_asr::TAIL_PAD_SAMPLES` | One second of silence appended, or Whisper drops the last words |

## Layout

```
ios/
  project.yml            XcodeGen source — the project is generated, never edited by hand
  DEPLOY.md              registering App IDs, the App Group, TestFlight
  Shared/                compiled into BOTH targets
    WhimprCore.swift     the bridge; a transport with no judgement of its own
    Handoff.swift        App Group + Darwin notifications (start/stop/cancel/result/state/alive)
    LevelChannel.swift   the live mic level, one Float in a memory-mapped file
    Settings.swift       settings, appearance, and the Keychain (every key, one per line)
    Groq.swift           Whisper + chat-completions, rotating keys on a 429 via the core's ring
    Palette.swift        the colours, both appearances, as dynamic UIColors
    Theme.swift          SwiftUI wrapper over Palette
  WhimprFlow/            the app: Recorder (standby + capture), DictationController, views
  Keyboard/              the extension: KeyboardViewController, WaveformView
  Frameworks/            generated xcframework (gitignored)
```

## Building

```bash
cd ios && xcodegen generate          # after any change to project.yml
open WhimprFlow.xcodeproj
```

The Rust is built automatically by a pre-build phase running
`scripts/build-ios-core.sh`, which produces `ios/Frameworks/WhimprCore.xcframework`
with a device and a simulator slice. Nothing about it is visible to whoever installs
the app; it costs the *developer* one `rustup target add` and slower builds.

To build the core alone:

```bash
./scripts/build-ios-core.sh
```

### Traps that already cost time here

- **The repository path contains spaces** (`projects and learning`).
  `HEADER_SEARCH_PATHS` is a space-separated list, so an unquoted value is silently
  split into two nonexistent paths and the bridging header fails to resolve with
  `cannot find 'whimpr_call' in scope` — which reads as a linking problem and is not.
  It is quoted in `project.yml`; keep it that way.
- **Both targets need the bridging header**, not just the app. The keyboard links the
  core too. Setting it on one target only fails in the other with the same misleading
  error.
- **The two Rust targets are not interchangeable and cannot be `lipo`'d together.**
  `aarch64-apple-ios` and `aarch64-apple-ios-sim` are both arm64 and differ only by
  platform, which is exactly the case a fat archive cannot express. That is why this
  is an xcframework.
- **`xcodegen generate` overwrites the project.** Adding a file through Xcode's UI
  does not survive. Files are picked up by directory, so a new `.swift` inside an
  existing folder needs only a regenerate.
- **Xcode's build phases do not see your `PATH`.** They run without sourcing any shell
  profile, so `~/.cargo/bin` is absent and the Rust build phase dies with
  `rustup: command not found` — which Xcode reports only as *"Command
  PhaseScriptExecution failed with a nonzero exit code"*. Running the same script in a
  terminal works, because an interactive shell already added it, so it reads as a
  project fault rather than an environment one. `build-ios-core.sh` sources
  `~/.cargo/env` and prepends the usual locations itself; test any change to it the
  way Xcode will run it:

  ```bash
  env -i PATH=/usr/bin:/bin:/usr/sbin:/sbin HOME="$HOME" ./scripts/build-ios-core.sh
  ```
- **Do not pass `CODE_SIGNING_ALLOWED=NO` when testing on the simulator.** It builds
  and runs, but the app then carries no `application-identifier`, so every Keychain
  write fails and the API key silently will not save — which reads as a bug in the
  settings screen. `APIKey.save` now reports `errSecMissingEntitlement` rather than
  swallowing it, but the fix is to build signed:

  ```bash
  xcodebuild -project WhimprFlow.xcodeproj -scheme WhimprFlow \
    -sdk iphonesimulator -destination 'id=<simulator udid>' build
  ```

## Assets and metadata

The app icon is **generated**, by `scripts/make-ios-icon.py`, so its geometry and
colours live in a reviewable diff and stay tied to `ui/src/tokens/values.ts`. It is a
redraw of the Mac icon rather than a copy, because that file cannot be used as an iOS
app icon:

- it has an **alpha channel**, which App Store Connect rejects at upload validation —
  after the archive and the wait, not at build time;
- its **rounded corners are baked in**, and iOS applies its own mask, so it would
  render as a small rounded square inset inside the system's rounded square.

Re-render after changing the palette:

```bash
python3 scripts/make-ios-icon.py
```

One 1024×1024 file; Xcode derives every smaller size (iOS 17+ single-size app icons).

`ITSAppUsesNonExemptEncryption` is set to `false` in `Info.plist`. This answers the
export-compliance question once instead of at every upload — without it a build sits
in "Missing Compliance" and testers cannot install it until someone clicks through the
form. False is the correct answer: the app makes HTTPS calls and does nothing else
with cryptography, which is the standard exemption.

`UILaunchScreen` names the `LaunchBackground` colour rather than being an empty dict,
which would flash white before a dark app.

**Bump `CURRENT_PROJECT_VERSION` in `project.yml` for every TestFlight upload.** App
Store Connect rejects a build whose (`MARKETING_VERSION`, `CURRENT_PROJECT_VERSION`)
pair it has already seen, and the rejection arrives after the upload.

## Distribution

**Step-by-step procedure: [DEPLOY.md](DEPLOY.md)** — registering the App IDs and the
App Group, running on your own phone, and getting a build to someone else. The rest of
this section is the shape of it.

No App Store submission. The team is **VSTTF2AM22** — the paid team that holds the
Developer ID and the notary profile, *not* 3V3J78V32Q, which owns the older Apple
Development certs on this machine and fails with a misleading 403 about a missing
agreement.

- **TestFlight internal** (recommended): add testers as App Store Connect users, no
  Beta App Review, one-tap install. Builds expire after 90 days, so a fresh upload
  four times a year.
- **Ad Hoc**: register device UDIDs, one-year profile, no review — but delivering the
  `.ipa` is yours to arrange.

## Status

**Verified on an iPhone 15, in daily use** (2026-09-05):

- dictation from the keyboard in place, with no app switch — standby, the Darwin
  handoff, `insertText`;
- the live waveform, discard from both keyboard and app, light/dark/system theme;
- keyboard switching with no visible frame, established from device screen
  recordings pulled apart frame by frame (see the notes in `KeyboardViewController`);
- the `whimprflow://` fallback after a force-quit;
- the Mac and iOS shells pasting identical text: `cargo test -p whimpr-ffi`.

**Partly exercised on the device:** key rotation (2026-09-05). Two keys stored, and
dictation through the ring works on the phone (the first build shipped the previous
core and reported "no API key is set" — see the Xcode relink trap in the root
`CLAUDE.md`). That a 429 on one key actually moves a dictation to the next was
verified only by the core's tests and the bridge round-trip in `crates/whimpr-ffi`,
not by provoking a real limit on the device. Same standing on the Mac, where the log
names the key each call used.

**Silent standby** (2026-09-05): confirmed by ear that other apps play at full volume
with WhimprFlow in standby. Still to confirm on the device: that the indicator is off
between dictations, and that a backgrounded app can open the mic from a `.playback`
session when the keyboard asks — if iOS refuses, the mic key falls back to opening the
app, which is the behaviour with standby off.

**Not yet exercised:** TestFlight. Nothing has been archived or uploaded; Part 2 of
[DEPLOY.md](DEPLOY.md) is written but unwalked, and the globally unique App Store
Connect name is the likeliest snag.

[wispr]: https://docs.wisprflow.ai/articles/7453988911-set-up-the-flow-keyboard-on-iphone
[qa1872]: https://developer.apple.com/library/archive/qa/qa1872/_index.html
[dts]: https://developer.apple.com/forums/thread/812091
