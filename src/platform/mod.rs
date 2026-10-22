//! OS services used by background work. GPUI continues to own windows and input.
//! Child environments are immutable snapshots; never mutate the multithreaded app's environment.
#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod os;
#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod os;
mod shell;

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::PathBuf,
    sync::LazyLock,
    time::Duration,
};

type Environment = BTreeMap<OsString, OsString>;
const SHELL_TIMEOUT: Duration = Duration::from_secs(5);
static PLATFORM: tokio::sync::OnceCell<Platform> = tokio::sync::OnceCell::const_new();
static INHERITED: LazyLock<Platform> =
    LazyLock::new(|| Platform::inherited(std::env::vars_os().collect()));

pub struct Platform {
    environment: Environment,
    warning: Option<String>,
}

impl Platform {
    fn inherited(mut environment: Environment) -> Self {
        extend_path(&mut environment);
        Self {
            environment,
            warning: None,
        }
    }

    async fn load_from(mut environment: Environment, timeout: Duration) -> Self {
        let captured = shell::capture(&environment, timeout).await;
        let warning = match captured {
            Ok(captured) => {
                for (name, value) in captured {
                    // Keep launch controls, application storage, and shell bookkeeping stable.
                    if name.as_encoded_bytes().starts_with(b"NIGHTSHIFT_")
                        || ["HOME", "XDG_DATA_HOME", "PWD", "OLDPWD", "SHLVL", "_"]
                            .iter()
                            .any(|skip| name == OsStr::new(skip))
                    {
                        continue;
                    }
                    environment.insert(name, value);
                }
                None
            }
            Err(error) => Some(format!(
                "Could not load your shell environment: {error}. Using standard tool locations; check your shell startup files if a tool is missing."
            )),
        };
        let mut platform = Self::inherited(environment);
        platform.warning = warning;
        platform
    }

    /// Uses a child-specific PATH for executable lookup and for that executable's descendants.
    pub fn command(&self, program: impl AsRef<OsStr>) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(program);
        command.env_clear().envs(self.environment.iter());
        command
    }

    pub fn var_os(&self, name: &str) -> Option<&OsStr> {
        self.environment
            .get(OsStr::new(name))
            .map(OsString::as_os_str)
    }

    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    pub fn data_directory(&self) -> Option<PathBuf> {
        self.var_os("NIGHTSHIFT_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                self.var_os("HOME")
                    .map(|home| os::data_directory(home.into(), self.var_os("XDG_DATA_HOME")))
            })
    }
}

fn extend_path(environment: &mut Environment) {
    let mut paths: Vec<PathBuf> = environment
        .get(OsStr::new("PATH"))
        .map(|path| {
            std::env::split_paths(path)
                .filter(|path| !path.as_os_str().is_empty())
                .collect()
        })
        .unwrap_or_default();
    let home = environment.get(OsStr::new("HOME")).map(PathBuf::from);
    let fallbacks = home
        .iter()
        .flat_map(|home| [".local/bin", ".cargo/bin", ".bun/bin"].map(|path| home.join(path)))
        .chain(os::TOOL_DIRECTORIES.iter().map(PathBuf::from));
    for path in fallbacks {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    if let Ok(path) = std::env::join_paths(paths) {
        environment.insert("PATH".into(), path);
    }
}

/// Initialize off the UI thread before enabling operations that launch tools.
pub async fn initialize() -> &'static Platform {
    PLATFORM
        .get_or_init(|| Platform::load_from(std::env::vars_os().collect(), SHELL_TIMEOUT))
        .await
}

/// Library callers that do not initialize use their inherited environment plus fallback paths.
pub fn current() -> &'static Platform {
    PLATFORM.get().unwrap_or(&INHERITED)
}

pub fn command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    current().command(program)
}

#[cfg(test)]
mod tests;
