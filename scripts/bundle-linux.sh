#!/bin/sh
# Build a Linux release archive containing the binary, desktop entry, and VERSION file.
set -eu
cd "$(dirname "$0")/.."
case "$(uname -s)" in
  Linux) ;;
  *) echo "scripts/bundle-linux.sh can only run on Linux." >&2; exit 2 ;;
esac
cargo build --locked --release
version="${NIGHTSHIFT_VERSION:-$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)}"
architecture="$(uname -m)"
name="nightshift-linux-$architecture"
stage="target/release/$name"
rm -rf "$stage"
mkdir -p "$stage/bin" "$stage/share/applications"
cp target/release/nightshift "$stage/bin/nightshift"
cp packaging/linux/dev.nightshift.app.desktop "$stage/share/applications/dev.nightshift.app.desktop"
echo "$version" > "$stage/VERSION"
tar -C target/release -czf "target/release/$name.tar.gz" "$name"
echo "target/release/$name.tar.gz"
