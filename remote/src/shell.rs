use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand, ValueEnum};
use nix::unistd::geteuid;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

const MARKER: &[u8] = b"clipbridge-remote-shell-v1\n";
const BEGIN: &str = "# >>> ClipBridge remote shell >>>";
const END: &str = "# <<< ClipBridge remote shell <<<";
const SNIPPET: &str = r#"# Managed by clipbridge-remote shell install. Do not edit.
case $- in
    *i*)
        if [ "${_CLIPBRIDGE_CODEX_WRAPPED-}" != 1 ]; then
            if alias codex >/dev/null 2>&1 || typeset -f codex >/dev/null 2>&1; then
                printf '%s\n' 'ClipBridge: keeping your existing codex alias/function; use clipbridge-remote run -- codex explicitly.' >&2
            else
                function codex { "$HOME/.local/bin/clipbridge-remote" run -- codex "$@"; }
                _CLIPBRIDGE_CODEX_WRAPPED=1
            fi
        fi
        if [ "${_CLIPBRIDGE_CLAUDE_WRAPPED-}" != 1 ]; then
            if alias claude >/dev/null 2>&1 || typeset -f claude >/dev/null 2>&1; then
                printf '%s\n' 'ClipBridge: keeping your existing claude alias/function; use clipbridge-remote run -- claude explicitly.' >&2
            else
                function claude { "$HOME/.local/bin/clipbridge-remote" run -- claude "$@"; }
                _CLIPBRIDGE_CLAUDE_WRAPPED=1
            fi
        fi
        ;;
esac
"#;

#[derive(Subcommand)]
pub enum Action {
    /// Enable direct codex/claude commands in new interactive shells.
    Install(Options),
    /// Remove managed startup hooks while preserving other shell configuration.
    Uninstall(Options),
    /// Print the integration script for Bash/Zsh; does not start a clipboard server.
    Init,
}

#[derive(Args)]
pub struct Options {
    /// Shell to configure; defaults to the basename of SHELL.
    #[arg(long, value_enum)]
    shell: Option<Shell>,
    /// Configure only this absolute startup-file path.
    #[arg(long)]
    rc_file: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
enum Shell {
    Bash,
    Zsh,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    records: Vec<Record>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    path: PathBuf,
    added_newline: bool,
    backup: Option<String>,
}

#[derive(Clone)]
struct Snapshot {
    bytes: Vec<u8>,
    mode: u32,
}

struct Update {
    path: PathBuf,
    before: Option<Snapshot>,
    after: Option<Snapshot>,
}

fn home() -> Result<PathBuf> {
    let path = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
    check_absolute(&path)?;
    check_directory(&path)?;
    Ok(path)
}

fn check_absolute(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute()
            && path
                .components()
                .all(|component| !matches!(component, Component::ParentDir)),
        "path must be absolute without '..': {}",
        path.display()
    );
    ensure!(path.to_str().is_some(), "shell paths must be UTF-8");
    Ok(())
}

fn check_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect directory {}", path.display()))?;
    ensure!(
        metadata.is_dir() && metadata.uid() == geteuid().as_raw(),
        "directory must be owned by the current user and not a symlink: {}",
        path.display()
    );
    Ok(())
}

// Check each existing component below HOME. Ancestors such as macOS /tmp may be
// system-managed symlinks; HOME itself and all user-managed descendants may not be.
fn check_parents(path: &Path, home: &Path) -> Result<()> {
    check_absolute(path)?;
    let parent = path.parent().context("startup file has no parent")?;
    if let Ok(relative) = parent.strip_prefix(home) {
        let mut current = home.to_path_buf();
        check_directory(&current)?;
        for component in relative.components() {
            current.push(component);
            check_directory(&current)?;
        }
    } else {
        check_directory(parent)?;
        for ancestor in parent.ancestors().take_while(|p| p.parent().is_some()) {
            ensure!(
                !fs::symlink_metadata(ancestor)?.file_type().is_symlink(),
                "refusing symlink ancestor {}",
                ancestor.display()
            );
        }
    }
    Ok(())
}

fn read(path: &Path) -> Result<Option<Snapshot>> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("open {}", path.display())),
    };
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.uid() == geteuid().as_raw() && metadata.nlink() == 1,
        "refusing an unowned, non-regular or hard-linked file: {}",
        path.display()
    );
    ensure!(metadata.len() <= 4 * 1024 * 1024, "shell file is too large");
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(Some(Snapshot {
        bytes,
        mode: metadata.mode() & 0o7777,
    }))
}

fn unchanged(path: &Path, expected: &Option<Snapshot>) -> Result<()> {
    let actual = read(path)?;
    ensure!(
        actual.as_ref().map(|s| (&s.bytes, s.mode))
            == expected.as_ref().map(|s| (&s.bytes, s.mode)),
        "file changed during shell setup; retry: {}",
        path.display()
    );
    Ok(())
}

fn replace(path: &Path, content: &Option<Snapshot>) -> Result<()> {
    match content {
        Some(snapshot) => {
            let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
            temp.as_file()
                .set_permissions(fs::Permissions::from_mode(snapshot.mode))?;
            temp.write_all(&snapshot.bytes)?;
            temp.as_file().sync_all()?;
            temp.persist(path)
                .with_context(|| format!("replace {}", path.display()))?;
        }
        None => fs::remove_file(path).with_context(|| format!("remove {}", path.display()))?,
    }
    Ok(())
}

fn apply(updates: &[Update]) -> Result<()> {
    for update in updates {
        unchanged(&update.path, &update.before)?;
    }
    for (index, update) in updates.iter().enumerate() {
        let result = unchanged(&update.path, &update.before)
            .and_then(|()| replace(&update.path, &update.after));
        if let Err(error) = result {
            for prior in updates[..index].iter().rev() {
                if unchanged(&prior.path, &prior.after).is_ok() {
                    if let Err(rollback) = replace(&prior.path, &prior.before) {
                        eprintln!(
                            "Shell setup rollback failed for {}: {rollback:#}",
                            prior.path.display()
                        );
                    }
                } else {
                    eprintln!(
                        "Keeping concurrent edits in {} during rollback",
                        prior.path.display()
                    );
                }
            }
            return Err(error);
        }
    }
    Ok(())
}

fn shell(options: &Options) -> Result<Shell> {
    if let Some(shell) = options.shell {
        return Ok(shell);
    }
    match std::env::var_os("SHELL")
        .as_deref()
        .and_then(|s| Path::new(s).file_name())
        .and_then(|s| s.to_str())
    {
        Some("bash") => Ok(Shell::Bash),
        Some("zsh") => Ok(Shell::Zsh),
        _ => bail!("cannot infer Bash/Zsh from SHELL; pass --shell bash or --shell zsh"),
    }
}

fn targets(options: &Options, home: &Path) -> Result<Vec<PathBuf>> {
    if let Some(path) = &options.rc_file {
        check_parents(path, home)?;
        return Ok(vec![path.clone()]);
    }
    let paths = match shell(options)? {
        Shell::Zsh => {
            let directory = std::env::var_os("ZDOTDIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.to_path_buf());
            check_absolute(&directory)?;
            vec![directory.join(".zshrc")]
        }
        Shell::Bash => {
            let login = [".bash_profile", ".bash_login", ".profile"]
                .into_iter()
                .find(|name| fs::symlink_metadata(home.join(name)).is_ok())
                .unwrap_or(".profile");
            vec![home.join(".bashrc"), home.join(login)]
        }
    };
    for path in &paths {
        check_parents(path, home)?;
    }
    Ok(paths)
}

fn quote(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}

fn block(directory: &Path, added_newline: bool) -> Vec<u8> {
    let snippet = quote(&directory.join("init.sh"));
    format!(
        "{}{BEGIN}\ncase $- in *i*) if [ -n \"${{BASH_VERSION-}}${{ZSH_VERSION-}}\" ] && [ -r {snippet} ]; then . {snippet}; fi ;; esac\n{END}\n",
        if added_newline { "\n" } else { "" }
    )
    .into_bytes()
}

fn find(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
    haystack
        .windows(needle.len())
        .enumerate()
        .filter_map(|(i, bytes)| (bytes == needle).then_some(i))
        .collect()
}

fn locate(snapshot: &Snapshot, record: &Record, directory: &Path) -> Result<usize> {
    let expected = block(directory, record.added_newline);
    let positions = find(&snapshot.bytes, &expected);
    ensure!(
        positions.len() == 1
            && find(&snapshot.bytes, BEGIN.as_bytes()).len() == 1
            && find(&snapshot.bytes, END.as_bytes()).len() == 1,
        "managed shell block was modified or removed; restore it before continuing: {}",
        record.path.display()
    );
    Ok(positions[0])
}

fn prepare(home: &Path, install: bool) -> Result<Option<PathBuf>> {
    let directory = home.join(".local/share/clipbridge-remote/shell");
    let mut current = home.to_path_buf();
    for part in [".local", "share", "clipbridge-remote", "shell"] {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(_) => check_directory(&current)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !install {
                    return Ok(None);
                }
                fs::create_dir(&current)?;
                fs::set_permissions(&current, fs::Permissions::from_mode(0o700))?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    let marker = directory.join(".owned");
    match read(&marker)? {
        Some(snapshot) => ensure!(
            snapshot.bytes == MARKER,
            "unrecognized shell integration ownership marker"
        ),
        None => {
            ensure!(
                install && fs::read_dir(&directory)?.next().is_none(),
                "refusing an unfamiliar shell integration directory"
            );
            replace(
                &marker,
                &Some(Snapshot {
                    bytes: MARKER.to_vec(),
                    mode: 0o600,
                }),
            )?;
        }
    }
    Ok(Some(directory))
}

fn metadata(
    directory: &Path,
    home: &Path,
) -> Result<(Manifest, Option<Snapshot>, Option<Snapshot>)> {
    let manifest = read(&directory.join("manifest.json"))?;
    let snippet = read(&directory.join("init.sh"))?;
    let records: Manifest = match &manifest {
        Some(snapshot) => {
            serde_json::from_slice(&snapshot.bytes).context("read shell integration manifest")?
        }
        None => Manifest::default(),
    };
    if let Some(snapshot) = &snippet {
        ensure!(
            snapshot.bytes == SNIPPET.as_bytes(),
            "managed shell snippet was modified; restore init.sh before continuing"
        );
        ensure!(manifest.is_some(), "refusing untracked shell snippet");
    }
    ensure!(
        records.records.is_empty() || snippet.is_some(),
        "managed shell snippet is missing"
    );
    for (index, record) in records.records.iter().enumerate() {
        check_parents(&record.path, home)?;
        ensure!(
            !records.records[..index]
                .iter()
                .any(|r| r.path == record.path),
            "duplicate shell manifest record"
        );
        if let Some(backup) = &record.backup {
            ensure!(
                backup.starts_with("backup-") && Path::new(backup).components().count() == 1,
                "invalid backup filename"
            );
        }
    }
    Ok((records, manifest, snippet))
}

fn install(options: Options) -> Result<()> {
    let home = home()?;
    let paths = targets(&options, &home)?;
    let directory = prepare(&home, true)?.unwrap();
    let _lock = lock(&directory)?;
    let (mut manifest, manifest_before, snippet_before) = metadata(&directory, &home)?;
    let mut updates = Vec::new();
    for path in paths {
        let before = read(&path)?;
        if let Some(record) = manifest.records.iter().find(|r| r.path == path) {
            locate(
                before.as_ref().context("managed startup file is missing")?,
                record,
                &directory,
            )?;
            continue;
        }
        let mut after = before.clone().unwrap_or(Snapshot {
            bytes: Vec::new(),
            mode: 0o600,
        });
        ensure!(
            find(&after.bytes, BEGIN.as_bytes()).is_empty()
                && find(&after.bytes, END.as_bytes()).is_empty(),
            "refusing an untracked ClipBridge block in {}",
            path.display()
        );
        let backup = if let Some(snapshot) = &before {
            let mut file = tempfile::Builder::new()
                .prefix("backup-")
                .tempfile_in(&directory)?;
            file.write_all(&snapshot.bytes)?;
            file.as_file().sync_all()?;
            let (_, path) = file.keep()?;
            println!("Shell configuration backup: {}", path.display());
            Some(path.file_name().unwrap().to_str().unwrap().to_string())
        } else {
            None
        };
        let added_newline = !after.bytes.is_empty() && !after.bytes.ends_with(b"\n");
        after
            .bytes
            .extend_from_slice(&block(&directory, added_newline));
        manifest.records.push(Record {
            path: path.clone(),
            added_newline,
            backup,
        });
        updates.push(Update {
            path,
            before,
            after: Some(after),
        });
    }
    updates.insert(
        0,
        Update {
            path: directory.join("init.sh"),
            before: snippet_before,
            after: Some(Snapshot {
                bytes: SNIPPET.as_bytes().to_vec(),
                mode: 0o600,
            }),
        },
    );
    updates.push(Update {
        path: directory.join("manifest.json"),
        before: manifest_before,
        after: Some(Snapshot {
            bytes: serde_json::to_vec_pretty(&manifest)?,
            mode: 0o600,
        }),
    });
    apply(&updates)?;
    for update in &updates {
        if manifest
            .records
            .iter()
            .any(|record| record.path == update.path)
        {
            println!("Shell startup file updated: {}", update.path.display());
        }
    }
    println!(
        "Shell integration installed. Open a new SSH shell, then run codex or claude normally."
    );
    println!("For this shell: . {}", quote(&directory.join("init.sh")));
    Ok(())
}

fn lock(directory: &Path) -> Result<std::fs::File> {
    let path = directory.join(".lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(&path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.uid() == geteuid().as_raw() && metadata.nlink() == 1,
        "refusing unfamiliar shell lock"
    );
    file.try_lock()
        .context("another shell setup is running; retry")?;
    Ok(file)
}

fn uninstall(options: Options) -> Result<()> {
    let home = home()?;
    let Some(directory) = prepare(&home, false)? else {
        println!("Shell integration is already uninstalled.");
        return Ok(());
    };
    let _lock = lock(&directory)?;
    let (mut manifest, manifest_before, snippet_before) = metadata(&directory, &home)?;
    let selected = if options.shell.is_some() || options.rc_file.is_some() {
        Some(targets(&options, &home)?)
    } else {
        None
    };
    let mut updates = Vec::new();
    let mut retained = Vec::new();
    for record in manifest.records {
        if selected
            .as_ref()
            .is_some_and(|paths| !paths.contains(&record.path))
        {
            retained.push(record);
            continue;
        }
        let before = read(&record.path)?;
        let mut after = before.clone().context("managed startup file is missing")?;
        let start = locate(&after, &record, &directory)?;
        after
            .bytes
            .drain(start..start + block(&directory, record.added_newline).len());
        let after = if record.backup.is_none() && after.bytes.is_empty() {
            None
        } else {
            Some(after)
        };
        updates.push(Update {
            path: record.path,
            before,
            after,
        });
    }
    manifest.records = retained;
    let empty = manifest.records.is_empty();
    if empty {
        if snippet_before.is_some() {
            updates.push(Update {
                path: directory.join("init.sh"),
                before: snippet_before,
                after: None,
            });
        }
        if manifest_before.is_some() {
            updates.push(Update {
                path: directory.join("manifest.json"),
                before: manifest_before,
                after: None,
            });
        }
    } else {
        updates.push(Update {
            path: directory.join("manifest.json"),
            before: manifest_before,
            after: Some(Snapshot {
                bytes: serde_json::to_vec_pretty(&manifest)?,
                mode: 0o600,
            }),
        });
    }
    apply(&updates)?;
    println!(
        "Shell integration removed from selected startup files. Reopen the shell to clear existing functions."
    );
    println!("Original backups are preserved in {}", directory.display());
    Ok(())
}

pub fn execute(action: Action) -> Result<()> {
    match action {
        Action::Init => {
            print!("{SNIPPET}");
            Ok(())
        }
        Action::Install(options) => install(options),
        Action::Uninstall(options) => uninstall(options),
    }
}
