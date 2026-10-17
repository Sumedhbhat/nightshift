#!/bin/sh
# Install a Nightshift release from GitHub on macOS or Linux.
# Usage: sh install.sh [stable|nightly|vX.Y.Z]   (default: stable)
set -eu
repo="${NIGHTSHIFT_REPO:-Sumedhbhat/nightshift}"
channel="${1:-stable}"

fail() { echo "nightshift install: $*" >&2; exit 1; }

command -v curl >/dev/null 2>&1 || fail "curl is required."
case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  arm64 | aarch64) arch=aarch64 ;;
  *) fail "unsupported architecture $(uname -m)." ;;
esac
case "$(uname -s)" in
  Darwin) asset="nightshift-macos-$arch.zip" ;;
  Linux) asset="nightshift-linux-$arch.tar.gz" ;;
  *) fail "unsupported operating system $(uname -s). Download a release manually." ;;
esac

case "$channel" in
  stable) base="https://github.com/$repo/releases/latest/download" ;;
  nightly)
    # Releases are listed newest first; take the first prerelease.
    tag="$(curl -fsSL "https://api.github.com/repos/$repo/releases?per_page=50" |
      awk -F'"' '/"tag_name":/ { tag = $4 } /"prerelease": true/ { print tag; exit }')" ||
      fail "could not list releases for $repo."
    [ -n "$tag" ] || fail "no nightly release found for $repo."
    base="https://github.com/$repo/releases/download/$tag"
    ;;
  v*) base="https://github.com/$repo/releases/download/$channel" ;;
  *) fail "unknown channel '$channel'. Use stable, nightly, or a tag such as v0.1.0." ;;
esac

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
echo "Downloading $asset ($channel)"
curl -fsSL -o "$work/$asset" "$base/$asset" ||
  fail "no $asset in the $channel release. If no stable release exists yet, try: sh install.sh nightly"
curl -fsSL -o "$work/SHA256SUMS" "$base/SHA256SUMS" || fail "the release has no SHA256SUMS."
(
  cd "$work"
  grep "  $asset\$" SHA256SUMS > expected || fail "SHA256SUMS has no entry for $asset."
  if command -v sha256sum >/dev/null 2>&1; then sha256sum -c expected; else shasum -a 256 -c expected; fi
)

case "$asset" in
  *.zip)
    destination="${NIGHTSHIFT_APP_DIR:-$HOME/Applications}"
    ditto -x -k "$work/$asset" "$work"
    mkdir -p "$destination"
    rm -rf "$destination/Nightshift.app"
    mv "$work/Nightshift.app" "$destination/Nightshift.app"
    # The bundle is not notarized; clear quarantine so Gatekeeper allows it to open.
    xattr -dr com.apple.quarantine "$destination/Nightshift.app" 2>/dev/null || true
    echo "Installed Nightshift $(cat "$destination/Nightshift.app/Contents/Resources/VERSION") to $destination/Nightshift.app"
    echo "Run: open \"$destination/Nightshift.app\""
    ;;
  *.tar.gz)
    prefix="${NIGHTSHIFT_PREFIX:-$HOME/.local}"
    tar -xzf "$work/$asset" -C "$work"
    stage="$work/${asset%.tar.gz}"
    mkdir -p "$prefix/bin" "$prefix/share/applications"
    install -m 755 "$stage/bin/nightshift" "$prefix/bin/nightshift"
    # Launchers may not search ~/.local/bin, so point the desktop entry at the installed path.
    sed "s|^Exec=.*|Exec=$prefix/bin/nightshift|" "$stage/share/applications/dev.nightshift.app.desktop" \
      > "$prefix/share/applications/dev.nightshift.app.desktop"
    echo "Installed Nightshift $(cat "$stage/VERSION") to $prefix/bin/nightshift"
    case ":$PATH:" in
      *":$prefix/bin:"*) echo "Run: nightshift" ;;
      *) echo "Run: $prefix/bin/nightshift (add $prefix/bin to PATH to run 'nightshift')" ;;
    esac
    ;;
esac
