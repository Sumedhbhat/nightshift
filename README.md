# Nightshift

A native macOS and Linux app for adding local projects and fetching their GitHub issues through the GitHub CLI.

## Install a release

Stable releases come from `release/X.Y` branches. Nightly builds come from every push to `main`, for anyone who wants changes sooner. Both are on the [releases page](https://github.com/Sumedhbhat/nightshift/releases).

Download the package for your machine from a release:

- macOS on Apple silicon: `nightshift-macos-aarch64.dmg`. Intel Macs: `nightshift-macos-x86_64.dmg`. Open it and drag Nightshift into Applications.
- Debian or Ubuntu: `nightshift-linux-<arch>.deb`. Install it with `sudo apt install ./nightshift-linux-x86_64.deb`.

The macOS app isn't notarized yet, so the first launch needs one extra step. See [releases](docs/releases.md#macos). The installed version appears next to the app name and from `nightshift --version`.

Linux builds need glibc 2.35 or newer, Git, and a Wayland or X11 session with Vulkan drivers. See [releases](docs/releases.md) for how releases are cut and patched.

## Run from source

Requires Rust through rustup. On macOS, also install Xcode's macOS toolchain. On Debian-based Linux, run `sh scripts/install-linux-deps.sh`. Rust and GPUI are pinned; Cargo fetches the dependencies without needing sibling checkouts.

```sh
cargo run --locked
```

Or build a local app bundle:

```sh
sh scripts/bundle.sh --release        # macOS: target/release/Nightshift.app
sh scripts/bundle-dmg.sh              # macOS: target/release/nightshift-macos-<arch>.dmg
sh scripts/bundle-deb.sh              # Linux: target/release/nightshift-linux-<arch>.deb
```

## Use

1. Click **Add project** and select an existing Git repository with at least one commit and a `github.com` remote that your GitHub CLI account can access.
2. If its remotes point to different GitHub repositories, choose one.
3. Click **Search issues**. Nightshift fetches open issues across your added projects, excluding pull requests.
4. Select a project in the sidebar to view its results. Click an issue to open it on GitHub.
5. Click **Filter: all open issues** to search only issues labeled `ready-for-agent`. Click again to show all open issues.

Projects persist between launches. Issue results are fetched when you search. Install the GitHub CLI and run `gh auth login` first; Nightshift uses that account and never signs in by itself. Failures appear in the window.

Command-O adds a project; Command-R searches; Command-Q quits. On Linux, use Ctrl instead of Command.

## Development

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
```

Application data lives in `~/Library/Application Support/Nightshift` on macOS and `$XDG_DATA_HOME/nightshift` (default `~/.local/share/nightshift`) on Linux. Set `NIGHTSHIFT_DATA_DIR` to a separate directory for manual testing. The original synthetic replay is retained as a library test utility and is no longer in the UI.

See [public issue discovery](docs/public-issue-discovery.md), [storage](docs/storage-implementation.md), and the [implementation plan](docs/implementation-plan.md). See [releases](docs/releases.md) for the release workflow.

## License

Nightshift is available under the [MIT License](LICENSE).
