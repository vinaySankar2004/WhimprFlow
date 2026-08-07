#!/usr/bin/env bash
#
# Builds WhimprFlow and installs it into /Applications for THIS machine.
#
# This is deliberately not scripts/build-macos.sh. That script produces artifacts
# for other people and therefore refuses to sign with anything but a Developer ID —
# the right call, because a development certificate is what made v0.1.0 unopenable
# for its first user. None of that applies here: an app built locally is never
# quarantined, so Gatekeeper does not scrutinise it, and an Apple Development
# certificate is enough. Use build-macos.sh to ship; use this one to run.
#
# The part that is easy to get wrong: `tauri build` does NOT bundle the local LLM
# worker (tauri.conf.json declares no externalBin). Without it the app still starts,
# still transcribes, and silently pastes RAW text — local cleanup just never runs,
# because worker_bin_path() falls back to a hardcoded ~/WhimprFlow path. So the
# worker is copied in and the bundle re-signed, every time.
#
# Updating in place: the old bundle is quit and deleted before the new one lands,
# and the new one is checked to make sure macOS still considers it the SAME app —
# see the designated-requirement check below, which is what keeps the Accessibility
# and Microphone grants from silently going stale on every update.
#
# Packaging for someone else (--package <dir>): the same build and the same worker
# copy, signed with a real Apple timestamp and zipped with ditto, ready for
# `gh release upload`. Deliberately a mode of this script rather than a third one —
# the sequence below (worker in, nested code signed before the bundle) is the part
# that is easy to get wrong, and a second copy of it would drift.
#
# It is not scripts/build-macos.sh, which demands a Developer ID and notarizes. There
# is no Developer ID here, so the recipient clears the download's quarantine flag
# instead and scripts/setup-macos.sh does that for them. Gatekeeper only ever
# assesses *quarantined* files, so a cleared bundle launches happily on an Apple
# Development certificate — the v0.1.0 failure was a quarantined one, which is a
# different situation, not this one.
#
# Usage: scripts/install-macos.sh [--no-launch] [--reset-permissions]
#        scripts/install-macos.sh --package <output-dir>
set -euo pipefail

cd "$(dirname "$0")/.."
REPO_ROOT="$PWD"
APP_DEST="/Applications/WhimprFlow.app"
BUNDLE_ID="com.whimpr.whimprflow"
LAUNCH=1
RESET_PERMS=0
PACKAGE_DIR=""

while [ $# -gt 0 ]; do
  case "$1" in
    --no-launch) LAUNCH=0; shift ;;
    --reset-permissions) RESET_PERMS=1; shift ;;
    --package) PACKAGE_DIR="${2:?--package needs an output directory}"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

# A signature with no secure timestamp stops verifying once the signing certificate
# expires, which for an Apple Development cert is a year out. Irrelevant for a local
# install that gets rebuilt constantly; not irrelevant for a zip someone else keeps.
TIMESTAMP_ARG="--timestamp=none"
[ -n "$PACKAGE_DIR" ] && TIMESTAMP_ARG="--timestamp"

# TCC keys a permission grant to the app's designated requirement. Capture the
# outgoing app's DR so the new build can be compared against it: if they match, the
# existing grants keep working; if they drift, they silently stop and the fix is to
# clear the stale entry rather than let the user wonder why Fn died.
OLD_DR=""
if [ -z "$PACKAGE_DIR" ] && [ -d "$APP_DEST" ]; then
  OLD_DR="$(codesign -d -r- "$APP_DEST" 2>/dev/null | sed -n 's/^designated => //p')"
fi

# Prefer a Developer ID if the machine happens to have one, else the first Apple
# Development identity. Local install, so either is fine.
IDENTITY="${APPLE_SIGNING_IDENTITY:-}"
if [ -z "$IDENTITY" ]; then
  IDENTITY="$(security find-identity -v -p codesigning \
    | sed -n 's/.*"\(Developer ID Application:[^"]*\)".*/\1/p' | head -1)"
fi
if [ -z "$IDENTITY" ]; then
  IDENTITY="$(security find-identity -v -p codesigning \
    | sed -n 's/.*"\(Apple Development:[^"]*\)".*/\1/p' | head -1)"
fi
if [ -z "$IDENTITY" ]; then
  echo "No codesigning identity found in the keychain." >&2
  security find-identity -v -p codesigning >&2
  exit 1
fi

TAURI="$REPO_ROOT/ui/node_modules/.bin/tauri"
if [ ! -x "$TAURI" ]; then
  echo "Tauri CLI missing — run 'pnpm install' in ui/ first." >&2
  exit 1
fi

echo "==> Signing identity: $IDENTITY"
export APPLE_SIGNING_IDENTITY="$IDENTITY"

echo "==> Building the local LLM worker"
cargo build --release -p whimpr-llm-worker

echo "==> Building WhimprFlow.app"
# From src-tauri: run from the repo root and the CLI resolves ui/ as the app dir
# (the only package.json), making beforeBuildCommand's `pnpm --dir ui build` look
# for ui/ui and fail.
(cd "$REPO_ROOT/src-tauri" && "$TAURI" build --bundles app)

APP="$REPO_ROOT/target/release/bundle/macos/WhimprFlow.app"
[ -d "$APP" ] || { echo "No WhimprFlow.app was produced." >&2; exit 1; }

WORKER="$REPO_ROOT/target/release/whimpr-llm-worker"
[ -x "$WORKER" ] || { echo "No whimpr-llm-worker binary at $WORKER" >&2; exit 1; }

echo "==> Adding the LLM worker to the bundle"
cp "$WORKER" "$APP/Contents/MacOS/whimpr-llm-worker"

# Sign a bundle with no stray extended attributes on it. ditto preserves xattrs into
# the zip, so anything clinging to the build travels to the recipient.
xattr -cr "$APP" 2>/dev/null || true

# Nested code first, then the bundle — signing the outer bundle seals what is inside
# it, so doing this in the other order invalidates the signature immediately.
codesign --force --sign "$IDENTITY" --options runtime \
  --entitlements "$REPO_ROOT/src-tauri/Entitlements.plist" "$TIMESTAMP_ARG" \
  "$APP/Contents/MacOS/whimpr-llm-worker"
codesign --force --sign "$IDENTITY" --options runtime \
  --entitlements "$REPO_ROOT/src-tauri/Entitlements.plist" "$TIMESTAMP_ARG" "$APP"

echo "==> Verifying the signature"
codesign --verify --deep --strict "$APP"

# Packaging stops here: nothing is installed, nothing on this machine is touched.
if [ -n "$PACKAGE_DIR" ]; then
  mkdir -p "$PACKAGE_DIR"
  ZIP="$PACKAGE_DIR/WhimprFlow.app.zip"
  echo "==> Zipping for release"
  # ditto, not `zip`: it is the only archiver that round-trips a bundle's symlinks
  # and extended attributes, and a zip that loses them unpacks into a broken
  # signature on the far side.
  rm -f "$ZIP"
  /usr/bin/ditto -c -k --keepParent "$APP" "$ZIP"

  # setup-macos.sh checks the download against this, so a truncated transfer is
  # caught before it becomes "the app is damaged".
  shasum -a 256 "$ZIP" | awk '{print $1}' > "$ZIP.sha256"

  VERSION="$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
    "$REPO_ROOT/src-tauri/tauri.conf.json" | head -1)"

  echo
  echo "Zip:     $ZIP"
  echo "SHA256:  $(cat "$ZIP.sha256")"
  echo "Signed:  $IDENTITY"
  echo "Version: $VERSION"
  echo
  echo "Publish it:"
  echo "  gh release create \"v$VERSION\" \"$ZIP\" \"$ZIP.sha256\" --generate-notes"
  echo "Or replace the assets on an existing release:"
  echo "  gh release upload \"v$VERSION\" \"$ZIP\" \"$ZIP.sha256\" --clobber"
  exit 0
fi

# A running copy cannot be replaced in place, and macOS keeps the old binary alive.
if pgrep -f "$APP_DEST/Contents/MacOS/whimpr-tauri" >/dev/null 2>&1; then
  echo "==> Quitting the running WhimprFlow"
  pkill -f "$APP_DEST/Contents/MacOS/whimpr-tauri" || true
  pkill -f "$APP_DEST/Contents/MacOS/whimpr-llm-worker" || true
  sleep 1
fi

echo "==> Removing the old install"
rm -rf "$APP_DEST"

echo "==> Installing to $APP_DEST"
cp -R "$APP" "$APP_DEST"
xattr -cr "$APP_DEST" 2>/dev/null || true
codesign --verify --deep --strict "$APP_DEST"

echo
echo "Installed: $APP_DEST"
ls -1 "$APP_DEST/Contents/MacOS/"

# Did this update keep its identity? Same DR => macOS still sees the same app and
# every existing permission grant applies to the new build untouched.
NEW_DR="$(codesign -d -r- "$APP_DEST" 2>/dev/null | sed -n 's/^designated => //p')"
echo
if [ -z "$OLD_DR" ]; then
  echo "==> First install — grant Accessibility and Microphone when prompted."
  RESET_PERMS=0
elif [ "$OLD_DR" = "$NEW_DR" ]; then
  echo "==> Identity unchanged — existing Accessibility/Microphone grants still apply."
  RESET_PERMS=0
else
  echo "==> WARNING: the app's signing identity CHANGED across this update." >&2
  echo "    old: $OLD_DR" >&2
  echo "    new: $NEW_DR" >&2
  echo "    macOS now treats this as a different app, so its old permission grants" >&2
  echo "    are stale and Fn will not work until they are re-granted." >&2
  RESET_PERMS=1
fi

# Clearing the stale entries makes the app prompt cleanly instead of sitting next to
# a ticked-but-dead row in System Settings, which is the confusing failure mode.
if [ "$RESET_PERMS" = "1" ]; then
  echo "==> Clearing stale permission entries for $BUNDLE_ID"
  tccutil reset Accessibility "$BUNDLE_ID" 2>/dev/null || true
  tccutil reset Microphone "$BUNDLE_ID" 2>/dev/null || true
  tccutil reset ListenEvent "$BUNDLE_ID" 2>/dev/null || true
  open "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility" || true
fi

if [ "$LAUNCH" = "1" ]; then
  echo
  echo "==> Launching"
  # Clear the previous snapshot so a stale file can't be mistaken for this launch.
  PERMS="$HOME/Library/Application Support/WhimprFlow/permissions.json"
  rm -f "$PERMS"
  open -a "$APP_DEST"

  # The app writes down what it can actually do; asking macOS from here would report
  # the SHELL's permissions instead of the app's.
  echo "==> Waiting for the app to report its permissions"
  for _ in $(seq 1 20); do
    [ -f "$PERMS" ] && break
    sleep 1
  done

  if [ ! -f "$PERMS" ]; then
    echo "    (no report — check Settings → Permissions in the Hub)"
  elif grep -q '"accessibility":true' "$PERMS"; then
    echo "    Accessibility: GRANTED — Fn works in every app."
  else
    echo
    echo "    Accessibility: NOT GRANTED."
    echo "    Fn will only work while WhimprFlow itself is frontmost — in any other"
    echo "    app, holding Fn does nothing at all. Tick WhimprFlow in the window that"
    echo "    just opened. If it is already ticked, untick and re-tick it: replacing"
    echo "    the app leaves the old entry behind, pointing at a bundle that is gone."
    open "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility" || true
  fi
fi
