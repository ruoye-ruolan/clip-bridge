//! Opt-in remote image clipboard setup and publication over the configured SSH host.
use crate::common::{self, CommandSpec, Context, atomic_write, shell_quote};
use crate::config::{Config, SSH_OPTIONS};
use crate::{monitor, release};
use anyhow::{Context as _, Result, bail, ensure};
use flate2::{Compression, write::GzEncoder};
use serde_json::Value;
use sha2::{Digest, Sha256};
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupMode {
    Prebuilt,
    BuildFromSource,
}

fn preflight_script() -> String {
    shell_script(
        r#"set -eu
system=$(uname -s)
[ "$system" = Linux ] || { printf '%s\n' 'ClipBridge remote image support requires Linux (including WSL).' >&2; exit 1; }
missing=
for tool in xvfb-run Xvfb xauth xclip sha256sum; do
  command -v "$tool" >/dev/null 2>&1 || missing="$missing $tool"
done
if [ -n "$missing" ]; then
  printf 'Missing remote tools:%s\n' "$missing" >&2
  printf '%s\n' 'Install the missing tools manually on the remote host before setup.' 'Debian/Ubuntu: apt-get install xvfb xauth xclip coreutils' 'Fedora/RHEL: use dnf to install packages providing Xvfb, xvfb-run, xauth, xclip and sha256sum.' 'Alpine: use apk to install packages providing Xvfb, xvfb-run, xauth, xclip and sha256sum.' >&2
  exit 1
fi
[ -d /proc/self ] || { printf '%s\n' 'Linux procfs is required for image publication.' >&2; exit 1; }
printf 'CLIPBRIDGE_PLATFORM_V1\n%s\n%s\n%s\nCLIPBRIDGE_PLATFORM_END\n' "$system" "$(uname -m)" "$HOME"
"#,
    )
}

fn shell_script(script: &str) -> String {
    format!("sh -c {}", shell_quote(script))
}

fn parse_platform(host: &str, output: &str) -> Result<String> {
    let lines: Vec<_> = output.lines().collect();
    ensure!(
        lines.len() == 5
            && lines[0] == "CLIPBRIDGE_PLATFORM_V1"
            && lines[1] == "Linux"
            && lines[4] == "CLIPBRIDGE_PLATFORM_END",
        "invalid remote Linux platform response; remove unexpected shell startup output"
    );
    Config::new(host, lines[3]).context("remote HOME must be a supported absolute path")?;
    ensure!(!lines[2].is_empty(), "remote CPU architecture is missing");
    Ok(lines[2].to_owned())
}

// Each component is checked before entering the next one. Existing permissions
// are preserved; private program directories cannot be shared with other users.
fn native_helpers() -> &'static str {
    r#"uid=$(id -u)
owned_directory() {
  [ ! -L "$1" ] && [ -d "$1" ] && [ -w "$1" ] && [ -x "$1" ] &&
  [ "$(find "$1" -prune -type d -user "$uid" ! -perm -020 ! -perm -002 -print)" = "$1" ]
}
private_directory() {
  owned_directory "$1" &&
  [ "$(find "$1" -prune ! -perm -040 ! -perm -010 ! -perm -004 ! -perm -001 -print)" = "$1" ]
}
owned_file() {
  [ ! -L "$1" ] && [ -f "$1" ] &&
  [ "$(find "$1" -prune -type f -user "$uid" -links 1 ! -perm -020 ! -perm -002 -print)" = "$1" ]
}
"#
}

fn native_guard() -> String {
    format!(
        r#"{helpers}root="$HOME/.local/share/clipbridge-remote"
owned_directory "$HOME" && owned_directory "$HOME/.local" && owned_directory "$HOME/.local/share" || {{ printf '%s\n' 'Refusing unsafe remote helper parent directory' >&2; exit 1; }}
private_directory "$root" && owned_file "$root/.owned" && [ "$(cat "$root/.owned")" = '{MARKER}' ] || {{ printf '%s\n' 'Remote helper directory is not privately managed by ClipBridge' >&2; exit 1; }}
private_directory "$root/bin" && private_directory "$root/staging" || {{ printf '%s\n' 'Refusing unsafe remote helper subdirectory' >&2; exit 1; }}
"#,
        helpers = native_helpers(),
    )
}

fn native_prepare_script(id: &str) -> String {
    let helpers = native_helpers();
    shell_script(&format!(
        r#"set -eu
umask 077
{helpers}
owned_directory "$HOME" || {{ printf '%s\n' 'Refusing unsafe remote HOME' >&2; exit 1; }}
for path in "$HOME/.local" "$HOME/.local/share"; do
  if [ ! -e "$path" ] && [ ! -L "$path" ]; then mkdir "$path"; fi
  owned_directory "$path" || {{ printf '%s\n' 'Refusing unsafe helper parent directory' >&2; exit 1; }}
done
root="$HOME/.local/share/clipbridge-remote"
if [ -e "$root" ] || [ -L "$root" ]; then
  private_directory "$root" && owned_file "$root/.owned" && [ "$(cat "$root/.owned")" = '{MARKER}' ] || {{ printf '%s\n' 'Remote helper directory is not privately managed by ClipBridge' >&2; exit 1; }}
else
  mkdir "$root"
  printf '%s\n' '{MARKER}' > "$root/.owned"
fi
for path in "$root/bin" "$root/staging"; do
  if [ ! -e "$path" ] && [ ! -L "$path" ]; then mkdir "$path"; fi
  private_directory "$path" || {{ printf '%s\n' 'Refusing unsafe remote helper subdirectory' >&2; exit 1; }}
done
mkdir "$root/staging/{id}"
"#
    ))
}

fn native_install_script(id: &str, size: u64, digest: &str, integrate_shell: bool) -> String {
    let shell = if integrate_shell {
        "\"$installed\" shell install"
    } else {
        ":"
    };
    shell_script(&format!(
        r#"set -eu
umask 077
{guard}
stage="$root/staging/{id}"
private_directory "$stage" || {{ printf '%s\n' 'Refusing unsafe upload staging directory' >&2; exit 1; }}
candidate="$stage/clipbridge-remote"
backup="$stage/previous"
installed="$root/bin/clipbridge-remote"
launcher="$HOME/.local/bin/clipbridge-remote"
lockdir="$root/.install-lock"
hash() {{ result=$(sha256sum "$1") || return; printf '%s\n' "${{result%% *}}"; }}
owned_file "$candidate" || {{ printf '%s\n' 'Refusing non-regular uploaded helper' >&2; exit 1; }}
[ "$(wc -c < "$candidate")" -eq {size} ] && [ "$(hash "$candidate")" = '{digest}' ] || {{ printf '%s\n' 'Remote helper checksum or byte count does not match' >&2; exit 1; }}
chmod 700 "$candidate"
[ "$("$candidate" --version)" = 'clipbridge-remote {version}' ] || {{ printf '%s\n' 'Remote helper version does not match' >&2; exit 1; }}
"$candidate" doctor
"$candidate" status
if [ ! -e "$HOME/.local/bin" ] && [ ! -L "$HOME/.local/bin" ]; then mkdir "$HOME/.local/bin"; fi
owned_directory "$HOME/.local/bin" || {{ printf '%s\n' 'Refusing unsafe command directory' >&2; exit 1; }}
had_binary=0
promoted=0
created_launcher=0
success=0
previous_hash=
held_lock=0
cleanup() {{
  status=$?
  trap - EXIT HUP INT TERM
  set +e
  preserve=0
  if [ "$success" != 1 ] && [ "$promoted" = 1 ]; then
    if owned_file "$installed" && [ "$(hash "$installed")" = '{digest}' ]; then
      if [ "$had_binary" = 1 ]; then
        if owned_file "$backup" && [ "$(hash "$backup")" = "$previous_hash" ]; then
          mv -f "$backup" "$installed" || preserve=1
        else preserve=1; fi
      else rm "$installed" || preserve=1; fi
    else preserve=1; fi
    if [ "$created_launcher" = 1 ]; then
      if [ -L "$launcher" ] && [ "$(readlink "$launcher")" = "$installed" ]; then
        rm "$launcher" || preserve=1
      elif [ -e "$launcher" ] || [ -L "$launcher" ]; then preserve=1; fi
    fi
  fi
  if [ "$preserve" = 0 ]; then
    if [ -e "$backup" ] || [ -L "$backup" ]; then
      if owned_file "$backup" && [ "$(hash "$backup")" = "$previous_hash" ]; then rm "$backup" || preserve=1; else preserve=1; fi
    fi
    if owned_file "$candidate" && [ "$(hash "$candidate")" = '{digest}' ]; then rm "$candidate" || preserve=1; fi
    rmdir "$stage" 2>/dev/null || :
  fi
  if [ "$held_lock" = 1 ]; then
    if private_directory "$lockdir" && owned_file "$lockdir/owner" && [ "$(cat "$lockdir/owner")" = '{id}' ]; then
      rm "$lockdir/owner" && rmdir "$lockdir" || preserve=1
    else preserve=1; fi
  fi
  if [ "$preserve" != 0 ]; then
    printf 'Remote helper recovery could not safely finish; preserved files at %s\n' "$stage" >&2
    status=1
  fi
  exit "$status"
}}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
mkdir "$lockdir" || {{ printf '%s\n' 'Another remote helper installation holds .install-lock; inspect it before retrying.' >&2; exit 1; }}
held_lock=1
printf '%s\n' '{id}' > "$lockdir/owner"
if [ -e "$installed" ] || [ -L "$installed" ]; then
  owned_file "$installed" || {{ printf '%s\n' 'Refusing unfamiliar existing helper executable' >&2; exit 1; }}
  had_binary=1
fi
if [ -e "$launcher" ] || [ -L "$launcher" ]; then
  [ -L "$launcher" ] && [ "$(find "$launcher" -prune -type l -user "$uid" -print)" = "$launcher" ] && [ "$(readlink "$launcher")" = "$installed" ] || {{ printf '%s\n' 'Existing remote command is unmanaged' >&2; exit 1; }}
fi
if [ "$had_binary" = 1 ]; then
  previous_hash=$(hash "$installed")
  cp -p "$installed" "$backup"
fi
mv -f "$candidate" "$installed"
promoted=1
if [ ! -L "$launcher" ]; then
  ln -s "$installed" "$launcher"
  created_launcher=1
fi
{shell}
"$installed" --version
success=1
printf 'CLIPBRIDGE_INSTALLED_{id}\n'
"#,
        guard = native_guard(),
        version = release::companion_version()
    ))
}

fn operation_config(ctx: &Context, config_path: &Path) -> Result<(Config, Config)> {
    let configured = Config::load(config_path)?;
    let host =
        monitor::active_host(ctx, &configured).unwrap_or_else(|| configured.ssh_host.clone());
    let selected = configured.for_host(&host)?;
    println!("SSH route: {host}");
    Ok((configured, selected))
}

fn check_setup_destination(current: &Config, original: &Config) -> Result<()> {
    current.validate()?;
    if current.ssh_host != original.ssh_host
        || current.ssh_aliases != original.ssh_aliases
        || current.remote_directory.trim_end_matches('/')
            != original.remote_directory.trim_end_matches('/')
    {
        bail!(
            "SSH destination changed during setup; synchronization was not enabled. Rerun setup with the current configuration."
        );
    }
    Ok(())
}

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

/// Deploy the verified native companion by default, without remote build tools.
pub fn setup_with_shell(
    ctx: &Context,
    config_path: &Path,
    integrate_shell: bool,
) -> Result<String> {
    setup_with_options(ctx, config_path, integrate_shell, SetupMode::Prebuilt, None)
}

/// Select one SSH route for the entire deployment. System packages are never installed.
pub fn setup_with_options(
    ctx: &Context,
    config_path: &Path,
    integrate_shell: bool,
    mode: SetupMode,
    assets_dir: Option<&Path>,
) -> Result<String> {
    ensure!(
        mode == SetupMode::Prebuilt || assets_dir.is_none(),
        "--assets-dir cannot be combined with --build-from-source"
    );
    let (original, config) = operation_config(ctx, config_path)?;
    let platform = ssh(ctx, &config, &preflight_script(), 35, false)?;
    let machine = parse_platform(&config.ssh_host, &platform)?;
    let deployment = match mode {
        SetupMode::Prebuilt => {
            let target = release::remote_target(&machine)?;
            println!("Preparing verified Linux companion for {target}...");
            let file = release::download_companion(ctx, target, assets_dir)?;
            check_setup_destination(&Config::load(config_path)?, &original)?;
            let size = file.as_file().metadata()?.len();
            let digest = format!("{:x}", Sha256::digest(fs::read(file.path())?));
            let id = common::unique_id();
            ssh(ctx, &config, &native_prepare_script(&id), 35, false)?;
            let destination = format!(
                "{}:.local/share/clipbridge-remote/staging/{id}/clipbridge-remote",
                config.ssh_host
            );
            ctx.exec(
                CommandSpec::new("/usr/bin/scp")
                    .args(["-q"])
                    .args(SSH_OPTIONS.iter().copied())
                    .args([file.path().as_os_str(), destination.as_ref()])
                    .timeout(150),
            )?;
            check_setup_destination(&Config::load(config_path)?, &original)?;
            if ctx.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                bail!("setup cancelled before activating the staged remote helper");
            }
            // Once promotion starts, allow the remote transaction to finish or roll
            // back even if the caller cancels. No clipboard server is started.
            let output = ssh(
                ctx,
                &config,
                &native_install_script(&id, size, &digest, integrate_shell),
                90,
                true,
            )
            .context("Remote helper activation failed; inspect the reported recovery result")?;
            let receipt = format!("CLIPBRIDGE_INSTALLED_{id}");
            ensure!(
                output
                    .split_inclusive('\n')
                    .any(|line| line == format!("{receipt}\n")),
                "remote helper installation was not confirmed; inspect its status before retrying"
            );
            output
                .lines()
                .filter(|line| *line != receipt)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n"
        }
        SetupMode::BuildFromSource => {
            setup_from_source(ctx, config_path, &original, &config, integrate_shell)?
        }
    };
    enable_configuration(config_path, &original)?;
    let startup = if integrate_shell {
        "Reconnect SSH once, then start codex, claude or opencode normally. Existing CLI processes need to be relaunched."
    } else {
        "Manual startup selected. In your SSH terminal, launch with:\n  ~/.local/bin/clipbridge-remote run -- codex\n  ~/.local/bin/clipbridge-remote run -- claude\n  ~/.local/bin/clipbridge-remote run -- opencode"
    };
    Ok(format!(
        "{deployment}Remote image clipboard enabled in {}.\nRestart local ClipBridge to load this setting.\n{startup}\nWait for the remote-ready notification before Ctrl+V.",
        config_path.display(),
    ))
}

fn setup_from_source(
    ctx: &Context,
    config_path: &Path,
    original: &Config,
    config: &Config,
    integrate_shell: bool,
) -> Result<String> {
    println!("Preparing remote source and locked dependencies on this Mac...");
    let file = archive(ctx)?;
    let id = common::unique_id();
    ssh(ctx, config, &prepare_script(), 20, false)?;
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
    let version = ssh(ctx, config, &install_script(&id), 600, false)?;
    helper(ctx,config,"doctor",20).context(
        "Remote helper is deployed but prerequisites are missing. On the remote Linux host install xvfb, xclip and xauth, then rerun remote setup")?;
    helper(ctx, config, "status", 20).context(
        "Remote helper was updated, but its running backend is incompatible or unavailable. Run clipbridge remote stop, rerun remote setup, and relaunch remote CLIs through the wrapper")?;
    let shell_result = if integrate_shell {
        check_setup_destination(&Config::load(config_path)?, original)?;
        helper(ctx, config, "shell install", 20).context(
            "Remote helper is deployed, but shell integration could not be installed. Resolve the reported shell configuration issue, or rerun remote setup --no-shell to keep manual wrapper startup")?
    } else {
        String::new()
    };
    Ok(format!("{version}{shell_result}"))
}

fn enable_configuration(config_path: &Path, original: &Config) -> Result<()> {
    // Preserve formatting-independent unknown fields in the user's configuration.
    let latest = fs::read(config_path)?;
    let current: Config = serde_json::from_slice(&latest)?;
    check_setup_destination(&current, original)?;
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
    Ok(())
}
pub fn doctor(ctx: &Context, config_path: &Path) -> Result<String> {
    let (_, config) = operation_config(ctx, config_path)?;
    helper(ctx, &config, "doctor", 20)
}
pub fn status(ctx: &Context, config_path: &Path) -> Result<String> {
    let (_, config) = operation_config(ctx, config_path)?;
    helper(ctx, &config, "status", 20)
}
pub fn stop(ctx: &Context, config_path: &Path) -> Result<String> {
    let (_, config) = operation_config(ctx, config_path)?;
    helper(ctx, &config, "stop", 20)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{CommandOutput, Runner};
    use std::collections::BTreeMap;
    use std::io::Read;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::PathBuf;
    use std::process::{Command, Output};
    use std::sync::{Arc, Mutex, atomic::AtomicBool};
    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<CommandSpec>>,
        missing: bool,
        incompatible: bool,
        shell_failure: bool,
        config_update: Option<(PathBuf, Vec<u8>)>,
        vendor_fixture: bool,
        active_alias: Option<String>,
        platform: Option<String>,
        preflight_failure: bool,
        omit_receipt: bool,
        update_after_transfer: Option<(PathBuf, Vec<u8>)>,
        cancel_after_transfer: bool,
    }
    impl Runner for Fake {
        fn run(&self, spec: &CommandSpec, cancelled: &AtomicBool) -> Result<CommandOutput> {
            self.calls.lock().unwrap().push(spec.clone());
            if spec.program == Path::new("/usr/bin/scp") {
                if let Some((path, bytes)) = &self.update_after_transfer {
                    fs::write(path, bytes)?;
                }
                if self.cancel_after_transfer {
                    cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
            if spec
                .args
                .last()
                .is_some_and(|arg| arg.to_string_lossy().contains("CLIPBRIDGE_PLATFORM_V1"))
            {
                return Ok(CommandOutput {
                    code: i32::from(self.preflight_failure),
                    stdout: self.platform.clone().unwrap_or_else(|| "CLIPBRIDGE_PLATFORM_V1\nLinux\nx86_64\n/home/example\nCLIPBRIDGE_PLATFORM_END\n".into()),
                    stderr: if self.preflight_failure { "Missing remote tools: xclip".into() } else { String::new() },
                });
            }
            if let Some(command) = spec.args.last().map(|arg| arg.to_string_lossy())
                && let Some(start) = command.find("CLIPBRIDGE_INSTALLED_")
            {
                let receipt: String = command[start..]
                    .chars()
                    .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                    .collect();
                return Ok(CommandOutput {
                    code: 0,
                    stdout: if self.omit_receipt {
                        receipt
                    } else {
                        receipt + "\n"
                    },
                    stderr: String::new(),
                });
            }
            if spec.program == Path::new("/bin/ps") {
                return Ok(CommandOutput {
                    code: 0,
                    stdout: self
                        .active_alias
                        .as_ref()
                        .map(|host| format!("42 1 501 ssh {host}\n"))
                        .unwrap_or_default(),
                    stderr: String::new(),
                });
            }
            if spec.program == Path::new("/usr/sbin/lsof") {
                return Ok(CommandOutput {
                    code: 0,
                    stdout: "n127.0.0.1:1234->127.0.0.2:22\n".into(),
                    stderr: String::new(),
                });
            }
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
        setup_with_options(ctx, path, true, SetupMode::BuildFromSource, None)
    }

    #[test]
    fn platform_probe_accepts_linux_hosts_and_rejects_noise_or_unsafe_home() {
        for machine in ["x86_64", "aarch64", "riscv64"] {
            let record = format!(
                "CLIPBRIDGE_PLATFORM_V1\nLinux\n{machine}\n/home/example\nCLIPBRIDGE_PLATFORM_END\n"
            );
            assert_eq!(parse_platform("server", &record).unwrap(), machine);
        }
        for record in [
            "CLIPBRIDGE_PLATFORM_V1\nDarwin\narm64\n/home/example\nCLIPBRIDGE_PLATFORM_END\n",
            "banner\nCLIPBRIDGE_PLATFORM_V1\nLinux\nx86_64\n/home/example\nCLIPBRIDGE_PLATFORM_END\n",
            "CLIPBRIDGE_PLATFORM_V1\nLinux\nx86_64\nrelative\nCLIPBRIDGE_PLATFORM_END\n",
        ] {
            assert!(parse_platform("server", record).is_err());
        }
    }

    #[test]
    fn missing_dependencies_or_unsupported_native_platform_do_not_download_or_mutate() {
        for (preflight_failure, platform) in [
            (true, None),
            (false, Some("CLIPBRIDGE_PLATFORM_V1\nLinux\nsparc64\n/home/example\nCLIPBRIDGE_PLATFORM_END\n".into())),
        ] {
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join("config.json");
            Config::new("server", "/home/example/images").unwrap().save(&path).unwrap();
            let before = fs::read(&path).unwrap();
            let runner = Arc::new(Fake { preflight_failure, platform, ..Fake::default() });
            let ctx = context(home.path(), runner.clone());
            assert!(setup_with_shell(&ctx, &path, true).is_err());
            assert_eq!(fs::read(&path).unwrap(), before);
            assert!(runner.calls.lock().unwrap().iter().all(|call| {
                call.program != Path::new("cargo") && call.program != Path::new("/usr/bin/scp")
                    && !call.args.iter().any(|arg| arg.to_string_lossy().contains("curl"))
                    && call.args.last().is_none_or(|arg| !arg.to_string_lossy().contains(MARKER))
            }));
        }
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

    fn offline_asset(directory: &Path, target: &str) {
        let name = release::remote_asset_name(target).unwrap();
        let contents = b"isolated companion fixture";
        fs::write(directory.join(&name), contents).unwrap();
        fs::write(
            directory.join(format!("{name}.sha256")),
            format!("{:x}  {name}\n", Sha256::digest(contents)),
        )
        .unwrap();
    }

    #[test]
    fn prebuilt_setup_selects_linux_architecture_and_pins_alias_without_cargo() {
        for machine in ["x86_64", "aarch64"] {
            let home = tempfile::tempdir().unwrap();
            let assets = tempfile::tempdir().unwrap();
            offline_asset(assets.path(), release::remote_target(machine).unwrap());
            let path = home.path().join("config.json");
            fs::write(&path, br#"{"ssh_host":"primary","ssh_aliases":["lan"],"remote_directory":"/home/example/images","custom":"preserve"}"#).unwrap();
            let runner = Arc::new(Fake {
                active_alias: Some("lan".into()),
                platform: Some(format!(
                    "CLIPBRIDGE_PLATFORM_V1\nLinux\n{machine}\n/home/example\nCLIPBRIDGE_PLATFORM_END\n"
                )),
                ..Fake::default()
            });
            let ctx = context(home.path(), runner.clone());
            let message =
                setup_with_options(&ctx, &path, true, SetupMode::Prebuilt, Some(assets.path()))
                    .unwrap();
            assert!(message.contains("start codex, claude or opencode normally"));
            let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(saved["remote_clipboard"], true);
            assert_eq!(saved["ssh_aliases"], serde_json::json!(["lan"]));
            assert_eq!(saved["custom"], "preserve");
            let calls = runner.calls.lock().unwrap();
            assert!(calls.iter().all(|call| call.program != Path::new("cargo")
                && call.program != Path::new("/usr/bin/curl")));
            for call in calls
                .iter()
                .filter(|call| call.program == Path::new("/usr/bin/ssh"))
            {
                assert_eq!(call.args[call.args.len() - 2], "lan");
                for option in SSH_OPTIONS {
                    assert!(call.args.iter().any(|value| value == option));
                }
            }
            let transfer = calls
                .iter()
                .find(|call| call.program == Path::new("/usr/bin/scp"))
                .unwrap();
            assert!(
                transfer
                    .args
                    .last()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("lan:.local/share/clipbridge-remote/staging/")
            );
        }
    }

    #[test]
    fn failed_prebuilt_checksum_or_missing_receipt_preserves_configuration() {
        for checksum_failure in [true, false] {
            let home = tempfile::tempdir().unwrap();
            let assets = tempfile::tempdir().unwrap();
            let target = "x86_64-unknown-linux-musl";
            offline_asset(assets.path(), target);
            if checksum_failure {
                fs::write(
                    assets
                        .path()
                        .join(release::remote_asset_name(target).unwrap()),
                    b"changed",
                )
                .unwrap();
            }
            let path = home.path().join("config.json");
            Config::new("server", "/home/example/images")
                .unwrap()
                .save(&path)
                .unwrap();
            let before = fs::read(&path).unwrap();
            let runner = Arc::new(Fake {
                omit_receipt: true,
                ..Fake::default()
            });
            let ctx = context(home.path(), runner.clone());
            assert!(
                setup_with_options(&ctx, &path, true, SetupMode::Prebuilt, Some(assets.path()))
                    .is_err()
            );
            assert_eq!(fs::read(&path).unwrap(), before);
            let calls = runner.calls.lock().unwrap();
            assert!(calls.iter().all(|call| call.program != Path::new("cargo")));
            if checksum_failure {
                assert!(
                    calls
                        .iter()
                        .all(|call| call.program != Path::new("/usr/bin/scp"))
                );
            }
        }
    }

    struct NativeFixture {
        home: tempfile::TempDir,
        binary: Vec<u8>,
        old: Option<Vec<u8>>,
    }

    #[test]
    fn cancelled_or_reconfigured_prebuilt_setup_does_not_activate_staged_helper() {
        for cancel in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let assets = tempfile::tempdir().unwrap();
            offline_asset(assets.path(), "x86_64-unknown-linux-musl");
            let path = home.path().join("config.json");
            Config::new("server", "/home/example/images")
                .unwrap()
                .save(&path)
                .unwrap();
            let before = fs::read(&path).unwrap();
            let replacement = br#"{"ssh_host":"changed","remote_directory":"/home/example/images","custom":"preserve"}"#.to_vec();
            let runner = Arc::new(Fake {
                update_after_transfer: (!cancel).then(|| (path.clone(), replacement.clone())),
                cancel_after_transfer: cancel,
                ..Fake::default()
            });
            let ctx = context(home.path(), runner.clone());
            assert!(
                setup_with_options(&ctx, &path, true, SetupMode::Prebuilt, Some(assets.path()))
                    .is_err()
            );
            assert_eq!(
                fs::read(&path).unwrap(),
                if cancel { before } else { replacement }
            );
            assert!(runner.calls.lock().unwrap().iter().all(|call| {
                call.args
                    .last()
                    .is_none_or(|arg| !arg.to_string_lossy().contains("CLIPBRIDGE_INSTALLED_"))
            }));
        }
    }

    impl NativeFixture {
        fn new(existing: bool) -> Self {
            let home = tempfile::Builder::new()
                .prefix("native helper ' ")
                .tempdir()
                .unwrap();
            let tools = home.path().join("tools");
            fs::create_dir(&tools).unwrap();
            let sum = if cfg!(target_os = "macos") {
                "#!/bin/sh\nexec /usr/bin/shasum -a 256 \"$@\"\n"
            } else {
                "#!/bin/sh\nexec /usr/bin/sha256sum \"$@\"\n"
            };
            fs::write(tools.join("sha256sum"), sum).unwrap();
            fs::set_permissions(tools.join("sha256sum"), fs::Permissions::from_mode(0o700))
                .unwrap();
            let binary = format!(
                r#"#!/bin/sh
set -eu
case "$1" in
  --version) printf '%s\n' 'clipbridge-remote {}' ;;
  doctor|status) [ ! -f "$HOME/fail-$1" ] || exit 7 ;;
  shell)
    if [ -f "$HOME/replace-during-shell" ]; then
      printf '%s\n' 'user replacement' > "$HOME/foreign-binary"
      mv "$HOME/foreign-binary" "$0"
      exit 7
    fi
    [ ! -f "$HOME/fail-shell" ] || exit 7
    printf '%s\n' installed > "$HOME/shell-installed"
    ;;
  *) exit 19 ;;
esac
"#,
                release::companion_version()
            )
            .into_bytes();
            let mut fixture = Self {
                home,
                binary,
                old: None,
            };
            let prepared = fixture.run(&native_prepare_script("test123"));
            assert!(
                prepared.status.success(),
                "{}",
                String::from_utf8_lossy(&prepared.stderr)
            );
            fs::write(fixture.candidate(), &fixture.binary).unwrap();
            fs::set_permissions(fixture.candidate(), fs::Permissions::from_mode(0o600)).unwrap();
            fs::write(fixture.home.path().join("running-backend"), b"unchanged").unwrap();
            if existing {
                let old = b"#!/bin/sh\n# original helper\nexit 0\n".to_vec();
                fs::write(fixture.installed(), &old).unwrap();
                fs::set_permissions(fixture.installed(), fs::Permissions::from_mode(0o700))
                    .unwrap();
                fs::create_dir(fixture.home.path().join(".local/bin")).unwrap();
                symlink(fixture.installed(), fixture.launcher()).unwrap();
                fixture.old = Some(old);
            }
            fixture
        }
        fn root(&self) -> PathBuf {
            self.home.path().join(".local/share/clipbridge-remote")
        }
        fn installed(&self) -> PathBuf {
            self.root().join("bin/clipbridge-remote")
        }
        fn launcher(&self) -> PathBuf {
            self.home.path().join(".local/bin/clipbridge-remote")
        }
        fn candidate(&self) -> PathBuf {
            self.root().join("staging/test123/clipbridge-remote")
        }
        fn run(&self, command: &str) -> Output {
            Command::new("/bin/sh")
                .args(["-c", command])
                .env("HOME", self.home.path())
                .env(
                    "PATH",
                    format!("{}:/usr/bin:/bin", self.home.path().join("tools").display()),
                )
                .output()
                .unwrap()
        }
        fn install(&self) -> Output {
            self.run(&native_install_script(
                "test123",
                self.binary.len() as u64,
                &format!("{:x}", Sha256::digest(&self.binary)),
                true,
            ))
        }
        fn unchanged(&self) {
            if let Some(old) = &self.old {
                assert_eq!(fs::read(self.installed()).unwrap(), *old);
                assert_eq!(fs::read_link(self.launcher()).unwrap(), self.installed());
            } else {
                assert!(!self.installed().exists());
                assert!(!self.launcher().is_symlink());
            }
            assert_eq!(
                fs::read(self.home.path().join("running-backend")).unwrap(),
                b"unchanged"
            );
        }
    }

    #[test]
    fn native_transaction_installs_and_upgrades_without_touching_backend_state() {
        for existing in [false, true] {
            let fixture = NativeFixture::new(existing);
            let result = fixture.install();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert!(
                String::from_utf8_lossy(&result.stdout).contains("CLIPBRIDGE_INSTALLED_test123")
            );
            assert_eq!(fs::read(fixture.installed()).unwrap(), fixture.binary);
            assert_eq!(
                fs::read_link(fixture.launcher()).unwrap(),
                fixture.installed()
            );
            assert!(fixture.home.path().join("shell-installed").is_file());
            assert!(!fixture.root().join("staging/test123").exists());
            assert!(!fixture.root().join(".install-lock").exists());
            assert_eq!(
                fs::read(fixture.home.path().join("running-backend")).unwrap(),
                b"unchanged"
            );
        }
    }

    #[test]
    fn staged_validation_and_shell_failure_preserve_or_restore_previous_installation() {
        for existing in [false, true] {
            for failure in ["checksum", "doctor", "status", "shell"] {
                let fixture = NativeFixture::new(existing);
                if failure == "checksum" {
                    fs::write(fixture.candidate(), b"corrupt").unwrap();
                } else {
                    fs::write(fixture.home.path().join(format!("fail-{failure}")), b"fail")
                        .unwrap();
                }
                let result = fixture.install();
                assert!(!result.status.success(), "{failure}");
                fixture.unchanged();
                assert!(!fixture.home.path().join("shell-installed").exists());
                assert!(!fixture.root().join(".install-lock").exists());
            }
        }
    }

    #[test]
    fn native_deployment_preserves_unfamiliar_files_and_contending_lock() {
        for failure in ["symlink", "hardlink", "shared", "lock", "launcher"] {
            let fixture = NativeFixture::new(true);
            match failure {
                "symlink" => {
                    fs::remove_file(fixture.candidate()).unwrap();
                    symlink(fixture.installed(), fixture.candidate()).unwrap();
                }
                "hardlink" => {
                    fs::remove_file(fixture.candidate()).unwrap();
                    fs::hard_link(fixture.installed(), fixture.candidate()).unwrap();
                }
                "shared" => {
                    fs::set_permissions(fixture.root(), fs::Permissions::from_mode(0o777)).unwrap()
                }
                "lock" => {
                    fs::create_dir(fixture.root().join(".install-lock")).unwrap();
                    fs::write(fixture.root().join(".install-lock/foreign"), b"keep").unwrap();
                }
                "launcher" => {
                    fs::remove_file(fixture.launcher()).unwrap();
                    fs::write(fixture.launcher(), b"personal command").unwrap();
                }
                _ => unreachable!(),
            }
            let result = fixture.install();
            assert!(!result.status.success(), "{failure}");
            assert_eq!(
                fs::read(fixture.installed()).unwrap(),
                *fixture.old.as_ref().unwrap()
            );
            if failure == "lock" {
                assert_eq!(
                    fs::read(fixture.root().join(".install-lock/foreign")).unwrap(),
                    b"keep"
                );
            }
            if failure == "launcher" {
                assert_eq!(fs::read(fixture.launcher()).unwrap(), b"personal command");
            }
        }
    }

    #[test]
    fn failed_recovery_keeps_concurrent_binary_and_previous_backup() {
        let fixture = NativeFixture::new(true);
        fs::write(fixture.home.path().join("replace-during-shell"), b"replace").unwrap();
        let result = fixture.install();
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("recovery could not safely finish")
        );
        assert_eq!(
            fs::read(fixture.installed()).unwrap(),
            b"user replacement\n"
        );
        assert_eq!(
            fs::read(fixture.root().join("staging/test123/previous")).unwrap(),
            *fixture.old.as_ref().unwrap()
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
            let result = setup_with_options(
                &ctx,
                &path,
                integrate_shell,
                SetupMode::BuildFromSource,
                None,
            );
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
        assert!(message.contains("start codex, claude or opencode normally"));
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
    fn setup_uses_one_active_alternative_for_all_remote_steps_and_preserves_aliases() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.json");
        fs::write(&path, br#"{"ssh_host":"primary","ssh_aliases":["lan"],"remote_directory":"/tmp/images/","custom":"keep"}"#).unwrap();
        let runner = Arc::new(Fake {
            active_alias: Some("lan".into()),
            ..Fake::default()
        });
        let ctx = context(home.path(), runner.clone());
        setup(&ctx, &path).unwrap();
        let saved = Config::load(&path).unwrap();
        assert_eq!(saved.ssh_host, "primary");
        assert_eq!(saved.ssh_aliases, ["lan"]);
        assert!(saved.remote_clipboard);
        let value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["custom"], "keep");
        for call in runner.calls.lock().unwrap().iter() {
            if call.program == Path::new("/usr/bin/ssh") {
                assert_eq!(call.args[call.args.len() - 2], "lan");
            } else if call.program == Path::new("/usr/bin/scp") {
                assert!(
                    call.args
                        .last()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("lan:")
                );
            }
        }
    }

    #[test]
    fn changing_aliases_or_directory_during_setup_refuses_shell_edits_and_enablement() {
        for replacement in [
            br#"{"ssh_host":"primary","ssh_aliases":["different"],"remote_directory":"/tmp/images"}"#.as_slice(),
            br#"{"ssh_host":"primary","ssh_aliases":["lan"],"remote_directory":"/tmp/new"}"#.as_slice(),
        ] {
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join("config.json");
            fs::write(&path, br#"{"ssh_host":"primary","ssh_aliases":["lan"],"remote_directory":"/tmp/images"}"#).unwrap();
            let runner = Arc::new(Fake {
                active_alias: Some("lan".into()),
                config_update: Some((path.clone(), replacement.to_vec())),
                ..Fake::default()
            });
            let ctx = context(home.path(), runner.clone());
            assert!(format!("{:#}", setup(&ctx, &path).unwrap_err()).contains("SSH destination changed"));
            assert_eq!(fs::read(&path).unwrap(), replacement);
            assert!(runner.calls.lock().unwrap().iter().all(|call| call.args.last().is_none_or(|arg| !arg.to_string_lossy().ends_with(" shell install"))));
        }
    }

    #[test]
    fn remote_management_selects_active_alias_or_primary_without_network_failover() {
        type Operation = fn(&Context, &Path) -> Result<String>;
        for active in [None, Some("lan")] {
            for operation in [doctor as Operation, status, stop] {
                let home = tempfile::tempdir().unwrap();
                let path = home.path().join("config.json");
                let mut config = Config::new("primary", "/tmp/images").unwrap();
                config.ssh_aliases = vec!["lan".into()];
                config.save(&path).unwrap();
                let runner = Arc::new(Fake {
                    active_alias: active.map(str::to_owned),
                    ..Fake::default()
                });
                let ctx = context(home.path(), runner.clone());
                operation(&ctx, &path).unwrap();
                let calls = runner.calls.lock().unwrap();
                let ssh: Vec<_> = calls
                    .iter()
                    .filter(|call| call.program == Path::new("/usr/bin/ssh"))
                    .collect();
                assert_eq!(ssh.len(), 1);
                assert_eq!(
                    ssh[0].args[ssh[0].args.len() - 2],
                    active.unwrap_or("primary")
                );
            }
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
