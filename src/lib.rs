pub mod application;
pub mod codex;
pub mod domain;
pub mod git;
pub mod github;
pub mod issue_query;
pub mod perf;
pub mod platform;
pub mod storage;
pub mod update;

pub mod replay;

/// Release version shown to users. CI sets `NIGHTSHIFT_VERSION` for every build;
/// local builds use the `Cargo.toml` version.
pub const VERSION: &str = match option_env!("NIGHTSHIFT_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};
