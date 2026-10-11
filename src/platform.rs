use crate::storage::Error;
use std::{
    collections::HashMap,
    env,
    ffi::{OsStr, OsString},
    path::PathBuf,
};

#[derive(Clone, Copy)]
enum OperatingSystem {
    Macos,
    Linux,
    Windows,
    Other,
}

#[derive(Clone, Copy)]
pub struct Platform {
    operating_system: OperatingSystem,
}

#[derive(Clone, Copy)]
pub struct ShortcutModifier {
    binding: &'static str,
    label: &'static str,
    separator: &'static str,
    word_binding: &'static str,
}

#[derive(Clone, Copy)]
pub struct TextInputShortcuts {
    pub line_start: Option<&'static str>,
    pub line_end: Option<&'static str>,
    pub character_palette: Option<&'static str>,
}

impl Platform {
    pub fn current() -> Self {
        let operating_system = match env::consts::OS {
            "macos" => OperatingSystem::Macos,
            "linux" => OperatingSystem::Linux,
            "windows" => OperatingSystem::Windows,
            _ => OperatingSystem::Other,
        };
        Self { operating_system }
    }

    pub fn data_directory(self) -> Result<PathBuf, Error> {
        resolve_data_directory(
            self.operating_system,
            &env::vars_os().collect::<HashMap<_, _>>(),
        )
    }

    pub fn shortcut_modifier(self) -> ShortcutModifier {
        match self.operating_system {
            OperatingSystem::Macos => ShortcutModifier {
                binding: "cmd",
                label: "⌘",
                separator: "",
                word_binding: "alt",
            },
            OperatingSystem::Linux | OperatingSystem::Windows | OperatingSystem::Other => {
                ShortcutModifier {
                    binding: "ctrl",
                    label: "Ctrl",
                    separator: "+",
                    word_binding: "ctrl",
                }
            }
        }
    }

    pub fn text_input_shortcuts(self) -> TextInputShortcuts {
        match self.operating_system {
            OperatingSystem::Macos => TextInputShortcuts {
                line_start: Some("cmd-left"),
                line_end: Some("cmd-right"),
                character_palette: Some("ctrl-cmd-space"),
            },
            OperatingSystem::Linux | OperatingSystem::Windows | OperatingSystem::Other => {
                TextInputShortcuts {
                    line_start: None,
                    line_end: None,
                    character_palette: None,
                }
            }
        }
    }
}

impl ShortcutModifier {
    pub fn binding(self) -> &'static str {
        self.binding
    }

    pub fn word_binding(self) -> &'static str {
        self.word_binding
    }

    pub fn format(self, key: &str) -> String {
        format!("{}{separator}{key}", self.label, separator = self.separator)
    }
}

fn resolve_data_directory(
    operating_system: OperatingSystem,
    variables: &HashMap<OsString, OsString>,
) -> Result<PathBuf, Error> {
    if let Some(directory) = variable(variables, "NIGHTSHIFT_DATA_DIR") {
        return Ok(directory);
    }

    let directory = match operating_system {
        OperatingSystem::Macos => home_directory(variables)?.join("Library/Application Support"),
        OperatingSystem::Linux => {
            match variable(variables, "XDG_DATA_HOME").filter(|path| path.is_absolute()) {
                Some(directory) => directory,
                None => home_directory(variables)?.join(".local/share"),
            }
        }
        OperatingSystem::Windows => variable(variables, "APPDATA")
            .or_else(|| variable(variables, "LOCALAPPDATA"))
            .or_else(|| variable(variables, "USERPROFILE").map(|home| home.join("AppData/Roaming")))
            .ok_or(Error::InvalidInput(
                "application data directory is unavailable",
            ))?,
        OperatingSystem::Other => {
            return Err(Error::InvalidInput("unsupported operating system"));
        }
    };
    Ok(directory.join("Nightshift"))
}

fn home_directory(variables: &HashMap<OsString, OsString>) -> Result<PathBuf, Error> {
    variable(variables, "HOME")
        .or_else(|| variable(variables, "USERPROFILE"))
        .ok_or(Error::InvalidInput("home directory is unavailable"))
}

fn variable(variables: &HashMap<OsString, OsString>, name: &str) -> Option<PathBuf> {
    variables.get(OsStr::new(name)).map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::{OperatingSystem, resolve_data_directory};
    use std::{collections::HashMap, ffi::OsString, path::PathBuf};

    fn variables(entries: &[(&str, &str)]) -> HashMap<OsString, OsString> {
        entries
            .iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value)))
            .collect()
    }

    #[test]
    fn resolves_data_directory_by_operating_system() {
        assert_eq!(
            resolve_data_directory(
                OperatingSystem::Macos,
                &variables(&[("HOME", "/users/alex")]),
            )
            .unwrap(),
            PathBuf::from("/users/alex/Library/Application Support/Nightshift")
        );
        assert_eq!(
            resolve_data_directory(
                OperatingSystem::Windows,
                &variables(&[("APPDATA", "C:\\Users\\Alex\\Roaming")]),
            )
            .unwrap(),
            PathBuf::from("C:\\Users\\Alex\\Roaming/Nightshift")
        );
    }

    #[test]
    fn formats_primary_shortcuts_for_each_desktop_family() {
        assert_eq!(
            super::Platform {
                operating_system: OperatingSystem::Macos,
            }
            .shortcut_modifier()
            .format("R"),
            "⌘R"
        );
        assert_eq!(
            super::Platform {
                operating_system: OperatingSystem::Linux,
            }
            .shortcut_modifier()
            .format("R"),
            "Ctrl+R"
        );
        assert_eq!(
            super::Platform {
                operating_system: OperatingSystem::Windows,
            }
            .shortcut_modifier()
            .format("R"),
            "Ctrl+R"
        );
        assert_eq!(
            super::Platform {
                operating_system: OperatingSystem::Macos,
            }
            .text_input_shortcuts()
            .character_palette,
            Some("ctrl-cmd-space")
        );
        assert_eq!(
            super::Platform {
                operating_system: OperatingSystem::Linux,
            }
            .text_input_shortcuts()
            .character_palette,
            None
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn uses_xdg_fallback_and_explicit_data_override() {
        assert_eq!(
            resolve_data_directory(
                OperatingSystem::Linux,
                &variables(&[("HOME", "/home/alex"), ("XDG_DATA_HOME", "/data")]),
            )
            .unwrap(),
            PathBuf::from("/data/Nightshift")
        );
        assert_eq!(
            resolve_data_directory(
                OperatingSystem::Linux,
                &variables(&[("HOME", "/home/alex")]),
            )
            .unwrap(),
            PathBuf::from("/home/alex/.local/share/Nightshift")
        );
        assert_eq!(
            resolve_data_directory(
                OperatingSystem::Windows,
                &variables(&[("NIGHTSHIFT_DATA_DIR", "C:\\nightshift")]),
            )
            .unwrap(),
            PathBuf::from("C:\\nightshift")
        );
    }
}
