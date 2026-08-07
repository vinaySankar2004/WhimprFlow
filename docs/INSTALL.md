# Installing WhimprFlow on someone else's Mac

For a Mac that is **not** the one that built the app — a friend's, a parent's, a
second machine. Nothing here needs Rust, Node, CMake, Xcode, or a clone of this
repository. The app in a release is 14 MB and statically linked; the models are
downloaded straight from Hugging Face.

If you are reading this as an AI agent asked to install WhimprFlow: run the one
command below and then tell the user about the *one* thing you cannot do for them —
granting Accessibility. Everything else is handled.

## Requirements

- **Apple Silicon** (M1 or newer). The release binary is arm64 and ASR runs on
  Metal; there is no Intel path and the script refuses to continue on one.
- **macOS 14 or newer.**
- **~2–3 GB free** — 14 MB of app, the rest models.
- An **admin** account, or the install step will ask for a password.

## The one command

```bash
curl -fsSL https://raw.githubusercontent.com/vinaySankar2004/WhimprFlow/main/scripts/setup-macos.sh | bash
```

That is the whole installation. It takes a few minutes, almost all of it
downloading models.

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
   resuming if the connection drops.
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
arbitrary. Whisper's weights live in a Metal buffer that stays resident for as long
as the app runs — 716 MB paid around the clock — while the llama worker mmaps its
GGUF and pages it in only while it is actually cleaning up. So the big file is the
cheap one to keep. Dropping to a smaller *Whisper* model would save less memory and
cost far more accuracy: `ggml-base.en.bin` mis-hears ordinary names, which is most of
what a personal dictionary then exists to repair.

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

Prefer not to hold a key? Settings → Dictation Key → press-to-start / press-to-stop.

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
OpenAI, with the base URL pointed at any OpenAI-compatible API. The transcript is
sent; the audio never is.

## Updating

Run the same command again. It replaces the app, skips models that are already
present and correct, and — because the release is signed with the same identity every
time — leaves the existing Accessibility and Microphone grants intact.

## Publishing a release (for the maintainer)

The recipient's side of this only works if there is a release to fetch.

```bash
scripts/install-macos.sh --package /tmp/whimpr-release
gh release create "v0.1.1" \
  /tmp/whimpr-release/WhimprFlow.app.zip \
  /tmp/whimpr-release/WhimprFlow.app.zip.sha256 \
  --generate-notes
```

`--package` is a mode of `install-macos.sh` rather than a separate script so the
worker-bundling and sign-nested-code-first sequence cannot drift from the one that is
exercised on every local install. It signs with a real Apple timestamp, since a
signature without one stops verifying when the certificate expires — irrelevant for a
local build that gets replaced daily, not irrelevant for a zip someone keeps.

Both asset names matter: `setup-macos.sh` fetches
`releases/latest/download/WhimprFlow.app.zip` and its `.sha256` by exact name.

**This is not `scripts/build-macos.sh`.** That script demands a Developer ID and
notarizes, which is the correct way to ship to strangers and needs the paid Apple
Developer Program. Without it, the release is signed with an Apple *Development*
certificate — fine here only because Gatekeeper assesses **quarantined** files, and
`setup-macos.sh` clears that flag before first launch. The code signature is still
verified, both by the script and by macOS at every launch.

Enrolling in the Developer Program later is a strict improvement: `build-macos.sh`
then produces a notarized, stapled dmg that opens on a double-click with no script at
all. Be aware that switching to a Developer ID **changes the app's designated
requirement**, so every existing installation's Accessibility and Microphone grants
go stale and have to be re-granted once.
