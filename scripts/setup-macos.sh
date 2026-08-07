#!/usr/bin/env bash
#
# Installs WhimprFlow on a Mac that did NOT build it, from a published release.
# Nothing here needs Rust, Node, CMake, Xcode or the repository — the app in the
# release is 14 MB and statically linked, so the whole job is: put the bundle in
# /Applications, fetch two model files, and get out of the way.
#
# Why the quarantine step exists, since it looks like a security bypass and is not:
# Gatekeeper only assesses files carrying `com.apple.quarantine`, which is stamped
# by whatever downloaded them. The release is signed with an Apple *Development*
# certificate rather than a Developer ID (this project is not enrolled in the paid
# programme), and a development certificate is refused when assessed — that is what
# made v0.1.0 unopenable for its first user. `xattr -cr` removes the download flag,
# after which macOS still verifies the code signature at launch but does not run a
# Gatekeeper policy check. The signature is real and checked below either way.
#
# Which models, and why it depends on the machine: the cleanup model is the one to
# scale down on a small machine, because a smaller Whisper mis-hears ordinary names
# (which is what a personal dictionary then exists to repair) while a smaller cleanup
# model only loses polish on text that is already right. Both ladders take the best
# file present, so the app needs no configuring afterwards.
#
# Bash 3.2 compatible on purpose — that is what /bin/bash is on a stock Mac.
#
# Usage: scripts/setup-macos.sh [--cleanup-model auto|small|large] [--no-launch]
set -euo pipefail

REPO="vinaySankar2004/WhimprFlow"
APP_DEST="/Applications/WhimprFlow.app"
SUPPORT="$HOME/Library/Application Support/WhimprFlow"
MODELS="$SUPPORT/models"
ASSET="WhimprFlow.app.zip"
CLEANUP_CHOICE="auto"
LAUNCH=1

while [ $# -gt 0 ]; do
  case "$1" in
    --cleanup-model) CLEANUP_CHOICE="${2:?--cleanup-model needs auto|small|large}"; shift 2 ;;
    --no-launch) LAUNCH=0; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

case "$CLEANUP_CHOICE" in
  auto|small|large) ;;
  *) echo "--cleanup-model must be auto, small or large (got '$CLEANUP_CHOICE')" >&2; exit 2 ;;
esac

# "name|expected-bytes|url" — the name is the filename the app searches for, which is
# not always the name at the far end of the URL, so the two are kept separate.
WHISPER_MODEL="ggml-large-v3-turbo-q5_0.bin|574041195|https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin"
CLEANUP_SMALL="qwen2.5-1.5b-instruct-q4_k_m.gguf|1117320736|https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/main/qwen2.5-1.5b-instruct-q4_k_m.gguf"
CLEANUP_LARGE="qwen3-4b-instruct-2507-q4_k_m.gguf|2497281120|https://huggingface.co/unsloth/Qwen3-4B-Instruct-2507-GGUF/resolve/main/Qwen3-4B-Instruct-2507-Q4_K_M.gguf"

step() { echo; echo "==> $1"; }
die()  { echo "ERROR: $1" >&2; exit 1; }

# ---------------------------------------------------------------- preflight

step "Checking this Mac"

ARCH="$(uname -m)"
[ "$ARCH" = "arm64" ] || die "WhimprFlow is Apple Silicon only — this Mac reports '$ARCH'.
       The release binary is arm64 and the ASR runs on Metal; neither has an Intel path."

OS_MAJOR="$(sw_vers -productVersion | cut -d. -f1)"
[ "$OS_MAJOR" -ge 14 ] 2>/dev/null \
  || die "macOS 14 or newer is required — this Mac is on $(sw_vers -productVersion)."

MEM_BYTES="$(sysctl -n hw.memsize)"
MEM_GB=$(( MEM_BYTES / 1024 / 1024 / 1024 ))
echo "    $(sysctl -n machdep.cpu.brand_string), ${MEM_GB} GB, macOS $(sw_vers -productVersion)"

# /Applications is group-writable by admins, so an admin user needs no sudo at all.
# Asking for a password when it is not needed is worse than not asking.
SUDO=""
if [ ! -w /Applications ]; then
  echo "    /Applications is not writable by this user — sudo will be needed to install."
  SUDO="sudo"
fi

# The 4B cleanup model is far better at spoken self-corrections and structure, and
# costs 2.3 GB of paged-in weights while it runs. Below 16 GB the 1.5B is the sane
# default; local_llm.rs falls back to it by design.
if [ "$CLEANUP_CHOICE" = "auto" ]; then
  if [ "$MEM_GB" -ge 16 ]; then CLEANUP_CHOICE="large"; else CLEANUP_CHOICE="small"; fi
  echo "    Cleanup model chosen automatically for ${MEM_GB} GB: $CLEANUP_CHOICE"
fi
if [ "$CLEANUP_CHOICE" = "large" ]; then CLEANUP_MODEL="$CLEANUP_LARGE"
else CLEANUP_MODEL="$CLEANUP_SMALL"; fi

# ---------------------------------------------------------------- the app

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

step "Downloading WhimprFlow"
# /releases/latest/download/<asset> always redirects to the newest release's asset of
# that name, so there is no API call and no JSON to parse.
BASE="https://github.com/$REPO/releases/latest/download"
curl -fL --retry 5 --progress-bar -o "$WORK/$ASSET" "$BASE/$ASSET" \
  || die "could not download $ASSET from $BASE
       Check https://github.com/$REPO/releases for a published release."

# A truncated download unpacks into a bundle macOS calls "damaged", which sends people
# hunting for a permissions problem. Catch it here where the cause is still obvious.
if curl -fsL --retry 3 -o "$WORK/$ASSET.sha256" "$BASE/$ASSET.sha256"; then
  EXPECTED="$(tr -d '[:space:]' < "$WORK/$ASSET.sha256")"
  ACTUAL="$(shasum -a 256 "$WORK/$ASSET" | awk '{print $1}')"
  [ "$EXPECTED" = "$ACTUAL" ] || die "checksum mismatch on $ASSET
       expected $EXPECTED
       got      $ACTUAL
       The download is corrupt or incomplete — run this script again."
  echo "    Checksum verified."
else
  echo "    (no published checksum for this release — skipping that check)"
fi

step "Unpacking"
# ditto, not unzip: it is what preserves a bundle's symlinks and extended attributes,
# and losing them unpacks into a signature that no longer verifies.
/usr/bin/ditto -x -k "$WORK/$ASSET" "$WORK/unpacked"
NEW_APP="$WORK/unpacked/WhimprFlow.app"
[ -d "$NEW_APP" ] || die "the archive did not contain WhimprFlow.app"

# macOS keeps a running binary alive and will not let it be replaced in place.
if pgrep -f "$APP_DEST/Contents/MacOS/whimpr-tauri" >/dev/null 2>&1; then
  step "Quitting the running WhimprFlow"
  pkill -f "$APP_DEST/Contents/MacOS/whimpr-tauri" || true
  pkill -f "$APP_DEST/Contents/MacOS/whimpr-llm-worker" || true
  sleep 1
fi

step "Installing to $APP_DEST"
$SUDO rm -rf "$APP_DEST"
$SUDO cp -R "$NEW_APP" "$APP_DEST"

# See the header: this clears the download flag, not the signature.
$SUDO xattr -cr "$APP_DEST" 2>/dev/null || true

# Verify the signature that is actually installed. A dev-signed bundle passes this;
# a corrupted or tampered one does not, which is the property worth checking.
codesign --verify --deep --strict "$APP_DEST" \
  || die "the installed bundle failed signature verification — do not run it.
       Delete $APP_DEST and run this script again."
echo "    Signature verified: $(codesign -dv --verbose=2 "$APP_DEST" 2>&1 \
  | sed -n 's/^Authority=//p' | head -1)"

# Without this the app starts, transcribes, and silently pastes RAW uncleaned text,
# because local cleanup runs in a separate process that simply never spawns.
[ -x "$APP_DEST/Contents/MacOS/whimpr-llm-worker" ] \
  || die "the release is missing whimpr-llm-worker — cleanup would silently never run.
       Report this: the app was packaged without it."

# ---------------------------------------------------------------- models

fetch_model() {
  local name rest size url dest human have got
  name="${1%%|*}"; rest="${1#*|}"; size="${rest%%|*}"; url="${rest#*|}"
  dest="$MODELS/$name"
  human=$(( size / 1024 / 1024 ))

  if [ -f "$dest" ]; then
    have="$(stat -f%z "$dest")"
    if [ "$have" = "$size" ]; then
      echo "    $name — already present, skipping."
      return 0
    fi
    # Resuming into a file that is somehow larger than the target cannot end well.
    if [ "$have" -gt "$size" ]; then rm -f "$dest"; fi
  fi

  echo "    $name (${human} MB)"
  # -C - resumes a part-finished download, which matters a lot on a home connection
  # when the file is gigabytes and the Wi-Fi drops.
  curl -fL --retry 5 --progress-bar -C - -o "$dest" "$url" \
    || die "failed to download $name from $url"

  got="$(stat -f%z "$dest")"
  [ "$got" = "$size" ] || die "$name is $got bytes, expected $size — download incomplete.
       Run this script again; it will resume."
}

step "Fetching models into $MODELS"
mkdir -p "$MODELS"
fetch_model "$WHISPER_MODEL"
fetch_model "$CLEANUP_MODEL"

# ---------------------------------------------------------------- the Fn key

step "Freeing up the Fn key"
# macOS runs its own action on a lone Fn press — usually the emoji picker — and it
# fires on top of dictation. The setting is in com.apple.HIToolbox, NOT NSGlobalDomain
# where the name suggests, and an ABSENT key means the macOS default (emoji), not
# "Do Nothing". So absent is treated as needing the write.
FN_NOW="$(defaults read com.apple.HIToolbox AppleFnUsageType 2>/dev/null || echo absent)"
if [ "$FN_NOW" = "0" ]; then
  echo "    Already set to Do Nothing."
else
  defaults write com.apple.HIToolbox AppleFnUsageType -int 0
  echo "    Set 'Press 🌐 key to' → Do Nothing (was: $FN_NOW)."
  echo "    The emoji picker is still on ⌃⌘Space."
  echo "    If Fn still opens the emoji picker, log out and back in once."
fi

# ---------------------------------------------------------------- permissions

if [ "$LAUNCH" = "0" ]; then
  echo; echo "Installed. Open WhimprFlow from Applications to finish setup."
  exit 0
fi

step "Launching WhimprFlow"
# Clear the old snapshot so a stale file cannot be mistaken for this launch.
PERMS="$SUPPORT/permissions.json"
rm -f "$PERMS"
open -a "$APP_DEST"

# The app writes down what it can actually do, because AXIsProcessTrusted() answers
# for the CALLING process: asking from this script would report the terminal's
# permissions, not the app's. Only the app can answer honestly.
echo "    Waiting for the app to report its permissions"
for _ in $(seq 1 25); do
  [ -f "$PERMS" ] && break
  sleep 1
done

echo
echo "-------------------------------------------------------------------"
if [ ! -f "$PERMS" ]; then
  echo "The app did not report in. Open it from Applications and use its"
  echo "setup screen to grant permissions."
elif grep -q '"accessibility":true' "$PERMS"; then
  echo "Accessibility: GRANTED — hold Fn in any app and dictate."
else
  echo "ONE THING LEFT, and it cannot be scripted: macOS will not let any"
  echo "program grant itself Accessibility."
  echo
  echo "In the window that just opened — System Settings → Privacy &"
  echo "Security → Accessibility — switch WhimprFlow ON."
  echo
  echo "Without it, holding Fn does nothing in every app except WhimprFlow"
  echo "itself, which looks like the app is broken rather than unpermitted."
  open "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility" || true
fi
echo
echo "Then: hold Fn, speak, let go. The text lands at your cursor."
echo "The microphone is asked for on your first dictation — say yes."
echo "-------------------------------------------------------------------"
