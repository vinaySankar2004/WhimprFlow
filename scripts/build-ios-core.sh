#!/usr/bin/env bash
#
# Build whimpr-ffi for iOS and package it as WhimprCore.xcframework.
#
# Two slices, and both are needed: `aarch64-apple-ios` is the device, and
# `aarch64-apple-ios-sim` is the simulator on an Apple Silicon Mac. They are
# different targets producing incompatible binaries, and an xcframework is the only
# packaging Xcode will pick between automatically — a fat `lipo` archive containing
# both *cannot* be built, because the two slices have the same architecture
# (arm64) and differ only in platform.
#
# Run from anywhere. Xcode calls this from a build phase; see ios/README.md.

set -euo pipefail

# Xcode runs a build phase with a minimal PATH and without sourcing any shell
# profile, so `~/.cargo/bin` is simply absent and the script dies with
# "rustup: command not found" — surfacing in Xcode as the uninformative "Command
# PhaseScriptExecution failed with a nonzero exit code". Running the same script
# from a terminal works, because an interactive shell has already added it, which
# makes this look like a project problem rather than an environment one.
if [ -f "$HOME/.cargo/env" ]; then
  # shellcheck source=/dev/null
  . "$HOME/.cargo/env"
fi
# Homebrew too, for a toolchain installed that way rather than through rustup.
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

for tool in cargo rustup; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "error: $tool is not on PATH." >&2
    echo "  Building the iOS app needs the Rust toolchain: https://rustup.rs" >&2
    echo "  If it is installed, it is somewhere this script does not look;" >&2
    echo "  PATH was: $PATH" >&2
    exit 1
  }
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/ios/Frameworks"
FRAMEWORK="$OUT/WhimprCore.xcframework"
HEADERS="$ROOT/crates/whimpr-ffi/include"

# Debug builds are ~10x slower through the bridge and it is not on any hot path, but
# release also strips, which makes a Rust panic message useless in a crash log. The
# app is for three people; keep the symbols.
PROFILE="${PROFILE:-release}"
case "$PROFILE" in
  release) CARGO_FLAGS="--release"; DIR="release" ;;
  debug)   CARGO_FLAGS="";          DIR="debug"   ;;
  *) echo "PROFILE must be 'release' or 'debug', got '$PROFILE'" >&2; exit 1 ;;
esac

for target in aarch64-apple-ios aarch64-apple-ios-sim; do
  if ! rustup target list --installed | grep -qx "$target"; then
    echo "==> installing missing Rust target $target"
    rustup target add "$target"
  fi
done

echo "==> building whimpr-ffi ($PROFILE)"
cd "$ROOT"
# -p keeps this to the FFI crate and its dependencies. The workspace also contains
# `src-tauri`, which is macOS-only and would fail for an iOS target.
cargo build -p whimpr-ffi $CARGO_FLAGS --target aarch64-apple-ios
cargo build -p whimpr-ffi $CARGO_FLAGS --target aarch64-apple-ios-sim

DEVICE_LIB="$ROOT/target/aarch64-apple-ios/$DIR/libwhimpr_ffi.a"
SIM_LIB="$ROOT/target/aarch64-apple-ios-sim/$DIR/libwhimpr_ffi.a"
for lib in "$DEVICE_LIB" "$SIM_LIB"; do
  [ -f "$lib" ] || { echo "cargo did not produce $lib" >&2; exit 1; }
done

echo "==> packaging $FRAMEWORK"
# Rebuilt from scratch every time: xcodebuild refuses to write into an existing
# xcframework, and a stale slice inside one is invisible until it fails to link.
rm -rf "$FRAMEWORK"
mkdir -p "$OUT"
xcodebuild -create-xcframework \
  -library "$DEVICE_LIB" -headers "$HEADERS" \
  -library "$SIM_LIB"    -headers "$HEADERS" \
  -output "$FRAMEWORK" >/dev/null

echo "==> done"
xcodebuild -version >/dev/null # fail loudly if the toolchain vanished mid-run
find "$FRAMEWORK" -name '*.a' -exec sh -c 'echo "    $1 ($(lipo -archs "$1"))"' _ {} \;
