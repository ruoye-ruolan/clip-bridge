//! Clipboard intake stays on the main thread; one worker uploads captured files.

use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow, bail};

use crate::clipboard::{ClipboardAccess, NativeClipboard};
use crate::common::{CommandSpec, Context, atomic_write, lock, unique_id};
use crate::config::Config;
use crate::control;
use crate::transport::{TransferResult, UploadTransport};

const POLL_INTERVAL: Duration = Duration::from_millis(300);
const MAX_PENDING_UPLOADS: usize = 16;
const MAX_PENDING_NOTIFICATIONS: usize = 16;

pub fn is_session(args: &[String], host: &str) -> bool {
    let Some(program) = args.first() else {
        return false;
    };
    if Path::new(program)
        .file_name()
        .is_none_or(|name| name != "ssh")
    {
        return false;
    }
    let takes_value = "BbcDEeFIiJLlmOoPpQRSWw";
    let mut index = 1;
    while let Some(option) = args.get(index).filter(|arg| arg.starts_with('-')) {
        if option == "--" {
            index += 1;
            break;
        }
        // Queries, background/no-command tunnels and control operations are not
        // interactive sessions. Also reject combined forms such as -vG/-fN.
        if option.starts_with("--") {
            return false;
        }
        let mut consumes_next = false;
        for (offset, flag) in option[1..].char_indices() {
            if matches!(flag, 'G' | 'V' | 'O' | 'Q' | 'N' | 'f') {
                return false;
            }
            if takes_value.contains(flag) {
                consumes_next = offset + flag.len_utf8() == option.len() - 1;
                break;
            }
        }
        index += if consumes_next { 2 } else { 1 };
    }
    args.get(index).is_some_and(|target| target == host) && args.len() == index + 1
}

/// Require both a matching SSH process owned by this user and an established socket.
pub fn connected(ctx: &Context, host: &str) -> bool {
    connected_inner(ctx, host).unwrap_or(false)
}

/// Select only a configured alias with a live session, in configuration order.
pub fn active_host(ctx: &Context, config: &Config) -> Option<String> {
    config
        .ssh_hosts()
        .find(|host| connected(ctx, host))
        .map(str::to_owned)
}

struct Process {
    pid: u32,
    ppid: u32,
    uid: u32,
    command: String,
}

/// Take a leading number; ps aligns numeric columns with variable whitespace.
fn split_number(text: &str) -> Option<(u32, &str)> {
    let (number, rest) = text.trim_start().split_once(char::is_whitespace)?;
    Some((number.parse().ok()?, rest.trim_start()))
}

fn parse_process(line: &str) -> Option<Process> {
    let (pid, rest) = split_number(line)?;
    let (ppid, rest) = split_number(rest)?;
    let (uid, command) = split_number(rest)?;
    Some(Process {
        pid,
        ppid,
        uid,
        command: command.to_owned(),
    })
}

/// A session reaching its host through ProxyCommand or ProxyJump leaves the TCP
/// socket with a helper it started, so the session's descendants are probed too.
fn with_descendants(processes: &[Process], uid: u32, roots: Vec<u32>) -> Vec<u32> {
    let mut pids = roots;
    let mut next = 0;
    while let Some(&parent) = pids.get(next) {
        next += 1;
        let children: Vec<u32> = processes
            .iter()
            .filter(|process| {
                process.uid == uid && process.ppid == parent && !pids.contains(&process.pid)
            })
            .map(|process| process.pid)
            .collect();
        pids.extend(children);
    }
    pids
}

fn connected_inner(ctx: &Context, host: &str) -> Result<bool> {
    // These probes also run while draining uploads after cancellation.
    let listing = ctx.exec(
        CommandSpec::new("/bin/ps")
            .args(["-axo", "pid=,ppid=,uid=,args="])
            .uncancellable(),
    )?;
    let processes: Vec<Process> = listing.stdout.lines().filter_map(parse_process).collect();
    let sessions: Vec<u32> = processes
        .iter()
        .filter(|process| {
            process.uid == ctx.uid
                && shell_words::split(&process.command).is_ok_and(|args| is_session(&args, host))
        })
        .map(|process| process.pid)
        .collect();
    if sessions.is_empty() {
        return Ok(false);
    }
    let pids = with_descendants(&processes, ctx.uid, sessions)
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let sockets = ctx.exec(
        CommandSpec::new("/usr/sbin/lsof")
            .args([
                "-nP",
                "-a",
                "-p",
                &pids,
                "-iTCP",
                "-sTCP:ESTABLISHED",
                "-F",
                "n",
            ])
            .uncancellable(),
    )?;
    Ok(sockets
        .stdout
        .lines()
        .any(|line| line.starts_with('n') && line.contains("->")))
}

fn log(message: impl std::fmt::Display) {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    eprintln!("{seconds} {message}");
}

fn notify(ctx: &Context, message: &str) {
    let script =
        "on run argv\n display notification (item 1 of argv) with title \"ClipBridge\"\nend run";
    if let Err(error) = ctx.exec(
        CommandSpec::new("/usr/bin/osascript")
            .args(["-e", script, message])
            .uncancellable(),
    ) {
        log(format_args!("Could not show notification: {error:#}"));
    }
}

struct Capture {
    path: PathBuf,
    ssh_host: String,
    started: Instant,
}

enum UploadOutcome {
    Uploaded {
        local: PathBuf,
        ssh_host: String,
        remote: String,
        remote_clipboard: bool,
        elapsed: Duration,
    },
    RemoteClipboardFailed {
        local: PathBuf,
        ssh_host: String,
        remote: String,
        error: anyhow::Error,
        elapsed: Duration,
    },
    Skipped,
    Failed {
        local: PathBuf,
        ssh_host: String,
        error: anyhow::Error,
        elapsed: Duration,
    },
}

fn capture(
    ctx: &Context,
    config: &Config,
    clipboard: &impl ClipboardAccess,
    count: i64,
) -> Result<Option<Capture>> {
    let started = Instant::now();
    if !clipboard.has_image() {
        return Ok(None);
    }
    let Some(ssh_host) = active_host(ctx, config) else {
        return Ok(None);
    };
    // The clipboard implementation checks its change count before and after conversion.
    let Some(png) = clipboard.capture_png(count)? else {
        return Ok(None);
    };
    let path = ctx.cache().join(format!("clipboard-{}.png", unique_id()));
    atomic_write(&path, &png, 0o600)?;
    log(format_args!(
        "Captured image for ssh {ssh_host}: {}",
        path.display()
    ));
    Ok(Some(Capture {
        path,
        ssh_host,
        started,
    }))
}

fn upload_with_transport(
    ctx: &Context,
    config: &Config,
    capture: Capture,
    transport: &mut UploadTransport,
) -> UploadOutcome {
    let mut retained = capture.path.clone();
    let result = (|| -> Result<UploadOutcome> {
        // Keep queued images on the route selected at capture. A different live
        // alias must not receive an image intended for a disconnected session.
        let config = config.for_host(&capture.ssh_host)?;
        if !connected(ctx, &config.ssh_host) {
            fs::remove_file(&capture.path)?;
            log(format_args!(
                "Skipped after ssh {} disconnected: {}",
                config.ssh_host,
                capture.path.display()
            ));
            return Ok(UploadOutcome::Skipped);
        }
        if !fs::symlink_metadata(&capture.path)?.file_type().is_file() {
            bail!("Captured image is not a regular file (symlinks are rejected)");
        }
        let directory = ctx.cache().join(format!("upload-{}", unique_id()));
        fs::DirBuilder::new().mode(0o700).create(&directory)?;
        let local = directory.join(format!("shot-{}.png", unique_id()));
        fs::rename(&capture.path, &local)?;
        retained = local.clone();
        let filename = local
            .file_name()
            .and_then(|filename| filename.to_str())
            .ok_or_else(|| anyhow!("Invalid generated image name"))?;
        let remote = format!(
            "{}/{filename}",
            config.remote_directory.trim_end_matches('/')
        );
        if let TransferResult::PublicationFailed(error) =
            transport.upload(&config, &local, &remote)?
        {
            return Ok(UploadOutcome::RemoteClipboardFailed {
                local,
                ssh_host: config.ssh_host,
                remote,
                error,
                elapsed: capture.started.elapsed(),
            });
        }
        Ok(UploadOutcome::Uploaded {
            local,
            ssh_host: config.ssh_host,
            remote,
            remote_clipboard: config.remote_clipboard,
            elapsed: capture.started.elapsed(),
        })
    })();
    result.unwrap_or_else(|error| UploadOutcome::Failed {
        local: retained,
        ssh_host: capture.ssh_host,
        error,
        elapsed: capture.started.elapsed(),
    })
}

fn finish_upload(outcome: UploadOutcome) -> Option<&'static str> {
    match outcome {
        UploadOutcome::Uploaded {
            local,
            ssh_host,
            remote,
            remote_clipboard,
            elapsed,
        } => {
            log(format_args!(
                "Uploaded {} -> {ssh_host}:{remote}; local clipboard unchanged{}; capture-to-completion {} ms",
                local.display(),
                if remote_clipboard {
                    "; remote image clipboard ready"
                } else {
                    ""
                },
                elapsed.as_millis(),
            ));
            let cleanup = fs::remove_file(&local).and_then(|()| {
                if let Some(parent) = local.parent() {
                    fs::remove_dir(parent)
                } else {
                    Ok(())
                }
            });
            if let Err(error) = cleanup {
                log(format_args!(
                    "Could not clean successful upload {}: {error}",
                    local.display()
                ));
            }
            Some(if remote_clipboard {
                "Uploaded; remote image clipboard ready; local clipboard unchanged."
            } else {
                "Uploaded; clipboard unchanged."
            })
        }
        UploadOutcome::RemoteClipboardFailed {
            local,
            ssh_host,
            remote,
            error,
            elapsed,
        } => {
            log(format_args!(
                "Uploaded to {ssh_host}:{remote}, but remote clipboard sync failed: {error:#}; local clipboard unchanged; image retained at {}; capture-to-completion {} ms",
                local.display(),
                elapsed.as_millis()
            ));
            Some(
                "Uploaded, but remote clipboard sync failed; local image retained. See the ClipBridge log.",
            )
        }
        UploadOutcome::Skipped => None,
        UploadOutcome::Failed {
            local,
            ssh_host,
            error,
            elapsed,
        } => {
            log(format_args!(
                "Upload via ssh {ssh_host} failed: {error:#}; retained at {}; capture-to-completion {} ms",
                local.display(),
                elapsed.as_millis()
            ));
            Some("Upload failed; local image retained. See the ClipBridge log.")
        }
    }
}

fn complete(outcome: UploadOutcome, notifications: &SyncSender<&'static str>) -> Result<()> {
    if let Some(message) = finish_upload(outcome) {
        match notifications.try_send(message) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => log(
                "Notification queue is full; skipped notification (upload result is in the log)",
            ),
            Err(TrySendError::Disconnected(_)) => bail!("Notification worker exited unexpectedly"),
        }
    }
    Ok(())
}

fn submit_capture(sender: &SyncSender<Capture>, capture: Capture) -> Result<()> {
    match sender.try_send(capture) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(capture)) => {
            fs::remove_file(&capture.path).with_context(|| {
                format!(
                    "Upload queue is full; could not discard {}",
                    capture.path.display()
                )
            })?;
            log(format_args!(
                "Upload queue is full; skipped newly captured image {}",
                capture.path.display()
            ));
            Ok(())
        }
        Err(TrySendError::Disconnected(capture)) => bail!(
            "Upload worker exited unexpectedly; image retained at {}",
            capture.path.display()
        ),
    }
}

fn intake(
    ctx: &Context,
    config: &Config,
    clipboard: &impl ClipboardAccess,
    registration: &control::Registration,
    sender: &SyncSender<Capture>,
    results: &mpsc::Receiver<UploadOutcome>,
    notifications: &SyncSender<&'static str>,
) -> Result<()> {
    // Never send whatever was on the clipboard before monitoring started.
    let mut previous = clipboard.change_count();
    while !ctx.cancelled.load(Ordering::SeqCst) && !registration.handoff_requested()? {
        for outcome in results.try_iter() {
            complete(outcome, notifications)?;
        }
        let current = clipboard.change_count();
        if current != previous {
            // Advance while offline as well: reconnecting must not send old images.
            previous = current;
            match capture(ctx, config, clipboard, current) {
                Ok(Some(capture)) => submit_capture(sender, capture)?,
                Ok(None) => {}
                Err(error) => log(format_args!("Could not capture clipboard image: {error:#}")),
            }
        }
        thread::sleep(POLL_INTERVAL);
    }
    Ok(())
}

pub fn run(ctx: &Context, config_path: &Path, foreground: bool) -> Result<()> {
    let config = Config::load(config_path)?;
    let clipboard = NativeClipboard::general()?;
    run_with_clipboard(ctx, config_path, &config, foreground, &clipboard)
}

fn run_with_clipboard(
    ctx: &Context,
    config_path: &Path,
    config: &Config,
    foreground: bool,
    clipboard: &impl ClipboardAccess,
) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(ctx.cache())?;
    let _lock = lock(&ctx.cache().join("watcher.lock"))
        .context("Another ClipBridge instance may already be running")?;
    let registration = control::registration(ctx, config_path, foreground)?;
    log(format_args!(
        "Watching new clipboard images; requires an established SSH session for one of: {}",
        config.ssh_hosts().collect::<Vec<_>>().join(", ")
    ));
    thread::scope(|scope| {
        let (sender, jobs) = mpsc::sync_channel(MAX_PENDING_UPLOADS);
        // The main thread drains results before submitting each new capture.
        // Pending results are therefore limited by the bounded job queue and
        // in-flight jobs; an unbounded channel here avoids worker/join deadlocks.
        let (completed, results) = mpsc::channel();
        let (notifications, messages) = mpsc::sync_channel(MAX_PENDING_NOTIFICATIONS);
        let notifier = scope.spawn(move || {
            for message in messages {
                notify(ctx, message);
            }
        });
        let worker = scope.spawn(move || {
            let mut transport = UploadTransport::new(ctx);
            for capture in jobs {
                let outcome = upload_with_transport(ctx, config, capture, &mut transport);
                if completed.send(outcome).is_err() {
                    break;
                }
            }
            transport.close()
        });
        let intake_result = intake(
            ctx,
            config,
            clipboard,
            &registration,
            &sender,
            &results,
            &notifications,
        );
        log("Stopping monitor; finishing queued uploads before releasing its lock");
        drop(sender);
        // Even cancellation or intake failure must drain and join before the lock
        // and registration above can drop. No clipboard object crosses threads.
        let mut completion_result = Ok(());
        for outcome in results {
            if let Err(error) = complete(outcome, &notifications) {
                completion_result = Err(error);
            }
        }
        drop(notifications);
        let worker_result = worker
            .join()
            .map_err(|_| anyhow!("Upload worker panicked; inspect the local cache"));
        let notifier_result = notifier
            .join()
            .map_err(|_| anyhow!("Notification worker panicked"));
        worker_result??;
        notifier_result?;
        intake_result.and(completion_result)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{CommandOutput, Runner, lock_held};
    use std::cell::Cell;
    use std::sync::{Arc, Mutex, atomic::AtomicBool};

    fn receipt(spec: &CommandSpec) -> String {
        let command = spec.args.last().unwrap().to_string_lossy();
        let start = command.find("CLIPBRIDGE_COMMITTED_").unwrap();
        command[start..]
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
            .collect()
    }

    fn assert_stream_destination(command: &str, remote: &str) {
        let arguments = shell_words::split(command).unwrap();
        assert_eq!(&arguments[..2], &["sh", "-c"]);
        let script = &arguments[2];
        let path = Path::new(remote);
        assert!(script.contains(&format!("cd -P -- {}", path.parent().unwrap().display())));
        assert!(script.contains(&format!(
            "destination={}",
            path.file_name().unwrap().to_str().unwrap()
        )));
        assert!(script.contains("\"/proc/$$/cwd/$destination\""));
    }

    fn control_path(spec: &CommandSpec) -> String {
        spec.args
            .iter()
            .find_map(|arg| {
                arg.to_string_lossy()
                    .strip_prefix("ControlPath=")
                    .map(str::to_owned)
            })
            .unwrap()
    }

    fn upload(ctx: &Context, config: &Config, capture: Capture) -> UploadOutcome {
        let mut transport = UploadTransport::new(ctx);
        let outcome = upload_with_transport(ctx, config, capture, &mut transport);
        transport.close().unwrap();
        outcome
    }

    struct NotificationGate {
        started: AtomicBool,
        released: AtomicBool,
        finished: AtomicBool,
        cache: PathBuf,
    }

    #[derive(Default)]
    struct FakeRunner {
        calls: Mutex<Vec<CommandSpec>>,
        listing: Mutex<Option<String>>,
        socket_pids: Mutex<Option<Vec<String>>>,
        offline: AtomicBool,
        fail_transfer: AtomicBool,
        fail_publish: AtomicBool,
        published: AtomicBool,
        check_drain: Option<PathBuf>,
        expect_cancelled: bool,
        notification_gate: Option<Arc<NotificationGate>>,
    }

    impl Runner for FakeRunner {
        fn run(&self, spec: &CommandSpec, cancelled: &AtomicBool) -> Result<CommandOutput> {
            self.calls.lock().unwrap().push(spec.clone());
            let mut output = CommandOutput::default();
            match spec.program.to_str().unwrap() {
                "/bin/ps" => {
                    output.stdout = if self.offline.load(Ordering::SeqCst) {
                        String::new()
                    } else {
                        self.listing.lock().unwrap().clone().unwrap_or_else(|| {
                            " 42  1  501  ssh dev-server\n43  1  502  ssh dev-server\n44 1 501 ssh elsewhere\n".into()
                        })
                    };
                }
                "/usr/sbin/lsof" => {
                    let connected = self.socket_pids.lock().unwrap();
                    let has_socket = spec.args[3].to_string_lossy().split(',').any(|pid| {
                        connected
                            .as_ref()
                            .map_or(pid == "42", |pids| pids.iter().any(|value| value == pid))
                    });
                    if has_socket {
                        output.stdout = "n127.0.0.1:54000->127.0.0.1:22\n".into();
                    }
                }
                "/usr/bin/osascript" => {
                    if let Some(gate) = &self.notification_gate {
                        gate.started.store(true, Ordering::SeqCst);
                        let start = Instant::now();
                        while !gate.released.load(Ordering::SeqCst) {
                            assert!(
                                start.elapsed() < Duration::from_secs(3),
                                "Notification blocked clipboard intake"
                            );
                            assert!(lock_held(&gate.cache.join("watcher.lock")).unwrap());
                            thread::sleep(Duration::from_millis(5));
                        }
                        assert!(lock_held(&gate.cache.join("watcher.lock")).unwrap());
                        assert!(gate.cache.join("monitor.json").is_file());
                        gate.finished.store(true, Ordering::SeqCst);
                    }
                }
                "/usr/bin/ssh" => {
                    if spec.stdin_file.is_none() {
                        assert!(spec.args.iter().any(|arg| arg == "-O"));
                        let socket = spec.args.windows(2).find(|args| args[0] == "-S").unwrap();
                        fs::remove_file(Path::new(&socket[1])).unwrap();
                        if let Some(cache) = &self.check_drain {
                            assert!(lock_held(&cache.join("watcher.lock")).unwrap());
                            assert!(cache.join("monitor.json").exists());
                            fs::write(cache.join("transport-finished"), b"finished").unwrap();
                        }
                        return Ok(output);
                    }
                    let script = spec.args.last().unwrap().to_string_lossy();
                    let receipt = receipt(spec);
                    if let Some(cache) = &self.check_drain {
                        assert!(lock_held(&cache.join("watcher.lock")).unwrap());
                        assert!(cache.join("monitor.json").exists());
                        if self.expect_cancelled {
                            assert!(cancelled.load(Ordering::SeqCst));
                        }
                        assert!(!spec.cancellable);
                        // A slow transfer and transport teardown both retain the monitor lock.
                        thread::sleep(Duration::from_millis(350));
                        assert!(lock_held(&cache.join("watcher.lock")).unwrap());
                        fs::write(cache.join("worker-finished"), b"finished").unwrap();
                        let socket = control_path(spec).replace("%C", &"a".repeat(40));
                        if !Path::new(&socket).exists() {
                            let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
                            use std::os::unix::fs::PermissionsExt;
                            fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))
                                .unwrap();
                            drop(listener);
                        }
                    }
                    if self.fail_transfer.load(Ordering::SeqCst) {
                        output.code = 1;
                        output.stderr = "test transfer failure".into();
                        return Ok(output);
                    }
                    output.stdout = format!("{receipt}\n");
                    if script.contains(" publish -- ") {
                        self.published.store(true, Ordering::SeqCst);
                        if let Some(cache) = &self.check_drain {
                            fs::write(cache.join("publish-finished"), b"finished").unwrap();
                        }
                        if self.fail_publish.load(Ordering::SeqCst) {
                            output.code = 1;
                            output.stderr = "test remote clipboard failure".into();
                        }
                    }
                }
                unexpected => panic!("Unexpected subprocess: {unexpected}"),
            }
            Ok(output)
        }
    }

    struct FakeClipboard {
        count: Cell<i64>,
        image: bool,
        stale_capture: bool,
        count_reads: Cell<usize>,
        produce_after_baseline: bool,
        cancel_after_capture: Option<Arc<AtomicBool>>,
        fail_handoff_on_capture: Option<PathBuf>,
    }

    impl Default for FakeClipboard {
        fn default() -> Self {
            Self {
                count: Cell::new(10),
                image: true,
                stale_capture: false,
                count_reads: Cell::new(0),
                produce_after_baseline: false,
                cancel_after_capture: None,
                fail_handoff_on_capture: None,
            }
        }
    }

    impl ClipboardAccess for FakeClipboard {
        fn change_count(&self) -> i64 {
            let reads = self.count_reads.get();
            self.count_reads.set(reads + 1);
            if reads == 1 && self.produce_after_baseline {
                self.count.set(11);
            }
            self.count.get()
        }
        fn has_image(&self) -> bool {
            self.image
        }
        fn capture_png(&self, expected: i64) -> Result<Option<Vec<u8>>> {
            if let Some(path) = &self.fail_handoff_on_capture {
                fs::create_dir(path)?;
            }
            if let Some(cancel) = &self.cancel_after_capture {
                cancel.store(true, Ordering::SeqCst);
            }
            Ok(
                (self.image && !self.stale_capture && self.count.get() == expected)
                    .then(|| b"test PNG bytes".to_vec()),
            )
        }
    }

    fn context(home: &Path, runner: Arc<FakeRunner>) -> Context {
        Context {
            home: home.to_owned(),
            executable: home.join("clipbridge"),
            source: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            runner,
            uid: 501,
        }
    }

    fn config() -> Config {
        Config {
            ssh_host: "dev-server".into(),
            ssh_aliases: Vec::new(),
            remote_directory: "/home/example/images".into(),
            remote_clipboard: false,
        }
    }

    fn captured(ctx: &Context) -> Capture {
        let path = ctx.cache().join("capture.png");
        atomic_write(&path, b"test PNG bytes", 0o600).unwrap();
        Capture {
            path,
            ssh_host: "dev-server".into(),
            started: Instant::now(),
        }
    }

    #[test]
    fn accepts_only_sessions_for_exact_host() {
        for command in [
            "ssh dev-server",
            "/usr/bin/ssh -t -p 22 dev-server",
            "ssh -p22 dev-server",
            "ssh -vp 22 dev-server",
            "ssh -- dev-server",
            "ssh -o ProxyCommand=none dev-server",
        ] {
            assert!(
                is_session(&shell_words::split(command).unwrap(), "dev-server"),
                "{command}"
            );
        }
        for command in [
            "",
            "ssh elsewhere",
            "scp x dev-server:x",
            "ssh dev-server true",
            "ssh -G dev-server",
            "ssh -vG dev-server",
            "ssh -O check dev-server",
            "ssh -Ocheck dev-server",
            "ssh -Q cipher",
            "ssh -V",
            "ssh -N dev-server",
            "ssh -fN dev-server",
            "ssh -f dev-server",
            "ssh: /private/tmp/clipbridge-socket [mux]",
            "ssh -o ControlMaster=auto -o ControlPersist=60 dev-server 'sh -c upload'",
            "ssh --bad dev-server",
            "ssh -p",
            "ssh -p dev-server",
            "ssh -o User=dev",
            "ssh -- dev-server true",
        ] {
            assert!(
                !is_session(&shell_words::split(command).unwrap(), "dev-server"),
                "{command}"
            );
        }
    }

    #[test]
    fn connection_requires_same_user_session_and_socket() {
        let home = tempfile::tempdir().unwrap();
        let runner = Arc::new(FakeRunner::default());
        let ctx = context(home.path(), runner.clone());
        assert!(connected(&ctx, "dev-server"));
        let calls = runner.calls.lock().unwrap();
        let sockets = &calls[1];
        assert_eq!(sockets.args[3], "42");
        drop(calls);
        runner.offline.store(true, Ordering::SeqCst);
        assert!(!connected(&ctx, "dev-server"));
        assert_eq!(runner.calls.lock().unwrap().len(), 3);
    }

    #[test]
    fn connection_counts_sockets_held_by_proxy_descendants_of_a_session() {
        // ProxyCommand/ProxyJump helpers own the TCP socket; the session holds only pipes.
        for (listing, socket_pid, probed) in [
            (
                "1224 1036 501 ssh dev-server\n1225 1224 501 /usr/bin/nc 192.0.2.1 22\n",
                "1225",
                "1224,1225",
            ),
            (
                "1224 1036 501 ssh dev-server\n1225 1224 501 /bin/sh -c helper\n\
                 1226 1225 501 /usr/bin/nc 192.0.2.1 22\n",
                "1226",
                "1224,1225,1226",
            ),
        ] {
            let home = tempfile::tempdir().unwrap();
            let runner = Arc::new(FakeRunner::default());
            let ctx = context(home.path(), runner.clone());
            *runner.listing.lock().unwrap() = Some(listing.into());
            *runner.socket_pids.lock().unwrap() = Some(vec![socket_pid.into()]);
            assert!(connected(&ctx, "dev-server"), "{listing}");
            assert_eq!(runner.calls.lock().unwrap()[1].args[3], probed);
        }
    }

    #[test]
    fn connection_ignores_sockets_outside_the_session_process_tree() {
        let home = tempfile::tempdir().unwrap();
        let runner = Arc::new(FakeRunner::default());
        let ctx = context(home.path(), runner.clone());
        // An unrelated child, another user's child of the session, the session's own
        // parent shell and the child of a session for a different alias never qualify.
        *runner.listing.lock().unwrap() = Some(
            "1224 1036 501 ssh dev-server\n1230 1500 501 /usr/bin/nc 192.0.2.1 22\n\
             1231 1224 502 /usr/bin/nc 192.0.2.1 22\n1036 1 501 -zsh\n\
             1240 1 501 ssh elsewhere\n1241 1240 501 /usr/bin/nc 192.0.2.1 22\n"
                .into(),
        );
        *runner.socket_pids.lock().unwrap() =
            Some(["1230", "1231", "1036", "1241"].map(String::from).to_vec());
        assert!(!connected(&ctx, "dev-server"));
        assert_eq!(runner.calls.lock().unwrap()[1].args[3], "1224");
    }

    #[test]
    fn active_host_preserves_primary_and_selects_live_aliases_in_order() {
        let home = tempfile::tempdir().unwrap();
        let runner = Arc::new(FakeRunner::default());
        let ctx = context(home.path(), runner.clone());
        let mut config = config();
        assert_eq!(active_host(&ctx, &config).as_deref(), Some("dev-server"));
        config.ssh_aliases = vec!["dev-lan".into(), "dev-ipv6".into()];
        *runner.listing.lock().unwrap() =
            Some("42 1 501 ssh dev-server\n45 1 501 ssh dev-lan\n46 1 501 ssh dev-ipv6\n".into());
        *runner.socket_pids.lock().unwrap() = Some(vec!["42".into(), "45".into(), "46".into()]);
        assert_eq!(active_host(&ctx, &config).as_deref(), Some("dev-server"));
        *runner.socket_pids.lock().unwrap() = Some(vec!["45".into(), "46".into()]);
        assert_eq!(active_host(&ctx, &config).as_deref(), Some("dev-lan"));
        *runner.socket_pids.lock().unwrap() = Some(vec!["46".into()]);
        assert_eq!(active_host(&ctx, &config).as_deref(), Some("dev-ipv6"));
    }

    #[test]
    fn active_host_rejects_wrong_owner_missing_socket_and_unconfigured_aliases() {
        let home = tempfile::tempdir().unwrap();
        let runner = Arc::new(FakeRunner::default());
        let ctx = context(home.path(), runner.clone());
        let mut config = config();
        config.ssh_aliases = vec!["dev-lan".into()];
        *runner.listing.lock().unwrap() =
            Some("42 1 502 ssh dev-server\n45 1 501 ssh dev-lan\n46 1 501 ssh elsewhere\n".into());
        *runner.socket_pids.lock().unwrap() = Some(vec!["42".into(), "46".into()]);
        assert!(active_host(&ctx, &config).is_none());
        let calls = runner.calls.lock().unwrap();
        let socket_calls: Vec<_> = calls
            .iter()
            .filter(|call| call.program == Path::new("/usr/sbin/lsof"))
            .collect();
        assert_eq!(socket_calls.len(), 1);
        assert_eq!(socket_calls[0].args[3], "45");
    }

    #[test]
    fn captured_alias_routes_mkdir_transfer_and_publish_even_when_primary_returns() {
        let home = tempfile::tempdir().unwrap();
        let runner = Arc::new(FakeRunner::default());
        let ctx = context(home.path(), runner.clone());
        let mut config = config();
        config.ssh_aliases = vec!["dev-lan".into()];
        config.remote_clipboard = true;
        *runner.listing.lock().unwrap() = Some("45 1 501 ssh dev-lan\n".into());
        *runner.socket_pids.lock().unwrap() = Some(vec!["45".into()]);
        let image = capture(&ctx, &config, &FakeClipboard::default(), 10)
            .unwrap()
            .unwrap();
        assert_eq!(image.ssh_host, "dev-lan");
        // Queue delay can change which host has priority, but not this image's route.
        *runner.listing.lock().unwrap() =
            Some("42 1 501 ssh dev-server\n45 1 501 ssh dev-lan\n".into());
        *runner.socket_pids.lock().unwrap() = Some(vec!["42".into(), "45".into()]);
        assert_eq!(active_host(&ctx, &config).as_deref(), Some("dev-server"));
        runner.calls.lock().unwrap().clear();
        let outcome = upload(&ctx, &config, image);
        let UploadOutcome::Uploaded {
            ref ssh_host,
            ref remote,
            remote_clipboard: true,
            ..
        } = outcome
        else {
            panic!("Captured alias should receive the upload");
        };
        assert_eq!(ssh_host, "dev-lan");
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[1].args[3], "45", "Only the pinned session is checked");
        let transfers: Vec<_> = calls
            .iter()
            .filter(|call| call.stdin_file.is_some())
            .collect();
        assert_eq!(transfers.len(), 1);
        let transfer = transfers[0];
        assert_eq!(transfer.program, Path::new("/usr/bin/ssh"));
        assert_eq!(transfer.args[transfer.args.len() - 2], "dev-lan");
        let script = transfer.args.last().unwrap().to_string_lossy();
        assert_stream_destination(&script, remote);
        assert!(script.contains(" publish -- "));
        assert!(runner.published.load(Ordering::SeqCst));
        assert!(
            calls
                .iter()
                .all(|call| call.program != Path::new("/usr/bin/scp"))
        );
        drop(calls);
        finish_upload(outcome);
    }

    #[test]
    fn disconnected_capture_is_not_retargeted_to_another_live_alias() {
        let home = tempfile::tempdir().unwrap();
        let runner = Arc::new(FakeRunner::default());
        let ctx = context(home.path(), runner.clone());
        let mut config = config();
        config.ssh_aliases = vec!["dev-lan".into()];
        let image = capture(&ctx, &config, &FakeClipboard::default(), 10)
            .unwrap()
            .unwrap();
        let path = image.path.clone();
        *runner.listing.lock().unwrap() = Some("45 1 501 ssh dev-lan\n".into());
        *runner.socket_pids.lock().unwrap() = Some(vec!["45".into()]);
        assert_eq!(active_host(&ctx, &config).as_deref(), Some("dev-lan"));
        runner.calls.lock().unwrap().clear();
        assert!(matches!(
            upload(&ctx, &config, image),
            UploadOutcome::Skipped
        ));
        assert!(!path.exists());
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].program, Path::new("/bin/ps"));
    }

    #[test]
    fn consecutive_images_reuse_transport_and_measure_queue_wait() {
        let home = tempfile::tempdir().unwrap();
        let runner = Arc::new(FakeRunner::default());
        let ctx = context(home.path(), runner.clone());
        let mut config = config();
        config.ssh_aliases = vec!["dev-lan".into()];
        config.remote_clipboard = true;
        let mut transport = UploadTransport::new(&ctx);
        let mut first = captured(&ctx);
        first.started = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        let outcome = upload_with_transport(&ctx, &config, first, &mut transport);
        let UploadOutcome::Uploaded { elapsed, .. } = &outcome else {
            panic!("First image should be uploaded");
        };
        assert!(
            *elapsed >= Duration::from_secs(1),
            "Queue wait must be included"
        );
        finish_upload(outcome);
        let outcome = upload_with_transport(&ctx, &config, captured(&ctx), &mut transport);
        assert!(matches!(outcome, UploadOutcome::Uploaded { .. }));
        finish_upload(outcome);

        *runner.listing.lock().unwrap() = Some("45 1 501 ssh dev-lan\n".into());
        *runner.socket_pids.lock().unwrap() = Some(vec!["45".into()]);
        let mut alternate = captured(&ctx);
        alternate.ssh_host = "dev-lan".into();
        let outcome = upload_with_transport(&ctx, &config, alternate, &mut transport);
        assert!(matches!(outcome, UploadOutcome::Uploaded { .. }));
        finish_upload(outcome);
        let calls = runner.calls.lock().unwrap();
        let transfers: Vec<_> = calls
            .iter()
            .filter(|call| call.stdin_file.is_some())
            .collect();
        assert_eq!(transfers.len(), 3);
        assert_eq!(control_path(transfers[0]), control_path(transfers[1]));
        assert_ne!(receipt(transfers[0]), receipt(transfers[1]));
        assert_ne!(control_path(transfers[0]), control_path(transfers[2]));
        for transfer in transfers {
            let args = std::iter::once(transfer.program.to_string_lossy().into_owned())
                .chain(
                    transfer
                        .args
                        .iter()
                        .map(|arg| arg.to_string_lossy().into_owned()),
                )
                .collect::<Vec<_>>();
            assert!(!is_session(&args, "dev-server"));
            assert!(!is_session(&args, "dev-lan"));
        }
        drop(calls);
        transport.close().unwrap();
    }

    #[test]
    fn unconfigured_captured_alias_is_rejected_without_subprocesses() {
        let home = tempfile::tempdir().unwrap();
        let runner = Arc::new(FakeRunner::default());
        let ctx = context(home.path(), runner.clone());
        let mut image = captured(&ctx);
        image.ssh_host = "elsewhere".into();
        let path = image.path.clone();
        assert!(matches!(
            upload(&ctx, &config(), image),
            UploadOutcome::Failed { .. }
        ));
        assert!(path.exists());
        assert!(runner.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn failed_transfer_never_retries_on_another_live_alias() {
        let home = tempfile::tempdir().unwrap();
        let runner = Arc::new(FakeRunner::default());
        let ctx = context(home.path(), runner.clone());
        let mut config = config();
        config.ssh_aliases = vec!["dev-lan".into()];
        config.remote_clipboard = true;
        *runner.listing.lock().unwrap() =
            Some("42 1 501 ssh dev-server\n45 1 501 ssh dev-lan\n".into());
        *runner.socket_pids.lock().unwrap() = Some(vec!["42".into(), "45".into()]);
        runner.fail_transfer.store(true, Ordering::SeqCst);
        let outcome = upload(&ctx, &config, captured(&ctx));
        assert!(matches!(outcome, UploadOutcome::Failed { .. }));
        let calls = runner.calls.lock().unwrap();
        let transfers: Vec<_> = calls
            .iter()
            .filter(|call| call.stdin_file.is_some())
            .collect();
        assert_eq!(transfers.len(), 1);
        assert_eq!(transfers[0].args[transfers[0].args.len() - 2], "dev-server");
        assert!(!runner.published.load(Ordering::SeqCst));
    }

    #[test]
    fn malformed_processes_and_inspection_failures_are_offline() {
        let home = tempfile::tempdir().unwrap();
        let runner = Arc::new(FakeRunner::default());
        *runner.listing.lock().unwrap() =
            Some("x 1 501 ssh dev-server\n42 x 501 ssh dev-server\n42 1 invalid ssh dev-server\n42 1 501 ssh 'unclosed\n".into());
        let mut ctx = context(home.path(), runner);
        assert!(!connected(&ctx, "dev-server"));
        struct FailingRunner;
        impl Runner for FailingRunner {
            fn run(&self, _: &CommandSpec, _: &AtomicBool) -> Result<CommandOutput> {
                bail!("process inspection unavailable")
            }
        }
        ctx.runner = Arc::new(FailingRunner);
        assert!(!connected(&ctx, "dev-server"));
    }

    #[test]
    fn stale_or_offline_capture_does_not_create_a_file() {
        let home = tempfile::tempdir().unwrap();
        let runner = Arc::new(FakeRunner::default());
        let ctx = context(home.path(), runner.clone());
        let board = FakeClipboard {
            stale_capture: true,
            ..FakeClipboard::default()
        };
        assert!(capture(&ctx, &config(), &board, 10).unwrap().is_none());
        let board = FakeClipboard::default();
        runner.offline.store(true, Ordering::SeqCst);
        assert!(capture(&ctx, &config(), &board, 10).unwrap().is_none());
        assert!(!ctx.cache().exists());
    }

    #[test]
    fn text_never_probes_network() {
        let home = tempfile::tempdir().unwrap();
        let runner = Arc::new(FakeRunner::default());
        let ctx = context(home.path(), runner.clone());
        let board = FakeClipboard {
            image: false,
            ..FakeClipboard::default()
        };
        assert!(capture(&ctx, &config(), &board, 10).unwrap().is_none());
        assert!(runner.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn failed_upload_retains_image_and_never_updates_clipboard() {
        let home = tempfile::tempdir().unwrap();
        let runner = Arc::new(FakeRunner::default());
        runner.fail_transfer.store(true, Ordering::SeqCst);
        let ctx = context(home.path(), runner.clone());
        let board = FakeClipboard::default();
        let image = capture(&ctx, &config(), &board, 10).unwrap().unwrap();
        let outcome = upload(&ctx, &config(), image);
        let UploadOutcome::Failed { ref local, .. } = outcome else {
            panic!("Upload should fail");
        };
        assert_eq!(fs::read(local).unwrap(), b"test PNG bytes");
        assert!(finish_upload(outcome).unwrap().contains("Upload failed"));
        assert_eq!(board.count.get(), 10);
        assert_eq!(board.capture_png(10).unwrap().unwrap(), b"test PNG bytes");
        let calls = runner.calls.lock().unwrap();
        let transfer = calls.iter().find(|call| call.stdin_file.is_some()).unwrap();
        assert_eq!(transfer.program, Path::new("/usr/bin/ssh"));
        assert_eq!(transfer.timeout, Duration::from_secs(150));
        assert!(!transfer.cancellable);
        assert!(
            transfer
                .args
                .iter()
                .any(|arg| arg == "StrictHostKeyChecking=yes")
        );
        assert!(transfer.args.iter().any(|arg| arg == "BatchMode=yes"));
        assert!(transfer.args.iter().any(|arg| arg == "ConnectTimeout=30"));
    }

    #[test]
    fn successful_upload_never_replaces_original_or_newer_clipboard() {
        for changed in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let runner = Arc::new(FakeRunner::default());
            let ctx = context(home.path(), runner.clone());
            let mut board = FakeClipboard::default();
            let image = capture(&ctx, &config(), &board, 10).unwrap().unwrap();
            if changed {
                board.count.set(11);
                board.image = false;
            }
            let outcome = upload(&ctx, &config(), image);
            let UploadOutcome::Uploaded {
                ref local,
                ref remote,
                ..
            } = outcome
            else {
                panic!("Upload should succeed");
            };
            let local = local.clone();
            let remote = remote.clone();
            assert!(remote.starts_with("/home/example/images/shot-"));
            let notification = finish_upload(outcome).unwrap();
            assert_eq!(board.count.get(), if changed { 11 } else { 10 });
            assert_eq!(board.has_image(), !changed);
            if !changed {
                assert_eq!(board.capture_png(10).unwrap().unwrap(), b"test PNG bytes");
            }
            assert!(!local.exists());
            assert!(!local.parent().unwrap().exists());
            assert!(notification.contains("clipboard unchanged"));
            assert_eq!(
                runner
                    .calls
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|call| call.program == Path::new("/usr/bin/ssh"))
                    .count(),
                1,
                "remote clipboard must be opt-in"
            );
        }
    }

    #[test]
    fn enabled_remote_clipboard_publishes_after_upload_without_changing_local_image() {
        let home = tempfile::tempdir().unwrap();
        let runner = Arc::new(FakeRunner::default());
        let ctx = context(home.path(), runner.clone());
        let mut config = config();
        config.remote_clipboard = true;
        let board = FakeClipboard::default();
        let image = capture(&ctx, &config, &board, 10).unwrap().unwrap();
        let outcome = upload(&ctx, &config, image);
        let UploadOutcome::Uploaded {
            ref remote,
            remote_clipboard: true,
            ..
        } = outcome
        else {
            panic!("Remote clipboard publication should succeed");
        };
        let calls = runner.calls.lock().unwrap();
        let transfers: Vec<_> = calls
            .iter()
            .filter(|call| call.stdin_file.is_some())
            .collect();
        assert_eq!(transfers.len(), 1);
        let transfer = transfers[0];
        assert_eq!(transfer.program, Path::new("/usr/bin/ssh"));
        assert_eq!(transfer.args[transfer.args.len() - 2], "dev-server");
        let remote_command = transfer.args.last().unwrap().to_string_lossy();
        assert_stream_destination(&remote_command, remote);
        assert!(remote_command.contains(" publish -- "));
        assert!(!transfer.cancellable);
        assert_eq!(transfer.timeout, Duration::from_secs(150));
        assert!(
            transfer
                .args
                .iter()
                .any(|arg| arg == "StrictHostKeyChecking=yes")
        );
        assert!(runner.published.load(Ordering::SeqCst));
        drop(calls);
        assert!(
            finish_upload(outcome)
                .unwrap()
                .contains("remote image clipboard ready")
        );
        assert_eq!(board.count.get(), 10);
        assert_eq!(board.capture_png(10).unwrap().unwrap(), b"test PNG bytes");
    }

    #[test]
    fn remote_clipboard_failure_reports_partial_success_and_retains_local_image() {
        let home = tempfile::tempdir().unwrap();
        let runner = Arc::new(FakeRunner::default());
        runner.fail_publish.store(true, Ordering::SeqCst);
        let ctx = context(home.path(), runner);
        let mut config = config();
        config.remote_clipboard = true;
        let outcome = upload(&ctx, &config, captured(&ctx));
        let UploadOutcome::RemoteClipboardFailed {
            ref local,
            ref remote,
            ..
        } = outcome
        else {
            panic!("File upload should succeed while remote clipboard fails");
        };
        let local = local.clone();
        assert!(remote.starts_with("/home/example/images/shot-"));
        assert_eq!(fs::read(&local).unwrap(), b"test PNG bytes");
        let notification = finish_upload(outcome).unwrap();
        assert!(notification.contains("Uploaded, but remote clipboard sync failed"));
        assert!(!notification.contains("ready"));
        assert!(local.is_file());
    }

    #[test]
    fn failed_file_transfer_never_publishes_remote_clipboard() {
        let home = tempfile::tempdir().unwrap();
        let runner = Arc::new(FakeRunner::default());
        runner.fail_transfer.store(true, Ordering::SeqCst);
        let ctx = context(home.path(), runner.clone());
        let mut config = config();
        config.remote_clipboard = true;
        assert!(matches!(
            upload(&ctx, &config, captured(&ctx)),
            UploadOutcome::Failed { .. }
        ));
        assert!(!runner.published.load(Ordering::SeqCst));
        assert_eq!(
            runner
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|call| call.stdin_file.is_some())
                .count(),
            1
        );
    }

    #[test]
    fn queued_capture_is_discarded_after_disconnect() {
        let home = tempfile::tempdir().unwrap();
        let runner = Arc::new(FakeRunner::default());
        runner.offline.store(true, Ordering::SeqCst);
        let ctx = context(home.path(), runner.clone());
        let capture = captured(&ctx);
        let path = capture.path.clone();
        assert!(matches!(
            upload(&ctx, &config(), capture),
            UploadOutcome::Skipped
        ));
        assert!(!path.exists());
        assert_eq!(runner.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn symlink_capture_cannot_upload_some_other_file() {
        let home = tempfile::tempdir().unwrap();
        let runner = Arc::new(FakeRunner::default());
        let ctx = context(home.path(), runner.clone());
        let capture = captured(&ctx);
        fs::remove_file(&capture.path).unwrap();
        let other = home.path().join("private-file");
        fs::write(&other, b"private").unwrap();
        std::os::unix::fs::symlink(&other, &capture.path).unwrap();
        assert!(matches!(
            upload(&ctx, &config(), capture),
            UploadOutcome::Failed { .. }
        ));
        assert!(
            runner
                .calls
                .lock()
                .unwrap()
                .iter()
                .all(|call| call.stdin_file.is_none())
        );
        assert_eq!(fs::read(other).unwrap(), b"private");
    }

    #[test]
    fn cancellation_drains_worker_before_releasing_lock() {
        let home = tempfile::tempdir().unwrap();
        let cache = home.path().join("Library/Caches/clipbridge/auto");
        let runner = Arc::new(FakeRunner {
            check_drain: Some(cache.clone()),
            expect_cancelled: true,
            ..FakeRunner::default()
        });
        let ctx = context(home.path(), runner);
        let board = FakeClipboard {
            produce_after_baseline: true,
            cancel_after_capture: Some(ctx.cancelled.clone()),
            ..FakeClipboard::default()
        };
        let mut config = config();
        config.remote_clipboard = true;
        run_with_clipboard(&ctx, &ctx.default_config(), &config, true, &board).unwrap();
        assert!(cache.join("worker-finished").exists());
        assert!(cache.join("publish-finished").exists());
        assert!(cache.join("transport-finished").exists());
        assert_eq!(board.count.get(), 11);
        assert!(board.has_image());
        assert!(!lock_held(&cache.join("watcher.lock")).unwrap());
        assert!(!cache.join("monitor.json").exists());
    }

    #[test]
    fn intake_error_still_drains_worker_before_releasing_lock() {
        let home = tempfile::tempdir().unwrap();
        let cache = home.path().join("Library/Caches/clipbridge/auto");
        let runner = Arc::new(FakeRunner {
            check_drain: Some(cache.clone()),
            ..FakeRunner::default()
        });
        let ctx = context(home.path(), runner);
        let board = FakeClipboard {
            produce_after_baseline: true,
            fail_handoff_on_capture: Some(cache.join("handoff.json")),
            ..FakeClipboard::default()
        };
        assert!(run_with_clipboard(&ctx, &ctx.default_config(), &config(), true, &board).is_err());
        assert!(cache.join("worker-finished").exists());
        assert!(cache.join("transport-finished").exists());
        assert_eq!(board.count.get(), 11);
        assert!(board.has_image());
        assert!(!lock_held(&cache.join("watcher.lock")).unwrap());
        assert!(!cache.join("monitor.json").exists());
    }

    #[test]
    fn startup_baselines_existing_image_without_uploading() {
        let home = tempfile::tempdir().unwrap();
        let runner = Arc::new(FakeRunner::default());
        let ctx = context(home.path(), runner.clone());
        let cancel = ctx.cancelled.clone();
        let worker = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            cancel.store(true, Ordering::SeqCst);
        });
        run_with_clipboard(
            &ctx,
            &ctx.default_config(),
            &config(),
            true,
            &FakeClipboard::default(),
        )
        .unwrap();
        worker.join().unwrap();
        assert!(runner.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn reconnect_does_not_upload_image_copied_while_offline() {
        let home = tempfile::tempdir().unwrap();
        let runner = Arc::new(FakeRunner::default());
        runner.offline.store(true, Ordering::SeqCst);
        let ctx = context(home.path(), runner.clone());
        struct ReconnectClipboard {
            reads: Cell<usize>,
            runner: Arc<FakeRunner>,
            cancel: Arc<AtomicBool>,
        }
        impl ClipboardAccess for ReconnectClipboard {
            fn change_count(&self) -> i64 {
                let reads = self.reads.get();
                self.reads.set(reads + 1);
                if reads >= 2 {
                    self.runner.offline.store(false, Ordering::SeqCst);
                    self.cancel.store(true, Ordering::SeqCst);
                }
                if reads == 0 { 10 } else { 11 }
            }
            fn has_image(&self) -> bool {
                true
            }
            fn capture_png(&self, _: i64) -> Result<Option<Vec<u8>>> {
                panic!("An offline snapshot must never be captured after reconnection");
            }
        }
        let board = ReconnectClipboard {
            reads: Cell::new(0),
            runner: runner.clone(),
            cancel: ctx.cancelled.clone(),
        };
        run_with_clipboard(&ctx, &ctx.default_config(), &config(), true, &board).unwrap();
        assert_eq!(runner.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn slow_notification_does_not_block_intake_and_is_joined_before_unlock() {
        let home = tempfile::tempdir().unwrap();
        let gate = Arc::new(NotificationGate {
            started: AtomicBool::new(false),
            released: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            cache: home.path().join("Library/Caches/clipbridge/auto"),
        });
        let runner = Arc::new(FakeRunner {
            notification_gate: Some(gate.clone()),
            ..FakeRunner::default()
        });
        let ctx = context(home.path(), runner);
        struct TwoImages {
            reads: Cell<usize>,
            captures: Cell<usize>,
            gate: Arc<NotificationGate>,
            cancel: Arc<AtomicBool>,
        }
        impl ClipboardAccess for TwoImages {
            fn change_count(&self) -> i64 {
                let reads = self.reads.get();
                self.reads.set(reads + 1);
                if reads == 0 {
                    10
                } else if self.gate.started.load(Ordering::SeqCst) {
                    12
                } else {
                    11
                }
            }
            fn has_image(&self) -> bool {
                true
            }
            fn capture_png(&self, expected: i64) -> Result<Option<Vec<u8>>> {
                self.captures.set(self.captures.get() + 1);
                if expected == 12 {
                    self.gate.released.store(true, Ordering::SeqCst);
                    self.cancel.store(true, Ordering::SeqCst);
                }
                Ok(Some(b"test PNG bytes".to_vec()))
            }
        }
        let board = TwoImages {
            reads: Cell::new(0),
            captures: Cell::new(0),
            gate: gate.clone(),
            cancel: ctx.cancelled.clone(),
        };
        run_with_clipboard(&ctx, &ctx.default_config(), &config(), true, &board).unwrap();
        assert_eq!(board.captures.get(), 2);
        assert!(gate.finished.load(Ordering::SeqCst));
        assert!(!lock_held(&gate.cache.join("watcher.lock")).unwrap());
        assert!(!gate.cache.join("monitor.json").exists());
    }

    #[test]
    fn full_upload_queue_discards_only_new_capture_without_blocking() {
        let home = tempfile::tempdir().unwrap();
        let (sender, pending) = mpsc::sync_channel(MAX_PENDING_UPLOADS);
        let mut existing = Vec::new();
        for index in 0..MAX_PENDING_UPLOADS {
            let path = home.path().join(format!("pending-{index}.png"));
            fs::write(&path, b"queued image").unwrap();
            existing.push(path.clone());
            submit_capture(
                &sender,
                Capture {
                    path,
                    ssh_host: "dev-server".into(),
                    started: Instant::now(),
                },
            )
            .unwrap();
        }
        let discarded = home.path().join("new-image.png");
        fs::write(&discarded, b"new image").unwrap();
        let extra = Capture {
            path: discarded.clone(),
            ssh_host: "dev-server".into(),
            started: Instant::now(),
        };
        let (done, result) = mpsc::channel();
        let worker = thread::spawn(move || done.send(submit_capture(&sender, extra)).unwrap());
        let completion = result.recv_timeout(Duration::from_secs(1));
        // Release a potentially regressed blocking sender before asserting failure.
        drop(pending);
        worker.join().unwrap();
        completion.expect("Full queue blocked intake").unwrap();
        assert!(!discarded.exists());
        assert!(existing.iter().all(|path| path.is_file()));
        assert_eq!(
            fs::read_dir(home.path()).unwrap().count(),
            MAX_PENDING_UPLOADS
        );
    }

    #[test]
    fn full_notification_queue_completes_upload_cleanup_without_blocking() {
        let home = tempfile::tempdir().unwrap();
        let local = home.path().join("upload-test/shot.png");
        atomic_write(&local, b"uploaded image", 0o600).unwrap();
        let (notifications, pending) = mpsc::sync_channel(MAX_PENDING_NOTIFICATIONS);
        for _ in 0..MAX_PENDING_NOTIFICATIONS {
            notifications.try_send("existing notification").unwrap();
        }
        let outcome = UploadOutcome::Uploaded {
            local: local.clone(),
            ssh_host: "dev-server".into(),
            remote: "/home/example/images/shot.png".into(),
            remote_clipboard: false,
            elapsed: Duration::from_millis(25),
        };
        let (done, result) = mpsc::channel();
        let worker = thread::spawn(move || {
            done.send(complete(outcome, &notifications)).unwrap();
        });
        let completion = result.recv_timeout(Duration::from_secs(1));
        drop(pending);
        worker.join().unwrap();
        completion
            .expect("Full notification queue blocked completion")
            .unwrap();
        assert!(!local.exists());
    }
}
