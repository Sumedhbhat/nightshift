# Nightshift

A native macOS and Linux scaffold for adding local projects and fetching issues from public GitHub repositories.

## Install a release

Every push to `main` replaces the `nightly` prerelease on the [releases page](https://github.com/Sumedhbhat/nightshift/releases). Pushing a `vX.Y.Z` tag that matches the `Cargo.toml` version publishes a versioned release. Each release has archives for macOS (Apple silicon, Intel) and Linux (x86_64, aarch64), plus `SHA256SUMS` and `install.sh`.

The repository is private, so downloads need the [GitHub CLI](https://cli.github.com) signed in with `gh auth login`. To install or update to the latest `main` build:

```sh
gh release download nightly -R Sumedhbhat/nightshift -p install.sh -O - | sh
```

To install a specific version, pass its tag: `... | sh -s v0.1.0`. The script verifies the checksum. On macOS it installs `~/Applications/Nightshift.app`; on Linux it installs `~/.local/bin/nightshift` and a desktop entry. Set `NIGHTSHIFT_APP_DIR` or `NIGHTSHIFT_PREFIX` to install somewhere else.

Linux builds need glibc 2.35 or newer, Git, and a Wayland or X11 session with Vulkan drivers. The macOS bundle has only an ad-hoc signature and is not notarized. The script clears its quarantine flag. If you download the zip manually, right-click the app and choose **Open** the first time.

To cut a versioned release, bump `version` in `Cargo.toml` and merge it. Then tag `main`:

```sh
git tag v0.1.0 && git push origin v0.1.0
```

## Run from source

Requires Rust through rustup. On macOS, also install Xcode's macOS toolchain. On Debian-based Linux, run `sh scripts/install-linux-deps.sh`. Rust and GPUI are pinned; Cargo fetches the dependencies without needing sibling checkouts.

```sh
cargo run --locked
```

Or build a local app bundle:

```sh
sh scripts/bundle.sh --release        # macOS: target/release/Nightshift.app
sh scripts/bundle-linux.sh            # Linux: target/release/nightshift-linux-<arch>.tar.gz
```

## Use

1. Click **Add project** and select an existing Git repository with at least one commit and a public `github.com` remote.
2. If its remotes point to different GitHub repositories, choose one.
3. Click **Search issues**. Nightshift fetches open issues across your added projects, excluding pull requests.
4. Select a project in the sidebar to view its results. Click an issue to open it on GitHub.
5. Click **Filter: all open issues** to search only issues labeled `ready-for-agent`. Click again to show all open issues.

Projects persist between launches. Issue results are fetched when you search. Public access needs no sign-in; GitHub's unauthenticated API rate limits apply, and failures appear in the window. Private repositories, Codex execution, worktrees, and pull requests are not implemented.

Command-O adds a project; Command-R searches; Command-Q quits. On Linux, use Ctrl instead of Command.

## Development

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
```

Application data lives in `~/Library/Application Support/Nightshift` on macOS and `$XDG_DATA_HOME/nightshift` (default `~/.local/share/nightshift`) on Linux. Set `NIGHTSHIFT_DATA_DIR` to a separate directory for manual testing. The original synthetic replay is retained as a library test utility and is no longer in the UI.

See [public issue discovery](docs/public-issue-discovery.md), [storage](docs/storage-implementation.md), and the [implementation plan](docs/implementation-plan.md). The [Release workflow](.github/workflows/release.yml) builds and publishes the release archives.

## License

Nightshift is available under the [MIT License](LICENSE).
