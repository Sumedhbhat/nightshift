#!/bin/sh
# Build a Debian package containing the binary, desktop entry, and icon.
set -eu
cd "$(dirname "$0")/.."
case "$(uname -s)" in
  Linux) ;;
  *) echo "scripts/bundle-deb.sh can only run on Linux." >&2; exit 2 ;;
esac
cargo build --locked --release
version="${NIGHTSHIFT_VERSION:-$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)}"
architecture="$(dpkg --print-architecture)"
case "$(uname -m)" in
  x86_64) name=nightshift-linux-x86_64 ;;
  aarch64) name=nightshift-linux-aarch64 ;;
  *) name="nightshift-linux-$(uname -m)" ;;
esac
stage="target/release/deb"
rm -rf "$stage"
mkdir -p "$stage/DEBIAN" "$stage/usr/bin" "$stage/usr/share/applications" "$stage/usr/share/pixmaps" "$stage/usr/share/doc/nightshift"
install -m 755 target/release/nightshift "$stage/usr/bin/nightshift"
install -m 644 packaging/linux/dev.nightshift.app.desktop "$stage/usr/share/applications/dev.nightshift.app.desktop"
install -m 644 packaging/icons/nightshift.png "$stage/usr/share/pixmaps/dev.nightshift.app.png"
install -m 644 LICENSE "$stage/usr/share/doc/nightshift/copyright"
echo "$version" > "$stage/usr/share/doc/nightshift/VERSION"

# Derive library dependencies from the binary. GPUI loads Vulkan at runtime, and
# Nightshift runs git, so neither shows up as a linked library.
shlibs="$(mktemp -d)"
mkdir -p "$shlibs/debian"
printf 'Source: nightshift\n\nPackage: nightshift\nArchitecture: any\n' > "$shlibs/debian/control"
binary="$(pwd)/$stage/usr/bin/nightshift"
libraries="$(cd "$shlibs" && dpkg-shlibdeps -O "$binary" | sed -n 's/^shlibs:Depends=//p')"
rm -rf "$shlibs"

cat > "$stage/DEBIAN/control" <<CONTROL
Package: nightshift
Version: $version
Architecture: $architecture
Maintainer: Sumedh Bhat
Depends: $libraries, libvulkan1, git
Section: devel
Priority: optional
Homepage: https://github.com/Sumedhbhat/nightshift
Description: Fetch GitHub issues for your local projects
CONTROL
dpkg-deb --root-owner-group --build "$stage" "target/release/$name.deb" >/dev/null
echo "target/release/$name.deb"
