# Installing WhimprFlow on someone else's Mac

For a Mac that is **not** the one that built the app — a friend's, a parent's, a
second machine. Nothing here needs Rust, Node, CMake, Xcode, or a clone of this
repository. The app in a release is 14 MB and statically linked.

---

## Runbook for an AI agent asked to install this

You have probably been handed a repository link and asked to "download this" or "set
this up". **Do not build anything.** Do not run `cargo`, `pnpm`, `npm` or `tauri`; do
not install a toolchain; the repository is not needed at all. There is a published
release and a script that installs it. Ignore `CLAUDE.md` — that is for people
changing the code.

Work through these in order and do not skip the verification, because every failure
mode in this app looks like "it just doesn't do anything".

**1. Pick the install mode.** Ask the user which, in these terms:

| | Downloads | Speech stays on the Mac? | Needs |
|---|---|---|---|
| **Cloud** (`--cloud`) | 14 MB | No — audio and text go to Groq | a free Groq API key |
| **Local** (default) | ~2.9 GB | Yes, entirely | nothing |

Cloud is the right answer for most people who did not build this: it installs in
seconds instead of downloading gigabytes, and the free tier needs no card. Local is
the right answer for anyone who does not want their voice leaving the machine. **Ask;
do not assume.** It is a privacy decision and it is theirs.

**2. Run the installer.** One command, and it is not silent — expect a few minutes on
the local path, almost all of it model downloads.

```bash
curl -fsSL https://raw.githubusercontent.com/vinaySankar2004/WhimprFlow/main/scripts/setup-macos.sh | bash -s -- --cloud
```

Drop `-s -- --cloud` for the local install. If the machine is Intel or on macOS 13 or
older the script refuses and says why; that is a hard stop, not something to work
around.

**3. On the cloud path, get the user a Groq key.** The app cannot transcribe without
one, and its setup screen will not let them past until it has one. Walk them through
it — **do not ask them to paste the key to you, and do not type it in for them:**

1. Open <https://console.groq.com> and sign in. No card is required.
2. **API Keys → Create API Key**, then copy it.
3. Paste it into the **Groq API key** step in the WhimprFlow window.

It goes into their macOS Keychain, never into a file.

**4. Tell them the one thing no script can do.** macOS will not let any program grant
itself Accessibility — there is no flag, no API, and `tccutil` can only *reset* a
grant. The script opens the right pane; the switch is theirs to flip:

> **System Settings → Privacy & Security → Accessibility → turn WhimprFlow on.**

Say why, because the symptom is misleading: without it, holding Fn does nothing in
every app *except* WhimprFlow itself — which is never, since dictating means being in
some other app. It reads as a broken app rather than a missing permission.

**5. Verify, rather than declaring success.** The app writes down what it can actually
do at each launch:

```bash
cat ~/Library/Application\ Support/WhimprFlow/permissions.json
```

`"accessibility":true` is the one that matters. **Do not try to check this yourself
with a script or a binary you run from the shell** — `AXIsProcessTrusted()` answers
for the *calling* process, so anything you launch reports your terminal's permissions
and not the app's. That file is the only honest answer.

Then have the user hold **Fn**, say a sentence, and let go, with the cursor in a text
field. That is the only real test. If the pill appears and says something is wrong, it
names the fix — read it to them.

**6. Do not clean up the repository if you cloned one.** They may want it. Ask.

Failure modes and what they actually mean are in *When something is wrong*, below.

---

## Requirements

- **Apple Silicon** (M1 or newer). The release binary is arm64 and ASR runs on
  Metal; there is no Intel path and the script refuses to continue on one.
- **macOS 14 or newer.**
- **~2–3 GB free** for a local install — 14 MB of app, the rest models. A
  `--cloud` install needs 14 MB and no models at all.
- An **admin** account, or the install step will ask for a password.

## The one command

```bash
curl -fsSL https://raw.githubusercontent.com/vinaySankar2004/WhimprFlow/main/scripts/setup-macos.sh | bash
```

That is the whole installation. It takes a few minutes, almost all of it
downloading models.

**Or, with no models at all** — 14 MB, installs in seconds, and both stages run on
Groq's free tier instead of on this Mac:

```bash
curl -fsSL https://raw.githubusercontent.com/vinaySankar2004/WhimprFlow/main/scripts/setup-macos.sh \
  | bash -s -- --cloud
```

That is the better install for a Mac that did not build the app: a multi-gigabyte
download is where a setup like this actually stalls. The trade is real and worth
stating plainly — cloud ASR uploads the recording and cloud cleanup uploads the
transcript, where a local install sends neither. It also needs a free Groq API key
(no card), which the app's setup screen asks for and will not let you past without.
Everything else is identical, and switching to local later is just downloading the
models into `~/Library/Application Support/WhimprFlow/models/` and picking the
on-device engines in Settings.

To choose the cleanup model yourself instead of letting it pick by RAM:

```bash
curl -fsSL https://raw.githubusercontent.com/vinaySankar2004/WhimprFlow/main/scripts/setup-macos.sh \
  | bash -s -- --cleanup-model small
```

## What it does

1. **Checks the machine** — architecture, macOS version, RAM.
2. **Downloads the latest release** and verifies it against the published SHA-256.
3. **Installs to `/Applications`**, then clears the download's quarantine flag and
   verifies the code signature.
4. **Downloads two models** into `~/Library/Application Support/WhimprFlow/models/`,
   resuming if the connection drops — or, with `--cloud`, downloads nothing and
   writes a `settings.json` pointed at Groq for both stages. It will not overwrite a
   `settings.json` that already exists, so re-running it to update the app never
   resets anyone's choices.
5. **Frees up the Fn key** by setting `AppleFnUsageType` to Do Nothing.
6. **Launches the app** and reports which permissions it actually got.

### Which models, and why RAM decides

Both model ladders take the best file present, so there is nothing to configure
afterwards — the choice is only about what gets downloaded.

| | file | size | when |
|---|---|---|---|
| ASR | `ggml-large-v3-turbo-q5_0.bin` | 547 MB | always |
| Cleanup | `qwen3-4b-instruct-2507-q4_k_m.gguf` | 2.3 GB | 16 GB RAM or more |
| Cleanup | `qwen2.5-1.5b-instruct-q4_k_m.gguf` | 1.0 GB | under 16 GB |

The cleanup model is the one that scales down, not the ASR one, and that is not
arbitrary. Dropping to a smaller *Whisper* model costs far more than it saves:
`ggml-base.en.bin` mis-hears ordinary names, which is most of what a personal
dictionary then exists to repair, while a smaller cleanup model only loses polish on
text that is already right. Whisper's weights sit in a Metal buffer while local
recognition is the selected engine, so on a cloud install they are not loaded at all.

The 4B cleanup model is meaningfully better at spoken self-corrections and structure.
On a 16 GB machine, take it.

## The parts no script can do

**Accessibility must be granted by hand.** macOS will not let any program grant
itself Accessibility — `tccutil` can only *reset* a grant, never issue one, and the
database is protected by SIP. There is no flag, no API, and no workaround.

So: **System Settings → Privacy & Security → Accessibility → switch WhimprFlow on.**
The script opens that pane for you.

This one matters more than it sounds. Without Accessibility the Fn tap only works
while WhimprFlow itself is frontmost — which is never, because dictating means being
in some *other* app. The symptom is "holding Fn does nothing" or "the pill only shows
on the desktop", and it reads like a broken app rather than a missing permission.

**The microphone is asked for on the first dictation.** Say yes.

**Input Monitoring** may also be requested. The Hub's setup screen walks it.

## Using it

Hold **Fn**, speak, let go. Cleaned text lands at the cursor.

WhimprFlow has no Dock icon — it is a menu-bar app. Its icon sits up by the clock and
battery, and that is where you open the Hub from.

Prefer not to hold a key? Settings → Dictation Key offers press-to-start /
press-to-stop, and a double-tap-to-start mode that leaves a lone Fn press to macOS so
Fn+Delete and Fn+arrows keep working.

While dictating: **■** stops and pastes, **✕** or **Esc** discards.

## When something is wrong

**"Holding Fn does nothing in other apps."** Accessibility. If WhimprFlow already
looks switched on, switch it off and on again — replacing the app can leave a stale
entry pointing at a bundle that no longer exists.

**Fn opens the emoji picker.** The script sets `AppleFnUsageType` to Do Nothing, but
the change may need a log out and back in to take hold. Check it under System
Settings → Keyboard → "Press 🌐 key to". Note that the setting lives in the
`com.apple.HIToolbox` domain, so `defaults read -g AppleFnUsageType` reports "does
not exist" on a machine that has it set — that is a false negative, not the answer.

**Nothing is heard while on a call, or on AirPods.** Fixed as of v0.1.2 — the app now
tries every input device and format rather than only the system default, so a headset
that has switched to its call profile falls back to the built-in mic. On an older
build, update.

**Nothing explains what went wrong.** There is a log, and it is the first thing to
read. Everything the app decided — which engine served a dictation, why one fell back,
why a paste came out raw — is in it with a timestamp:

```bash
tail -50 ~/Library/Application\ Support/WhimprFlow/logs/whimpr.log
```

Settings → Permissions has a button that reveals it in Finder, for sending it on. Per-
dictation usage (which engine, how long each stage took, and why it degraded) is
recorded in `stats.json` beside it.

**"WhimprFlow is damaged and can't be opened."** The download was truncated. Run the
command again; the checksum check exists to catch this before install.

**The text that gets pasted is messy — fillers and all.** The bundled cleanup worker
is missing or no cleanup model was found. Check both:

```bash
ls -l /Applications/WhimprFlow.app/Contents/MacOS/
ls -lh ~/Library/Application\ Support/WhimprFlow/models/
```

`whimpr-llm-worker` must be in the first, and a `.gguf` in the second. The app starts
fine and transcribes fine without either, and silently pastes raw text.

**No local cleanup model and no wish to download one.** Settings → Cleanup Engine →
Cloud, then the **Groq** preset and a free key from console.groq.com. The transcript
is sent; the audio never is.

**On a `--cloud` install, the pill says "No API key. Open WhimprFlow to add one."**
Exactly what it says: there are no models on this Mac, so without a key there is
nothing to transcribe with. Open WhimprFlow and paste a key from console.groq.com
into the **Groq API key** step. The pill names the fix rather than failing silently
because this is the one state a cloud install can be left in.

**On a `--cloud` install, dictation stops working after a lot of use in one day.**
The free tier has a daily request cap. Cleanup absorbs that on a machine with a local
model; a cloud-only one has nothing to fall back to, so it waits for the cap to
reset. Downloading the two models (see *Which models*) removes the ceiling for good.

## Updating

Run the same command again. It replaces the app, skips models that are already
present and correct, and — because the release is signed with the same identity every
time — leaves the existing Accessibility and Microphone grants intact.

## Publishing a release (for the maintainer)

The recipient's side of this only works if there is a release to fetch. Since v0.1.7
releases are signed with a Developer ID and notarized, which is what lets the app open
on a stranger's Mac with no script at all.

1. Bump `version` in `Cargo.toml` and `src-tauri/tauri.conf.json`.
2. Build, sign and notarize:

   ```bash
   APPLE_SIGNING_IDENTITY="Developer ID Application: <name> (<team>)" \
   NOTARY_PROFILE=<notarytool profile> ./scripts/build-macos.sh
   ```

   The identity comes from the environment on purpose: the script refuses to
   produce a release with anything but a Developer ID, because v0.1.0 shipped with a
   development certificate, passed every local check, and would not open on the
   first user's Mac. The profile is one stored by `xcrun notarytool
   store-credentials`, so no secret is ever on the command line.
3. Publish with the `gh release create` line the script prints, attaching all three
   assets. Two of the names are load-bearing: `setup-macos.sh` fetches
   `releases/latest/download/WhimprFlow.app.zip` and its `.sha256` by exact name.
4. **Verify the published asset cold** — download it from the release page, not the
   build directory, and check `codesign -dv` says `Developer ID Application`,
   `xcrun stapler validate` finds a ticket, and `Contents/MacOS/` holds **both**
   `whimpr-tauri` and `whimpr-llm-worker`. Two real releases shipped broken because
   the local build was trusted instead: one signed with a development certificate,
   one missing the worker, which pastes raw uncleaned text and looks like cleanup
   being broken.

`scripts/install-macos.sh --package <dir>` still exists for a build without the
Developer Program. It signs with a development certificate, which works only because
`setup-macos.sh` clears the quarantine flag before first launch. Do not switch between
the two identities casually: it **changes the app's designated requirement**, so
every existing installation's Accessibility and Microphone grants go stale and must be
re-granted once.
