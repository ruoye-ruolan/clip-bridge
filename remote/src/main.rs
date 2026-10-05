use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::{Pid, geteuid};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

mod shell;

const MAX_FRAME: u64 = 16 * 1024;
const MAX_PNG: u64 = 64 * 1024 * 1024;
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
const START_TIMEOUT: Duration = Duration::from_secs(8);
const IO_TIMEOUT: Duration = Duration::from_secs(1);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const PUBLISH_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Start the private clipboard server if it is not already running.
    Ensure,
    /// Put an uploaded PNG onto the private clipboard and verify it is readable.
    Publish { path: PathBuf },
    /// Run a terminal application with access to the private image clipboard.
    Run {
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<OsString>,
    },
    /// Show private server status without starting it.
    Status,
    /// Stop the private server and its clipboard owner.
    Stop,
    /// Check for the external X11 utilities; does not install anything.
    Doctor,
    /// Integrate image clipboard access into interactive Bash or Zsh sessions.
    Shell {
        #[command(subcommand)]
        command: shell::Action,
    },
    #[command(hide = true)]
    Serve,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Status,
    Publish { path: PathBuf },
    Stop,
}

#[derive(Debug, Deserialize, Serialize)]
struct Response {
    ok: bool,
    version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    environment: Option<Environment>,
    image_ready: bool,
}

impl Response {
    fn success(environment: &Environment, image_ready: bool) -> Self {
        Self {
            ok: true,
            version: env!("CARGO_PKG_VERSION").into(),
            error: None,
            environment: Some(environment.clone()),
            image_ready,
        }
    }

    fn failure(error: &anyhow::Error) -> Self {
        Self {
            ok: false,
            version: env!("CARGO_PKG_VERSION").into(),
            error: Some(format!("{error:#}")),
            environment: None,
            image_ready: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Environment {
    display: String,
    xauthority: PathBuf,
}

impl Environment {
    fn from_server() -> Result<Self> {
        let value = Self {
            display: std::env::var("DISPLAY").context("xvfb-run did not set DISPLAY")?,
            xauthority: std::env::var_os("XAUTHORITY")
                .context("xvfb-run did not set XAUTHORITY")?
                .into(),
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<()> {
        let display = self
            .display
            .strip_prefix(':')
            .context("expected a private local X display")?;
        ensure!(
            !display.is_empty() && display.bytes().all(|b| b.is_ascii_digit()),
            "invalid private X display"
        );
        ensure!(self.xauthority.is_absolute(), "XAUTHORITY must be absolute");
        let metadata =
            fs::symlink_metadata(&self.xauthority).context("read X authority metadata")?;
        ensure!(
            metadata.is_file()
                && metadata.uid() == geteuid().as_raw()
                && metadata.mode() & 0o077 == 0,
            "X authority must be a private regular file owned by the current user"
        );
        Ok(())
    }

    fn apply(&self, command: &mut Command) {
        command
            .env("DISPLAY", &self.display)
            .env("XAUTHORITY", &self.xauthority)
            .env_remove("WAYLAND_DISPLAY")
            .env("XDG_SESSION_TYPE", "x11");
    }
}

struct State {
    directory: PathBuf,
}

impl State {
    fn from_environment() -> Result<Self> {
        let directory = if let Some(path) = std::env::var_os("CLIPBRIDGE_REMOTE_STATE_DIR") {
            PathBuf::from(path)
        } else {
            PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?)
                .join(".local/state/clipbridge-remote")
        };
        ensure!(directory.is_absolute(), "state directory must be absolute");
        Ok(Self { directory })
    }

    fn prepare(&self) -> Result<()> {
        match fs::symlink_metadata(&self.directory) {
            Ok(metadata) => ensure!(
                metadata.is_dir()
                    && metadata.uid() == geteuid().as_raw()
                    && metadata.mode() & 0o077 == 0,
                "state directory must be a private, owned directory, not a symlink"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut builder = fs::DirBuilder::new();
                builder.recursive(true).mode(0o700);
                match builder.create(&self.directory) {
                    Ok(()) => (),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
                    Err(error) => return Err(error).context("create private state directory"),
                }
                return self.prepare();
            }
            Err(error) => return Err(error).context("inspect state directory"),
        }
        Ok(())
    }

    fn socket(&self) -> PathBuf {
        self.directory.join("control.sock")
    }

    fn private_file(&self, name: &str) -> Result<File> {
        let path = self.directory.join(name);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file()
                && metadata.uid() == geteuid().as_raw()
                && metadata.mode() & 0o077 == 0,
            "{} must be a private regular file owned by the current user",
            path.display()
        );
        Ok(file)
    }

    fn validate_socket(&self) -> Result<()> {
        let metadata = fs::symlink_metadata(self.socket())
            .context("private clipboard server is not running")?;
        ensure!(
            metadata.file_type().is_socket()
                && metadata.uid() == geteuid().as_raw()
                && metadata.mode() & 0o077 == 0,
            "refusing an unfamiliar control socket"
        );
        Ok(())
    }

    fn startup_lock(&self) -> Result<File> {
        let lock = self.private_file("startup.lock")?;
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        loop {
            match lock.try_lock() {
                Ok(()) => return Ok(lock),
                Err(std::fs::TryLockError::WouldBlock) => {
                    ensure!(
                        Instant::now() < deadline,
                        "clipboard server startup is still in progress"
                    );
                    thread::sleep(Duration::from_millis(25));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    /// Called while holding startup.lock so a new daemon cannot race stale-socket cleanup.
    fn stopped(&self, _startup_lock: &File, remove_stale_socket: bool) -> Result<bool> {
        let server_lock = self.private_file("server.lock")?;
        match server_lock.try_lock() {
            Ok(()) => (),
            Err(std::fs::TryLockError::WouldBlock) => return Ok(false),
            Err(error) => return Err(error.into()),
        }
        match fs::symlink_metadata(self.socket()) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
            Err(error) => return Err(error).context("inspect control socket"),
            Ok(_) => self.validate_socket()?,
        }
        // Do not unlink an unfamiliar live listener merely because it does not take our lock.
        match UnixStream::connect(self.socket()) {
            Ok(_) => bail!(
                "control socket has a live listener without the server lock; refusing to remove it"
            ),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) => {}
            Err(error) => return Err(error).context("check stale control socket"),
        }
        if remove_stale_socket {
            fs::remove_file(self.socket()).context("remove owned stale control socket")?;
        }
        Ok(true)
    }

    fn request(&self, request: &Request) -> Result<Response> {
        let timeout = if matches!(request, Request::Publish { .. }) {
            PUBLISH_TIMEOUT
        } else {
            REQUEST_TIMEOUT
        };
        self.request_timeout(request, timeout)
    }

    fn request_timeout(&self, request: &Request, timeout: Duration) -> Result<Response> {
        self.validate_socket()?;
        let mut stream =
            UnixStream::connect(self.socket()).context("connect to private clipboard server")?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        write_frame(&mut stream, request)?;
        let response: Response = serde_json::from_slice(&read_frame(BufReader::new(stream))?)
            .context("invalid server response")?;
        ensure!(
            response.ok,
            "{}",
            response
                .error
                .unwrap_or_else(|| "remote clipboard operation failed".into())
        );
        ensure!(
            matches!(request, Request::Stop) || response.version == env!("CARGO_PKG_VERSION"),
            "remote server version {} differs; run clipbridge-remote stop, then retry",
            response.version
        );
        Ok(response)
    }
}

fn read_frame(reader: impl BufRead) -> Result<Vec<u8>> {
    let mut frame = Vec::new();
    reader
        .take(MAX_FRAME + 1)
        .read_until(b'\n', &mut frame)
        .context("read clipboard protocol frame")?;
    ensure!(
        frame.len() as u64 <= MAX_FRAME && frame.last() == Some(&b'\n'),
        "clipboard protocol frame is incomplete or exceeds 16 KiB"
    );
    Ok(frame)
}

fn write_frame(writer: &mut impl Write, value: &impl Serialize) -> Result<()> {
    let mut frame = serde_json::to_vec(value)?;
    frame.push(b'\n');
    ensure!(
        frame.len() as u64 <= MAX_FRAME,
        "clipboard protocol frame exceeds 16 KiB"
    );
    writer
        .write_all(&frame)
        .context("write clipboard protocol frame")
}

fn read_png(path: &Path) -> Result<Vec<u8>> {
    ensure!(path.is_absolute(), "PNG path must be absolute");
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("open PNG {}", path.display()))?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file(),
        "PNG path must be a regular file, not a symlink or device"
    );
    ensure!(metadata.len() <= MAX_PNG, "PNG exceeds 64 MiB");
    let mut bytes = Vec::new();
    file.take(MAX_PNG + 1)
        .read_to_end(&mut bytes)
        .context("read PNG")?;
    ensure!(bytes.len() as u64 <= MAX_PNG, "PNG exceeds 64 MiB");
    ensure!(
        bytes.starts_with(PNG_SIGNATURE),
        "file does not have a PNG signature"
    );
    Ok(bytes)
}

fn executable_exists(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|directory| {
            fs::metadata(directory.join(name))
                .is_ok_and(|meta| meta.is_file() && meta.mode() & 0o111 != 0)
        })
    })
}

fn dependencies() -> Result<()> {
    let missing: Vec<_> = ["xvfb-run", "Xvfb", "xauth", "xclip"]
        .into_iter()
        .filter(|name| !executable_exists(name))
        .collect();
    ensure!(
        missing.is_empty(),
        "missing remote tools: {}. On Debian/Ubuntu, install xvfb, xauth and xclip with your package manager",
        missing.join(", ")
    );
    Ok(())
}

fn ensure_server(state: &State) -> Result<Response> {
    let deadline = Instant::now() + START_TIMEOUT;
    state.prepare()?;
    if let Ok(response) = state.request_timeout(&Request::Status, IO_TIMEOUT) {
        return Ok(response);
    }
    dependencies()?;
    let startup_lock = state.private_file("startup.lock")?;
    loop {
        match startup_lock.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) => {
                ensure!(
                    Instant::now() < deadline,
                    "another clipboard server startup is still in progress"
                );
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(error.into()),
        }
    }
    if let Ok(response) = state.request_timeout(&Request::Status, IO_TIMEOUT) {
        return Ok(response);
    }
    // A publisher can occupy the single request loop briefly. A held server lock means
    // retry its socket rather than spawning a competing X server after a read timeout.
    let server_lock = state.private_file("server.lock")?;
    loop {
        match server_lock.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) => {
                if let Ok(response) = state.request_timeout(&Request::Status, IO_TIMEOUT) {
                    return Ok(response);
                }
                ensure!(
                    Instant::now() < deadline,
                    "the existing clipboard server is not responding; inspect {}",
                    state.directory.join("server.log").display()
                );
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(error.into()),
        }
    }
    drop(server_lock);
    // Never pass the caller's desktop clipboard environment to the private server.
    let log = state.private_file("server.log")?;
    // Preflight this path with O_NOFOLLOW before xvfb-run opens its diagnostics file.
    drop(state.private_file("xvfb.log")?);
    let mut log = log;
    log.seek(SeekFrom::End(0))?;
    let child = Command::new("xvfb-run")
        .args([
            "-a",
            "-n",
            "90",
            "-s",
            "-screen 0 640x480x24 -nolisten tcp",
            "-e",
        ])
        .arg(state.directory.join("xvfb.log"))
        .arg(std::env::current_exe().context("locate remote helper")?)
        .arg("serve")
        .env("CLIPBRIDGE_REMOTE_STATE_DIR", &state.directory)
        .env_remove("DISPLAY")
        .env_remove("XAUTHORITY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .process_group(0)
        .spawn()
        .context("start private Xvfb clipboard server")?;
    let mut starting = StartingServer {
        child,
        ready: false,
    };
    loop {
        if let Ok(response) = state.request_timeout(&Request::Status, IO_TIMEOUT) {
            starting.ready = true;
            return Ok(response);
        }
        if let Some(status) = starting.child.try_wait()? {
            bail!(
                "clipboard server exited with {status}; inspect {}",
                state.directory.join("server.log").display()
            );
        }
        if Instant::now() >= deadline {
            bail!(
                "private clipboard server did not become ready within the startup timeout; inspect {}",
                state.directory.join("server.log").display()
            );
        }
        thread::sleep(Duration::from_millis(25));
    }
}

struct StartingServer {
    child: Child,
    ready: bool,
}

impl Drop for StartingServer {
    fn drop(&mut self) {
        // Any startup failure must clean up descendants, including an exited launcher.
        // Once ready, the deliberately detached daemon owns its own cleanup.
        if !self.ready {
            terminate_group(&mut self.child);
        }
    }
}

// The process group belongs exclusively to the child just spawned by this process.
fn terminate_group(child: &mut Child) {
    let group = Pid::from_raw(child.id() as i32);
    let _ = killpg(group, Signal::SIGTERM);
    let deadline = Instant::now() + Duration::from_millis(400);
    while Instant::now() < deadline {
        // Reaping the launcher does not mean all group members have exited. In particular,
        // an X server or clipboard owner may still be handling (or ignoring) SIGTERM.
        let _ = child.try_wait();
        if matches!(killpg(group, None), Err(nix::errno::Errno::ESRCH)) {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let _ = killpg(group, Signal::SIGKILL);
    let _ = child.wait();
}

struct ManagedChild(Child);

impl Drop for ManagedChild {
    fn drop(&mut self) {
        match self.0.try_wait() {
            Ok(Some(_)) => (),
            _ => {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }
}

struct Owner {
    process: ManagedChild,
    bytes: Vec<u8>,
}

fn clipboard_command(environment: &Environment) -> Command {
    let mut command = Command::new("xclip");
    command.args(["-selection", "clipboard", "-target", "image/png"]);
    environment.apply(&mut command);
    command
}

fn read_clipboard(
    environment: &Environment,
    directory: &Path,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let mut output = tempfile::tempfile_in(directory).context("create clipboard read buffer")?;
    let mut error = tempfile::tempfile_in(directory)?;
    let mut child = ManagedChild(
        clipboard_command(environment)
            .arg("-o")
            .stdin(Stdio::null())
            .stdout(Stdio::from(output.try_clone()?))
            .stderr(Stdio::from(error.try_clone()?))
            .spawn()
            .context("read private image clipboard")?,
    );
    let deadline = Instant::now() + timeout;
    loop {
        ensure!(
            output.metadata()?.len() <= MAX_PNG,
            "clipboard reader exceeded 64 MiB"
        );
        ensure!(
            error.metadata()?.len() <= 64 * 1024,
            "clipboard reader produced too much diagnostic output"
        );
        if let Some(status) = child.0.try_wait()? {
            if !status.success() {
                error.seek(SeekFrom::Start(0))?;
                let mut message = String::new();
                error.take(4096).read_to_string(&mut message)?;
                bail!("xclip could not read image/png: {}", message.trim());
            }
            output.seek(SeekFrom::Start(0))?;
            let mut bytes = Vec::new();
            output.take(MAX_PNG + 1).read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() as u64 <= MAX_PNG,
                "clipboard reader exceeded 64 MiB"
            );
            return Ok(bytes);
        }
        ensure!(
            Instant::now() < deadline,
            "timed out reading private image clipboard"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn start_owner(environment: &Environment, directory: &Path, bytes: Vec<u8>) -> Result<Owner> {
    // A private snapshot removes the validation-to-open race and never exposes a shell command.
    let mut source = tempfile::tempfile_in(directory)?;
    source.write_all(&bytes)?;
    source.seek(SeekFrom::Start(0))?;
    // -quiet runs in the foreground. Omitting -loops keeps the image available for repeated pastes.
    let process = ManagedChild(
        clipboard_command(environment)
            .args(["-quiet", "-i"])
            .stdin(Stdio::from(source))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("start private image clipboard owner")?,
    );
    let mut owner = Owner { process, bytes };
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        ensure!(
            owner.process.0.try_wait()?.is_none(),
            "xclip clipboard owner exited before image publication"
        );
        let remaining = deadline.saturating_duration_since(Instant::now());
        ensure!(
            !remaining.is_zero(),
            "image was not readable from the remote clipboard within 3 seconds"
        );
        if read_clipboard(
            environment,
            directory,
            remaining.min(Duration::from_millis(750)),
        )
        .is_ok_and(|read| read == owner.bytes)
            && owner.process.0.try_wait()?.is_none()
        {
            return Ok(owner);
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn publish(
    owner: &mut Option<Owner>,
    environment: &Environment,
    directory: &Path,
    path: &Path,
) -> Result<()> {
    let bytes = read_png(path)?;
    match start_owner(environment, directory, bytes) {
        Ok(new_owner) => {
            *owner = Some(new_owner);
            Ok(())
        }
        Err(error) => {
            // A candidate may take selection ownership and then fail. Restore the previous image
            // from our immutable snapshot instead of falsely reporting that it remains available.
            if let Some(previous) = owner.take() {
                let old_bytes = previous.bytes.clone();
                drop(previous);
                match start_owner(environment, directory, old_bytes) {
                    Ok(restored) => *owner = Some(restored),
                    Err(restore_error) => {
                        return Err(error).context(format!(
                            "previous clipboard image could not be restored: {restore_error:#}"
                        ));
                    }
                }
            }
            Err(error)
        }
    }
}

struct SocketGuard(PathBuf);
impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn serve(state: &State) -> Result<()> {
    state.prepare()?;
    let environment = Environment::from_server()?;
    let server_lock = state.private_file("server.lock")?;
    server_lock
        .try_lock()
        .context("a private clipboard server already owns this state directory")?;
    if state.socket().symlink_metadata().is_ok() {
        state.validate_socket()?;
        fs::remove_file(state.socket()).context("remove stale control socket")?;
    }
    let listener = UnixListener::bind(state.socket()).context("bind private control socket")?;
    let _socket_guard = SocketGuard(state.socket());
    fs::set_permissions(state.socket(), fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let stopping = Arc::new(AtomicBool::new(false));
    let signal_stop = Arc::clone(&stopping);
    ctrlc::set_handler(move || {
        signal_stop.store(true, Ordering::Relaxed);
    })
    .context("install clipboard server termination handler")?;
    let mut owner: Option<Owner> = None;
    while !stopping.load(Ordering::Relaxed) {
        let (mut stream, _) = match listener.accept() {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(15));
                continue;
            }
            Err(error) => return Err(error).context("accept clipboard request"),
        };
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        let outcome = (|| -> Result<Response> {
            let request: Request =
                serde_json::from_slice(&read_frame(BufReader::new(&mut stream))?)
                    .context("invalid clipboard request")?;
            match request {
                Request::Status => (),
                Request::Publish { path } => {
                    publish(&mut owner, &environment, &state.directory, &path)?
                }
                Request::Stop => stopping.store(true, Ordering::Relaxed),
            }
            let ready = match owner.as_mut() {
                Some(owner) => owner.process.0.try_wait()?.is_none(),
                None => false,
            };
            Ok(Response::success(&environment, ready))
        })();
        let response = outcome.unwrap_or_else(|error| Response::failure(&error));
        if let Err(error) = write_frame(&mut stream, &response) {
            eprintln!("{error:#}");
        }
    }
    drop(owner);
    Ok(())
}

fn run_application(state: &State, arguments: &[OsString]) -> Result<()> {
    let response = ensure_server(state)?;
    let environment = response
        .environment
        .context("clipboard server did not provide its private environment")?;
    environment.validate()?;
    let executable = arguments
        .first()
        .context("supply an application after --")?;
    let mut command = Command::new(executable);
    command.args(&arguments[1..]);
    environment.apply(&mut command);
    // exec preserves the terminal, signals and application exit status; no shell expansion occurs.
    Err(command.exec()).with_context(|| format!("execute {}", executable.to_string_lossy()))
}

fn print_status(response: &Response) {
    println!(
        "ClipBridge remote {} is running; image {}.",
        response.version,
        if response.image_ready {
            "ready"
        } else {
            "not yet published"
        }
    );
}

fn execute() -> Result<()> {
    let cli = Cli::parse();
    if let Action::Shell { command } = cli.command {
        return shell::execute(command);
    }
    if matches!(cli.command, Action::Doctor) {
        dependencies()?;
        println!("Remote clipboard dependencies are available (xvfb-run, Xvfb, xauth, xclip).");
        return Ok(());
    }
    let state = State::from_environment()?;
    match cli.command {
        Action::Ensure => print_status(&ensure_server(&state)?),
        Action::Publish { path } => {
            // Validate before starting a server; the daemon validates again before reading.
            drop(read_png(&path)?);
            ensure_server(&state)?;
            let response = state.request(&Request::Publish { path })?;
            ensure!(
                response.image_ready,
                "image publication did not leave a live clipboard owner"
            );
            println!("Image ready on the remote clipboard.");
        }
        Action::Run { command } => run_application(&state, &command)?,
        Action::Status => {
            if !state.directory.exists() {
                println!("ClipBridge remote is stopped.");
                return Ok(());
            }
            state.prepare()?;
            let startup_lock = state.startup_lock()?;
            if state.stopped(&startup_lock, false)? {
                println!("ClipBridge remote is stopped.");
                return Ok(());
            }
            print_status(&state.request(&Request::Status)?);
        }
        Action::Stop => {
            if !state.directory.exists() {
                println!("ClipBridge remote is already stopped.");
                return Ok(());
            }
            state.prepare()?;
            let startup_lock = state.startup_lock()?;
            if state.stopped(&startup_lock, true)? {
                println!("ClipBridge remote is already stopped.");
                return Ok(());
            }
            state.request(&Request::Stop)?;
            let deadline = Instant::now() + Duration::from_secs(3);
            while state.socket().exists() {
                ensure!(
                    Instant::now() < deadline,
                    "clipboard server did not finish stopping within 3 seconds"
                );
                thread::sleep(Duration::from_millis(20));
            }
            println!("ClipBridge remote stopped; uploaded images are preserved.");
        }
        Action::Serve => serve(&state)?,
        Action::Doctor | Action::Shell { .. } => unreachable!(),
    }
    Ok(())
}

fn main() {
    if let Err(error) = execute() {
        eprintln!("Error: {error:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Cursor, os::unix::fs::symlink};

    #[test]
    fn protocol_accepts_exactly_one_bounded_line() {
        assert_eq!(read_frame(Cursor::new(b"{}\nignored")).unwrap(), b"{}\n");
        assert!(read_frame(Cursor::new(vec![b'x'; 16 * 1024 + 1])).is_err());
        assert!(read_frame(Cursor::new(b"{}")).is_err());
    }

    #[test]
    fn accepts_absolute_regular_png_without_interpreting_shell_characters() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image ';$(touch sentinel).png");
        let data = b"\x89PNG\r\n\x1a\ncontent";
        fs::write(&path, data).unwrap();
        assert_eq!(read_png(&path).unwrap(), data);
    }

    #[test]
    fn rejects_relative_symlink_non_png_and_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.png");
        fs::write(&path, b"not a PNG").unwrap();
        assert!(read_png(&path).is_err());
        assert!(read_png(Path::new("image.png")).is_err());
        fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();
        let alias = dir.path().join("alias.png");
        symlink(&path, &alias).unwrap();
        assert!(read_png(&alias).is_err());
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(64 * 1024 * 1024 + 1)
            .unwrap();
        assert!(read_png(&path).is_err());
        assert!(read_png(dir.path()).is_err());
    }
}
