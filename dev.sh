#!/bin/bash
# Run WhimprFlow in development: starts the Vite UI server + the app with hot reload.
# The app loads its UI from the dev server, so the pill actually renders.
set -e
cd "$(dirname "$0")"
TAURI="$PWD/ui/node_modules/.bin/tauri"
# Run from src-tauri, not the repo root: from the root the CLI picks ui/ as the
# app dir (the only package.json), so beforeDevCommand's `pnpm --dir ui dev`
# resolves to ui/ui and dies with ENOENT. scripts/build-macos.sh already does this.
cd src-tauri
exec "$TAURI" dev "$@"
