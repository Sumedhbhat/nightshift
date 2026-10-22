//! Shared Unix shell capture for the supported macOS and Linux backends.
use super::{Environment, os};
use std::{
    ffi::OsStr,
    os::unix::{ffi::OsStringExt, process::CommandExt},
    path::Path,
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncReadExt;

const MAX_ENV_BYTES: u64 = 1024 * 1024;
const BEGIN: &[u8] = b"\0NIGHTSHIFT_ENV_BEGIN\0";
const END: &[u8] = b"\0NIGHTSHIFT_ENV_END\0";
const CAPTURE: &str = "/usr/bin/printf '\\000NIGHTSHIFT_ENV_BEGIN\\000'; /usr/bin/env -0; /usr/bin/printf '\\000NIGHTSHIFT_ENV_END\\000'";

pub(super) async fn capture(
    environment: &Environment,
    timeout: Duration,
) -> Result<Environment, String> {
    let shell = environment
        .get(OsStr::new("SHELL"))
        .filter(|shell| !shell.is_empty())
        .map(Path::new)
        .unwrap_or_else(|| Path::new(os::DEFAULT_SHELL));
    if !shell.is_absolute() {
        return Err("SHELL must be an absolute path".into());
    }
    let mut command = tokio::process::Command::new(shell);
    command
        .env_clear()
        .envs(environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    // An interactive shell must not take control of the terminal that launched Nightshift.
    // SAFETY: setsid is async-signal-safe; this callback performs no allocation or locking.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    if let Some(home) = environment.get(OsStr::new("HOME")) {
        command.current_dir(home);
    }
    match shell.file_name().and_then(OsStr::to_str) {
        Some("csh" | "tcsh") => {
            command.as_std_mut().arg0("-");
            command.args(["-i", "-c", CAPTURE]);
        }
        Some("nu") => {
            command.args(["-l", "-e", &format!("{CAPTURE}; exit")]);
        }
        Some("fish") => {
            command.args(["-l", "-i", "-c", &format!("emit fish_prompt; {CAPTURE}")]);
        }
        _ => {
            command.args(["-l", "-i", "-c", CAPTURE]);
        }
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start login shell: {error}"))?;
    let pid = child.id().expect("spawned shell");
    let result = tokio::time::timeout(timeout, async {
        let mut output = Vec::new();
        child
            .stdout
            .take()
            .expect("piped")
            .take(MAX_ENV_BYTES + 1)
            .read_to_end(&mut output)
            .await
            .map_err(|error| format!("could not read login shell: {error}"))?;
        if output.len() as u64 > MAX_ENV_BYTES {
            return Err("login shell output exceeded 1 MiB".into());
        }
        let status = child
            .wait()
            .await
            .map_err(|error| format!("could not wait for login shell: {error}"))?;
        // Valid capture is usable even if a logout hook subsequently failed.
        parse(&output)
            .ok_or_else(|| format!("login shell returned no usable environment ({status})"))
    })
    .await
    .unwrap_or_else(|_| Err("login shell timed out".into()));
    if result.is_err() {
        // Kill the capture's process group too: a profile command can hold its stdout open.
        let _ = tokio::process::Command::new("/bin/kill")
            .args(["-KILL", "--", &format!("-{pid}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status()
            .await;
        let _ = child.kill().await;
    }
    result
}

fn parse(output: &[u8]) -> Option<Environment> {
    let start = output.windows(BEGIN.len()).position(|part| part == BEGIN)? + BEGIN.len();
    let output = &output[start..];
    let end = output.windows(END.len()).position(|part| part == END)?;
    let mut environment = Environment::new();
    for entry in output[..end]
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let equals = entry.iter().position(|byte| *byte == b'=')?;
        if equals == 0 {
            return None;
        }
        environment.insert(
            std::ffi::OsString::from_vec(entry[..equals].to_vec()),
            std::ffi::OsString::from_vec(entry[equals + 1..].to_vec()),
        );
    }
    environment
        .get(OsStr::new("PATH"))
        .filter(|path| !path.is_empty())?;
    Some(environment)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_preserves_multiline_values_equals_and_non_utf8_bytes() {
        let mut output = b"profile banner".to_vec();
        output.extend(BEGIN);
        output.extend(b"PATH=/usr/bin:/custom\0VALUE=line 1\nline=2\0BYTES=\xff\0");
        output.extend(END);
        output.extend(b"logout banner");
        let captured = parse(&output).unwrap();
        assert_eq!(captured.get(OsStr::new("VALUE")).unwrap(), "line 1\nline=2");
        assert_eq!(
            captured
                .get(OsStr::new("BYTES"))
                .unwrap()
                .as_encoded_bytes(),
            b"\xff"
        );
        assert!(parse(b"PATH=/usr/bin\0").is_none());
        assert!(parse(&[BEGIN, b"PATH=\0", END].concat()).is_none());
        assert!(parse(&[BEGIN, b"PATH=/bin\0malformed\0", END].concat()).is_none());
    }
}
