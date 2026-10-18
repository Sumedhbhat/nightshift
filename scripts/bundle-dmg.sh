#!/bin/sh
# Build a disk image that installs Nightshift.app by dragging it to Applications.
set -eu
cd "$(dirname "$0")/.."
case "$(uname -s)" in
  Darwin) ;;
  *) echo "scripts/bundle-dmg.sh can only run on macOS." >&2; exit 2 ;;
esac
sh scripts/bundle.sh --release
case "$(uname -m)" in
  arm64) name=nightshift-macos-aarch64 ;;
  *) name="nightshift-macos-$(uname -m)" ;;
esac
stage="target/release/dmg"
rm -rf "$stage" "target/release/$name.dmg"
mkdir -p "$stage"
cp -R target/release/Nightshift.app "$stage/Nightshift.app"
ln -s /Applications "$stage/Applications"
hdiutil create -volname Nightshift -srcfolder "$stage" -format UDZO -ov "target/release/$name.dmg" >/dev/null
echo "target/release/$name.dmg"
