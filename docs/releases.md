# Releases

Nightshift ships on two channels from two kinds of branch. The [Release workflow](../.github/workflows/release.yml) builds and publishes both.

| Channel | Branch | Version | GitHub release |
| --- | --- | --- | --- |
| Nightly (beta) | `main` | `<Cargo.toml version>-nightly.<run number>` | Prerelease `v0.2.0-nightly.57`, one per push |
| Stable | `release/X.Y` | `Cargo.toml` version, `X.Y.Z` | `vX.Y.Z`, published when that version first lands on the branch |

Every release is permanent and separate. The workflow never moves a tag or replaces an asset.

## Version

`Cargo.toml` is the only place the version is written. The workflow passes the full release version to the build as `NIGHTSHIFT_VERSION`, and it shows up in four places:

- next to the app name in the sidebar
- `nightshift --version`
- a `VERSION` file in each package (`Nightshift.app/Contents/Resources/VERSION` on macOS, `/usr/share/doc/nightshift/VERSION` on Linux), and the Debian package version
- the release title and tag

Local builds without `NIGHTSHIFT_VERSION` show the plain `Cargo.toml` version. The macOS `Info.plist` always uses the plain version, because macOS requires a numeric one.

`main` carries the next minor version, so nightlies sort after the newest stable release.

## Cut a stable release

1. Check that `Cargo.toml` on `main` has version `X.Y.0`.
2. Run `git switch -c release/X.Y main && git push -u origin release/X.Y`. The workflow publishes `vX.Y.0`.
3. On `main`, open a PR that bumps `Cargo.toml` to `X.(Y+1).0`.

## Fix a released version

1. Branch from the release line, for example `git switch -c fix/<topic> origin/release/X.Y`.
2. Fix the problem and bump `Cargo.toml` to `X.Y.(Z+1)` in the same PR.
3. Open the PR against `release/X.Y`. Merging it publishes `vX.Y.(Z+1)`. A push that leaves the version unchanged builds but doesn't publish.
4. If `main` has the same bug, cherry-pick the fix onto `main` in a separate PR. Never merge a release branch into `main`.

The workflow rejects branches named `release/X.Y.Z` and fails when `Cargo.toml` on `release/X.Y` doesn't hold an `X.Y.*` version.

## Builds

Each release has these packages, plus `SHA256SUMS`:

| File | Platform |
| --- | --- |
| `nightshift-macos-aarch64.dmg` | macOS on Apple silicon |
| `nightshift-macos-x86_64.dmg` | macOS on Intel |
| `nightshift-linux-x86_64.deb` | Debian and Ubuntu on x86_64 |
| `nightshift-linux-aarch64.deb` | Debian and Ubuntu on aarch64 |

GitHub adds the "Source code" archives to every release itself. They can't be removed, and you don't need them to install.

Pull requests build and verify every package without publishing. Locally, `sh scripts/bundle-dmg.sh` and `sh scripts/bundle-deb.sh` build the same files.

### macOS

Open the disk image and drag Nightshift into Applications. The app has an ad-hoc signature and isn't notarized, so macOS blocks the first launch. Allow it in **System Settings → Privacy & Security → Open Anyway**, or run:

```sh
xattr -dr com.apple.quarantine /Applications/Nightshift.app
```

### Linux

Install with `sudo apt install ./nightshift-linux-x86_64.deb`. apt pulls in the libraries Nightshift needs, plus Git and the Vulkan loader. The package adds a desktop entry, so Nightshift appears in application launchers. It has no icon yet. The packages are built on Ubuntu 22.04 and need glibc 2.35 or newer.
