//! Native payload lifecycle with real executable copies, isolated HOME and fake launchd.
use anyhow::Result;
use clipbridge::{
    common::{CommandOutput, CommandSpec, Context, Runner, SystemRunner},
    installer,
};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, atomic::AtomicBool};

struct NoLiveServices;
impl Runner for NoLiveServices {
    fn run(&self, spec: &CommandSpec, cancelled: &AtomicBool) -> Result<CommandOutput> {
        if spec.program == Path::new("/bin/launchctl") {
            assert_eq!(
                spec.args.first().unwrap(),
                "print",
                "integration test must never mutate launchd"
            );
            return Ok(CommandOutput {
                code: 113,
                stdout: String::new(),
                stderr: "Could not find service in domain".into(),
            });
        }
        assert!(
            spec.args
                .first()
                .is_some_and(|arg| arg == "--version" || arg == "--help")
        );
        SystemRunner.run(spec, cancelled)
    }
}

#[test]
fn installed_native_binary_survives_source_deletion_and_preserves_config_on_upgrade_and_uninstall()
{
    let home = tempfile::tempdir().unwrap();
    let home = home.path().canonicalize().unwrap();
    let source = home.join("source tree");
    let actual = Path::new(env!("CARGO_MANIFEST_DIR"));
    for name in [
        "Cargo.toml",
        "Cargo.lock",
        "VERSION",
        "LICENSE",
        "src/config.example.json",
    ] {
        let file = source.join(name);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::copy(actual.join(name), file).unwrap();
    }
    let mut ctx = Context {
        home: home.clone(),
        executable: Path::new(env!("CARGO_BIN_EXE_clipbridge"))
            .canonicalize()
            .unwrap(),
        source: Some(source.clone()),
        cancelled: Arc::new(AtomicBool::new(false)),
        runner: Arc::new(NoLiveServices),
        uid: 501,
    };
    let prefix = home.join("prefix & spaces");
    installer::install(&ctx, &source, Some(&prefix), false).unwrap();
    let root = prefix.join("share/clipbridge");
    let command = prefix.join("bin/clipbridge");
    let first = root.join("current").canonicalize().unwrap();
    fs::write(
        ctx.default_config(),
        b"{\"ssh_host\":\"mine\",\"remote_directory\":\"/tmp/mine\"}\n",
    )
    .unwrap();
    let before = fs::read(ctx.default_config()).unwrap();
    // Upgrade the source installation from an extracted native payload. The
    // runner allows only executable probes, so invoking Cargo would fail here.
    let candidate = source.join("clipbridge");
    fs::copy(&ctx.executable, &candidate).unwrap();
    fs::write(
        source.join("bundle.json"),
        serde_json::to_vec(&serde_json::json!({
            "product": "clipbridge", "schema": 1,
            "version": env!("CARGO_PKG_VERSION"),
            "target": format!("{}-apple-darwin", std::env::consts::ARCH),
            "binary_sha256": format!("{:x}", Sha256::digest(fs::read(&candidate).unwrap()))
        }))
        .unwrap(),
    )
    .unwrap();
    clipbridge::release::validate_native_bundle(&source).unwrap();
    ctx.executable = candidate;
    ctx.source = None;
    installer::install(&ctx, &source, Some(&prefix), true).unwrap();
    assert_eq!(root.join("previous").canonicalize().unwrap(), first);
    assert_eq!(fs::read(ctx.default_config()).unwrap(), before);
    fs::remove_dir_all(&source).unwrap();
    let output = Command::new(&command)
        .arg("--version")
        .current_dir(&home)
        .env("PATH", "/nonexistent")
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        format!("clipbridge {}", env!("CARGO_PKG_VERSION"))
    );
    let release = root.join("current").canonicalize().unwrap();
    assert!(!release.join("src/auto_upload.py").exists());
    assert!(!release.join("src/swift").exists());
    ctx.executable = release.join("clipbridge");
    ctx.source = None;
    installer::uninstall(&ctx, None).unwrap();
    assert!(!root.exists());
    assert!(!command.exists());
    assert_eq!(fs::read(ctx.default_config()).unwrap(), before);
}
