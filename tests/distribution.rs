//! Offline distribution checks never invoke live services or a real installer.
use anyhow::{Result, bail};
use clipbridge::common::{CommandOutput, CommandSpec, Context, Runner, VERSION};
use clipbridge::release;
use flate2::{Compression, GzBuilder};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex, atomic::AtomicBool};

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Default)]
struct FakeDownload {
    calls: Mutex<Vec<CommandSpec>>,
    tamper: bool,
}
impl Runner for FakeDownload {
    fn run(&self, command: &CommandSpec, _: &AtomicBool) -> Result<CommandOutput> {
        self.calls.lock().unwrap().push(command.clone());
        if command.program != Path::new("/usr/bin/curl") {
            bail!("unexpected executable");
        }
        let args: Vec<_> = command
            .args
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect();
        let url = args.last().unwrap();
        let asset = release::remote_asset_name("x86_64-unknown-linux-musl")?;
        let root =
            format!("https://github.com/ruoye-ruolan/clip-bridge/releases/download/v{VERSION}/");
        let output = args.iter().position(|arg| arg == "--output").unwrap() + 1;
        let bytes = b"synthetic companion bytes";
        if url == &format!("{root}{asset}.sha256") {
            fs::write(
                args[output].as_ref(),
                format!("{}  {asset}\n", digest(bytes)),
            )?;
        } else if url == &format!("{root}{asset}") {
            fs::write(
                args[output].as_ref(),
                if self.tamper {
                    b"tampered".as_slice()
                } else {
                    bytes
                },
            )?;
        } else {
            bail!("unexpected URL: {url}");
        }
        Ok(CommandOutput::default())
    }
}
fn context(home: &Path, runner: Arc<dyn Runner>) -> Context {
    Context {
        home: home.to_owned(),
        executable: home.join("not-executed"),
        source: None,
        cancelled: Arc::new(AtomicBool::new(false)),
        runner,
        uid: 501,
    }
}
fn local_asset(directory: &Path, content: &[u8]) -> String {
    let name = release::remote_asset_name("x86_64-unknown-linux-musl").unwrap();
    fs::write(directory.join(&name), content).unwrap();
    fs::write(
        directory.join(format!("{name}.sha256")),
        format!("{}  {name}\n", digest(content)),
    )
    .unwrap();
    name
}
#[test]
fn linux_architectures_have_one_versioned_asset_without_wsl_special_cases() {
    assert_eq!(
        release::remote_target("x86_64\n").unwrap(),
        "x86_64-unknown-linux-musl"
    );
    assert_eq!(
        release::remote_target("aarch64").unwrap(),
        "aarch64-unknown-linux-musl"
    );
    assert_eq!(
        release::remote_target("arm64").unwrap(),
        "aarch64-unknown-linux-musl"
    );
    assert!(release::remote_target("armv7l").is_err());
    assert!(release::remote_asset_name("../../other").is_err());
    assert_eq!(release::companion_version(), "0.3.2");
}
#[test]
fn official_download_is_https_bounded_versioned_and_checksum_verified() {
    let home = tempfile::tempdir().unwrap();
    let runner = Arc::new(FakeDownload::default());
    let ctx = context(home.path(), runner.clone());
    let file = release::download_companion(&ctx, "x86_64-unknown-linux-musl", None).unwrap();
    assert_eq!(fs::read(file.path()).unwrap(), b"synthetic companion bytes");
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    for call in calls.iter() {
        let args: Vec<_> = call.args.iter().map(|arg| arg.to_string_lossy()).collect();
        assert!(args.windows(2).any(|pair| pair == ["--proto", "=https"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--proto-redir", "=https"])
        );
        assert!(args.iter().any(|arg| arg == "--max-filesize"));
        assert!(call.timeout.as_secs() <= 190);
    }
    drop(calls);
    let broken = context(
        home.path(),
        Arc::new(FakeDownload {
            tamper: true,
            ..FakeDownload::default()
        }),
    );
    assert!(release::download_companion(&broken, "x86_64-unknown-linux-musl", None).is_err());
}
#[test]
fn offline_assets_reject_mismatch_other_names_extra_lines_and_symlinks() {
    let directory = tempfile::tempdir().unwrap();
    let runner = Arc::new(FakeDownload::default());
    let ctx = context(directory.path(), runner.clone());
    let name = local_asset(directory.path(), b"companion");
    let verified =
        release::download_companion(&ctx, "x86_64-unknown-linux-musl", Some(directory.path()))
            .unwrap();
    assert_eq!(fs::read(verified.path()).unwrap(), b"companion");
    assert!(runner.calls.lock().unwrap().is_empty());
    for checksum in [
        format!("{}  other\n", digest(b"companion")),
        format!("{}  {name}\nextra\n", digest(b"companion")),
        format!("{}  {name}\n", "0".repeat(64)),
    ] {
        fs::write(directory.path().join(format!("{name}.sha256")), checksum).unwrap();
        assert!(
            release::download_companion(&ctx, "x86_64-unknown-linux-musl", Some(directory.path()))
                .is_err()
        );
    }
    local_asset(directory.path(), b"companion");
    let binary = directory.path().join(&name);
    fs::rename(&binary, directory.path().join("original")).unwrap();
    symlink("original", &binary).unwrap();
    assert!(
        release::download_companion(&ctx, "x86_64-unknown-linux-musl", Some(directory.path()))
            .is_err()
    );
    assert!(runner.calls.lock().unwrap().is_empty());
}
#[test]
fn offline_assets_reject_special_and_oversized_files_before_copying() {
    let directory = tempfile::tempdir().unwrap();
    let ctx = context(directory.path(), Arc::new(FakeDownload::default()));
    let name = local_asset(directory.path(), b"companion");
    let binary = directory.path().join(name);
    fs::OpenOptions::new()
        .write(true)
        .open(&binary)
        .unwrap()
        .set_len(64 * 1024 * 1024 + 1)
        .unwrap();
    assert!(
        release::download_companion(&ctx, "x86_64-unknown-linux-musl", Some(directory.path()))
            .is_err()
    );
    fs::remove_file(&binary).unwrap();
    nix::unistd::mkfifo(&binary, nix::sys::stat::Mode::S_IRUSR).unwrap();
    assert!(
        release::download_companion(&ctx, "x86_64-unknown-linux-musl", Some(directory.path()))
            .is_err()
    );
}
fn native_target() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64-apple-darwin"
    } else {
        "x86_64-apple-darwin"
    }
}
fn manifest(version: &str, target: &str, binary: &[u8]) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"product":"clipbridge","schema":1,"version":version,"target":target,"binary_sha256":digest(binary)})).unwrap()
}
fn native_fixture(directory: &Path, version: &str) {
    fs::create_dir_all(directory.join("src")).unwrap();
    fs::write(directory.join("clipbridge"), b"native executable").unwrap();
    fs::write(
        directory.join("bundle.json"),
        manifest(version, native_target(), b"native executable"),
    )
    .unwrap();
    fs::write(directory.join("VERSION"), version).unwrap();
    for file in [
        "Cargo.toml",
        "Cargo.lock",
        "LICENSE",
        "src/config.example.json",
    ] {
        fs::write(directory.join(file), b"fixture").unwrap();
    }
}
#[test]
fn native_upgrade_accepts_a_new_version_but_rejects_mismatched_payloads() {
    let directory = tempfile::tempdir().unwrap();
    native_fixture(directory.path(), "99.0.0");
    release::validate_native_bundle(directory.path()).unwrap();
    fs::write(directory.path().join("VERSION"), "98.0.0").unwrap();
    assert!(release::validate_native_bundle(directory.path()).is_err());
    native_fixture(directory.path(), "99.0.0");
    fs::write(directory.path().join("clipbridge"), b"different binary").unwrap();
    assert!(release::validate_native_bundle(directory.path()).is_err());
    native_fixture(directory.path(), "99.0.0");
    fs::write(
        directory.path().join("bundle.json"),
        manifest("99.0.0", "x86_64-unknown-linux-musl", b"native executable"),
    )
    .unwrap();
    assert!(release::validate_native_bundle(directory.path()).is_err());
    native_fixture(directory.path(), "99.0.0");
    fs::rename(
        directory.path().join("src"),
        directory.path().join("elsewhere"),
    )
    .unwrap();
    symlink("elsewhere", directory.path().join("src")).unwrap();
    assert!(release::validate_native_bundle(directory.path()).is_err());
}
fn bootstrap_assets(directory: &Path, extra: Option<(&str, bool)>) -> PathBuf {
    let name = format!("clipbridge-{VERSION}-{}", native_target());
    let executable = b"#!/bin/sh\nprintf 'invoked\\n' >> \"$CLIPBRIDGE_TEST_SENTINEL\"\n";
    let metadata = manifest(VERSION, native_target(), executable);
    let archive = directory.join(format!("{name}.tar.gz"));
    let writer = GzBuilder::new()
        .mtime(0)
        .write(fs::File::create(&archive).unwrap(), Compression::default());
    let mut tar = tar::Builder::new(writer);
    for (relative, bytes) in [
        ("clipbridge", executable.as_slice()),
        ("bundle.json", metadata.as_slice()),
        ("VERSION", VERSION.as_bytes()),
        ("Cargo.toml", b"manifest"),
        ("Cargo.lock", b"lock"),
        ("LICENSE", b"MIT"),
        ("src/config.example.json", b"{}"),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, format!("{name}/{relative}"), bytes)
            .unwrap();
    }
    if let Some((path, symbolic)) = extra {
        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o644);
        if symbolic {
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_link_name("/outside/must-not-touch").unwrap();
        }
        header.set_cksum();
        tar.append_data(&mut header, path.replace("ROOT", &name), std::io::empty())
            .unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap();
    fs::write(
        archive.with_extension("gz.sha256"),
        format!("{}  {name}.tar.gz\n", digest(&fs::read(&archive).unwrap())),
    )
    .unwrap();
    archive
}
fn bootstrap(home: &Path, assets: &Path) -> Output {
    Command::new("/bin/sh")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("install-release.sh"))
        .args(["--assets-dir", assets.to_str().unwrap()])
        .env("HOME", home)
        .env("TMPDIR", home)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("CLIPBRIDGE_TEST_SENTINEL", home.join("executed"))
        .output()
        .unwrap()
}
#[test]
fn standalone_bootstrap_verifies_assets_without_any_language_runtime() {
    let home = tempfile::tempdir().unwrap();
    let assets = tempfile::tempdir().unwrap();
    bootstrap_assets(assets.path(), None);
    let output = bootstrap(home.path(), assets.path());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(home.path().join("executed")).unwrap(),
        b"invoked\n"
    );
    assert_eq!(
        fs::read_dir(home.path()).unwrap().count(),
        1,
        "temporary extraction should be cleaned"
    );
}

#[test]
fn standalone_setup_preserves_absolute_offline_assets_and_stdin() {
    let home = tempfile::tempdir().unwrap();
    let assets = home.path().join("offline assets");
    fs::create_dir(&assets).unwrap();
    bootstrap_assets(&assets, None);
    let installed = home.path().join(".local/bin/clipbridge");
    fs::create_dir_all(installed.parent().unwrap()).unwrap();
    fs::write(
        &installed,
        b"#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" > \"$HOME/setup-argv\"\n[ \"$#\" = 3 ] && [ \"$1\" = setup ] && [ \"$2\" = --assets-dir ] || exit 77\n[ -d \"$3\" ] || exit 78\nIFS= read -r answer\nprintf '%s\\n' \"$answer\" > \"$HOME/setup-stdin\"\n",
    )
    .unwrap();
    fs::set_permissions(&installed, fs::Permissions::from_mode(0o700)).unwrap();
    let mut child = Command::new("/bin/sh")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("install-release.sh"))
        .args(["--assets-dir", "offline assets", "--setup"])
        .current_dir(home.path())
        .env("HOME", home.path())
        .env("TMPDIR", home.path())
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("CLIPBRIDGE_TEST_SENTINEL", home.path().join("executed"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"wizard input remains available\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(home.path().join("setup-argv")).unwrap(),
        format!(
            "setup\n--assets-dir\n{}\n",
            assets.canonicalize().unwrap().display()
        )
    );
    assert_eq!(
        fs::read_to_string(home.path().join("setup-stdin")).unwrap(),
        "wizard input remains available\n"
    );
    assert!(fs::read_dir(home.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("clipbridge-install.")
    }));
}
#[test]
fn standalone_bootstrap_never_executes_bad_checksums_or_unsafe_archives() {
    for extra in [
        None,
        Some(("outside/file", false)),
        Some(("ROOT/install.sh", true)),
        Some(("ROOT/LICENSE", false)),
    ] {
        let home = tempfile::tempdir().unwrap();
        let assets = tempfile::tempdir().unwrap();
        let archive = bootstrap_assets(assets.path(), extra);
        if extra.is_none() {
            fs::write(
                archive.with_extension("gz.sha256"),
                format!(
                    "{}  {}\n",
                    "0".repeat(64),
                    archive.file_name().unwrap().to_str().unwrap()
                ),
            )
            .unwrap();
        }
        let output = bootstrap(home.path(), assets.path());
        assert!(!output.status.success());
        assert!(!home.path().join("executed").exists());
        assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
    }
}
#[test]
fn standalone_bootstrap_refuses_to_downgrade_an_owned_installation() {
    let home = tempfile::tempdir().unwrap();
    let installation = home.path().join(".local/share/clipbridge");
    fs::create_dir_all(installation.join("releases/newer")).unwrap();
    fs::write(
        installation.join(".clipbridge-install.json"),
        br#"{"product":"clipbridge","schema":1}"#,
    )
    .unwrap();
    fs::write(
        installation.join("releases/newer/.clipbridge-release.json"),
        br#"{"product":"clipbridge","schema":1,"version":"99.0.0"}"#,
    )
    .unwrap();
    symlink("releases/newer", installation.join("current")).unwrap();
    let output = bootstrap(home.path(), &home.path().join("absent-assets"));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Refusing to downgrade"));
    assert!(!home.path().join("executed").exists());
}
#[test]
fn native_package_is_reproducible_and_does_not_ship_source_or_personal_files() {
    let directory = tempfile::tempdir().unwrap();
    let package = || {
        Command::new("/bin/bash")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/package-native.sh"))
            .args([
                "--target",
                native_target(),
                "--binary",
                env!("CARGO_BIN_EXE_clipbridge"),
                "--output-dir",
            ])
            .arg(directory.path())
            .output()
            .unwrap()
    };
    let output = package();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let archive = directory
        .path()
        .join(format!("clipbridge-{VERSION}-{}.tar.gz", native_target()));
    let first = fs::read(&archive).unwrap();
    assert!(package().status.success());
    assert_eq!(first, fs::read(&archive).unwrap());
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(first.as_slice()));
    let names: Vec<_> = tar
        .entries()
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            assert!(entry.header().entry_type().is_file());
            entry.path().unwrap().to_string_lossy().into_owned()
        })
        .collect();
    assert!(names.iter().any(|name| name.ends_with("/bundle.json")));
    assert!(names.iter().any(|name| name.ends_with("/clipbridge")));
    assert!(!names.iter().any(|name| name.ends_with(".rs")
        || name.ends_with("/config.json")
        || name.contains("tutorial/")));
}
