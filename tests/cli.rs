//! Public entrypoint tests use temporary configuration and a private pasteboard.
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_clipbridge")
}

#[test]
fn version_and_help_do_not_need_python_or_swift() {
    let out = Command::new(binary())
        .args(["--version"])
        .env("PATH", "/nonexistent")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().trim(),
        format!("clipbridge {}", env!("CARGO_PKG_VERSION"))
    );
    let out = Command::new(binary())
        .args(["--help"])
        .env("PATH", "/nonexistent")
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    for command in [
        "install",
        "upgrade",
        "uninstall",
        "configure",
        "hosts",
        "setup",
        "doctor",
        "start",
        "restart",
        "run",
        "stop",
        "logs",
    ] {
        assert!(text.contains(command));
    }
}

#[test]
fn host_listing_is_offline_and_noninteractive_setup_requires_an_explicit_startup_choice() {
    let home = tempfile::tempdir().unwrap();
    fs::create_dir(home.path().join(".ssh")).unwrap();
    fs::write(
        home.path().join(".ssh/config"),
        "Host linux-prod wsl\n  HostName example.invalid\nHost *\n  ServerAliveInterval 30\n",
    )
    .unwrap();
    let output = Command::new(binary())
        .args(["hosts", "--json"])
        .env("HOME", home.path())
        .env("PATH", "/nonexistent")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["aliases"], serde_json::json!(["linux-prod", "wsl"]));
    assert!(value["configured_host"].is_null());
    let output = Command::new(binary())
        .args(["setup", "--host", "linux-prod"])
        .env("HOME", home.path())
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--start or --no-start"));
    assert!(!home.path().join(".config/clipbridge/config.json").exists());
}

#[test]
fn offline_configuration_preserves_file_on_invalid_update_and_backs_up_corrupt_json() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("configuration with spaces.json");
    let configure = |host: &str| {
        Command::new(binary())
            .args([
                "configure",
                "--host",
                host,
                "--remote-dir",
                "/tmp/images",
                "--no-check",
                "--config",
            ])
            .arg(&path)
            .env("HOME", home.path())
            .output()
            .unwrap()
    };
    assert!(configure("test-server").status.success());
    let before = fs::read(&path).unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!configure("bad;host").status.success());
    assert_eq!(fs::read(&path).unwrap(), before);
    fs::write(&path, b"{broken json").unwrap();
    assert!(configure("test-server").status.success());
    let backups: Vec<_> = fs::read_dir(home.path())
        .unwrap()
        .map(Result::unwrap)
        .filter(|e| e.file_name().to_string_lossy().contains(".backup-"))
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(backups[0].path()).unwrap(), b"{broken json");
}

#[test]
fn native_clipboard_contract_uses_only_a_private_pasteboard() {
    let out = Command::new(binary())
        .arg("clipboard-self-test")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
