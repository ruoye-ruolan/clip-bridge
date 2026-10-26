//! One streamed SSH transfer per image, with private, short-lived multiplexing.
use crate::common::{CommandOutput, CommandSpec, Context, shell_quote, unique_id};
use crate::config::{Config, SSH_OPTIONS, directory_guard};
use anyhow::{Context as _, Result, anyhow, bail, ensure};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const CONTROL_IDLE_SECONDS: u64 = 60;
const TRANSFER_TIMEOUT_SECONDS: u64 = 150;
const CONNECT_TIMEOUT_SECONDS: u64 = 30;
const CLEANUP_TIMEOUT_SECONDS: u64 = 3;
const RECEIPT_PREFIX: &str = "CLIPBRIDGE_COMMITTED_";

#[derive(Debug)]
pub enum TransferResult {
    Uploaded,
    PublicationFailed(anyhow::Error),
}

struct Session {
    directory: tempfile::TempDir,
    device: u64,
    inode: u64,
}

impl Session {
    fn validate(&self, uid: u32) -> Result<()> {
        let metadata = fs::symlink_metadata(self.directory.path())?;
        ensure!(
            metadata.is_dir()
                && metadata.uid() == uid
                && metadata.mode() & 0o077 == 0
                && metadata.dev() == self.device
                && metadata.ino() == self.inode,
            "refusing an unfamiliar SSH control directory: {}",
            self.directory.path().display()
        );
        Ok(())
    }
}

/// The sequential upload worker owns this cache. Construction performs no I/O.
/// Connections are established by actual uploads, never by session detection.
pub struct UploadTransport {
    ctx: Context,
    sessions: BTreeMap<String, Session>,
}

impl UploadTransport {
    pub fn new(ctx: &Context) -> Self {
        Self {
            ctx: ctx.clone(),
            sessions: BTreeMap::new(),
        }
    }

    fn control_path(&mut self, alias: &str) -> Result<PathBuf> {
        if let Some(Err(error)) = self
            .sessions
            .get(alias)
            .map(|session| session.validate(self.ctx.uid))
        {
            let missing = error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound);
            // Temp cleanup or external replacement must not leave this alias stuck.
            // Disarm TempDir before evicting so a replacement is never deleted.
            let old = self
                .sessions
                .remove(alias)
                .expect("cached session was inspected");
            let preserved = old.directory.keep();
            if !missing {
                return Err(error.context(format!(
                    "preserved unrecognized control directory {}; this image was not sent",
                    preserved.display()
                )));
            }
        }
        if !self.sessions.contains_key(alias) {
            // The normal macOS TMPDIR can exceed the Unix-socket path limit.
            // tempfile creates a private random directory beneath this OS temp root.
            let base = if cfg!(target_os = "macos") {
                Path::new("/private/tmp")
            } else {
                Path::new("/tmp")
            };
            let directory = tempfile::Builder::new()
                .prefix("cb-ssh-")
                .tempdir_in(base)?;
            fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
            let metadata = fs::symlink_metadata(directory.path())?;
            let session = Session {
                directory,
                device: metadata.dev(),
                inode: metadata.ino(),
            };
            session.validate(self.ctx.uid)?;
            self.sessions.insert(alias.to_owned(), session);
        }
        let session = &self.sessions[alias];
        session.validate(self.ctx.uid)?;
        // OpenSSH expands %C from the effective host/user/port/jump configuration.
        // A changed HostName therefore cannot reuse the old host's connection.
        Ok(session.directory.path().join("s-%C"))
    }

    pub fn upload(
        &mut self,
        route: &Config,
        local: &Path,
        remote_path: &str,
    ) -> Result<TransferResult> {
        route.validate()?;
        validate_destination(route, remote_path)?;
        let metadata = fs::symlink_metadata(local)
            .with_context(|| format!("inspect captured image {}", local.display()))?;
        ensure!(
            metadata.is_file(),
            "captured image must be a regular file, not a symlink"
        );
        ensure!(metadata.len() > 0, "captured image is empty");
        let receipt = format!("{RECEIPT_PREFIX}{}", unique_id());
        let control = self.control_path(&route.ssh_host)?;
        let script = transfer_script(route, remote_path, metadata.len(), &receipt);
        let options = SSH_OPTIONS.iter().map(|option| {
            if option.starts_with("ConnectTimeout=") {
                format!("ConnectTimeout={CONNECT_TIMEOUT_SECONDS}")
            } else {
                (*option).to_owned()
            }
        });
        let command = CommandSpec::new("/usr/bin/ssh")
            .args(options)
            .args([
                "-o".to_owned(),
                format!("ControlPath={}", control.display()),
                "-o".into(),
                "ControlMaster=auto".into(),
                "-o".into(),
                format!("ControlPersist={CONTROL_IDLE_SECONDS}"),
                "-o".into(),
                "ForwardAgent=no".into(),
                "-o".into(),
                "ForwardX11=no".into(),
                "-o".into(),
                "ClearAllForwardings=yes".into(),
                "-o".into(),
                "RequestTTY=no".into(),
                "-o".into(),
                "PermitLocalCommand=no".into(),
                "-o".into(),
                "StdinNull=no".into(),
                route.ssh_host.clone(),
                format!("sh -c {}", shell_quote(&script)),
            ])
            .stdin_file(local, metadata.len())
            .timeout(TRANSFER_TIMEOUT_SECONDS)
            .uncancellable()
            .unchecked();
        let outcome = self
            .ctx
            .exec(command)
            .and_then(|output| classify(output, &receipt, route.remote_clipboard));
        match outcome {
            Ok(TransferResult::Uploaded) => Ok(TransferResult::Uploaded),
            Ok(TransferResult::PublicationFailed(error)) => {
                let error = match self.invalidate(&route.ssh_host) {
                    Ok(()) => error,
                    Err(cleanup) => {
                        error.context(format!("SSH transport cleanup also failed: {cleanup:#}"))
                    }
                };
                Ok(TransferResult::PublicationFailed(error))
            }
            Err(error) => match self.invalidate(&route.ssh_host) {
                Ok(()) => Err(error),
                Err(cleanup) => {
                    Err(error.context(format!("SSH transport cleanup also failed: {cleanup:#}")))
                }
            },
        }
    }

    fn invalidate(&mut self, alias: &str) -> Result<()> {
        if let Some(session) = self.sessions.remove(alias) {
            close_session(&self.ctx, session)?;
        }
        Ok(())
    }

    /// Close all owned control sockets while the monitor still holds its lock.
    pub fn close(&mut self) -> Result<()> {
        let mut errors = Vec::new();
        for (_, session) in std::mem::take(&mut self.sessions) {
            if let Err(error) = close_session(&self.ctx, session) {
                errors.push(format!("{error:#}"));
            }
        }
        if !errors.is_empty() {
            bail!("{}", errors.join("; "));
        }
        Ok(())
    }
}

impl Drop for UploadTransport {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            eprintln!("Could not close private SSH transports: {error:#}");
        }
    }
}

fn validate_destination(route: &Config, remote_path: &str) -> Result<()> {
    // Reuse the existing restricted POSIX-path validation, then require a direct child.
    Config::new(&route.ssh_host, remote_path)?;
    ensure!(
        Path::new(remote_path).parent() == Some(Path::new(&route.remote_directory))
            && Path::new(remote_path).file_name().is_some(),
        "upload target must be a file in the configured remote directory"
    );
    Ok(())
}

fn transfer_script(route: &Config, remote_path: &str, size: u64, receipt: &str) -> String {
    let destination = Path::new(remote_path)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    let mut script = format!(
        r#"set -eu
umask 077
{}
destination={}
receipt={receipt}
temporary=
trap 'if [ -n "$temporary" ]; then rm -f -- "$temporary"; fi' EXIT
trap 'exit 1' HUP INT TERM
temporary=$(mktemp .clipbridge-XXXXXX)
cat > "$temporary"
received=$(wc -c < "$temporary")
[ "$received" -eq {size} ] || {{ printf '%s\n' 'Incomplete image transfer' >&2; exit 1; }}
[ ! -e "$destination" ] && [ ! -L "$destination" ] || {{ printf '%s\n' 'Upload target already exists' >&2; exit 1; }}
mv -f -- "$temporary" "$destination"
temporary=
[ -f "$destination" ] && [ ! -L "$destination" ] || {{ printf '%s\n' 'Upload target is not a regular file' >&2; exit 1; }}
printf '%s\n' "$receipt"
"#,
        directory_guard(&route.remote_directory, true),
        shell_quote(destination),
    );
    if route.remote_clipboard {
        // The daemon must read the same pinned directory, even if an ancestor is
        // renamed/replaced. Linux procfs resolves this shell's cwd inode directly;
        // this shell remains alive while the companion reads the image.
        script.push_str(
            "publication_status=0\n\"$HOME/.local/bin/clipbridge-remote\" publish -- \"/proc/$$/cwd/$destination\" || publication_status=$?\nexit \"$publication_status\"\n",
        );
    }
    script
}

fn classify(output: CommandOutput, receipt: &str, publication: bool) -> Result<TransferResult> {
    let expected = format!("{receipt}\n");
    let committed = output
        .stdout
        .split_inclusive('\n')
        .any(|line| line == expected);
    if !committed {
        bail!(
            "image transfer was not confirmed (SSH exit {}): {}",
            output.code,
            output.stderr.trim()
        );
    }
    if output.code == 0 {
        return Ok(TransferResult::Uploaded);
    }
    let error = anyhow!(
        "SSH command failed (exit {}): {}",
        output.code,
        output.stderr.trim()
    );
    if publication {
        Ok(TransferResult::PublicationFailed(error))
    } else {
        Err(error
            .context("image was committed, but the upload command did not finish successfully"))
    }
}

fn valid_socket_name(name: &str) -> bool {
    let Some(hash) = name.strip_prefix("s-") else {
        return false;
    };
    matches!(hash.len(), 40 | 64)
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn socket_metadata(path: &Path, uid: u32) -> Result<Option<fs::Metadata>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        metadata.file_type().is_socket()
            && metadata.uid() == uid
            && metadata.mode() & 0o077 == 0
            && metadata.nlink() == 1,
        "refusing an unfamiliar SSH control socket: {}",
        path.display()
    );
    Ok(Some(metadata))
}

fn remove_stale_socket(path: &Path, original: &fs::Metadata, uid: u32) -> Result<bool> {
    let current = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        current.file_type().is_socket()
            && current.uid() == uid
            && current.mode() & 0o077 == 0
            && current.nlink() == 1
            && current.dev() == original.dev()
            && current.ino() == original.ino(),
        "SSH control socket changed during cleanup"
    );
    match UnixStream::connect(path) {
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => (),
        Err(error) => return Err(error).context("inspect private SSH control socket"),
    }
    match fs::remove_file(path) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    Ok(true)
}

fn close_socket(ctx: &Context, path: &Path) -> Result<()> {
    let Some(original) = socket_metadata(path, ctx.uid)? else {
        // ControlPersist may expire between read_dir and this check.
        return Ok(());
    };
    let result = ctx.exec(
        CommandSpec::new("/usr/bin/ssh")
            .args([
                "-F".as_ref(),
                Path::new("/dev/null").as_os_str(),
                "-S".as_ref(),
                path.as_os_str(),
                "-O".as_ref(),
                "exit".as_ref(),
                "localhost".as_ref(),
            ])
            .timeout(CLEANUP_TIMEOUT_SECONDS)
            .uncancellable()
            .unchecked(),
    );
    let deadline = Instant::now() + Duration::from_millis(250);
    loop {
        if remove_stale_socket(path, &original, ctx.uid)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    match result {
        Ok(output) => bail!(
            "private SSH control socket remains active after cleanup (exit {}): {}",
            output.code,
            output.stderr.trim()
        ),
        Err(error) => {
            Err(error.context("private SSH control socket remains active after failed cleanup"))
        }
    }
}

fn close_session(ctx: &Context, session: Session) -> Result<()> {
    let result = (|| -> Result<()> {
        session.validate(ctx.uid)?;
        let mut errors = Vec::new();
        for entry in fs::read_dir(session.directory.path())? {
            let entry = entry?;
            if !entry.file_name().to_str().is_some_and(valid_socket_name) {
                errors.push(format!(
                    "unfamiliar file in SSH control directory: {}",
                    entry.path().display()
                ));
                continue;
            }
            if let Err(error) = close_socket(ctx, &entry.path()) {
                errors.push(format!("{error:#}"));
            }
        }
        if !errors.is_empty() {
            bail!("{}", errors.join("; "));
        }
        Ok(())
    })();
    match result {
        Ok(()) => session
            .directory
            .close()
            .context("remove private SSH control directory"),
        Err(error) => {
            // An OS temporary-file cleanup can also race worker shutdown. There
            // is no pathname left to control; an orphaned master has its idle limit.
            let disappeared = error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
                && matches!(fs::symlink_metadata(session.directory.path()), Err(error) if error.kind() == std::io::ErrorKind::NotFound);
            let path = session.directory.keep();
            if disappeared {
                return Ok(());
            }
            Err(error.context(format!(
                "preserved SSH control directory {}",
                path.display()
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{Runner, SystemRunner};
    use std::io::Write;
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex, atomic::AtomicBool};

    const TEST_RECEIPT: &str = "CLIPBRIDGE_COMMITTED_0123456789abcdef0123456789abcdef";
    enum ResponseMode {
        Success,
        Failed,
        PublicationFailed,
        MissingReceipt,
        ExecutionError,
    }
    struct FakeRunner {
        calls: Mutex<Vec<CommandSpec>>,
        mode: Mutex<ResponseMode>,
        make_socket: bool,
    }
    impl FakeRunner {
        fn new(make_socket: bool) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                mode: Mutex::new(ResponseMode::Success),
                make_socket,
            }
        }
    }
    fn control_path(spec: &CommandSpec) -> PathBuf {
        PathBuf::from(
            spec.args
                .iter()
                .find_map(|arg| {
                    arg.to_str()?
                        .strip_prefix("ControlPath=")
                        .map(str::to_owned)
                })
                .unwrap(),
        )
    }
    fn receipt(spec: &CommandSpec) -> String {
        let command = spec.args.last().unwrap().to_str().unwrap();
        let start = command.find(RECEIPT_PREFIX).unwrap();
        command[start..]
            .chars()
            .take(RECEIPT_PREFIX.len() + 32)
            .collect()
    }
    impl Runner for FakeRunner {
        fn run(&self, spec: &CommandSpec, _: &AtomicBool) -> Result<CommandOutput> {
            self.calls.lock().unwrap().push(spec.clone());
            assert_eq!(spec.program, Path::new("/usr/bin/ssh"));
            if spec.stdin_file.is_none() {
                assert_eq!(spec.args[0], "-F");
                assert_eq!(spec.args[1], "/dev/null");
                assert_eq!(spec.args[2], "-S");
                assert_eq!(spec.args[4], "-O");
                assert_eq!(spec.args[5], "exit");
                assert_eq!(spec.args[6], "localhost");
                assert!(!spec.cancellable);
                assert!(spec.timeout <= Duration::from_secs(3));
                fs::remove_file(Path::new(&spec.args[3]))?;
                return Ok(CommandOutput::default());
            }
            if self.make_socket {
                let socket = PathBuf::from(
                    control_path(spec)
                        .to_str()
                        .unwrap()
                        .replace("%C", &"a".repeat(40)),
                );
                if !socket.exists() {
                    drop(UnixListener::bind(&socket)?);
                    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
                }
            }
            let mut output = CommandOutput {
                stdout: format!("{}\n", receipt(spec)),
                ..CommandOutput::default()
            };
            match *self.mode.lock().unwrap() {
                ResponseMode::Success => (),
                ResponseMode::Failed => {
                    output.code = 255;
                    output.stdout.clear();
                    output.stderr = "connection closed".into();
                }
                ResponseMode::PublicationFailed => {
                    output.code = 1;
                    output.stderr = "publication failed".into();
                }
                ResponseMode::MissingReceipt => output.stdout.clear(),
                ResponseMode::ExecutionError => bail!("deadline exceeded"),
            }
            Ok(output)
        }
    }
    struct Fixture {
        home: tempfile::TempDir,
        ctx: Context,
        runner: Arc<FakeRunner>,
        local: PathBuf,
        config: Config,
    }
    impl Fixture {
        fn new(make_socket: bool) -> Self {
            let home = tempfile::tempdir().unwrap();
            let runner = Arc::new(FakeRunner::new(make_socket));
            let ctx = Context {
                home: home.path().into(),
                executable: home.path().join("clipbridge"),
                source: None,
                cancelled: Arc::new(AtomicBool::new(false)),
                runner: runner.clone(),
                uid: fs::metadata(home.path()).unwrap().uid(),
            };
            let local = home.path().join("original.png");
            fs::write(&local, b"\x89PNG\r\n\x1a\nfixture data").unwrap();
            Self {
                home,
                ctx,
                runner,
                local,
                config: Config::new("server", "/tmp/images").unwrap(),
            }
        }
    }

    #[test]
    fn construction_and_close_without_images_perform_no_io_or_commands() {
        let fixture = Fixture::new(false);
        let mut pool = UploadTransport::new(&fixture.ctx);
        assert!(pool.sessions.is_empty());
        pool.close().unwrap();
        drop(pool);
        assert!(fixture.runner.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn removed_idle_control_directory_is_recreated_for_the_next_image() {
        let fixture = Fixture::new(false);
        let mut pool = UploadTransport::new(&fixture.ctx);
        pool.upload(&fixture.config, &fixture.local, "/tmp/images/first.png")
            .unwrap();
        let first = control_path(&fixture.runner.calls.lock().unwrap()[0]);
        fs::remove_dir(first.parent().unwrap()).unwrap();
        pool.upload(&fixture.config, &fixture.local, "/tmp/images/second.png")
            .unwrap();
        let calls = fixture.runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        let second = control_path(&calls[1]);
        assert_ne!(first, second);
        drop(calls);
        // The old TempDir must have been disarmed, even if its name is reused later.
        fs::create_dir(first.parent().unwrap()).unwrap();
        fs::write(first.parent().unwrap().join("unrelated"), b"keep").unwrap();
        pool.close().unwrap();
        assert!(!second.parent().unwrap().exists());
        assert_eq!(
            fs::read(first.parent().unwrap().join("unrelated")).unwrap(),
            b"keep"
        );
        fs::remove_dir_all(first.parent().unwrap()).unwrap();
    }

    #[test]
    fn replaced_control_directory_is_preserved_and_evicted_without_sending_image() {
        let fixture = Fixture::new(false);
        let mut pool = UploadTransport::new(&fixture.ctx);
        let first = pool.control_path("server").unwrap();
        let original = first.parent().unwrap().with_extension("original");
        fs::rename(first.parent().unwrap(), &original).unwrap();
        fs::create_dir(first.parent().unwrap()).unwrap();
        fs::set_permissions(first.parent().unwrap(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(first.parent().unwrap().join("unrelated"), b"keep").unwrap();
        assert!(
            pool.upload(&fixture.config, &fixture.local, "/tmp/images/first.png")
                .is_err()
        );
        assert!(pool.sessions.is_empty());
        assert!(fixture.runner.calls.lock().unwrap().is_empty());
        assert_eq!(
            fs::read(first.parent().unwrap().join("unrelated")).unwrap(),
            b"keep"
        );
        pool.upload(&fixture.config, &fixture.local, "/tmp/images/second.png")
            .unwrap();
        let second = control_path(&fixture.runner.calls.lock().unwrap()[0]);
        assert_ne!(first, second);
        pool.close().unwrap();
        assert!(first.parent().unwrap().join("unrelated").is_file());
        fs::remove_dir_all(first.parent().unwrap()).unwrap();
        fs::remove_dir_all(original).unwrap();
    }

    #[test]
    fn transfers_reuse_only_the_pinned_alias_socket_and_close_before_return() {
        let fixture = Fixture::new(true);
        let before = fs::read(&fixture.local).unwrap();
        let mut pool = UploadTransport::new(&fixture.ctx);
        for _ in 0..2 {
            assert!(matches!(
                pool.upload(&fixture.config, &fixture.local, "/tmp/images/shot.png")
                    .unwrap(),
                TransferResult::Uploaded
            ));
        }
        let other = Config::new("server-lan", "/tmp/images").unwrap();
        pool.upload(&other, &fixture.local, "/tmp/images/shot.png")
            .unwrap();
        let calls = fixture.runner.calls.lock().unwrap();
        assert_eq!(
            calls.len(),
            3,
            "one command per transfer, no preflight probes"
        );
        assert_eq!(control_path(&calls[0]), control_path(&calls[1]));
        assert_ne!(control_path(&calls[0]), control_path(&calls[2]));
        assert_ne!(receipt(&calls[0]), receipt(&calls[1]));
        let dirs: Vec<_> = calls
            .iter()
            .map(|call| control_path(call).parent().unwrap().to_owned())
            .collect();
        for (index, call) in calls.iter().enumerate() {
            assert!(control_path(call).to_str().unwrap().ends_with("/s-%C"));
            assert!(control_path(call).as_os_str().len() + 38 < 104);
            assert_eq!(fs::metadata(&dirs[index]).unwrap().mode() & 0o777, 0o700);
            assert!(!call.cancellable);
            assert_eq!(call.timeout, Duration::from_secs(150));
            assert!(!call.check);
            assert_eq!(
                call.stdin_file.as_ref().unwrap().expected_len,
                before.len() as u64
            );
            for option in [
                "StrictHostKeyChecking=yes",
                "BatchMode=yes",
                "ConnectTimeout=30",
                "ControlMaster=auto",
                "ControlPersist=60",
                "ForwardAgent=no",
                "ForwardX11=no",
                "ClearAllForwardings=yes",
                "RequestTTY=no",
                "PermitLocalCommand=no",
                "StdinNull=no",
            ] {
                assert!(
                    call.args.iter().any(|arg| arg == option),
                    "missing {option}"
                );
            }
            assert!(!call.args.iter().any(|arg| arg == "ConnectTimeout=8"));
            assert_eq!(
                call.args[call.args.len() - 2],
                if index == 2 { "server-lan" } else { "server" }
            );
        }
        drop(calls);
        pool.close().unwrap();
        assert!(dirs.iter().all(|dir| !dir.exists()));
        assert_eq!(
            fixture
                .runner
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|call| call.stdin_file.is_none())
                .count(),
            2
        );
        assert_eq!(fs::read(&fixture.local).unwrap(), before);
    }

    #[test]
    fn failure_invalidates_only_that_alias_without_retrying_or_removing_input() {
        for mode in [
            ResponseMode::Failed,
            ResponseMode::MissingReceipt,
            ResponseMode::ExecutionError,
            ResponseMode::PublicationFailed,
        ] {
            let mut fixture = Fixture::new(true);
            fixture.config.remote_clipboard = true;
            let mut pool = UploadTransport::new(&fixture.ctx);
            pool.upload(&fixture.config, &fixture.local, "/tmp/images/shot.png")
                .unwrap();
            let previous = control_path(&fixture.runner.calls.lock().unwrap()[0]);
            *fixture.runner.mode.lock().unwrap() = mode;
            let outcome = pool.upload(&fixture.config, &fixture.local, "/tmp/images/shot2.png");
            assert!(
                outcome.is_err()
                    || matches!(outcome.unwrap(), TransferResult::PublicationFailed(_))
            );
            assert!(fixture.local.is_file());
            assert!(pool.sessions.is_empty());
            assert!(!previous.parent().unwrap().exists());
            let calls = fixture.runner.calls.lock().unwrap();
            assert_eq!(
                calls
                    .iter()
                    .filter(|call| call.stdin_file.is_some())
                    .count(),
                2,
                "no retry after failed image"
            );
            assert_eq!(
                calls
                    .iter()
                    .filter(|call| call.stdin_file.is_none())
                    .count(),
                1
            );
            drop(calls);
            *fixture.runner.mode.lock().unwrap() = ResponseMode::Success;
            pool.upload(&fixture.config, &fixture.local, "/tmp/images/new-image.png")
                .unwrap();
            let calls = fixture.runner.calls.lock().unwrap();
            assert_ne!(previous, control_path(calls.last().unwrap()));
        }
    }

    #[test]
    fn missing_partial_or_prefixed_receipts_never_report_success() {
        for stdout in [
            String::new(),
            TEST_RECEIPT.to_owned(),
            format!("prefix{TEST_RECEIPT}\n"),
            format!("{TEST_RECEIPT}suffix\n"),
        ] {
            assert!(
                classify(
                    CommandOutput {
                        stdout,
                        ..CommandOutput::default()
                    },
                    TEST_RECEIPT,
                    true
                )
                .is_err()
            );
        }
        assert!(matches!(
            classify(
                CommandOutput {
                    stdout: format!("noise\n{TEST_RECEIPT}\nready\n"),
                    ..CommandOutput::default()
                },
                TEST_RECEIPT,
                true
            )
            .unwrap(),
            TransferResult::Uploaded
        ));
    }

    #[test]
    fn invalid_local_or_remote_paths_never_create_a_transport() {
        let fixture = Fixture::new(false);
        let alias = fixture.home.path().join("alias.png");
        symlink(&fixture.local, &alias).unwrap();
        let empty = fixture.home.path().join("empty.png");
        fs::write(&empty, b"").unwrap();
        let mut pool = UploadTransport::new(&fixture.ctx);
        for path in [&alias, &empty] {
            assert!(
                pool.upload(&fixture.config, path, "/tmp/images/shot.png")
                    .is_err()
            );
        }
        for target in [
            "/tmp/elsewhere/shot.png",
            "/tmp/images/../shot.png",
            "/tmp/images/shot;touch.png",
            "relative.png",
        ] {
            assert!(
                pool.upload(&fixture.config, &fixture.local, target)
                    .is_err()
            );
        }
        assert!(pool.sessions.is_empty());
        assert!(fixture.runner.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn cleanup_preserves_unfamiliar_files_and_never_contacts_foreign_sockets() {
        for kind in ["regular", "symlink", "unknown", "public_socket"] {
            let fixture = Fixture::new(false);
            let mut pool = UploadTransport::new(&fixture.ctx);
            let template = pool.control_path("server").unwrap();
            let directory = template.parent().unwrap().to_owned();
            let socket = directory.join(format!("s-{}", "b".repeat(40)));
            let outside = fixture.home.path().join("outside");
            fs::write(&outside, b"keep").unwrap();
            match kind {
                "regular" => fs::write(&socket, b"unfamiliar").unwrap(),
                "symlink" => symlink(&outside, &socket).unwrap(),
                "unknown" => fs::write(directory.join("unknown"), b"keep").unwrap(),
                "public_socket" => {
                    drop(UnixListener::bind(&socket).unwrap());
                    fs::set_permissions(&socket, fs::Permissions::from_mode(0o666)).unwrap();
                }
                _ => unreachable!(),
            }
            assert!(pool.close().is_err());
            assert!(fixture.runner.calls.lock().unwrap().is_empty());
            assert!(directory.exists());
            assert_eq!(fs::read(&outside).unwrap(), b"keep");
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn cleanup_accepts_socket_expiry_without_issuing_a_connection_command() {
        let fixture = Fixture::new(false);
        let socket = fixture.home.path().join(format!("s-{}", "c".repeat(40)));
        close_socket(&fixture.ctx, &socket).unwrap();
        assert!(fixture.runner.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn shutdown_accepts_an_already_removed_control_directory() {
        let fixture = Fixture::new(false);
        let mut pool = UploadTransport::new(&fixture.ctx);
        let path = pool.control_path("server").unwrap();
        fs::remove_dir(path.parent().unwrap()).unwrap();
        pool.close().unwrap();
        assert!(fixture.runner.calls.lock().unwrap().is_empty());
    }

    fn run_script(
        directory: &Path,
        contents: &[u8],
        expected: u64,
        publish: bool,
        setup: impl FnOnce(&Path),
    ) -> (CommandOutput, PathBuf) {
        let destination = directory.join("remote");
        let home = directory.join("home");
        let bin = directory.join("bin");
        fs::create_dir_all(home.join(".local/bin")).unwrap();
        fs::create_dir(&bin).unwrap();
        let helper = home.join(".local/bin/clipbridge-remote");
        fs::write(
            &helper,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" > \"$HOME/published\"\nexit 17\n",
        )
        .unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        setup(&bin);
        let mut config = Config::new("server", destination.to_str().unwrap()).unwrap();
        config.remote_clipboard = publish;
        let target = destination.join("shot.png");
        let script = transfer_script(&config, target.to_str().unwrap(), expected, TEST_RECEIPT);
        let mut child = Command::new("/bin/sh")
            .args(["-c", &script])
            .env("HOME", &home)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(contents).unwrap();
        let output = child.wait_with_output().unwrap();
        (
            CommandOutput {
                code: output.status.code().unwrap_or(-1),
                stdout: String::from_utf8(output.stdout).unwrap(),
                stderr: String::from_utf8(output.stderr).unwrap(),
            },
            target,
        )
    }

    #[test]
    fn real_shell_script_commits_exact_bytes_atomically_with_private_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = b"\x89PNG\r\n\x1a\nsynthetic fixture\0bytes";
        let (output, target) =
            run_script(directory.path(), bytes, bytes.len() as u64, false, |_| {});
        assert_eq!(output.code, 0, "{}", output.stderr);
        assert!(matches!(
            classify(output, TEST_RECEIPT, false).unwrap(),
            TransferResult::Uploaded
        ));
        assert_eq!(fs::read(&target).unwrap(), bytes);
        assert_eq!(fs::metadata(&target).unwrap().mode() & 0o777, 0o600);
        assert_eq!(fs::read_dir(target.parent().unwrap()).unwrap().count(), 1);
        assert!(!directory.path().join("home/published").exists());
    }

    #[test]
    fn shared_writable_destination_is_rejected_before_staging_or_reading_stdin() {
        for mode in [0o777, 0o775, 0o702] {
            let directory = tempfile::tempdir().unwrap();
            let (output, target) = run_script(directory.path(), b"abc", 3, true, |bin| {
                let remote = bin.parent().unwrap().join("remote");
                fs::create_dir(&remote).unwrap();
                fs::set_permissions(&remote, fs::Permissions::from_mode(mode)).unwrap();
                fs::write(remote.join("unrelated"), b"preserve").unwrap();
                let cat = bin.join("cat");
                fs::write(&cat, "#!/bin/sh\ntouch \"$HOME/cat-called\"\nexit 1\n").unwrap();
                fs::set_permissions(cat, fs::Permissions::from_mode(0o700)).unwrap();
            });
            assert_ne!(output.code, 0);
            assert!(output.stderr.contains("not writable by group or others"));
            assert!(!target.exists());
            assert_eq!(
                fs::read(target.parent().unwrap().join("unrelated")).unwrap(),
                b"preserve"
            );
            assert_eq!(
                fs::metadata(target.parent().unwrap()).unwrap().mode() & 0o777,
                mode
            );
            assert_eq!(fs::read_dir(target.parent().unwrap()).unwrap().count(), 1);
            assert!(!directory.path().join("home/cat-called").exists());
            assert!(!directory.path().join("home/published").exists());
        }
    }

    #[test]
    fn owned_private_or_readable_destinations_keep_existing_permissions() {
        for mode in [0o700, 0o755] {
            let directory = tempfile::tempdir().unwrap();
            let (output, target) = run_script(directory.path(), b"abc", 3, false, |bin| {
                let remote = bin.parent().unwrap().join("remote");
                fs::create_dir(&remote).unwrap();
                fs::set_permissions(remote, fs::Permissions::from_mode(mode)).unwrap();
            });
            assert_eq!(output.code, 0, "{}", output.stderr);
            assert_eq!(fs::read(&target).unwrap(), b"abc");
            assert_eq!(
                fs::metadata(target.parent().unwrap()).unwrap().mode() & 0o777,
                mode
            );
        }
    }

    #[test]
    fn directory_rename_cannot_redirect_staging_or_final_write_to_a_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let moved = directory.path().join("pinned");
        let (output, old_target) = run_script(directory.path(), b"abc", 3, false, |bin| {
            let root = bin.parent().unwrap();
            let source = shell_quote(&root.join("remote").to_string_lossy());
            let pinned = shell_quote(&root.join("pinned").to_string_lossy());
            let cat = bin.join("cat");
            fs::write(&cat, format!("#!/bin/sh\nset -e\nmv -- {source} {pinned}\nmkdir -- {source}\nchmod 777 {source}\nexec /bin/cat \"$@\"\n")).unwrap();
            fs::set_permissions(cat, fs::Permissions::from_mode(0o700)).unwrap();
        });
        assert_eq!(output.code, 0, "{}", output.stderr);
        assert_eq!(fs::read(moved.join("shot.png")).unwrap(), b"abc");
        assert!(!old_target.exists());
        assert_eq!(
            fs::read_dir(old_target.parent().unwrap()).unwrap().count(),
            0
        );
        assert_eq!(fs::read_dir(&moved).unwrap().count(), 1);
    }

    #[test]
    fn truncated_input_and_cat_or_move_failure_leave_no_published_or_partial_file() {
        for failure in ["truncated", "cat", "mv"] {
            let directory = tempfile::tempdir().unwrap();
            let (output, target) = run_script(
                directory.path(),
                b"abc",
                if failure == "truncated" { 5 } else { 3 },
                true,
                |bin| {
                    if failure != "truncated" {
                        let file = bin.join(failure);
                        fs::write(&file, "#!/bin/sh\nexit 19\n").unwrap();
                        fs::set_permissions(file, fs::Permissions::from_mode(0o700)).unwrap();
                    }
                },
            );
            assert_ne!(output.code, 0);
            assert!(classify(output, TEST_RECEIPT, true).is_err());
            assert!(!target.exists());
            assert_eq!(fs::read_dir(target.parent().unwrap()).unwrap().count(), 0);
            assert!(!directory.path().join("home/published").exists());
        }
    }

    #[test]
    fn publication_failure_is_reported_only_after_verified_file_commit() {
        let directory = tempfile::tempdir().unwrap();
        let (output, target) = run_script(directory.path(), b"abc", 3, true, |_| {});
        assert_eq!(output.code, 17);
        assert_eq!(fs::read(&target).unwrap(), b"abc");
        assert!(matches!(
            classify(output, TEST_RECEIPT, true).unwrap(),
            TransferResult::PublicationFailed(_)
        ));
        let arguments = fs::read_to_string(directory.path().join("home/published")).unwrap();
        let pinned = arguments.trim().strip_prefix("publish -- /proc/").unwrap();
        let (pid, tail) = pinned.split_once('/').unwrap();
        assert!(pid.parse::<u32>().is_ok());
        assert_eq!(tail, "cwd/shot.png");
    }

    #[test]
    fn runner_detects_input_changes_after_transport_inspection() {
        let fixture = Fixture::new(false);
        let spec = CommandSpec::new("/usr/bin/wc")
            .args(["-c"])
            .stdin_file(&fixture.local, 1);
        assert!(SystemRunner.run(&spec, &AtomicBool::new(false)).is_err());
    }
}
