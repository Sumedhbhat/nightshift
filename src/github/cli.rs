//! GitHub CLI adapter. Nightshift runs an installed, already-authenticated `gh` and reads its
//! structured output. It never asks `gh` for its token, never logs in or installs anything, and
//! never falls back to anonymous HTTP.
use serde::de::DeserializeOwned;
use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc, time::Duration};

pub const HOST: &str = "github.com";
const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOutput {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GhError {
    #[error(
        "GitHub CLI (gh) was not found in Nightshift's command search path. Install it or add its directory to your shell's PATH, then reopen Nightshift. Run `gh auth login` outside Nightshift if needed."
    )]
    NotInstalled,
    #[error("GitHub CLI is not signed in to github.com. Run `gh auth login` outside Nightshift.")]
    NotAuthenticated,
    #[error("This GitHub CLI cannot run {0}. Update gh outside Nightshift, then try again.")]
    Incompatible(String),
    #[error(
        "GitHub CLI is signed in as {actual}, not the run owner {expected}. Switch back to {expected} or end the run."
    )]
    AccountChanged { expected: String, actual: String },
    #[error("GitHub CLI did not respond within 30 seconds.")]
    Timeout,
    #[error("GitHub rate limit reached. Nightshift will wait before trying again. {0}")]
    RateLimited(String),
    #[error("Not found or not accessible to the active GitHub account: {0}")]
    NotFound(String),
    #[error("GitHub denied access: {0}")]
    Forbidden(String),
    #[error("GitHub request failed: {0}")]
    Failed(String),
    #[error("GitHub CLI returned unreadable output for {0}.")]
    Unreadable(String),
}

impl GhError {
    /// Rate limits and transient failures back off; everything else needs the owner.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            GhError::RateLimited(_) | GhError::Timeout | GhError::Failed(_)
        )
    }
}

pub type GhResult<T> = Result<T, GhError>;
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Runs `gh` with arguments and optional standard input. Tests substitute a fake.
pub trait Runner: Send + Sync {
    fn run<'a>(
        &'a self,
        args: Vec<String>,
        stdin: Option<Vec<u8>>,
    ) -> BoxFuture<'a, GhResult<CommandOutput>>;
}

/// The real subprocess runner.
pub struct Subprocess {
    program: PathBuf,
}

impl Subprocess {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }

    /// `NIGHTSHIFT_GH` overrides the executable; otherwise use the platform's command search path.
    pub fn from_env() -> Self {
        Self::new(
            crate::platform::current()
                .var_os("NIGHTSHIFT_GH")
                .map(PathBuf::from)
                .unwrap_or_else(|| "gh".into()),
        )
    }
}

impl Runner for Subprocess {
    fn run<'a>(
        &'a self,
        args: Vec<String>,
        stdin: Option<Vec<u8>>,
    ) -> BoxFuture<'a, GhResult<CommandOutput>> {
        Box::pin(async move {
            use tokio::io::AsyncWriteExt;
            let mut command = crate::platform::command(&self.program);
            command
                .args(&args)
                // Non-interactive, uncolored, unpaged, and no update checks.
                .env("GH_PROMPT_DISABLED", "1")
                .env("GH_NO_UPDATE_NOTIFIER", "1")
                .env("GH_NO_EXTENSION_UPDATE_NOTIFIER", "1")
                .env("GH_PAGER", "cat")
                .env("NO_COLOR", "1")
                .env("GH_SPINNER_DISABLED", "1")
                .stdin(if stdin.is_some() {
                    std::process::Stdio::piped()
                } else {
                    std::process::Stdio::null()
                })
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true);
            let mut child = command.spawn().map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => GhError::NotInstalled,
                _ => GhError::Failed(format!("could not start gh: {error}")),
            })?;
            if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
                pipe.write_all(&input).await.map_err(|error| {
                    GhError::Failed(format!("could not send input to gh: {error}"))
                })?;
            }
            let output = tokio::time::timeout(TIMEOUT, child.wait_with_output())
                .await
                .map_err(|_| GhError::Timeout)?
                .map_err(|error| GhError::Failed(format!("gh failed: {error}")))?;
            if output.stdout.len() > MAX_OUTPUT_BYTES {
                return Err(GhError::Failed("gh output exceeded 16 MiB".into()));
            }
            Ok(CommandOutput {
                success: output.status.success(),
                stdout: output.stdout,
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            })
        })
    }
}

/// Classify a failed `gh api` call from its stderr. `gh` prints "(HTTP 404)" and similar.
pub fn classify(target: &str, stderr: &str) -> GhError {
    let lower = stderr.to_ascii_lowercase();
    let detail = if stderr.is_empty() {
        target.to_owned()
    } else {
        format!("{target}: {stderr}")
    };
    if lower.contains("rate limit")
        || lower.contains("http 429")
        || lower.contains("secondary rate")
    {
        GhError::RateLimited(detail)
    } else if lower.contains("http 401")
        || lower.contains("gh auth login")
        || lower.contains("authentication") && lower.contains("required")
    {
        GhError::NotAuthenticated
    } else if lower.contains("http 404") || lower.contains("http 410") {
        GhError::NotFound(detail)
    } else if lower.contains("http 403") {
        GhError::Forbidden(detail)
    } else if lower.contains("unknown flag")
        || lower.contains("unknown command")
        || lower.contains("unknown json field")
    {
        GhError::Incompatible(detail)
    } else {
        GhError::Failed(detail)
    }
}

/// Typed access to the commands Nightshift needs.
#[derive(Clone)]
pub struct Gh {
    runner: Arc<dyn Runner>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub login: String,
}

/// The `gh` features Nightshift relies on, checked directly rather than by version number.
pub const REQUIRED_API_FLAGS: [&str; 5] =
    ["--method", "--paginate", "--slurp", "--input", "--hostname"];

impl Gh {
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        Self { runner }
    }

    pub fn from_env() -> Self {
        Self::new(Arc::new(Subprocess::from_env()))
    }

    async fn raw(&self, args: &[&str], stdin: Option<Vec<u8>>) -> GhResult<CommandOutput> {
        self.runner
            .run(args.iter().map(|a| a.to_string()).collect(), stdin)
            .await
    }

    /// Verify installation, required capabilities, and authentication; return the active account.
    pub async fn probe(&self) -> GhResult<Account> {
        let version = self.raw(&["--version"], None).await?;
        if !version.success || !String::from_utf8_lossy(&version.stdout).starts_with("gh version") {
            return Err(GhError::Incompatible("`gh --version`".into()));
        }
        let help = self.raw(&["api", "--help"], None).await?;
        let help_text = String::from_utf8_lossy(&help.stdout).to_string() + &help.stderr;
        let missing: Vec<&str> = REQUIRED_API_FLAGS
            .iter()
            .copied()
            .filter(|flag| !help_text.contains(flag))
            .collect();
        if !missing.is_empty() {
            return Err(GhError::Incompatible(format!(
                "`gh api` with {}",
                missing.join(", ")
            )));
        }
        self.active_account().await
    }

    /// The active github.com account according to `gh auth status --json hosts`.
    /// This output never contains the token unless `--show-token` is passed, which Nightshift never does.
    pub async fn active_account(&self) -> GhResult<Account> {
        let output = self
            .raw(
                &["auth", "status", "--hostname", HOST, "--json", "hosts"],
                None,
            )
            .await?;
        if !output.success {
            let error = classify("`gh auth status --json hosts`", &output.stderr);
            return Err(match error {
                GhError::Failed(_) | GhError::NotFound(_) => GhError::NotAuthenticated,
                other => other,
            });
        }
        #[derive(serde::Deserialize)]
        struct Status {
            hosts: std::collections::HashMap<String, Vec<HostAccount>>,
        }
        #[derive(serde::Deserialize)]
        struct HostAccount {
            state: String,
            active: bool,
            login: String,
        }
        let status: Status = serde_json::from_slice(&output.stdout).map_err(|_| {
            GhError::Incompatible(
                "`gh auth status --json hosts` with state, active, and login fields".into(),
            )
        })?;
        let account = status
            .hosts
            .get(HOST)
            .and_then(|accounts| accounts.iter().find(|account| account.active))
            .ok_or(GhError::NotAuthenticated)?;
        if account.state != "success" || account.login.is_empty() {
            return Err(GhError::NotAuthenticated);
        }
        Ok(Account {
            login: account.login.clone(),
        })
    }

    /// Confirm the active account is still the run owner before a new execution.
    pub async fn verify_owner(&self, expected: &str) -> GhResult<Account> {
        let account = self.active_account().await?;
        if !account.login.eq_ignore_ascii_case(expected) {
            return Err(GhError::AccountChanged {
                expected: expected.to_owned(),
                actual: account.login,
            });
        }
        Ok(account)
    }

    /// `gh api <path>` returning parsed JSON.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> GhResult<T> {
        let output = self
            .raw(&["api", "--hostname", HOST, "--method", "GET", path], None)
            .await?;
        parse(path, output)
    }

    /// One page with its `rel="next"` link, read from `gh api --include` response headers.
    /// The next link is returned as an API path and must stay on api.github.com.
    pub async fn get_page<T: DeserializeOwned>(&self, path: &str) -> GhResult<(T, Option<String>)> {
        let output = self
            .raw(
                &[
                    "api",
                    "--hostname",
                    HOST,
                    "--method",
                    "GET",
                    "--include",
                    path,
                ],
                None,
            )
            .await?;
        if !output.success {
            return Err(classify(path, &output.stderr));
        }
        let (headers, body) =
            split_include(&output.stdout).ok_or_else(|| GhError::Unreadable(path.to_owned()))?;
        let next = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("link")
                    .then(|| value.trim().to_owned())
            })
            .and_then(|link| next_link(&link));
        let next = match next {
            Some(url) => Some(api_path(&url).ok_or_else(|| {
                GhError::Failed("GitHub returned a pagination link for an unexpected host.".into())
            })?),
            None => None,
        };
        let parsed =
            serde_json::from_slice(body).map_err(|_| GhError::Unreadable(path.to_owned()))?;
        Ok((parsed, next))
    }

    /// All pages of a list endpoint, flattened.
    pub async fn get_all<T: DeserializeOwned>(&self, path: &str) -> GhResult<Vec<T>> {
        let output = self
            .raw(
                &[
                    "api",
                    "--hostname",
                    HOST,
                    "--method",
                    "GET",
                    "--paginate",
                    "--slurp",
                    path,
                ],
                None,
            )
            .await?;
        let pages: Vec<Vec<T>> = parse(path, output)?;
        Ok(pages.into_iter().flatten().collect())
    }

    /// `gh api --method POST <path> --input -` with a JSON body.
    pub async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> GhResult<T> {
        self.send("POST", path, body).await
    }

    /// `gh api --method <method> <path> --input -` with a JSON body, for remote writes.
    pub async fn send<T: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: &serde_json::Value,
    ) -> GhResult<T> {
        let output = self
            .raw(
                &[
                    "api",
                    "--hostname",
                    HOST,
                    "--method",
                    method,
                    path,
                    "--input",
                    "-",
                ],
                Some(serde_json::to_vec(body).expect("serialize request body")),
            )
            .await?;
        parse(path, output)
    }
}

/// Split `gh api --include` output into the final header block and the body.
fn split_include(output: &[u8]) -> Option<(String, &[u8])> {
    let mut rest = output;
    loop {
        let end = rest
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|i| (i, 4))
            .or_else(|| rest.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2)))?;
        let headers = String::from_utf8_lossy(&rest[..end.0]).to_string();
        let body = &rest[end.0 + end.1..];
        // Skip interim responses such as "HTTP/2.0 100 Continue".
        if body.starts_with(b"HTTP/") {
            rest = body;
            continue;
        }
        return headers.starts_with("HTTP/").then_some((headers, body));
    }
}

/// Extract the `rel="next"` URL from a Link header.
pub fn next_link(link: &str) -> Option<String> {
    link.split(',')
        .find(|part| part.contains("rel=\"next\""))
        .and_then(|part| part.split_once('<'))
        .and_then(|(_, rest)| rest.split_once('>'))
        .map(|(url, _)| url.to_owned())
}

/// Convert an absolute API URL to a `gh api` path, accepting only https://api.github.com.
pub fn api_path(url: &str) -> Option<String> {
    let path = url.strip_prefix("https://api.github.com/")?;
    (!path.is_empty() && !path.contains("://") && !path.starts_with('/')).then(|| path.to_owned())
}

fn parse<T: DeserializeOwned>(path: &str, output: CommandOutput) -> GhResult<T> {
    if !output.success {
        return Err(classify(path, &output.stderr));
    }
    serde_json::from_slice(&output.stdout).map_err(|_| GhError::Unreadable(path.to_owned()))
}
