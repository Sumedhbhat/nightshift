use crate::storage::Error;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

#[cfg(any(target_os = "macos", target_os = "windows"))]
const DATA_DIRECTORY_NAME: &str = "Nightshift";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const DATA_DIRECTORY_NAME: &str = "nightshift";

#[cfg(target_os = "macos")]
pub const MONOSPACE_FONT: &str = "Menlo";
#[cfg(target_os = "windows")]
pub const MONOSPACE_FONT: &str = "Consolas";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const MONOSPACE_FONT: &str = "DejaVu Sans Mono";

/// Returns the per-user data directory, honoring `NIGHTSHIFT_DATA_DIR` for testing.
pub fn data_directory() -> Result<PathBuf, Error> {
    resolve_data_directory(std::env::var_os("NIGHTSHIFT_DATA_DIR"), dirs::data_dir())
}

fn resolve_data_directory(
    override_directory: Option<OsString>,
    platform_directory: Option<PathBuf>,
) -> Result<PathBuf, Error> {
    if let Some(directory) = override_directory.filter(|directory| !directory.is_empty()) {
        return Ok(PathBuf::from(directory));
    }
    platform_directory
        .map(|directory| directory.join(DATA_DIRECTORY_NAME))
        .ok_or(Error::InvalidInput(
            "The user data directory is unavailable",
        ))
}

/// Formats a shortcut that uses the platform's primary modifier, matching GPUI's `secondary-` bindings.
pub fn secondary_shortcut(key: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("⌘{key}")
    } else {
        format!("Ctrl+{key}")
    }
}

/// Spoken form of the primary modifier for screen readers.
pub fn secondary_modifier_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "Command"
    } else {
        "Control"
    }
}

/// Canonicalizes without the `\\?\` prefix that `std::fs::canonicalize` adds on Windows.
pub fn canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    dunce::canonicalize(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_wins_over_platform_directory() {
        let directory = resolve_data_directory(Some("/custom".into()), Some("/platform".into()));
        assert_eq!(directory.unwrap(), PathBuf::from("/custom"));
    }

    #[test]
    fn empty_override_falls_back_to_platform_directory() {
        let directory = resolve_data_directory(Some("".into()), Some("/platform".into()));
        assert_eq!(
            directory.unwrap(),
            PathBuf::from("/platform").join(DATA_DIRECTORY_NAME)
        );
    }

    #[test]
    fn missing_directories_are_an_error() {
        assert!(resolve_data_directory(None, None).is_err());
    }

    #[test]
    fn shortcuts_use_platform_modifier() {
        let shortcut = secondary_shortcut("O");
        if cfg!(target_os = "macos") {
            assert_eq!(shortcut, "⌘O");
        } else {
            assert_eq!(shortcut, "Ctrl+O");
        }
    }

    #[test]
    fn canonicalize_has_no_verbatim_prefix() {
        let directory = tempfile::tempdir().unwrap();
        let canonical = canonicalize(directory.path()).unwrap();
        assert!(!canonical.to_string_lossy().starts_with(r"\\?\"));
        assert!(canonical.is_dir());
    }
}
