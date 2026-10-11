#!/bin/sh
# Build a local app bundle so macOS can identify and launch Nightshift.
set -eu
cd "$(dirname "$0")/.."
case "$(uname -s)" in
  Darwin) ;;
  *) echo "This script creates a macOS .app bundle and can only run on macOS." >&2; exit 2 ;;
esac
profile=debug
case "${1:-}" in
  "") cargo build --locked ;;
  --release) profile=release; cargo build --locked --release ;;
  *) echo "Usage: sh scripts/bundle.sh [--release]" >&2; exit 2 ;;
esac
bundle="target/$profile/Nightshift.app"
mkdir -p "$bundle/Contents/MacOS"
cp "target/$profile/nightshift" "$bundle/Contents/MacOS/nightshift"
cat > "$bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleExecutable</key><string>nightshift</string>
  <key>CFBundleIdentifier</key><string>dev.nightshift.app</string>
  <key>CFBundleName</key><string>Nightshift</string>
  <key>CFBundleDisplayName</key><string>Nightshift</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
echo "$bundle"
