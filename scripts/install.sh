#!/bin/sh
# Install a Nightshift build from GitHub Releases on macOS or Linux.
# Usage: sh install.sh [nightly|vX.Y.Z]
# The repository is private, so downloads go through an authenticated GitHub CLI.
set -eu
repo="${NIGHTSHIFT_REPO:-Sumedhbhat/nightshift}"
tag="${1:-nightly}"

fail() { echo "nightshift install: $*" >&2; exit 1; }

command -v gh >/dev/null 2>&1 || fail "install the GitHub CLI (https://cli.github.com) and run 'gh auth login'."
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

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
echo "Downloading $asset from $repo release $tag"
gh release download "$tag" --repo "$repo" --pattern "$asset" --pattern SHA256SUMS --dir "$work"
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
    echo "Installed $destination/Nightshift.app"
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
    echo "Installed $prefix/bin/nightshift"
    case ":$PATH:" in
      *":$prefix/bin:"*) echo "Run: nightshift" ;;
      *) echo "Run: $prefix/bin/nightshift (add $prefix/bin to PATH to run 'nightshift')" ;;
    esac
    ;;
esac
