//! OS boundaries and private file primitives shared by the Rust application.
use anyhow::{Context as _, Result, bail};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const INSTALL_MARKER: &str = ".clipbridge-install.json";
pub const RELEASE_MARKER: &str = ".clipbridge-release.json";

#[derive(Clone, Debug)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub timeout: Duration,
    pub check: bool,
    pub cancellable: bool,
    pub cooperative_cancel: bool,
}
impl CommandSpec {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: PathBuf::from(program.as_ref()),
            args: vec![],
            timeout: Duration::from_secs(10),
            check: true,
            cancellable: true,
            cooperative_cancel: false,
        }
    }
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|s| s.as_ref().to_owned()));
        self
    }
    pub fn timeout(mut self, seconds: u64) -> Self {
        self.timeout = Duration::from_secs(seconds);
        self
    }
    pub fn cooperative(mut self) -> Self {
        self.cooperative_cancel = true;
        self
    }
    pub fn unchecked(mut self) -> Self {
        self.check = false;
        self
    }
    pub fn uncancellable(mut self) -> Self {
        self.cancellable = false;
        self
    }
}
#[derive(Clone, Debug, Default)]
pub struct CommandOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}
pub trait Runner: Send + Sync {
    fn run(&self, spec: &CommandSpec, cancelled: &AtomicBool) -> Result<CommandOutput>;
}
pub struct SystemRunner;

fn terminate_and_reap(child: &mut std::process::Child, group: nix::unistd::Pid) -> Result<()> {
    let group_error = match nix::sys::signal::killpg(group, nix::sys::signal::Signal::SIGKILL) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => None,
        Err(error) => {
            child.kill().with_context(|| {
                format!("could not stop child after process-group kill failed: {error}")
            })?;
            Some(error)
        }
    };
    child.wait().context("could not reap terminated child")?;
    if let Some(error) = group_error {
        bail!("direct child was terminated, but process-group cleanup failed: {error}");
    }
    Ok(())
}

impl Runner for SystemRunner {
    fn run(&self, spec: &CommandSpec, cancelled: &AtomicBool) -> Result<CommandOutput> {
        let mut child = Command::new(&spec.program)
            .args(&spec.args)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("could not run {}", spec.program.display()))?;
        let group = nix::unistd::Pid::from_raw(
            i32::try_from(child.id()).context("invalid child process ID")?,
        );
        let mut stdout = child.stdout.take().context("missing child stdout")?;
        let mut stderr = child.stderr.take().context("missing child stderr")?;
        let nonblocking = |fd: &dyn std::os::fd::AsFd| -> Result<()> {
            let flags = nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFL)?;
            nix::fcntl::fcntl(
                fd,
                nix::fcntl::FcntlArg::F_SETFL(
                    nix::fcntl::OFlag::from_bits_truncate(flags) | nix::fcntl::OFlag::O_NONBLOCK,
                ),
            )?;
            Ok(())
        };
        if let Err(error) = nonblocking(&stdout).and_then(|()| nonblocking(&stderr)) {
            if let Err(cleanup) = terminate_and_reap(&mut child, group) {
                return Err(error.context(format!("child cleanup also failed: {cleanup:#}")));
            }
            return Err(error);
        }
        let drain = |stream: &mut dyn Read, kept: &mut Vec<u8>| -> Result<bool> {
            let mut bytes = [0_u8; 8192];
            // Bound each batch so a continuously noisy child cannot starve the
            // cancellation/deadline checks. Retain at most one MiB per stream.
            for _ in 0..16 {
                match stream.read(&mut bytes) {
                    Ok(0) => return Ok(true),
                    Ok(count) => {
                        let remaining = (1024_usize * 1024).saturating_sub(kept.len());
                        kept.extend_from_slice(&bytes[..count.min(remaining)]);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        return Ok(false);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error.into()),
                }
            }
            Ok(false)
        };
        let deadline = Instant::now() + spec.timeout;
        let mut cancellation_sent = false;
        let mut status = None;
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let (mut out_closed, mut err_closed) = (false, false);
        let outcome = (|| -> Result<CommandOutput> {
            loop {
                if !out_closed {
                    out_closed = drain(&mut stdout, &mut out)?;
                }
                if !err_closed {
                    err_closed = drain(&mut stderr, &mut err)?;
                }
                if status.is_none() {
                    status = child.try_wait()?;
                }
                if let Some(status) = status
                    && out_closed
                    && err_closed
                {
                    return Ok(CommandOutput {
                        code: status.code().unwrap_or(-1),
                        stdout: String::from_utf8_lossy(&out).into_owned(),
                        stderr: String::from_utf8_lossy(&err).into_owned(),
                    });
                }
                if spec.cancellable && cancelled.load(Ordering::SeqCst) {
                    if spec.cooperative_cancel {
                        if !cancellation_sent && status.is_none() {
                            match nix::sys::signal::killpg(group, nix::sys::signal::Signal::SIGINT)
                            {
                                Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
                                Err(error) => {
                                    return Err(anyhow::anyhow!(error)
                                        .context("could not forward cancellation to child"));
                                }
                            }
                            cancellation_sent = true;
                        }
                    } else {
                        bail!("{} cancelled", spec.program.display());
                    }
                }
                if Instant::now() >= deadline {
                    bail!(
                        "{} timed out (including output pipe drainage)",
                        spec.program.display()
                    );
                }
                thread::sleep(Duration::from_millis(10));
            }
        })();
        if let Err(error) = outcome.as_ref()
            && status.is_none()
        {
            // The direct child is not reaped, so its process-group ID cannot
            // have been reused. Never signal a recycled group after reaping.
            if let Err(cleanup) = terminate_and_reap(&mut child, group) {
                bail!("{error:#}. Child cleanup also failed: {cleanup:#}");
            }
        }
        // Pipe descriptors close here even if a detached descendant retains its
        // write end. There are no detached reader threads or unbounded joins.
        outcome
    }
}

#[derive(Clone)]
pub struct Context {
    pub home: PathBuf,
    pub executable: PathBuf,
    pub source: Option<PathBuf>,
    pub cancelled: Arc<AtomicBool>,
    pub runner: Arc<dyn Runner>,
    pub uid: u32,
}
impl Context {
    pub fn discover() -> Result<Self> {
        let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
        let executable = std::env::current_exe()?.canonicalize()?;
        let source = executable
            .parent()
            .and_then(Path::parent)
            .filter(|target| target.file_name() == Some(OsStr::new("target")))
            .and_then(Path::parent)
            .filter(|root| root.join("Cargo.toml").is_file())
            .map(Path::to_owned);
        let cancelled = Arc::new(AtomicBool::new(false));
        let runner: Arc<dyn Runner> = Arc::new(SystemRunner);
        let id = runner.run(&CommandSpec::new("/usr/bin/id").args(["-u"]), &cancelled)?;
        if id.code != 0 {
            bail!("could not determine user ID: {}", id.stderr);
        }
        let uid = id.stdout.trim().parse().context("invalid user ID")?;
        Ok(Self {
            home,
            executable,
            source,
            cancelled,
            runner,
            uid,
        })
    }
    pub fn cache(&self) -> PathBuf {
        self.home.join("Library/Caches/clipbridge/auto")
    }
    pub fn default_config(&self) -> PathBuf {
        self.home.join(".config/clipbridge/config.json")
    }
    pub fn log_path(&self) -> PathBuf {
        self.home.join("Library/Logs/ClipBridge/clipbridge.log")
    }
    pub fn plist_path(&self) -> PathBuf {
        self.home
            .join("Library/LaunchAgents/local.clipbridge.plist")
    }
    pub fn installed_root(&self) -> Result<Option<PathBuf>> {
        let Some(release) = self.executable.parent() else {
            return Ok(None);
        };
        let Some(releases) = release.parent() else {
            return Ok(None);
        };
        if releases.file_name() != Some(OsStr::new("releases"))
            || !release.join(RELEASE_MARKER).is_file()
        {
            return Ok(None);
        }
        let root = releases.parent().context("invalid installation path")?;
        let marker: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join(INSTALL_MARKER))?)?;
        if marker["product"] != "clipbridge" || marker["schema"] != 1 {
            bail!("unrecognized installation marker");
        }
        Ok(Some(root.to_owned()))
    }
    pub fn exec(&self, spec: CommandSpec) -> Result<CommandOutput> {
        if spec.cancellable && self.cancelled.load(Ordering::SeqCst) {
            bail!("operation cancelled");
        }
        let output = self.runner.run(&spec, &self.cancelled)?;
        if spec.check && output.code != 0 {
            bail!(
                "{} failed (exit {}): {}",
                spec.program.display(),
                output.code,
                output.stderr.trim()
            );
        }
        Ok(output)
    }
}

pub fn private_dir(path: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .with_context(|| format!("could not create {}", path.display()))
}
pub fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let parent = path.parent().context("file has no parent")?;
    private_dir(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|e| e.error)
        .with_context(|| format!("could not save {}", path.display()))?;
    Ok(())
}
pub struct FileLock {
    file: File,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Explicit unlock also releases descriptors briefly inherited by a
        // concurrent fork before exec. This guard is deliberately not Clone.
        if let Err(error) = self.file.unlock() {
            eprintln!("Warning: could not unlock file; closing descriptor: {error}");
        }
    }
}

pub fn lock(path: &Path) -> Result<FileLock> {
    private_dir(path.parent().context("lock has no parent")?)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.try_lock().map_err(|e| {
        anyhow::anyhow!(
            "{} is already in use or cannot be locked: {e}",
            path.display()
        )
    })?;
    Ok(FileLock { file })
}
pub fn lock_held(path: &Path) -> Result<bool> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    match file.try_lock() {
        Ok(()) => {
            file.unlock()?;
            Ok(false)
        }
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}
pub fn unique_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}
pub fn absolute(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        Ok(path.canonicalize()?)
    } else if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}
pub fn expand_path(path: &Path, home: &Path) -> PathBuf {
    match path.to_str() {
        Some("~") => home.to_owned(),
        Some(s) if s.starts_with("~/") => home.join(&s[2..]),
        _ => path.to_owned(),
    }
}
pub fn shell_quote(value: &str) -> String {
    shell_words::quote(value).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lock_is_single_owner_and_never_unlinks_inode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        assert!(!lock_held(&path).unwrap());
        let held = lock(&path).unwrap();
        assert!(lock_held(&path).unwrap());
        assert!(lock(&path).is_err());
        drop(held);
        assert!(!lock_held(&path).unwrap());
        assert!(path.exists());
    }
    #[test]
    fn atomic_save_is_private_and_replaces_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/config.json");
        atomic_write(&path, b"first", 0o600).unwrap();
        atomic_write(&path, b"second", 0o600).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    #[test]
    fn child_failure_and_timeout_are_reported() {
        let runner = SystemRunner;
        let cancel = AtomicBool::new(false);
        let out = runner
            .run(
                &CommandSpec::new("/bin/sh").args(["-c", "printf out; printf err >&2; exit 7"]),
                &cancel,
            )
            .unwrap();
        assert_eq!(
            (out.code, out.stdout.as_str(), out.stderr.as_str()),
            (7, "out", "err")
        );
        assert!(
            runner
                .run(
                    &CommandSpec::new("/bin/sleep").args(["3"]).timeout(0),
                    &cancel
                )
                .is_err()
        );
    }

    #[test]
    fn descendant_pipes_cannot_extend_command_timeout() {
        let started = Instant::now();
        let result = SystemRunner.run(
            &CommandSpec::new("/bin/sh")
                .args(["-c", "sleep 8 & wait"])
                .timeout(1),
            &AtomicBool::new(false),
        );
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn cooperative_cancel_allows_child_cleanup() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let trigger = thread::spawn(move || {
            thread::sleep(Duration::from_millis(200));
            flag.store(true, Ordering::SeqCst);
        });
        let output = SystemRunner
            .run(
                &CommandSpec::new("/bin/sh")
                    .args([
                        "-c",
                        "trap 'printf cleaned; exit 0' INT; while :; do sleep 1; done",
                    ])
                    .cooperative()
                    .timeout(3),
                &cancelled,
            )
            .unwrap();
        trigger.join().unwrap();
        assert_eq!(output.code, 0);
        assert_eq!(output.stdout, "cleaned");
    }
}
