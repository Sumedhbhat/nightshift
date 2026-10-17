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
- a `VERSION` file in each archive (`Nightshift.app/Contents/Resources/VERSION` on macOS)
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

Each release has archives for macOS (Apple silicon, Intel) and Linux (x86_64, aarch64), plus `install.sh` and `SHA256SUMS`. Pull requests build and verify every archive without publishing. Linux builds run on Ubuntu 22.04 and need glibc 2.35 or newer.

The macOS bundle has an ad-hoc signature and isn't notarized. `install.sh` clears its quarantine flag. Someone who downloads the zip by hand has to right-click the app and choose **Open** the first time.

The Linux archive includes a desktop entry, `dev.nightshift.app.desktop`, so Nightshift appears in application launchers. It has no icon yet.
