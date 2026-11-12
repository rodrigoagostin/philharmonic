#!/usr/bin/env bash
# Build Philharmonic.app: one double-clickable app with the daemon and `ph` bundled inside.
# Usage: scripts/build-app.sh [--install]   (--install copies it to ~/Applications)
set -euo pipefail
cd "$(dirname "$0")/.."

triple=$(rustc -vV | sed -n 's/^host: //p')

echo "==> Building daemon + CLI (release)"
cargo build --release -p philharmonicd -p ph

# Tauri bundles these "sidecars" next to the app binary (Contents/MacOS/), where the app
# looks for philharmonicd and the daemon looks for ph.
mkdir -p app/src-tauri/binaries
cp target/release/philharmonicd "app/src-tauri/binaries/philharmonicd-$triple"
cp target/release/ph "app/src-tauri/binaries/ph-$triple"

echo "==> Building the app"
cd app
[ -d node_modules ] || npm install
npm run tauri build -- --config src-tauri/tauri.bundle.json --bundles app
cd ..

app_path="target/release/bundle/macos/Philharmonic.app"
echo "==> Built $app_path"

if [ "${1:-}" = "--install" ]; then
  mkdir -p "$HOME/Applications"
  rm -rf "$HOME/Applications/Philharmonic.app"
  cp -R "$app_path" "$HOME/Applications/"
  echo "==> Installed to ~/Applications/Philharmonic.app"
fi
