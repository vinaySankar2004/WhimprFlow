#!/usr/bin/env bash
#
# Builds WhimprFlow for macOS the way it has to be built to be *installable* by
# someone who is not the person who built it.
#
# v0.1.0 shipped signed with an "Apple Development: ..." certificate — a
# development certificate. It passed every local check and then refused to open
# on the first user's Mac ("cannot be opened because it is from an unidentified
# developer"), because a development certificate is not a distribution one.
# That is the bug this script exists to make impossible to repeat: the identity
# comes from the environment, and the script refuses to produce a release build
# with anything other than a Developer ID.
#
# Two notarization passes, on purpose:
#   1. Tauri notarizes and staples the .app when APPLE_ID / APPLE_PASSWORD /
#      APPLE_TEAM_ID are set.
#   2. Tauri then wraps that app in a dmg which it signs but never submits. An
#      unstapled dmg has to be checked against Apple over the network, so it
#      fails for a user who is offline or behind a filter — and the failure
#      reads as a corrupt download. So the dmg is submitted and stapled here,
#      separately, after the bundler finishes.
#
# Credentials, either way:
#   NOTARY_PROFILE=<name>   a profile stored by `xcrun notarytool
#                           store-credentials`, so no secret is ever passed in
#                           the environment. Preferred.
#   APPLE_ID / APPLE_PASSWORD / APPLE_TEAM_ID
#                           an app-specific password. Required for Tauri's own
#                           pass, which cannot read a keychain profile.
#
# Usage: scripts/build-macos.sh [--target <triple>] [--skip-notarize]
set -euo pipefail

cd "$(dirname "$0")/.."
REPO_ROOT="$PWD"
TARGET=""
SKIP_NOTARIZE=0

while [ $# -gt 0 ]; do
  case "$1" in
    --target) TARGET="$2"; shift 2 ;;
    --skip-notarize) SKIP_NOTARIZE=1; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

# No baked-in default. The proof-of-concept this began as hardcoded its own author's
# Developer ID here, which is not this project's identity and is not something to
# publish as a build default — so the identity is stated explicitly or not at all.
IDENTITY="${APPLE_SIGNING_IDENTITY:-}"
if [ -z "$IDENTITY" ]; then
  echo "Set APPLE_SIGNING_IDENTITY to a 'Developer ID Application: ...' identity." >&2
  echo "Codesigning identities in this keychain:" >&2
  security find-identity -v -p codesigning >&2
  exit 1
fi

# A development certificate signs cleanly and is still refused by Gatekeeper.
# Catching that here costs a second; catching it in a bug report costs a user.
case "$IDENTITY" in
  "Developer ID Application:"*) ;;
  *)
    echo "Refusing to build: '$IDENTITY' is not a Developer ID Application identity." >&2
    echo "That is exactly what shipped in v0.1.0 and could not be opened." >&2
    exit 1
    ;;
esac

if ! security find-identity -v -p codesigning | grep -qF "$IDENTITY"; then
  echo "No such identity in the keychain: $IDENTITY" >&2
  security find-identity -v -p codesigning >&2
  exit 1
fi

TAURI="$REPO_ROOT/ui/node_modules/.bin/tauri"
[ -x "$TAURI" ] || TAURI="$(command -v tauri || true)"
if [ -z "$TAURI" ] || [ ! -x "$TAURI" ]; then
  echo "Tauri CLI missing — run 'pnpm install' in ui/ first." >&2
  exit 1
fi

NOTARIZE=0
if [ "$SKIP_NOTARIZE" = "0" ]; then
  if [ -n "${NOTARY_PROFILE:-}" ]; then
    NOTARIZE=1
    NOTARY_ARGS=(--keychain-profile "$NOTARY_PROFILE")
  elif [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_PASSWORD:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ]; then
    NOTARIZE=1
    NOTARY_ARGS=(--apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID")
  else
    echo "==> No notarization credentials — the build will be signed but NOT notarized." >&2
    echo "    Set NOTARY_PROFILE, or APPLE_ID + APPLE_PASSWORD + APPLE_TEAM_ID." >&2
  fi
fi

# Tauri notarizes the .app itself only from these three, and a partial set makes
# it half-start a submission — so pass all three or none.
if [ "$NOTARIZE" = "1" ] && [ -z "${APPLE_PASSWORD:-}" ]; then
  echo "==> Tauri cannot read a keychain profile; the .app will be stapled after the fact."
  unset APPLE_ID APPLE_TEAM_ID 2>/dev/null || true
fi

echo "==> Building with identity: $IDENTITY"
export APPLE_SIGNING_IDENTITY="$IDENTITY"

BUILD_ARGS=(build)
[ -n "$TARGET" ] && BUILD_ARGS+=(--target "$TARGET")

cd "$REPO_ROOT/src-tauri"
"$TAURI" "${BUILD_ARGS[@]}"

TARGET_DIR="$REPO_ROOT/target"
[ -n "$TARGET" ] && TARGET_DIR="$TARGET_DIR/$TARGET"
APP="$TARGET_DIR/release/bundle/macos/WhimprFlow.app"
DMG="$(/usr/bin/find "$TARGET_DIR/release/bundle/dmg" -name "*.dmg" -print -quit 2>/dev/null || true)"

[ -d "$APP" ] || { echo "No WhimprFlow.app was produced." >&2; exit 1; }

# `tauri build` does not bundle the worker — there is no externalBin — so this has to
# put it there, exactly as install-macos.sh does for a local install. An app without it
# launches, records and transcribes perfectly and then pastes RAW, uncleaned text, with
# nothing in the UI to say why. On a cloud-cleanup install nothing is missed at all,
# which is what makes the gap survive testing: it only shows up for the local-model
# users, and it looks like cleanup being broken rather than absent.
#
# Before notarization, not after: the notarized ticket covers what was submitted, and
# adding a binary to a stapled bundle invalidates the signature it was granted for.
WORKER="$TARGET_DIR/release/whimpr-llm-worker"
if [ ! -x "$WORKER" ]; then
  echo "==> Building the local LLM worker"
  BUILD_WORKER=(cargo build --release -p whimpr-llm-worker)
  [ -n "$TARGET" ] && BUILD_WORKER+=(--target "$TARGET")
  (cd "$REPO_ROOT" && "${BUILD_WORKER[@]}")
fi
[ -x "$WORKER" ] || { echo "No whimpr-llm-worker binary at $WORKER" >&2; exit 1; }

echo "==> Adding the LLM worker to the bundle"
cp "$WORKER" "$APP/Contents/MacOS/whimpr-llm-worker"
# ditto preserves xattrs into the zip, so anything clinging to the build travels to
# whoever downloads it.
xattr -cr "$APP" 2>/dev/null || true
# Nested code first: signing the outer bundle seals what is inside it, so the reverse
# order invalidates the signature the moment it is made.
codesign --force --sign "$IDENTITY" --options runtime --timestamp \
  --entitlements "$REPO_ROOT/src-tauri/Entitlements.plist" \
  "$APP/Contents/MacOS/whimpr-llm-worker"
codesign --force --sign "$IDENTITY" --options runtime --timestamp \
  --entitlements "$REPO_ROOT/src-tauri/Entitlements.plist" "$APP"
codesign --verify --deep --strict "$APP"

if [ "$NOTARIZE" = "1" ]; then
  # The app first. Submitting it on its own means its ticket exists before the
  # dmg is ever assessed, and a stapled app keeps working with no network.
  if ! xcrun stapler validate "$APP" >/dev/null 2>&1; then
    echo "==> Notarizing WhimprFlow.app"
    ZIP="$(mktemp -d)/WhimprFlow.zip"
    /usr/bin/ditto -c -k --keepParent "$APP" "$ZIP"
    xcrun notarytool submit "$ZIP" "${NOTARY_ARGS[@]}" --wait
    xcrun stapler staple "$APP"
    rm -f "$ZIP"
  fi

  # The dmg Tauri produced still contains the *unstapled* app, so rebuild it
  # around the stapled one rather than shipping a container whose payload needs
  # the network to validate.
  if [ -n "$DMG" ]; then
    echo "==> Rebuilding the dmg around the stapled app"
    STAGE="$(mktemp -d)"
    cp -R "$APP" "$STAGE/WhimprFlow.app"
    ln -s /Applications "$STAGE/Applications"
    rm -f "$DMG"
    hdiutil create -volname "WhimprFlow" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
    rm -rf "$STAGE"

    codesign --force --sign "$IDENTITY" --timestamp "$DMG"
    echo "==> Notarizing the disk image itself"
    xcrun notarytool submit "$DMG" "${NOTARY_ARGS[@]}" --wait
    xcrun stapler staple "$DMG"
  fi
fi

# The two assets setup-macos.sh fetches by exact name from releases/latest, made
# here so a release cannot go out without them or with a checksum of some other
# zip. Bare hex in the .sha256, which is what the installer compares against.
BUNDLE_DIR="$(dirname "$(dirname "$APP")")"
ZIP_ASSET="$BUNDLE_DIR/WhimprFlow.app.zip"
rm -f "$ZIP_ASSET" "$ZIP_ASSET.sha256"
/usr/bin/ditto -c -k --keepParent "$APP" "$ZIP_ASSET"
shasum -a 256 "$ZIP_ASSET" | cut -d' ' -f1 > "$ZIP_ASSET.sha256"

VERSION="$(defaults read "$APP/Contents/Info.plist" CFBundleShortVersionString)"
echo
echo "App: $APP"
[ -n "$DMG" ] && echo "Dmg: $DMG"
echo "Zip: $ZIP_ASSET (+ .sha256)"
echo "Now run: scripts/verify-macos.sh, then publish with:"
echo "  gh release create v$VERSION --title \"WhimprFlow v$VERSION\" --notes-file <notes.md> \\"
echo "    \"$ZIP_ASSET\" \"$ZIP_ASSET.sha256\"${DMG:+ \\
    \"$DMG\"}"
