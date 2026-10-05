//! Opt-in remote image clipboard setup and publication over the configured SSH host.
use crate::common::{self, CommandSpec, Context, atomic_write, shell_quote};
use crate::config::{Config, SSH_OPTIONS};
use anyhow::{Context as _, Result, bail};
use flate2::{Compression, write::GzEncoder};
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::Path;

const HELPER_FILES: &[(&str, &[u8])] = &[
    ("Cargo.toml", include_bytes!("../remote/Cargo.toml")),
    ("Cargo.lock", include_bytes!("../remote/Cargo.lock")),
    ("src/main.rs", include_bytes!("../remote/src/main.rs")),
    ("src/shell.rs", include_bytes!("../remote/src/shell.rs")),
];
const MARKER: &str = "clipbridge-remote-install-v1";

fn ssh(
    ctx: &Context,
    config: &Config,
    command: &str,
    timeout: u64,
    draining: bool,
) -> Result<String> {
    config.validate()?;
    let mut spec = CommandSpec::new("/usr/bin/ssh")
        .args(SSH_OPTIONS.iter().copied())
        .args([config.ssh_host.as_str(), command])
        .timeout(timeout);
    if draining {
        spec = spec.uncancellable();
    }
    Ok(ctx.exec(spec)?.stdout)
}
fn helper(ctx: &Context, config: &Config, action: &str, timeout: u64) -> Result<String> {
    ssh(
        ctx,
        config,
        &format!("exec \"$HOME/.local/bin/clipbridge-remote\" {action}"),
        timeout,
        false,
    )
}

pub fn publish(ctx: &Context, config: &Config, remote_path: &str) -> Result<()> {
    let command = format!(
        "exec \"$HOME/.local/bin/clipbridge-remote\" publish -- {}",
        shell_quote(remote_path)
    );
    ssh(ctx, config, &command, 35, true)
        .context("Image uploaded, but remote clipboard publication failed")?;
    Ok(())
}

fn archive(ctx: &Context) -> Result<tempfile::NamedTempFile> {
    let source = tempfile::tempdir()?;
    for (name, contents) in HELPER_FILES {
        let path = source.path().join(name);
        fs::create_dir_all(path.parent().context("source file has no parent")?)?;
        fs::write(path, contents)?;
    }
    let vendor = source.path().join("vendor");
    fs::create_dir(&vendor)?;
    ctx.exec(CommandSpec::new("cargo")
        .args(["vendor", "--locked", "--respect-source-config", "--versioned-dirs", "--manifest-path"])
        .args([source.path().join("Cargo.toml").as_os_str(), vendor.as_os_str()])
        .current_dir(source.path()).timeout(600))
        .context("Remote setup needs Cargo on this Mac to prepare locked dependencies for an offline remote build")?;
    let file = tempfile::NamedTempFile::new()?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut tar = tar::Builder::new(encoder);
    for (name, contents) in HELPER_FILES {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o600);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_cksum();
        tar.append_data(&mut header, *name, *contents)?;
    }
    let configuration = b"[source.crates-io]\nreplace-with = 'vendored-sources'\n[source.vendored-sources]\ndirectory = 'vendor'\n";
    let mut header = tar::Header::new_gnu();
    header.set_size(configuration.len() as u64);
    header.set_mode(0o600);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_cksum();
    tar.append_data(&mut header, ".cargo/config.toml", configuration.as_slice())?;
    tar.append_dir_all("vendor", &vendor)?;
    let mut file = tar.into_inner()?.finish()?;
    file.flush()?;
    Ok(file)
}

fn prepare_script() -> String {
    format!(
        r#"set -eu
umask 077
root="$HOME/.local/share/clipbridge-remote"
if [ -e "$root" ] || [ -L "$root" ]; then
  [ ! -L "$root" ] && [ ! -L "$root/.owned" ] && [ -f "$root/.owned" ] && [ "$(cat "$root/.owned")" = "{MARKER}" ] || {{ echo 'Remote helper directory is not managed by ClipBridge' >&2; exit 1; }}
else
  mkdir -p "$root"
  printf '%s\n' '{MARKER}' > "$root/.owned"
fi
mkdir -p "$root/sources" "$root/bin"
[ ! -L "$root/sources" ] && [ ! -L "$root/bin" ] || {{ echo 'Refusing symlink helper subdirectory' >&2; exit 1; }}
"#
    )
}
fn install_script(id: &str) -> String {
    format!(
        r#"set -eu
umask 077
root="$HOME/.local/share/clipbridge-remote"
[ ! -L "$root" ] && [ "$(cat "$root/.owned")" = "{MARKER}" ]
source="$root/sources/{id}"
mkdir "$source"
tar -xzf "$root/upload-{id}.tar.gz" -C "$source"
rm "$root/upload-{id}.tar.gz"
cargo_bin="$HOME/.cargo/bin/cargo"
if [ ! -x "$cargo_bin" ]; then cargo_bin=$(command -v cargo); fi
cd "$source"
"$cargo_bin" build --quiet --release --frozen --manifest-path "$source/Cargo.toml" --target-dir "$root/build"
mkdir -p "$HOME/.local/bin"
[ ! -L "$HOME/.local/bin" ] || {{ echo 'Refusing symlink command directory' >&2; exit 1; }}
launcher="$HOME/.local/bin/clipbridge-remote"
if [ -e "$launcher" ] || [ -L "$launcher" ]; then
  [ -L "$launcher" ] && [ "$(readlink "$launcher")" = "$root/bin/clipbridge-remote" ] || {{ echo 'Existing remote command is unmanaged' >&2; exit 1; }}
fi
cp "$root/build/release/clipbridge-remote" "$root/bin/.new-{id}"
chmod 700 "$root/bin/.new-{id}"
mv -f "$root/bin/.new-{id}" "$root/bin/clipbridge-remote"
if [ ! -L "$launcher" ]; then ln -s "$root/bin/clipbridge-remote" "$launcher"; fi
"$launcher" --version
"#
    )
}

/// Deploy source as unprivileged user, verify dependencies, then enable sync.
/// Missing packages are reported; this function never runs sudo/apt itself.
pub fn setup_with_shell(
    ctx: &Context,
    config_path: &Path,
    integrate_shell: bool,
) -> Result<String> {
    let config = Config::load(config_path)?;
    println!("Preparing remote source and locked dependencies on this Mac...");
    let file = archive(ctx)?;
    let id = common::unique_id();
    ssh(ctx, &config, &prepare_script(), 20, false)?;
    let destination = format!(
        "{}:.local/share/clipbridge-remote/upload-{id}.tar.gz",
        config.ssh_host
    );
    ctx.exec(
        CommandSpec::new("/usr/bin/scp")
            .args(["-q"])
            .args(SSH_OPTIONS.iter().copied())
            .args([file.path().as_os_str(), destination.as_ref()])
            .timeout(60),
    )?;
    println!("Building the remote helper offline...");
    let version = ssh(ctx, &config, &install_script(&id), 600, false)?;
    helper(ctx,&config,"doctor",20).context(
        "Remote helper is deployed but prerequisites are missing. On the remote Linux host install xvfb, xclip and xauth, then rerun remote setup")?;
    helper(ctx, &config, "status", 20).context(
        "Remote helper was updated, but its running backend is incompatible or unavailable. Run clipbridge remote stop, rerun remote setup, and relaunch remote CLIs through the wrapper")?;
    let shell_result = if integrate_shell {
        if Config::load(config_path)?.ssh_host != config.ssh_host {
            bail!(
                "SSH destination changed during setup; synchronization was not enabled. Rerun setup for the new host."
            );
        }
        helper(ctx, &config, "shell install", 20).context(
            "Remote helper is deployed, but shell integration could not be installed. Resolve the reported shell configuration issue, or rerun remote setup --no-shell to keep manual wrapper startup")?
    } else {
        String::new()
    };
    // Preserve formatting-independent unknown fields in the user's configuration.
    let latest = fs::read(config_path)?;
    let current: Config = serde_json::from_slice(&latest)?;
    current.validate()?;
    if current.ssh_host != config.ssh_host {
        bail!(
            "SSH destination changed during setup; remote helper was deployed to the previous host but synchronization was not enabled. Rerun setup for the new host."
        );
    }
    let mut value: Value = serde_json::from_slice(&latest)?;
    let object = value
        .as_object_mut()
        .context("configuration must be a JSON object")?;
    object.insert("remote_clipboard".into(), true.into());
    if config_path.is_symlink() {
        bail!("configuration must not be a symlink");
    }
    let mut bytes = serde_json::to_vec_pretty(&value)?;
    bytes.push(b'\n');
    atomic_write(config_path, &bytes, 0o600)?;
    let startup = if integrate_shell {
        "Reconnect SSH once, then start codex or claude normally. Existing CLI processes need to be relaunched."
    } else {
        "Manual startup selected. In your SSH terminal, launch with:\n  ~/.local/bin/clipbridge-remote run -- codex\n  ~/.local/bin/clipbridge-remote run -- claude"
    };
    Ok(format!(
        "{}{}Remote image clipboard enabled in {}.\nRestart local ClipBridge to load this setting.\n{}\nWait for the remote-ready notification before Ctrl+V.",
        version,
        shell_result,
        config_path.display(),
        startup,
    ))
}
pub fn doctor(ctx: &Context, config_path: &Path) -> Result<String> {
    helper(ctx, &Config::load(config_path)?, "doctor", 20)
}
pub fn status(ctx: &Context, config_path: &Path) -> Result<String> {
    helper(ctx, &Config::load(config_path)?, "status", 20)
}
pub fn stop(ctx: &Context, config_path: &Path) -> Result<String> {
    helper(ctx, &Config::load(config_path)?, "stop", 20)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{CommandOutput, Runner};
    use std::collections::BTreeMap;
    use std::io::Read;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, atomic::AtomicBool};
    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<CommandSpec>>,
        missing: bool,
        incompatible: bool,
        shell_failure: bool,
        config_update: Option<(PathBuf, Vec<u8>)>,
        vendor_fixture: bool,
    }
    impl Runner for Fake {
        fn run(&self, spec: &CommandSpec, _: &AtomicBool) -> Result<CommandOutput> {
            self.calls.lock().unwrap().push(spec.clone());
            if self.shell_failure
                && spec
                    .args
                    .last()
                    .is_some_and(|arg| arg.to_string_lossy().ends_with(" shell install"))
            {
                return Ok(CommandOutput {
                    code: 1,
                    stdout: String::new(),
                    stderr: "Refusing modified shell integration".into(),
                });
            }
            if self.vendor_fixture && spec.program == Path::new("cargo") {
                let source = spec.cwd.as_ref().expect("vendoring needs an isolated cwd");
                let vendor = source.join("vendor/example-1.0.0");
                fs::create_dir_all(&vendor)?;
                fs::write(
                    vendor.join("Cargo.toml"),
                    b"[package]\nname='example'\nversion='1.0.0'\n",
                )?;
                fs::write(
                    vendor.join(".cargo-checksum.json"),
                    br#"{"files":{},"package":null}"#,
                )?;
            }
            let is_status = spec
                .args
                .last()
                .is_some_and(|arg| arg.to_string_lossy().ends_with(" status"));
            if is_status {
                if self.incompatible {
                    return Ok(CommandOutput {
                        code: 1,
                        stdout: String::new(),
                        stderr: "remote server version 0.2.0 differs".into(),
                    });
                }
                if let Some((path, contents)) = &self.config_update {
                    fs::write(path, contents)?;
                }
            }
            if self.missing
                && spec
                    .args
                    .last()
                    .is_some_and(|s| s.to_string_lossy().ends_with(" doctor"))
            {
                return Ok(CommandOutput {
                    code: 1,
                    stdout: String::new(),
                    stderr: "Missing xvfb-run xclip".into(),
                });
            }
            Ok(CommandOutput {
                code: 0,
                stdout: "ready\n".into(),
                stderr: String::new(),
            })
        }
    }
    fn context(home: &Path, runner: Arc<Fake>) -> Context {
        Context {
            home: home.into(),
            executable: home.join("clipbridge"),
            source: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            runner,
            uid: 501,
        }
    }
    fn setup(ctx: &Context, path: &Path) -> Result<String> {
        setup_with_shell(ctx, path, true)
    }
    #[test]
    fn publication_uses_ssh_argv_and_keeps_draining_after_cancellation() {
        let home = tempfile::tempdir().unwrap();
        let runner = Arc::new(Fake {
            calls: Mutex::new(vec![]),
            missing: false,
            ..Fake::default()
        });
        let ctx = context(home.path(), runner.clone());
        let cfg = Config::new("host", "/tmp/images").unwrap();
        publish(&ctx, &cfg, "/tmp/images/shot.png").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(!calls[0].cancellable);
        for option in SSH_OPTIONS {
            assert!(calls[0].args.iter().any(|a| a == option));
        }
        assert!(
            calls[0]
                .args
                .last()
                .unwrap()
                .to_string_lossy()
                .contains("publish -- /tmp/images/shot.png")
        );
    }
    #[test]
    fn setup_enables_flag_only_after_dependencies_pass_and_preserves_unknown_fields() {
        for missing in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let runner = Arc::new(Fake {
                calls: Mutex::new(vec![]),
                missing,
                ..Fake::default()
            });
            let ctx = context(home.path(), runner);
            let path = home.path().join("config.json");
            fs::write(
                &path,
                r#"{"ssh_host":"host","remote_directory":"/tmp/images","custom":"preserve"}"#,
            )
            .unwrap();
            let before = fs::read(&path).unwrap();
            let result = setup(&ctx, &path);
            if missing {
                assert!(result.is_err());
                assert_eq!(fs::read(&path).unwrap(), before);
            } else {
                assert!(result.is_ok());
                let value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                assert_eq!(value["remote_clipboard"], true);
                assert_eq!(value["custom"], "preserve");
            }
        }
    }

    #[test]
    fn incompatible_running_daemon_prevents_enabling_sync_and_preserves_configuration() {
        let home = tempfile::tempdir().unwrap();
        let runner = Arc::new(Fake {
            incompatible: true,
            ..Fake::default()
        });
        let ctx = context(home.path(), runner.clone());
        let path = home.path().join("config.json");
        let original = b"{\"ssh_host\":\"host\",\"remote_directory\":\"/tmp/images\",\"custom\":\"keep formatting\"}\n";
        fs::write(&path, original).unwrap();

        let error = setup(&ctx, &path).unwrap_err();

        assert_eq!(fs::read(&path).unwrap(), original);
        let message = format!("{error:#}");
        assert!(message.contains("running backend is incompatible"));
        assert!(message.contains("remote server version 0.2.0 differs"));
        assert!(message.contains("remote stop"));
        let calls = runner.calls.lock().unwrap();
        let doctor = calls
            .iter()
            .position(|call| {
                call.args
                    .last()
                    .is_some_and(|arg| arg.to_string_lossy().ends_with(" doctor"))
            })
            .unwrap();
        let status = calls
            .iter()
            .position(|call| {
                call.args
                    .last()
                    .is_some_and(|arg| arg.to_string_lossy().ends_with(" status"))
            })
            .unwrap();
        assert!(doctor < status);
        assert!(calls.iter().all(|call| {
            call.args
                .last()
                .is_none_or(|arg| !arg.to_string_lossy().ends_with(" shell install"))
        }));
    }

    #[test]
    fn shell_installation_failure_preserves_configuration_and_manual_mode_skips_it() {
        for integrate_shell in [true, false] {
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join("config.json");
            let original =
                br#"{"ssh_host":"host","remote_directory":"/tmp/images","custom":"keep"}"#;
            fs::write(&path, original).unwrap();
            let runner = Arc::new(Fake {
                shell_failure: true,
                ..Fake::default()
            });
            let ctx = context(home.path(), runner.clone());
            let result = setup_with_shell(&ctx, &path, integrate_shell);
            let calls = runner.calls.lock().unwrap();
            let shell = calls.iter().position(|call| {
                call.args
                    .last()
                    .is_some_and(|arg| arg.to_string_lossy().ends_with(" shell install"))
            });
            if integrate_shell {
                let error = format!("{:#}", result.unwrap_err());
                assert!(error.contains("remote setup --no-shell"));
                assert_eq!(fs::read(&path).unwrap(), original);
                let status = calls
                    .iter()
                    .position(|call| {
                        call.args
                            .last()
                            .is_some_and(|arg| arg.to_string_lossy().ends_with(" status"))
                    })
                    .unwrap();
                assert!(status < shell.unwrap());
            } else {
                assert!(result.unwrap().contains("Manual startup selected"));
                assert!(shell.is_none());
                assert!(Config::load(&path).unwrap().remote_clipboard);
            }
        }
    }

    #[test]
    fn successful_setup_installs_shell_integration_and_explains_normal_startup() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.json");
        Config::new("host", "/tmp/images")
            .unwrap()
            .save(&path)
            .unwrap();
        let runner = Arc::new(Fake::default());
        let ctx = context(home.path(), runner.clone());
        let message = setup(&ctx, &path).unwrap();
        assert!(message.contains("start codex or claude normally"));
        assert!(runner.calls.lock().unwrap().iter().any(|call| {
            call.args
                .last()
                .is_some_and(|arg| arg.to_string_lossy().ends_with(" shell install"))
        }));
        assert!(Config::load(&path).unwrap().remote_clipboard);
    }

    #[test]
    fn changing_ssh_host_during_setup_preserves_new_configuration_without_enabling_sync() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.json");
        fs::write(
            &path,
            br#"{"ssh_host":"original","remote_directory":"/tmp/images"}"#,
        )
        .unwrap();
        let replacement = br#"{"ssh_host":"replacement","remote_directory":"/tmp/new-images","remote_clipboard":false,"custom":"new setting"}"#.to_vec();
        let runner = Arc::new(Fake {
            config_update: Some((path.clone(), replacement.clone())),
            ..Fake::default()
        });
        let ctx = context(home.path(), runner.clone());

        let error = setup(&ctx, &path).unwrap_err();

        assert!(format!("{error:#}").contains("SSH destination changed during setup"));
        assert_eq!(fs::read(&path).unwrap(), replacement);
        let calls = runner.calls.lock().unwrap();
        for call in calls
            .iter()
            .filter(|call| call.program == Path::new("/usr/bin/ssh"))
        {
            assert_eq!(call.args[call.args.len() - 2], "original");
        }
    }

    #[test]
    fn archive_vendors_from_isolated_cargo_directory_and_includes_offline_source_configuration() {
        let home = tempfile::tempdir().unwrap();
        let runner = Arc::new(Fake {
            vendor_fixture: true,
            ..Fake::default()
        });
        let ctx = context(home.path(), runner.clone());

        let file = archive(&ctx).unwrap();

        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let vendor = &calls[0];
        assert_eq!(vendor.program, Path::new("cargo"));
        let source = vendor.cwd.as_ref().unwrap();
        assert_ne!(source, &std::env::current_dir().unwrap());
        assert_ne!(source, home.path());
        assert!(
            !source.exists(),
            "temporary vendoring tree must be cleaned up after archiving"
        );
        assert_eq!(vendor.args.first().unwrap(), "vendor");
        assert!(vendor.args.iter().any(|arg| arg == "--locked"));
        let manifest_position = vendor
            .args
            .iter()
            .position(|arg| arg == "--manifest-path")
            .unwrap();
        assert_eq!(
            vendor.args[manifest_position + 1],
            source.join("Cargo.toml").as_os_str()
        );
        assert_eq!(
            vendor.args.last().unwrap(),
            source.join("vendor").as_os_str()
        );

        let compressed = flate2::read::GzDecoder::new(fs::File::open(file.path()).unwrap());
        let mut tar = tar::Archive::new(compressed);
        let mut contents = BTreeMap::new();
        for entry in tar.entries().unwrap() {
            let mut entry = entry.unwrap();
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let path = entry.path().unwrap().into_owned();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            contents.insert(path, bytes);
        }
        for (name, expected) in HELPER_FILES {
            assert_eq!(contents.get(Path::new(name)).unwrap(), expected);
        }
        let configuration =
            String::from_utf8(contents.remove(Path::new(".cargo/config.toml")).unwrap()).unwrap();
        assert!(configuration.contains("[source.crates-io]"));
        assert!(configuration.contains("replace-with = 'vendored-sources'"));
        assert!(configuration.contains("directory = 'vendor'"));
        assert!(contents.contains_key(Path::new("vendor/example-1.0.0/Cargo.toml")));
        assert!(contents.contains_key(Path::new("vendor/example-1.0.0/.cargo-checksum.json")));
        assert_eq!(contents.len(), HELPER_FILES.len() + 2);
    }
}
