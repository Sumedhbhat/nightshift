#!/bin/sh
# Build a local app bundle so macOS can identify and launch Nightshift.
set -eu
cd "$(dirname "$0")/.."
profile=debug
case "${1:-}" in
  "") cargo build --locked ;;
  --release) profile=release; cargo build --locked --release ;;
  *) echo "Usage: sh scripts/bundle.sh [--release]" >&2; exit 2 ;;
esac
# Info.plist needs the plain numeric version; CI sets NIGHTSHIFT_VERSION to the full release version.
version="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)"
bundle="target/$profile/Nightshift.app"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
echo "${NIGHTSHIFT_VERSION:-$version}" > "$bundle/Contents/Resources/VERSION"
cp "target/$profile/nightshift" "$bundle/Contents/MacOS/nightshift"
# Scale the 1024px source icon to every size an .icns file holds.
iconset="$(mktemp -d)/Nightshift.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" packaging/icons/nightshift.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z "$double" "$double" packaging/icons/nightshift.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil --convert icns --output "$bundle/Contents/Resources/Nightshift.icns" "$iconset"
rm -rf "$(dirname "$iconset")"
cat > "$bundle/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleExecutable</key><string>nightshift</string>
  <key>CFBundleIconFile</key><string>Nightshift</string>
  <key>CFBundleIdentifier</key><string>dev.nightshift.app</string>
  <key>CFBundleName</key><string>Nightshift</string>
  <key>CFBundleDisplayName</key><string>Nightshift</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
# Ad-hoc sign the whole bundle so its signature covers Info.plist as well as the executable.
codesign --force --sign - "$bundle"
echo "$bundle"
