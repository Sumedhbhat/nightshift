//! Self-update from Nightshift's GitHub releases. Nightly builds follow nightly prereleases and
//! stable builds follow stable releases; local and pull-request builds never update. Releases are
//! found and downloaded through the authenticated GitHub CLI and checked against the release's
//! `SHA256SUMS`. A downloaded update installs only when the owner asks to restart.
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    process::{Output, Stdio},
    sync::OnceLock,
    time::Duration,
};

pub const REPOSITORY: &str = "Sumedhbhat/nightshift";
/// The first check waits so it never competes with startup work.
pub const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);
pub const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);
const QUERY_TIMEOUT: Duration = Duration::from_secs(60);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const CHECKSUMS: &str = "SHA256SUMS";
/// Linux packages install the binary here; only that copy updates itself.
const PACKAGE_BINARY: &str = "/usr/bin/nightshift";
const PACKAGE_MARKER: &str = "/usr/share/doc/nightshift/VERSION";

/// Set once an update is installed; `relaunch_if_installed` starts this after shutdown.
static RELAUNCH: OnceLock<PathBuf> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Nightly,
    Stable,
}

/// A release version: `X.Y.Z` on the stable channel or `X.Y.Z-nightly.N` on the nightly channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    core: (u64, u64, u64),
    nightly: Option<u64>,
}

impl Version {
    /// Parses a release version. Pull-request builds (`-dev.N`) and other suffixes have no channel.
    pub fn parse(text: &str) -> Option<Self> {
        let (core, suffix) = match text.split_once('-') {
            Some((core, suffix)) => (core, Some(suffix)),
            None => (text, None),
        };
        let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
        let (Some(Some(major)), Some(Some(minor)), Some(Some(patch)), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        let nightly = match suffix {
            None => None,
            Some(suffix) => Some(suffix.strip_prefix("nightly.")?.parse().ok()?),
        };
        Some(Self {
            core: (major, minor, patch),
            nightly,
        })
    }

    pub fn channel(&self) -> Channel {
        if self.nightly.is_some() {
            Channel::Nightly
        } else {
            Channel::Stable
        }
    }

    /// The running build's version, when CI built it for a release channel.
    pub fn current() -> Option<Self> {
        option_env!("NIGHTSHIFT_VERSION").and_then(Self::parse)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Asset {
    pub name: String,
}

/// The newest published release on `current`'s channel that is newer and has `asset` and checksums.
pub fn newest<'a>(current: Version, releases: &'a [Release], asset: &str) -> Option<&'a Release> {
    let has = |release: &Release, name: &str| release.assets.iter().any(|a| a.name == name);
    releases
        .iter()
        .filter(|release| !release.draft)
        .filter_map(|release| {
            Some((
                Version::parse(release.tag_name.strip_prefix('v')?)?,
                release,
            ))
        })
        .filter(|(version, release)| {
            version.channel() == current.channel()
                && release.prerelease == (version.channel() == Channel::Nightly)
                && *version > current
                && has(release, asset)
                && has(release, CHECKSUMS)
        })
        .max_by_key(|(version, _)| *version)
        .map(|(_, release)| release)
}

/// The expected digest for `asset` in a `sha256sum`-format listing.
pub fn expected_checksum<'a>(sums: &'a str, asset: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let (digest, name) = line.split_once(char::is_whitespace)?;
        let name = name.trim_start();
        (name.strip_prefix('*').unwrap_or(name) == asset).then_some(digest)
    })
}

/// The release package for this operating system and architecture.
pub fn asset_name() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "nightshift-macos-aarch64.dmg",
        ("macos", "x86_64") => "nightshift-macos-x86_64.dmg",
        ("linux", "x86_64") => "nightshift-linux-x86_64.deb",
        ("linux", "aarch64") => "nightshift-linux-aarch64.deb",
        _ => return None,
    })
}

#[derive(Clone, Debug)]
enum Target {
    /// The running `Nightshift.app`, replaced in place.
    Bundle(PathBuf),
    /// The installed Debian package, upgraded through apt.
    Package,
}

/// A downloaded, verified update waiting for the owner to restart.
#[derive(Clone, Debug)]
pub struct Prepared {
    pub version: String,
    /// The staged app bundle on macOS, or the downloaded package on Linux.
    path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct Updater {
    current: Version,
    asset: &'static str,
    target: Target,
    directory: PathBuf,
    gh: PathBuf,
}

impl Updater {
    /// Returns an updater only for release builds installed from a release package.
    pub fn detect() -> Option<Self> {
        let current = Version::current()?;
        let asset = asset_name()?;
        let platform = crate::platform::current();
        let directory = platform.data_directory()?.join("updates");
        let executable = std::env::current_exe().ok()?.canonicalize().ok()?;
        let target = if cfg!(target_os = "macos") {
            let bundle = executable.parent()?.parent()?.parent()?.to_owned();
            // Translocated or disk-image copies are read-only; only an installed copy can update.
            (bundle.extension()? == "app" && writable(bundle.parent()?)).then_some(())?;
            Target::Bundle(bundle)
        } else {
            (executable == Path::new(PACKAGE_BINARY) && Path::new(PACKAGE_MARKER).exists())
                .then_some(())?;
            Target::Package
        };
        let gh = platform
            .var_os("NIGHTSHIFT_GH")
            .map(PathBuf::from)
            .unwrap_or_else(|| "gh".into());
        Some(Self {
            current,
            asset,
            target,
            directory,
            gh,
        })
    }

    /// Downloads, verifies, and stages the newest release on this build's channel, if any.
    pub async fn prepare_newest(&self) -> Result<Option<Prepared>, String> {
        if let Target::Bundle(bundle) = &self.target {
            // The bundle a previous update replaced; the running app no longer uses it.
            let _ = std::fs::remove_dir_all(sibling(bundle, "previous"));
        }
        let releases = self.gh(&["api", &format!("repos/{REPOSITORY}/releases?per_page=30")]);
        let releases = run(releases, QUERY_TIMEOUT, "release check").await?;
        let releases: Vec<Release> = serde_json::from_slice(&releases)
            .map_err(|error| format!("unreadable release list: {error}"))?;
        let Some(release) = newest(self.current, &releases, self.asset) else {
            return Ok(None);
        };
        let version = release.tag_name.trim_start_matches('v').to_owned();
        let _ = std::fs::remove_dir_all(&self.directory);
        std::fs::create_dir_all(&self.directory)
            .map_err(|error| format!("could not create {}: {error}", self.directory.display()))?;
        let directory = self.directory.to_string_lossy();
        let download = self.gh(&[
            "release",
            "download",
            &release.tag_name,
            "--repo",
            REPOSITORY,
            "--pattern",
            self.asset,
            "--pattern",
            CHECKSUMS,
            "--dir",
            &directory,
        ]);
        run(download, DOWNLOAD_TIMEOUT, "update download").await?;
        let package = self.directory.join(self.asset);
        self.verify(&package).await?;
        let path = match &self.target {
            Target::Bundle(bundle) => stage_bundle(&package, bundle, &version).await?,
            Target::Package => package,
        };
        Ok(Some(Prepared { version, path }))
    }

    async fn verify(&self, package: &Path) -> Result<(), String> {
        let sums = std::fs::read_to_string(self.directory.join(CHECKSUMS))
            .map_err(|error| format!("could not read {CHECKSUMS}: {error}"))?;
        let expected = expected_checksum(&sums, self.asset)
            .ok_or_else(|| format!("{CHECKSUMS} does not list {}", self.asset))?;
        let mut command = if cfg!(target_os = "macos") {
            let mut command = crate::platform::command("/usr/bin/shasum");
            command.args(["-a", "256"]);
            command
        } else {
            crate::platform::command("sha256sum")
        };
        command.arg(package);
        let output = run(command, QUERY_TIMEOUT, "checksum").await?;
        let actual = String::from_utf8_lossy(&output);
        match actual.split_whitespace().next() {
            Some(actual) if actual.eq_ignore_ascii_case(expected) => Ok(()),
            _ => Err(format!(
                "{} does not match its published checksum",
                self.asset
            )),
        }
    }

    /// Installs a prepared update. On success, quit and call `relaunch_if_installed`.
    pub async fn install(&self, prepared: &Prepared) -> Result<(), String> {
        let relaunch = match &self.target {
            Target::Bundle(bundle) => {
                let previous = sibling(bundle, "previous");
                let _ = std::fs::remove_dir_all(&previous);
                std::fs::rename(bundle, &previous)
                    .map_err(|error| format!("could not move the current app aside: {error}"))?;
                if let Err(error) = std::fs::rename(&prepared.path, bundle) {
                    let _ = std::fs::rename(&previous, bundle);
                    return Err(format!("could not move the update into place: {error}"));
                }
                bundle.clone()
            }
            Target::Package => {
                // pkexec asks for the owner's password through the desktop's authentication agent.
                let mut command = crate::platform::command("pkexec");
                command
                    .args(["/usr/bin/apt-get", "install", "--yes"])
                    .arg(&prepared.path);
                let output = output(command, DOWNLOAD_TIMEOUT, "pkexec").await?;
                match output.status.code() {
                    Some(0) => {}
                    Some(126 | 127) => return Err("authorization was cancelled".into()),
                    _ => {
                        return Err(format!(
                            "apt-get failed: {}",
                            String::from_utf8_lossy(&output.stderr).trim()
                        ));
                    }
                }
                PathBuf::from(PACKAGE_BINARY)
            }
        };
        let _ = std::fs::remove_dir_all(&self.directory);
        let _ = RELAUNCH.set(relaunch);
        Ok(())
    }

    fn gh(&self, args: &[&str]) -> tokio::process::Command {
        let mut command = crate::platform::command(&self.gh);
        command
            .args(args)
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("GH_SPINNER_DISABLED", "1")
            .env("NO_COLOR", "1");
        command
    }
}

/// Copies the app out of the disk image next to the installed bundle, so installing is a rename.
async fn stage_bundle(image: &Path, bundle: &Path, version: &str) -> Result<PathBuf, String> {
    let mount = image.with_extension("mount");
    let mut attach = crate::platform::command("/usr/bin/hdiutil");
    attach
        .args([
            "attach",
            "-nobrowse",
            "-readonly",
            "-noautoopen",
            "-mountpoint",
        ])
        .arg(&mount)
        .arg(image);
    run(attach, QUERY_TIMEOUT, "hdiutil attach").await?;
    let staged = sibling(bundle, "update");
    let result = async {
        let _ = std::fs::remove_dir_all(&staged);
        let mut copy = crate::platform::command("/usr/bin/ditto");
        copy.arg(mount.join("Nightshift.app")).arg(&staged);
        run(copy, DOWNLOAD_TIMEOUT, "ditto").await?;
        let mut verify = crate::platform::command("/usr/bin/codesign");
        verify.args(["--verify", "--strict"]).arg(&staged);
        run(verify, QUERY_TIMEOUT, "codesign").await?;
        let staged_version = std::fs::read_to_string(staged.join("Contents/Resources/VERSION"));
        match staged_version {
            Ok(text) if text.trim() == version => Ok(()),
            _ => Err(format!("the downloaded app is not version {version}")),
        }
    }
    .await;
    let mut detach = crate::platform::command("/usr/bin/hdiutil");
    detach.args(["detach", "-force"]).arg(&mount);
    let _ = run(detach, QUERY_TIMEOUT, "hdiutil detach").await;
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staged);
    }
    result.map(|()| staged)
}

/// A hidden sibling of the app bundle, such as `/Applications/.Nightshift.update.app`.
fn sibling(bundle: &Path, role: &str) -> PathBuf {
    let stem = bundle.file_stem().unwrap_or_default().to_string_lossy();
    bundle.with_file_name(format!(".{stem}.{role}.app"))
}

fn writable(directory: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(directory.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `path` is a valid NUL-terminated string for the duration of the call.
    unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
}

async fn output(
    mut command: tokio::process::Command,
    timeout: Duration,
    what: &str,
) -> Result<Output, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| format!("{what} did not finish in time"))?
        .map_err(|error| format!("could not run {what}: {error}"))
}

async fn run(
    command: tokio::process::Command,
    timeout: Duration,
    what: &str,
) -> Result<Vec<u8>, String> {
    let output = output(command, timeout, what).await?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(format!(
            "{what} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Starts the installed update after Nightshift has shut down and closed its storage.
pub fn relaunch_if_installed() {
    use std::os::unix::process::CommandExt;
    let Some(target) = RELAUNCH.get() else {
        return;
    };
    let mut command = if cfg!(target_os = "macos") {
        let mut command = std::process::Command::new("/usr/bin/open");
        command.arg("-n").arg(target);
        command
    } else {
        let mut command = std::process::Command::new(target);
        command.process_group(0);
        command
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Err(error) = command.spawn() {
        eprintln!("nightshift: could not relaunch after updating: {error}");
    }
}

#[cfg(test)]
mod tests;
