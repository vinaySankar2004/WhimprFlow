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

**No — but it has to be alive, and "alive" means the microphone is actually running.**
`UIBackgroundModes: audio` keeps an app resident only while audio is *active*;
declaring the mode and idling gets the app suspended seconds after it backgrounds. So
the app runs its capture engine in **standby** — discarding everything it hears until
the mic key says otherwise — and that is what keeps it reachable. The session runs in
`.default` mode, not `.measurement`: measurement strips output processing too and made
every other app audibly quieter all day.

Standby is bounded and visible. It is a **session with an idle timeout** — five
minutes by default, 15, 60, Always or Off in Settings (`Settings.standbyTimeout`,
`DictationController.armIdleTimer`) — which is exactly Wispr Flow's design once you
look: their mic is held the same way, for the same default. For as long as it lasts a
**Live Activity** puts the app's glyph in the Dynamic Island beside the orange
indicator, and its expanded view says whether the mic is ready, listening or
transcribing, which microphone, for how long, and offers **Finish / Discard / Release
mic**. Those are `LiveActivityIntent`s that post the keyboard's own Darwin signals, so
the app has one handler per action whichever surface asked; the keyboard's ≡ menu has
the same Release. When the timeout fires the mic is released, the indicator goes off,
the activity ends, and the next mic-key tap opens the app once to re-arm — the round
trip Wispr's "Flow is on" screen is. Every foreground visit re-arms.

Two iOS rules shape the activity code: an activity can be *requested* only from the
foreground (updated and ended from anywhere), and the system ends every activity after
eight hours. `StandbyActivityController` requests on `.active` and re-asks on each
foreground; with the default timeout the eight-hour limit never shows.

**Silent standby was tried and failed (2026-09-05).** Playing zeros under a
`.playback` session and switching to `.playAndRecord` only for the dictation, so the
indicator would show only while dictating. Opening the mic then failed with OSStatus
560557684, in the foreground too, and every mic tap bounced to the app. The code was
first read as `!cat`; decoded it is `!int`, `cannotInterruptOthers` — a *non-mixable*
session activated while something else held audio. So the category switch failed, not
recording from the background (that is `!rec`, 561145187), and whether a mixable
`.playAndRecord` session activated in the foreground can open its input later from
the background is still untested. A second attempt should keep one category for the
whole session and log which call throws, decoded, on the device.

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
    HandoffSignals.swift the `Handoff` declaration and the Darwin signals — all the widget needs
    Handoff.swift        the rest: App Group state (result/state/alive/input name/started-at)
    StandbyActivity.swift the Live Activity's attributes and its three intents
    LevelChannel.swift   the live mic level, one Float in a memory-mapped file
    Settings.swift       settings, appearance, and the Keychain (every key, one per line)
    Groq.swift           Whisper + chat-completions, rotating keys on a 429 via the core's ring
    Palette.swift        the colours, both appearances, as dynamic UIColors
    Theme.swift          SwiftUI wrapper over Palette
  WhimprFlow/            the app: Recorder (standby + capture), DictationController,
                         StandbyActivityController, Chime (the start pop), views
  Keyboard/              the extension — see "The keyboard" below
  Widgets/               the widget extension: the Live Activity's UI, nothing else
  Frameworks/            generated xcframework (gitignored)
```

## The keyboard

A top bar — ≡ menu, level pill, mic — over a full typing keyboard, and while a
dictation is happening the key area becomes the listening screen. The shape is Wispr
Flow's, adopted on purpose after looking at it side by side: it is what people who
dictate on a phone already know.

| File | Owns |
|---|---|
| `KeyboardViewController` | connecting the pieces to the host field and the app; the four screens (typing, listening, transcribing, failed) |
| `TopBar` | the bar, in typing and listening forms; the ≡ menu (Open WhimprFlow · Cleanup · Release the mic) |
| `KeyboardLayout` | the three planes as data — letters, numbers, symbols — in the stock arrangement |
| `KeyboardView` | the key grid: geometry, touch handling across keys, popups, delete repeat |
| `TypingEngine` | what typing does: sentence capitals honouring `autocapitalizationType`, double-space full stop, caps lock, the return key's word, autocorrect, smart insert of a dictation or a swiped word |
| `SwipeDecoder` | a finger path over the letters → a word, by path shape against `Resources/words-20k.txt` with frequency as the tie-break |
| `ListeningView` | waveform, "Listening", which microphone and for how long; transcribing; failure with Try again |
| `WaveformView` | twelve dots that rise into bars, driven from `LevelChannel` |

Things about it that are not obvious from the code:

- **Autocorrect is Apple's checker, used conservatively.** `UITextChecker` is
  available to a keyboard extension; a word is corrected on the space or punctuation
  that ends it only when every letter was typed here, the checker flags the whole
  word, its first guess keeps the first letter and is within one edit (two for longer
  words), and the field has not turned correction off. Delete straight after restores
  the typed word and teaches the checker it. Anything looser rewrites names, which is
  why people turn autocorrect off. No predictions strip: the bar's space belongs to
  the pill and the mic.
- **Swipe typing is SHARK²-shaped, on a 20k-word list.** Each candidate's ideal path
  through its key centres is compared with the drawn path (both resampled, mean
  distance in key widths), every letter must lie within reach of the path in order,
  and word frequency breaks ties. Other readings appear in the bar for a few seconds.
  The list and its provenance are in `Keyboard/Resources/README.md`; the decoder
  loads it on the first swipe, not at keyboard launch.
- **The globe is drawn only when `needsInputModeSwitchKey` is true.** On Face ID
  phones iOS draws it in the strip below every third-party keyboard.
- **Touches are handled by the grid, not by buttons.** Sliding onto the right key
  before lifting, two thumbs at once, delete repeating while held, the space-bar
  trackpad (hold, then drag to move the cursor; the keys dim), the iPad flick — all
  of it is touch handling *across* keys.
- **The pill writes the level into the shared container and posts `.settings`;** the
  app's `Settings` caches the level and must re-read it, or the next dictation is
  cleaned at the level the pill used to show.
- **A dictation gets a leading space when the cursor sits after a word.** The text
  itself is untouched — the parity test still holds byte for byte — only where it
  lands is decided here.
- **The start pop is the Mac's, re-made.** `Chime` synthesises it (iOS has no named
  system sound to borrow, and Apple's files are not ours to bundle) and the recorder
  ignores the microphone for its duration so Whisper is not handed the pop.
- **It is taller than the stock keyboard by the height of the bar.** The
  bottom-anchored layout described in `KeyboardViewController` is what makes that
  safe during a keyboard switch.
- **The iPad has its own arrangement, the stock one.** `KeyboardLayout.padRows`: tab
  and delete flank the top row, caps lock opens the home row and return ends it,
  shift sits at both ends of the third, and the bottom row is globe · .?123 · mic · space · .?123 · hide. Numbers
  and symbols are secondary labels on the letters, typed with a downward flick, so
  the grid stays four rows. `KeyboardView.Metrics` picks phone, iPad portrait or iPad
  landscape from the device and the width, and the height constraint follows it; the
  phone metrics stretched across an iPad gave hairline gaps between keys three times
  as wide as they were tall.
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
- **The simulator shows no software keyboard while "Connect Hardware Keyboard" is
  on**, which it is by default on a Mac — a focused field and nothing below it. Turn
  it off before testing the keyboard, and reboot the simulator for it to take:

  ```bash
  defaults write com.apple.iphonesimulator ConnectHardwareKeyboard -bool false
  ```
- **The keyboard can be enabled on a simulator without walking Settings.** The
  enabled list is a global default inside the device; append the extension's bundle
  id and it appears in the keyboard list at once. Full Access still has to be turned
  on by hand in Settings → General → Keyboard → Keyboards → WhimprFlow.

  ```bash
  xcrun simctl spawn <udid> defaults write -g AppleKeyboards -array \
    "en_US@sw=QWERTY;hw=Automatic" "emoji@sw=Emoji" "com.whimpr.whimprflow.keyboard"
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

**Verified on an iPhone 15, in daily use** (2026-09-05; the keyboard redesign was
installed the same evening and reported working in first use, and two device
screenshots confirmed the orange indicator beside the island glyph and the Lock Screen
banner counting down):

- dictation from the keyboard in place, with no app switch — standby, the Darwin
  handoff, `insertText`;
- the live waveform, discard from both keyboard and app, light/dark/system theme;
- keyboard switching with no visible frame, established from device screen
  recordings pulled apart frame by frame (see the notes in `KeyboardViewController`);
- the `whimprflow://` fallback after a force-quit;
- the Mac and iOS shells pasting identical text: `cargo test -p whimpr-ffi`.

**Verified on the iPad Air 11" (2026-09-05, his screenshot in landscape):** the stock
iPad arrangement with the flick secondaries, the bar scaled up, keys in stock
proportions. Shift, caps lock, the flick and the space-bar trackpad were then fixed or added and
reinstalled on both devices, not re-checked.

**Verified on the iPhone 17 Pro simulator** (2026-09-05, the keyboard redesign — the
simulator is honest about layout, ActivityKit and the bridge, and about nothing that
touches real audio or suspension):

- the three planes type into a Safari field; shift stays off in a field whose
  `autocapitalizationType` is none; the pill and the ≡ menu open; the failure notice
  has a way back to the keys; a swiped "hello" lands with its leading space; "teh" +
  space in Reminders becomes "The", with the sentence capital kept;
- the mic key starts a dictation over the Darwin handoff and the key area becomes the
  listening screen with the elapsed counter; ✕ discards and the keys return;
- the Live Activity appears in the Dynamic Island when standby starts, its expanded
  view shows the release countdown, and its **Release mic** button ends standby from
  the widget process (the intent → Darwin → app path, end to end);
- the app's standby card counts down and the setting migrates from the old switch;
- `cargo test -p whimpr-ffi` green — the pipeline is untouched.

**Not yet verified, and needs the phone:** the indicator going off when the timeout
fires and the next mic tap re-arming through the app; the start pop through the speaker and through AirPods, and
that the muted lead-in keeps it out of the transcript; a dictation's text landing with
the smart leading space; key click and haptics under Full Access; the keyboard-switch
frames with the new, taller keyboard (screen recording, `ffmpeg`, as before); standby
surviving a call with the activity intact; the eight-hour activity limit under
"Always". Also unverified anywhere: the double-space full stop and caps lock by double
tap were exercised only by reading `TypingEngine`, not on a device; swipe accuracy
has been tried on one word.

**Partly exercised on the device:** key rotation (2026-09-05). Two keys stored, and
dictation through the ring works on the phone (the first build shipped the previous
core and reported "no API key is set" — see the Xcode relink trap in the root
`CLAUDE.md`). That a 429 on one key actually moves a dictation to the next was
verified only by the core's tests and the bridge round-trip in `crates/whimpr-ffi`,
not by a live rate limit; the Mac logs each key used, masked, and the iOS Groq client
names the key each call used.

**Volume in standby** (2026-09-05): confirmed by ear that other apps play at full
volume with WhimprFlow holding the mic, after the move to `.default` mode.

**TestFlight** (2026-09-05): 0.1.0 (2) archived, uploaded and processed, and installed
from TestFlight on the developer's own iPhone and iPad. The name `WhimprFlow` was
available, so the unique-name snag Part 2 warned about did not arise.

Dictation works through that build on both devices, with the cable build deleted first
and Full Access re-granted — so the archive carried a current core, which is the thing
the [relink trap](../CLAUDE.md) takes away and which surfaces as "no API key is set".

**Not yet exercised:** a second person installing from an invite, and the 90-day
expiry cycle.

[wispr]: https://docs.wisprflow.ai/articles/7453988911-set-up-the-flow-keyboard-on-iphone
[qa1872]: https://developer.apple.com/library/archive/qa/qa1872/_index.html
[dts]: https://developer.apple.com/forums/thread/812091
