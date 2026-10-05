//! Clipboard intake stays on the main thread; one worker uploads captured files.

use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow, bail};

use crate::clipboard::{ClipboardAccess, NativeClipboard};
use crate::common::{CommandSpec, Context, atomic_write, lock, shell_quote, unique_id};
use crate::config::{Config, SSH_OPTIONS};
use crate::control;

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
        // -G/-V and control/query modes are not interactive sessions. Handle
        // combined short options as well, so -vG cannot masquerade as a session.
        if option.starts_with("--") {
            return false;
        }
        let mut consumes_next = false;
        for (offset, flag) in option[1..].char_indices() {
            if matches!(flag, 'G' | 'V' | 'O' | 'Q') {
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

fn connected_inner(ctx: &Context, host: &str) -> Result<bool> {
    // These probes also run while draining uploads after cancellation.
    let listing = ctx.exec(
        CommandSpec::new("/bin/ps")
            .args(["-axo", "pid=,uid=,args="])
            .uncancellable(),
    )?;
    let mut pids = Vec::new();
    for line in listing.stdout.lines() {
        let mut fields = line.trim().splitn(3, char::is_whitespace);
        let Some(pid) = fields.next().filter(|pid| pid.parse::<u32>().is_ok()) else {
            continue;
        };
        // ps aligns numeric columns with variable whitespace.
        let rest = line.trim_start()[pid.len()..].trim_start();
        let mut rest = rest.splitn(2, char::is_whitespace);
        let Some(uid) = rest.next().and_then(|uid| uid.parse::<u32>().ok()) else {
            continue;
        };
        let Some(command) = rest.next() else {
            continue;
        };
        if uid == ctx.uid
            && shell_words::split(command.trim_start()).is_ok_and(|args| is_session(&args, host))
        {
            pids.push(pid.to_owned());
        }
    }
    if pids.is_empty() {
        return Ok(false);
    }
    let pids = pids.join(",");
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
}

enum UploadOutcome {
    Uploaded {
        local: PathBuf,
        remote: String,
        remote_clipboard: bool,
    },
    RemoteClipboardFailed {
        local: PathBuf,
        remote: String,
        error: anyhow::Error,
    },
    Skipped,
    Failed {
        local: PathBuf,
        error: anyhow::Error,
    },
}

fn capture(
    ctx: &Context,
    config: &Config,
    clipboard: &impl ClipboardAccess,
    count: i64,
) -> Result<Option<Capture>> {
    if !clipboard.has_image() || !connected(ctx, &config.ssh_host) {
        return Ok(None);
    }
    // The clipboard implementation checks its change count before and after conversion.
    let Some(png) = clipboard.capture_png(count)? else {
        return Ok(None);
    };
    let path = ctx.cache().join(format!("clipboard-{}.png", unique_id()));
    atomic_write(&path, &png, 0o600)?;
    Ok(Some(Capture { path }))
}

fn upload(ctx: &Context, config: &Config, capture: Capture) -> UploadOutcome {
    let mut retained = capture.path.clone();
    let result = (|| -> Result<UploadOutcome> {
        if !connected(ctx, &config.ssh_host) {
            fs::remove_file(&capture.path)?;
            log(format_args!(
                "Skipped after disconnect: {}",
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
        let mkdir = format!(
            "umask 077; mkdir -p -- {}",
            shell_quote(&config.remote_directory)
        );
        ctx.exec(
            CommandSpec::new("/usr/bin/ssh")
                .args(SSH_OPTIONS.iter().copied())
                .args([config.ssh_host.as_str(), mkdir.as_str()])
                .timeout(35)
                .uncancellable(),
        )?;
        let destination = format!("{}:{remote}", config.ssh_host);
        ctx.exec(
            CommandSpec::new("/usr/bin/scp")
                .args(["-q"])
                .args(SSH_OPTIONS.iter().copied())
                .args([local.as_os_str(), destination.as_ref()])
                .timeout(120)
                .uncancellable(),
        )?;
        if config.remote_clipboard
            && let Err(error) = crate::remote::publish(ctx, config, &remote)
        {
            return Ok(UploadOutcome::RemoteClipboardFailed {
                local,
                remote,
                error,
            });
        }
        Ok(UploadOutcome::Uploaded {
            local,
            remote,
            remote_clipboard: config.remote_clipboard,
        })
    })();
    result.unwrap_or_else(|error| UploadOutcome::Failed {
        local: retained,
        error,
    })
}

fn finish_upload(outcome: UploadOutcome) -> Option<&'static str> {
    match outcome {
        UploadOutcome::Uploaded {
            local,
            remote,
            remote_clipboard,
        } => {
            log(format_args!(
                "Uploaded {} -> {remote}; local clipboard unchanged{}",
                local.display(),
                if remote_clipboard {
                    "; remote image clipboard ready"
                } else {
                    ""
                },
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
            remote,
            error,
        } => {
            log(format_args!(
                "Uploaded to {remote}, but remote clipboard sync failed: {error:#}; local clipboard unchanged; image retained at {}",
                local.display()
            ));
            Some(
                "Uploaded, but remote clipboard sync failed; local image retained. See the ClipBridge log.",
            )
        }
        UploadOutcome::Skipped => None,
        UploadOutcome::Failed { local, error } => {
            log(format_args!(
                "Upload failed: {error:#}; retained at {}",
                local.display()
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
        "Watching new clipboard images; requires established ssh {} session",
        config.ssh_host
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
            for capture in jobs {
                let outcome = upload(ctx, config, capture);
                if completed.send(outcome).is_err() {
                    break;
                }
            }
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
        worker_result?;
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
    use std::time::Instant;

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
        offline: AtomicBool,
        fail_scp: AtomicBool,
        fail_publish: AtomicBool,
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
                            " 42  501  ssh dev-server\n43  502  ssh dev-server\n44 501 ssh elsewhere\n".into()
                        })
                    };
                }
                "/usr/sbin/lsof" => output.stdout = "p42\nn127.0.0.1:54000->127.0.0.1:22\n".into(),
                "/usr/bin/scp" => {
                    if let Some(cache) = &self.check_drain {
                        assert!(lock_held(&cache.join("watcher.lock")).unwrap());
                        assert!(cache.join("monitor.json").exists());
                        if self.expect_cancelled {
                            assert!(cancelled.load(Ordering::SeqCst));
                        }
                        assert!(!spec.cancellable);
                        // Holding the lock through a slow worker prevents a second instance.
                        thread::sleep(Duration::from_millis(350));
                        assert!(lock_held(&cache.join("watcher.lock")).unwrap());
                        fs::write(cache.join("worker-finished"), b"finished").unwrap();
                    }
                    if self.fail_scp.load(Ordering::SeqCst) {
                        output.code = 1;
                        output.stderr = "test transfer failure".into();
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
                    if spec
                        .args
                        .last()
                        .is_some_and(|arg| arg.to_string_lossy().contains(" publish -- "))
                    {
                        if let Some(cache) = &self.check_drain {
                            assert!(lock_held(&cache.join("watcher.lock")).unwrap());
                            assert!(cache.join("monitor.json").exists());
                            if self.expect_cancelled {
                                assert!(cancelled.load(Ordering::SeqCst));
                            }
                            assert!(!spec.cancellable);
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
            remote_directory: "/home/example/images".into(),
            remote_clipboard: false,
        }
    }

    fn captured(ctx: &Context) -> Capture {
        let path = ctx.cache().join("capture.png");
        atomic_write(&path, b"test PNG bytes", 0o600).unwrap();
        Capture { path }
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
    fn malformed_processes_and_inspection_failures_are_offline() {
        let home = tempfile::tempdir().unwrap();
        let runner = Arc::new(FakeRunner::default());
        *runner.listing.lock().unwrap() =
            Some("x 501 ssh dev-server\n42 invalid ssh dev-server\n42 501 ssh 'unclosed\n".into());
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
        runner.fail_scp.store(true, Ordering::SeqCst);
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
        let scp = calls
            .iter()
            .find(|call| call.program == Path::new("/usr/bin/scp"))
            .unwrap();
        assert_eq!(scp.timeout, Duration::from_secs(120));
        assert!(!scp.cancellable);
        for option in SSH_OPTIONS {
            assert!(scp.args.iter().any(|arg| arg == option));
        }
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
        let publish = calls.last().unwrap();
        assert_eq!(publish.program, Path::new("/usr/bin/ssh"));
        assert_eq!(calls[calls.len() - 2].program, Path::new("/usr/bin/scp"));
        assert_eq!(publish.args[publish.args.len() - 2], "dev-server");
        let remote_command = publish.args.last().unwrap().to_string_lossy();
        assert_eq!(
            shell_words::split(&remote_command).unwrap(),
            [
                "exec",
                "$HOME/.local/bin/clipbridge-remote",
                "publish",
                "--",
                remote
            ]
        );
        assert!(!publish.cancellable);
        assert!(publish.timeout <= Duration::from_secs(35));
        for option in SSH_OPTIONS {
            assert!(publish.args.iter().any(|arg| arg == option));
        }
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
        runner.fail_scp.store(true, Ordering::SeqCst);
        let ctx = context(home.path(), runner.clone());
        let mut config = config();
        config.remote_clipboard = true;
        assert!(matches!(
            upload(&ctx, &config, captured(&ctx)),
            UploadOutcome::Failed { .. }
        ));
        assert!(runner.calls.lock().unwrap().iter().all(|call| {
            call.args
                .last()
                .is_none_or(|arg| !arg.to_string_lossy().contains(" publish -- "))
        }));
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
                .all(|call| call.program != Path::new("/usr/bin/scp"))
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
            submit_capture(&sender, Capture { path }).unwrap();
        }
        let discarded = home.path().join("new-image.png");
        fs::write(&discarded, b"new image").unwrap();
        let extra = Capture {
            path: discarded.clone(),
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
            remote: "/home/example/images/shot.png".into(),
            remote_clipboard: false,
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
