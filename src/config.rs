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
    pub remote_directory: String,
}
impl Config {
    pub fn new(host: impl Into<String>, remote: impl Into<String>) -> Result<Self> {
        let mut config = Self {
            ssh_host: host.into(),
            remote_directory: remote.into(),
        };
        config.validate()?;
        config.remote_directory = config.remote_directory.trim_end_matches('/').to_owned();
        if config.remote_directory.is_empty() {
            config.remote_directory.push('/');
        }
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        validate_host(&self.ssh_host)?;
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
        Self::new(config.ssh_host, config.remote_directory)
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
pub fn destination_command(remote: &str, create: bool) -> String {
    let quoted = shell_quote(remote);
    let check = format!("test -d {quoted} && test -w {quoted} && test -x {quoted}");
    if !create {
        return check;
    }
    let probe = shell_quote(&format!(
        "{}/.clipbridge-check.XXXXXXXX",
        remote.trim_end_matches('/')
    ));
    format!(
        "umask 077; mkdir -p -- {quoted} && {check} && probe=$(mktemp {probe}) && trap 'rm -f -- \"$probe\"' 0 HUP INT TERM"
    )
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
            remote_directory: "/tmp".into(),
        };
        assert!(bad.save(&file).is_err());
        assert_eq!(Config::load(&file).unwrap(), config);
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
}
