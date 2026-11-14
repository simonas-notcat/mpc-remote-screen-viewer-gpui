#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "$0")/.."

if [[ $(uname -s) != Darwin ]]; then
  echo 'Bundling requires macOS, full Xcode (including Metal), and its command-line tools.' >&2
  exit 1
fi
if [[ $# -gt 1 || ( $# -eq 1 && $1 != --universal ) ]]; then
  echo 'Usage: scripts/bundle-macos.sh [--universal]' >&2
  exit 2
fi

app='target/bundle/MPC Remote Screen Viewer GPUI.app'
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
if [[ ${1:-} == --universal ]]; then
  # Install both Rust targets first; build the Rust app for each architecture.
  for target in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --locked --release --features desktop --target "$target"
  done
  lipo -create \
    target/aarch64-apple-darwin/release/mpc-remote-screen-viewer-gpui \
    target/x86_64-apple-darwin/release/mpc-remote-screen-viewer-gpui \
    -output "$app/Contents/MacOS/mpc-remote-screen-viewer-gpui"
else
  cargo build --locked --release --features desktop
  cp target/release/mpc-remote-screen-viewer-gpui "$app/Contents/MacOS/"
fi
cp packaging/macos/Info.plist "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"
# Development signature only. Distribution signing/notarization is a separate gate.
codesign --force --sign - "$app"
codesign --verify --strict "$app"
printf 'Created development bundle: %s\n' "$app"
