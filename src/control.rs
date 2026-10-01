//! Cooperative handoff protocol, compatible with the original monitor metadata.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};

use crate::common::{Context, absolute, atomic_write, lock_held, unique_id};

const VERSION: u64 = 1;
const POLL_INTERVAL: Duration = Duration::from_millis(100);

fn read_json(path: &Path) -> Result<Option<Value>> {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("Read {}", path.display())),
    };
    Ok(serde_json::from_slice::<Value>(&data)
        .ok()
        .filter(Value::is_object))
}

fn remove_matching(path: &Path, token: &str, request_id: Option<&str>) -> Result<()> {
    if let Some(current) = read_json(path)?
        && current["token"].as_str() == Some(token)
        && request_id.is_none_or(|id| current["request_id"].as_str() == Some(id))
    {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("Remove {}", path.display())),
        }
    }
    Ok(())
}

pub struct Registration {
    cache: PathBuf,
    token: String,
    foreground: bool,
}

impl Registration {
    pub fn handoff_requested(&self) -> Result<bool> {
        if !self.foreground {
            return Ok(false);
        }
        Ok(
            read_json(&self.cache.join("handoff.json"))?.is_some_and(|request| {
                request["version"].as_u64() == Some(VERSION)
                    && request["token"].as_str() == Some(self.token.as_str())
                    && request["request_id"]
                        .as_str()
                        .is_some_and(|id| !id.is_empty())
            }),
        )
    }

    fn cleanup(&self) -> Result<()> {
        let request_result = remove_matching(&self.cache.join("handoff.json"), &self.token, None);
        let state_result = remove_matching(&self.cache.join("monitor.json"), &self.token, None);
        request_result.and(state_result)
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            eprintln!("Warning: could not clean monitor metadata: {error:#}");
        }
    }
}

/// The caller must keep watcher.lock held until this registration and all uploads finish.
pub fn registration(ctx: &Context, config: &Path, foreground: bool) -> Result<Registration> {
    let registration = Registration {
        cache: ctx.cache(),
        token: unique_id(),
        foreground,
    };
    let state = json!({
        "version": VERSION,
        "token": registration.token,
        "pid": std::process::id(),
        "config": absolute(config)?,
        "mode": if foreground { "foreground" } else { "background" },
    });
    atomic_write(
        &registration.cache.join("monitor.json"),
        &serde_json::to_vec(&state)?,
        0o600,
    )?;
    Ok(registration)
}

struct Request {
    path: PathBuf,
    token: String,
    id: String,
}

impl Drop for Request {
    fn drop(&mut self) {
        if let Err(error) = remove_matching(&self.path, &self.token, Some(&self.id)) {
            eprintln!("Warning: could not withdraw handoff request: {error:#}");
        }
    }
}

/// Ask a foreground instance to drain; never signal an arbitrary process.
pub fn request_handoff(ctx: &Context, timeout: Duration) -> Result<bool> {
    let cache = ctx.cache();
    let state_path = cache.join("monitor.json");
    let Some(state) = read_json(&state_path)? else {
        return Ok(false);
    };
    let Some(token) = state["token"].as_str().filter(|token| !token.is_empty()) else {
        return Ok(false);
    };
    if state["version"].as_u64() != Some(VERSION)
        || state["mode"].as_str() != Some("foreground")
        || !lock_held(&cache.join("watcher.lock"))?
    {
        return Ok(false);
    }
    let request = Request {
        path: cache.join("handoff.json"),
        token: token.to_owned(),
        id: unique_id(),
    };
    let message = json!({"version": VERSION, "token": request.token, "request_id": request.id});
    atomic_write(&request.path, &serde_json::to_vec(&message)?, 0o600)?;
    let started = Instant::now();
    loop {
        if ctx.cancelled.load(Ordering::SeqCst) {
            bail!("Handoff cancelled; the foreground monitor may already be draining uploads");
        }
        if read_json(&state_path)?.is_some_and(|current| current["token"].as_str() != Some(token)) {
            return Ok(false);
        }
        if !lock_held(&cache.join("watcher.lock"))? {
            return Ok(true);
        }
        if started.elapsed() >= timeout {
            bail!(
                "Timed out waiting for the foreground ClipBridge monitor to finish uploads and \
                 release its lock. It was not forcibly stopped; inspect its terminal before \
                 trying clipbridge start again."
            );
        }
        thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{CommandOutput, CommandSpec, Runner, lock};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, atomic::AtomicBool, mpsc};

    struct NoCommands;
    impl Runner for NoCommands {
        fn run(&self, _: &CommandSpec, _: &AtomicBool) -> Result<CommandOutput> {
            panic!("Control protocol must not launch any process");
        }
    }

    fn context(home: &Path) -> Context {
        Context {
            home: home.to_owned(),
            executable: home.join("clipbridge"),
            source: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            runner: Arc::new(NoCommands),
            uid: 501,
        }
    }

    fn write_request(ctx: &Context, token: &str, id: &str) {
        atomic_write(
            &ctx.cache().join("handoff.json"),
            &serde_json::to_vec(&json!({"version": 1, "token": token, "request_id": id})).unwrap(),
            0o600,
        )
        .unwrap();
    }

    #[test]
    fn registration_is_private_and_cleans_matching_metadata() {
        let home = tempfile::tempdir().unwrap();
        let ctx = context(home.path());
        let _lock = lock(&ctx.cache().join("watcher.lock")).unwrap();
        let registration = registration(&ctx, &ctx.default_config(), true).unwrap();
        let path = ctx.cache().join("monitor.json");
        let state = read_json(&path).unwrap().unwrap();
        assert_eq!(state["version"], 1);
        assert_eq!(state["mode"], "foreground");
        assert_eq!(state["pid"], std::process::id());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(!registration.handoff_requested().unwrap());
        write_request(&ctx, &registration.token, "request");
        assert!(registration.handoff_requested().unwrap());
        drop(registration);
        assert!(!path.exists());
        assert!(!ctx.cache().join("handoff.json").exists());
        assert!(ctx.cache().join("watcher.lock").exists());
    }

    #[test]
    fn stale_invalid_and_background_requests_are_ignored() {
        let home = tempfile::tempdir().unwrap();
        let ctx = context(home.path());
        let _lock = lock(&ctx.cache().join("watcher.lock")).unwrap();
        let current = registration(&ctx, &ctx.default_config(), true).unwrap();
        write_request(&ctx, "old-instance", "request");
        assert!(!current.handoff_requested().unwrap());
        write_request(&ctx, &current.token, "");
        assert!(!current.handoff_requested().unwrap());
        drop(current);
        let background = registration(&ctx, &ctx.default_config(), false).unwrap();
        write_request(&ctx, &background.token, "request");
        assert!(!background.handoff_requested().unwrap());
        assert!(!request_handoff(&ctx, Duration::ZERO).unwrap());
    }

    #[test]
    fn cleanup_preserves_replacement_state_and_other_callers_request() {
        let home = tempfile::tempdir().unwrap();
        let ctx = context(home.path());
        let _lock = lock(&ctx.cache().join("watcher.lock")).unwrap();
        let first = registration(&ctx, &ctx.default_config(), true).unwrap();
        let second = registration(&ctx, &ctx.default_config(), true).unwrap();
        write_request(&ctx, &second.token, "other-caller");
        drop(first);
        assert_eq!(
            read_json(&ctx.cache().join("monitor.json"))
                .unwrap()
                .unwrap()["token"],
            second.token
        );
        remove_matching(
            &ctx.cache().join("handoff.json"),
            &second.token,
            Some("first-caller"),
        )
        .unwrap();
        assert!(ctx.cache().join("handoff.json").exists());
    }

    #[test]
    fn stale_or_absent_registration_does_not_create_lock_or_request() {
        let home = tempfile::tempdir().unwrap();
        let ctx = context(home.path());
        assert!(!request_handoff(&ctx, Duration::ZERO).unwrap());
        let _registration = registration(&ctx, &ctx.default_config(), true).unwrap();
        assert!(!request_handoff(&ctx, Duration::ZERO).unwrap());
        assert!(!ctx.cache().join("watcher.lock").exists());
        assert!(!ctx.cache().join("handoff.json").exists());
    }

    #[test]
    fn timeout_and_cancellation_withdraw_their_request() {
        let home = tempfile::tempdir().unwrap();
        let ctx = context(home.path());
        let _lock = lock(&ctx.cache().join("watcher.lock")).unwrap();
        let registration = registration(&ctx, &ctx.default_config(), true).unwrap();
        assert!(
            request_handoff(&ctx, Duration::ZERO)
                .unwrap_err()
                .to_string()
                .contains("Timed out")
        );
        assert!(!registration.handoff_requested().unwrap());
        ctx.cancelled.store(true, Ordering::SeqCst);
        assert!(
            request_handoff(&ctx, Duration::from_secs(2))
                .unwrap_err()
                .to_string()
                .contains("cancelled")
        );
        assert!(!ctx.cache().join("handoff.json").exists());
        assert!(lock_held(&ctx.cache().join("watcher.lock")).unwrap());
    }

    #[test]
    fn handoff_waits_for_drain_and_lock_release() {
        let home = tempfile::tempdir().unwrap();
        let ctx = context(home.path());
        let worker_ctx = ctx.clone();
        let (ready, waiting) = mpsc::channel();
        let worker = thread::spawn(move || {
            let _lock = lock(&worker_ctx.cache().join("watcher.lock")).unwrap();
            let registration =
                registration(&worker_ctx, &worker_ctx.default_config(), true).unwrap();
            ready.send(()).unwrap();
            let start = Instant::now();
            while !registration.handoff_requested().unwrap() {
                assert!(start.elapsed() < Duration::from_secs(3));
                thread::sleep(Duration::from_millis(10));
            }
            thread::sleep(Duration::from_millis(50));
            fs::write(worker_ctx.cache().join("drained"), b"complete").unwrap();
        });
        waiting.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(request_handoff(&ctx, Duration::from_secs(3)).unwrap());
        assert!(ctx.cache().join("drained").exists());
        worker.join().unwrap();
        assert!(!ctx.cache().join("monitor.json").exists());
        assert!(!ctx.cache().join("handoff.json").exists());
        assert!(!lock_held(&ctx.cache().join("watcher.lock")).unwrap());
    }
}
