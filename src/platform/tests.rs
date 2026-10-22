use super::*;
use std::os::unix::fs::PermissionsExt;

fn run<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn executable(path: &std::path::Path, text: &str) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn desktop_environment(home: &std::path::Path, shell: &std::path::Path) -> Environment {
    [
        ("HOME".into(), home.as_os_str().to_owned()),
        ("SHELL".into(), shell.as_os_str().to_owned()),
        ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
        ("NIGHTSHIFT_GH".into(), "/explicit/gh".into()),
    ]
    .into()
}

#[test]
fn desktop_launch_finds_tools_and_their_script_interpreters_from_shell_path() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("custom tools");
    std::fs::create_dir(&bin).unwrap();
    // Neither the tool nor its interpreter is on the inherited desktop PATH.
    executable(
        &bin.join("nightshift-test-runtime"),
        "#!/bin/sh\nprintf '%s' \"$TEST_SHELL_VALUE\"\n",
    );
    for tool in ["gh", "codex", "git"] {
        executable(&bin.join(tool), "#!/usr/bin/env nightshift-test-runtime\n");
    }
    let shell = root.path().join("shell");
    executable(
        &shell,
        &format!(
            "#!/bin/sh\nexport PATH='{}':\"$PATH\"\nexport TEST_SHELL_VALUE='loaded from profile'\nexport NIGHTSHIFT_GH=/profile/override\nprintf 'profile startup noise\\n'\nfor capture do :; done\neval \"$capture\"\nprintf 'profile exit noise\\n'\n",
            bin.display()
        ),
    );
    run(async {
        let platform =
            Platform::load_from(desktop_environment(root.path(), &shell), SHELL_TIMEOUT).await;
        assert!(platform.warning().is_none(), "{:?}", platform.warning());
        assert_eq!(
            platform.var_os("NIGHTSHIFT_GH"),
            Some(OsStr::new("/explicit/gh"))
        );
        for tool in ["gh", "codex", "git"] {
            let output = platform.command(tool).output().await.unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(output.stdout, b"loaded from profile");
        }
        // Setup commands must receive the same environment as directly launched tools.
        let output = platform
            .command("/bin/sh")
            .args(["-c", "gh"])
            .output()
            .await
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"loaded from profile");
    });
}

#[test]
fn real_login_shell_profiles_are_loaded() {
    for (shell, profile) in [("/bin/bash", ".bash_profile"), ("/bin/zsh", ".zshrc")] {
        if !std::path::Path::new(shell).is_file() {
            continue;
        }
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join(profile),
            "export TEST_LOGIN_PROFILE=ready\n",
        )
        .unwrap();
        let environment = desktop_environment(root.path(), std::path::Path::new(shell));
        let platform = run(Platform::load_from(environment, SHELL_TIMEOUT));
        assert!(
            platform.warning().is_none(),
            "{shell}: {:?}",
            platform.warning()
        );
        assert_eq!(
            platform.var_os("TEST_LOGIN_PROFILE"),
            Some(OsStr::new("ready")),
            "{shell}"
        );
    }
}

#[test]
fn missing_shell_uses_standard_tool_locations_and_reports_a_warning() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    executable(
        &bin.join("nightshift-fallback-tool"),
        "#!/bin/sh\nprintf fallback\n",
    );
    let environment = desktop_environment(root.path(), &root.path().join("missing-shell"));
    let platform = run(Platform::load_from(environment, SHELL_TIMEOUT));
    assert!(
        platform
            .warning()
            .unwrap()
            .contains("could not start login shell")
    );
    let paths: Vec<_> = std::env::split_paths(platform.var_os("PATH").unwrap()).collect();
    assert_eq!(paths[0], PathBuf::from("/usr/bin"));
    assert!(paths.contains(&PathBuf::from(os::TOOL_DIRECTORIES[0])));
    let output = run(async {
        platform
            .command("nightshift-fallback-tool")
            .output()
            .await
            .unwrap()
    });
    assert!(output.status.success());
    assert_eq!(output.stdout, b"fallback");
}

#[test]
fn hanging_profile_is_bounded_and_its_child_is_stopped() {
    let root = tempfile::tempdir().unwrap();
    let shell = root.path().join("shell");
    let pid_file = root.path().join("child.pid");
    executable(
        &shell,
        &format!(
            "#!/bin/sh\n/bin/sleep 30 &\necho $! > '{}'\nwait\n",
            pid_file.display()
        ),
    );
    let started = std::time::Instant::now();
    let platform = run(Platform::load_from(
        desktop_environment(root.path(), &shell),
        Duration::from_millis(500),
    ));
    assert!(started.elapsed() < Duration::from_secs(4));
    assert!(platform.warning().unwrap().contains("timed out"));
    let pid = std::fs::read_to_string(pid_file).unwrap();
    // On Linux an exited orphan can briefly remain a zombie until init reaps it.
    let output = std::process::Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", pid.trim()])
        .output()
        .unwrap();
    assert!(
        !output.status.success()
            || String::from_utf8_lossy(&output.stdout)
                .trim()
                .starts_with('Z')
    );
}

#[test]
fn excessive_or_unusable_shell_output_falls_back_without_exposing_output() {
    let root = tempfile::tempdir().unwrap();
    let shell = root.path().join("shell");
    for (script, expected) in [
        (
            "#!/bin/sh\nprintf 'private profile content'\nexit 1\n",
            "no usable environment",
        ),
        (
            "#!/bin/sh\n/usr/bin/head -c 1048577 /dev/zero\n",
            "exceeded 1 MiB",
        ),
    ] {
        executable(&shell, script);
        let platform = run(Platform::load_from(
            desktop_environment(root.path(), &shell),
            SHELL_TIMEOUT,
        ));
        let warning = platform.warning().unwrap();
        assert!(warning.contains(expected), "{warning}");
        assert!(!warning.contains("private profile content"));
    }
}

#[test]
fn data_directory_and_launch_controls_stay_in_the_inherited_environment() {
    let root = tempfile::tempdir().unwrap();
    let shell = root.path().join("shell");
    executable(
        &shell,
        "#!/bin/sh\nexport HOME=/profile/home XDG_DATA_HOME=/profile/data NIGHTSHIFT_DATA_DIR=/profile/override\nfor capture do :; done\neval \"$capture\"\nexit 1\n",
    );
    let mut environment = desktop_environment(root.path(), &shell);
    let data = root.path().join("app-data");
    environment.insert("NIGHTSHIFT_DATA_DIR".into(), data.clone().into_os_string());
    let platform = run(Platform::load_from(environment, SHELL_TIMEOUT));
    // The capture succeeded even though the shell's logout hook returned a failure.
    assert!(platform.warning().is_none());
    assert_eq!(platform.data_directory(), Some(data));
    assert_eq!(platform.var_os("HOME"), Some(root.path().as_os_str()));
    assert_eq!(platform.var_os("XDG_DATA_HOME"), None);
}

#[test]
fn default_data_directory_follows_the_platform() {
    let environment = desktop_environment(
        std::path::Path::new("/home/owner"),
        std::path::Path::new("/bin/sh"),
    );
    let mut platform = Platform::inherited(environment.clone());
    #[cfg(target_os = "macos")]
    let expected = "/home/owner/Library/Application Support/Nightshift";
    #[cfg(target_os = "linux")]
    let expected = "/home/owner/.local/share/nightshift";
    assert_eq!(platform.data_directory(), Some(expected.into()));
    let mut environment = environment;
    environment.insert("XDG_DATA_HOME".into(), "/custom/data".into());
    platform = Platform::inherited(environment);
    #[cfg(target_os = "macos")]
    assert_eq!(platform.data_directory(), Some(expected.into()));
    #[cfg(target_os = "linux")]
    assert_eq!(
        platform.data_directory(),
        Some("/custom/data/nightshift".into())
    );
}
