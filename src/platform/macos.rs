use std::{ffi::OsStr, path::PathBuf};

pub(super) const DEFAULT_SHELL: &str = "/bin/zsh";
pub(super) const TOOL_DIRECTORIES: &[&str] = &[
    "/opt/homebrew/bin",
    "/opt/homebrew/sbin",
    "/usr/local/bin",
    "/usr/local/sbin",
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
];

pub(super) fn data_directory(home: PathBuf, _xdg: Option<&OsStr>) -> PathBuf {
    home.join("Library/Application Support/Nightshift")
}
