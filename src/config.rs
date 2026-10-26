//! Backward-compatible JSON settings and explicit SSH destination checks.
use crate::common::{CommandSpec, Context, atomic_write, expand_path, shell_quote};
use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub const SSH_OPTIONS: &[&str] = &[
    "-o",
    "BatchMode=yes",
    "-o",
    "StrictHostKeyChecking=yes",
    "-o",
    "ConnectTimeout=8",
    "-o",
    "ServerAliveInterval=10",
    "-o",
    "ServerAliveCountMax=2",
];

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Config {
    pub ssh_host: String,
    /// Alternative SSH aliases for the same remote machine and account.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ssh_aliases: Vec<String>,
    pub remote_directory: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub remote_clipboard: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}
impl Config {
    pub fn new(host: impl Into<String>, remote: impl Into<String>) -> Result<Self> {
        let mut config = Self {
            ssh_host: host.into(),
            ssh_aliases: Vec::new(),
            remote_directory: remote.into(),
            remote_clipboard: false,
        };
        config.validate()?;
        config.remote_directory = config.remote_directory.trim_end_matches('/').to_owned();
        if config.remote_directory.is_empty() {
            config.remote_directory.push('/');
        }
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        let mut seen = HashSet::new();
        for host in self.ssh_hosts() {
            validate_host(host)?;
            if !seen.insert(host) {
                bail!("duplicate SSH alias: {host}");
            }
        }
        let remote = &self.remote_directory;
        if !remote.starts_with('/')
            || !remote
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_./-".contains(&c))
            || remote.split('/').any(|part| part == "..")
        {
            bail!(
                "remote_directory must be an absolute POSIX path without spaces, '..' or shell expressions"
            );
        }
        Ok(())
    }
    pub fn load(path: &Path) -> Result<Self> {
        let config: Self = serde_json::from_slice(
            &fs::read(path)
                .with_context(|| format!("could not read configuration {}", path.display()))?,
        )
        .with_context(|| format!("invalid JSON configuration {}", path.display()))?;
        let mut normalized = Self::new(config.ssh_host, config.remote_directory)?;
        normalized.ssh_aliases = config.ssh_aliases;
        normalized.remote_clipboard = config.remote_clipboard;
        normalized.validate()?;
        Ok(normalized)
    }
    pub fn ssh_hosts(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.ssh_host.as_str()).chain(self.ssh_aliases.iter().map(String::as_str))
    }
    pub fn allows_host(&self, host: &str) -> bool {
        self.ssh_hosts().any(|allowed| allowed == host)
    }
    /// Create a single-route configuration; later operations must not reselect it.
    pub fn for_host(&self, host: &str) -> Result<Self> {
        self.validate()?;
        if !self.allows_host(host) {
            bail!("SSH alias is not configured: {host}");
        }
        let mut selected = Self::new(host, &self.remote_directory)?;
        selected.remote_clipboard = self.remote_clipboard;
        Ok(selected)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        if path.is_symlink() || path.parent().is_some_and(Path::is_symlink) {
            bail!("configuration file/directory must not be a symlink");
        }
        let mut data = serde_json::to_vec_pretty(self)?;
        data.push(b'\n');
        atomic_write(path, &data, 0o600)
    }
}

pub fn validate_host(host: &str) -> Result<()> {
    if !host
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        || !host
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
    {
        bail!(
            "ssh_host must be a literal SSH alias (letters, digits, _, . or -), beginning with a letter or digit"
        );
    }
    Ok(())
}
pub fn selected_path(ctx: &Context, explicit: Option<&Path>) -> PathBuf {
    let default = ctx.default_config();
    let selected = explicit
        .map(|p| expand_path(p, &ctx.home))
        .unwrap_or_else(|| default.clone());
    if selected == default
        && !selected.exists()
        && let Some(source) = &ctx.source
    {
        let legacy = source.join("src/config.json");
        if legacy.is_file() {
            return legacy;
        }
    }
    selected
}

pub fn resolve_remote_directory(ctx: &Context, host: &str) -> Result<String> {
    validate_host(host)?;
    let mut args: Vec<String> = SSH_OPTIONS.iter().map(|s| (*s).to_owned()).collect();
    args.extend([host.to_owned(), "printf '%s\\n' \"$HOME\"".to_owned()]);
    let output = ctx.exec(CommandSpec::new("/usr/bin/ssh").args(args).timeout(35))?;
    let home = output.stdout.strip_suffix('\n').unwrap_or(&output.stdout);
    Config::new(host, home)?;
    Ok(format!(
        "{}/.local/share/clipbridge/images",
        home.trim_end_matches('/')
    ))
}
/// Pin subsequent relative operations to a directory no other user can modify.
/// Works with GNU and BSD find; existing directory permissions are never changed.
pub(crate) fn directory_guard(remote: &str, create: bool) -> String {
    let quoted = shell_quote(remote);
    let mkdir = if create {
        format!("umask 077\nmkdir -p -- {quoted}\n")
    } else {
        String::new()
    };
    format!(
        "{mkdir}cd -P -- {quoted}\n\
         [ -w . ] && [ -x . ] || {{ printf '%s\\n' 'Remote directory must be writable and searchable by the SSH user' >&2; exit 1; }}\n\
         [ \"$(find . -prune -type d -user \"$(id -u)\" ! -perm -020 ! -perm -002 -print)\" = . ] || {{ printf '%s\\n' 'Remote directory must be owned by the SSH user and not writable by group or others' >&2; exit 1; }}\n"
    )
}
pub fn destination_command(remote: &str, create: bool) -> String {
    let mut script = format!("set -eu\n{}", directory_guard(remote, create));
    if create {
        script.push_str("probe=$(mktemp .clipbridge-check.XXXXXXXX)\ntrap 'rm -f -- \"$probe\"' EXIT\ntrap 'exit 1' HUP INT TERM\n");
    }
    format!("sh -c {}", shell_quote(&script))
}
pub fn check_destination(
    ctx: &Context,
    host: &str,
    remote: Option<&str>,
    create: bool,
) -> Result<Config> {
    validate_host(host)?;
    let directory = match remote {
        Some(remote) => remote.to_owned(),
        None => resolve_remote_directory(ctx, host)?,
    };
    let config = Config::new(host, directory)?;
    let mut args: Vec<String> = SSH_OPTIONS.iter().map(|s| (*s).to_owned()).collect();
    args.extend([
        host.to_owned(),
        destination_command(&config.remote_directory, create),
    ]);
    ctx.exec(CommandSpec::new("/usr/bin/ssh").args(args).timeout(35))?;
    Ok(config)
}
fn strip_comment(line: &str) -> String {
    let mut out = String::new();
    let mut quote = None;
    let mut escaped = false;
    for c in line.chars() {
        if escaped {
            out.push(c);
            escaped = false;
            continue;
        }
        if c == '\\' && quote != Some('\'') {
            out.push(c);
            escaped = true;
            continue;
        }
        if c == '#' && quote.is_none() {
            break;
        }
        if matches!(c, '\'' | '"') {
            if quote == Some(c) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(c);
            }
        }
        out.push(c);
    }
    out
}
pub fn discover_ssh_aliases(path: &Path, home: &Path) -> Vec<String> {
    fn visit(
        path: &Path,
        base: &Path,
        home: &Path,
        visited: &mut HashSet<PathBuf>,
        aliases: &mut BTreeSet<String>,
    ) {
        let Ok(real) = path.canonicalize() else {
            return;
        };
        if !visited.insert(real) {
            return;
        }
        let Ok(text) = fs::read_to_string(path) else {
            return;
        };
        for raw in text.lines() {
            let line = strip_comment(raw);
            let line = line.trim();
            let Some(split) = line.find(|c: char| c.is_whitespace() || c == '=') else {
                continue;
            };
            let key = line[..split].to_ascii_lowercase();
            let value = line[split..].trim().trim_start_matches('=').trim();
            let Ok(args) = shell_words::split(value) else {
                continue;
            };
            match key.as_str() {
                "host" => {
                    for alias in args {
                        if validate_host(&alias).is_ok() {
                            aliases.insert(alias);
                        }
                    }
                }
                "include" => {
                    for pattern in args {
                        let pattern = expand_path(Path::new(&pattern), home);
                        let pattern = if pattern.is_absolute() {
                            pattern
                        } else {
                            base.join(pattern)
                        };
                        if let Ok(paths) = glob::glob(&pattern.to_string_lossy()) {
                            for included in paths.flatten() {
                                visit(&included, base, home, visited, aliases);
                            }
                        }
                    }
                }
                _ => {} // Suggestions only: never evaluate Match exec or any SSH command.
            }
        }
    }
    let mut aliases = BTreeSet::new();
    visit(
        path,
        path.parent().unwrap_or(Path::new(".")),
        home,
        &mut HashSet::new(),
        &mut aliases,
    );
    aliases.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{CommandOutput, Runner};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Mutex, atomic::AtomicBool};
    struct Fake {
        calls: Mutex<Vec<CommandSpec>>,
    }
    impl Runner for Fake {
        fn run(&self, spec: &CommandSpec, _: &AtomicBool) -> Result<CommandOutput> {
            self.calls.lock().unwrap().push(spec.clone());
            Ok(CommandOutput {
                code: 0,
                stdout: "/home/test\n".into(),
                stderr: String::new(),
            })
        }
    }
    fn context(home: &Path, fake: Arc<Fake>) -> Context {
        Context {
            home: home.into(),
            executable: home.join("clipbridge"),
            source: None,
            uid: 501,
            cancelled: Arc::new(AtomicBool::new(false)),
            runner: fake,
        }
    }
    #[test]
    fn rejects_shell_flags_and_invalid_destinations() {
        for host in ["", "-bad", "user@host", "a;id", "x\ny", "host:22", "*.com"] {
            assert!(Config::new(host, "/tmp/images").is_err(), "{host}");
        }
        for remote in [
            "~/images",
            "relative",
            "/a b",
            "/a/../b",
            "/tmp/$(id)",
            "/tmp/图片",
        ] {
            assert!(Config::new("server", remote).is_err(), "{remote}");
        }
        assert_eq!(
            Config::new("dev-server", "/tmp/images/")
                .unwrap()
                .remote_directory,
            "/tmp/images"
        );
    }
    #[test]
    fn private_round_trip_and_failed_save_preserve_config() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.json");
        let config = Config::new("host", "/tmp/images").unwrap();
        config.save(&file).unwrap();
        assert_eq!(Config::load(&file).unwrap(), config);
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let bad = Config {
            ssh_host: "-bad".into(),
            ssh_aliases: Vec::new(),
            remote_directory: "/tmp".into(),
            remote_clipboard: false,
        };
        assert!(bad.save(&file).is_err());
        assert_eq!(Config::load(&file).unwrap(), config);
    }
    #[test]
    fn remote_clipboard_is_opt_in_and_survives_normalized_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.json");
        fs::write(
            &file,
            r#"{"ssh_host":"host","remote_directory":"/tmp/images/"}"#,
        )
        .unwrap();
        let mut config = Config::load(&file).unwrap();
        assert!(!config.remote_clipboard);
        assert!(config.ssh_aliases.is_empty());
        config.save(&file).unwrap();
        assert!(
            !fs::read_to_string(&file)
                .unwrap()
                .contains("remote_clipboard")
        );
        config.remote_clipboard = true;
        config.save(&file).unwrap();
        assert_eq!(Config::load(&file).unwrap(), config);
        assert_eq!(config.remote_directory, "/tmp/images");
        fs::write(
            &file,
            r#"{"ssh_host":"host","remote_directory":"/tmp/images","remote_clipboard":"true"}"#,
        )
        .unwrap();
        assert!(Config::load(&file).is_err());
    }
    #[test]
    fn alternative_aliases_survive_normalization_and_pin_only_authorized_routes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        fs::write(&path, br#"{"ssh_host":"server","ssh_aliases":["server.local","server-lan"],"remote_directory":"/tmp/images/","remote_clipboard":true}"#).unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.remote_directory, "/tmp/images");
        assert_eq!(
            config.ssh_hosts().collect::<Vec<_>>(),
            ["server", "server.local", "server-lan"]
        );
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);
        let selected = config.for_host("server.local").unwrap();
        assert_eq!(selected.ssh_host, "server.local");
        assert!(selected.ssh_aliases.is_empty());
        assert_eq!(selected.remote_directory, config.remote_directory);
        assert!(selected.remote_clipboard);
        assert!(config.for_host("unconfigured").is_err());
    }
    #[test]
    fn invalid_or_duplicate_alternative_aliases_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        for aliases in [
            serde_json::json!(["server"]),
            serde_json::json!(["lan", "lan"]),
            serde_json::json!(["-option"]),
            serde_json::json!(["user@server"]),
            serde_json::json!(["server;touch sentinel"]),
            serde_json::json!(["2001:db8::1"]),
            serde_json::json!("server.local"),
            serde_json::json!(null),
        ] {
            let value = serde_json::json!({"ssh_host":"server","ssh_aliases":aliases,"remote_directory":"/tmp/images"});
            fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(Config::load(&path).is_err(), "accepted {value}");
        }
    }
    #[test]
    fn includes_are_hints_and_cycles_do_not_recurse_forever() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("parts")).unwrap();
        fs::write(
            dir.path().join("config"),
            "Host dev *.example !skip\nInclude parts/*\nMatch exec evil\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("parts/work"),
            "Host=work alias # comment\nInclude config\n",
        )
        .unwrap();
        assert_eq!(
            discover_ssh_aliases(&dir.path().join("config"), dir.path()),
            vec!["alias", "dev", "work"]
        );
    }
    #[test]
    fn checks_use_strict_ssh_and_doctor_does_not_create_directories() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(Fake {
            calls: Mutex::new(vec![]),
        });
        let ctx = context(dir.path(), fake.clone());
        let config = check_destination(&ctx, "server", None, true).unwrap();
        assert_eq!(
            config.remote_directory,
            "/home/test/.local/share/clipbridge/images"
        );
        check_destination(&ctx, "server", Some("/tmp/images"), false).unwrap();
        let calls = fake.calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert!(
            calls[0]
                .args
                .iter()
                .any(|a| a == "StrictHostKeyChecking=yes")
        );
        assert!(
            calls[1]
                .args
                .last()
                .unwrap()
                .to_string_lossy()
                .contains("mktemp")
        );
        assert!(
            !calls[2]
                .args
                .last()
                .unwrap()
                .to_string_lossy()
                .contains("mkdir")
        );
    }
    #[test]
    fn destination_checks_share_safe_permissions_and_leave_existing_files_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("images");
        let run = |create| {
            std::process::Command::new("/bin/sh")
                .args([
                    "-c",
                    &destination_command(destination.to_str().unwrap(), create),
                ])
                .output()
                .unwrap()
        };
        assert!(!run(false).status.success());
        assert!(!destination.exists(), "doctor must remain read-only");
        assert!(run(true).status.success());
        assert_eq!(
            fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let original = destination.join("existing");
        fs::write(&original, b"keep").unwrap();
        for mode in [0o700, 0o755, 0o775, 0o777] {
            fs::set_permissions(&destination, fs::Permissions::from_mode(mode)).unwrap();
            for create in [false, true] {
                let output = run(create);
                assert_eq!(
                    output.status.success(),
                    mode & 0o022 == 0,
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert_eq!(
                    fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
                    mode
                );
                assert_eq!(fs::read(&original).unwrap(), b"keep");
                assert_eq!(fs::read_dir(&destination).unwrap().count(), 1);
            }
        }
    }
}
