use std::{ffi::OsStr, path::PathBuf};

pub(super) const DEFAULT_SHELL: &str = "/bin/sh";
pub(super) const TOOL_DIRECTORIES: &[&str] = &[
    "/usr/local/bin",
    "/usr/local/sbin",
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
    "/snap/bin",
    "/home/linuxbrew/.linuxbrew/bin",
];

pub(super) fn data_directory(home: PathBuf, xdg: Option<&OsStr>) -> PathBuf {
    xdg.map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".local/share"))
        .join("nightshift")
}
