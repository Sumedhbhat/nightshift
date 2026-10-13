#!/bin/sh
# Build a Linux release archive containing the binary and desktop entry.
set -eu
cd "$(dirname "$0")/.."
case "$(uname -s)" in
  Linux) ;;
  *) echo "scripts/bundle-linux.sh can only run on Linux." >&2; exit 2 ;;
esac
cargo build --locked --release
architecture="$(uname -m)"
name="nightshift-linux-$architecture"
stage="target/release/$name"
rm -rf "$stage"
mkdir -p "$stage/bin" "$stage/share/applications"
cp target/release/nightshift "$stage/bin/nightshift"
cp packaging/linux/dev.nightshift.app.desktop "$stage/share/applications/dev.nightshift.app.desktop"
tar -C target/release -czf "target/release/$name.tar.gz" "$name"
echo "target/release/$name.tar.gz"
