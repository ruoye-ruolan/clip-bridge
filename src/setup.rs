//! Configure both hosts without replacing a working local configuration on failure.

use crate::common::{self, Context, atomic_write, lock};
use crate::config::{self, Config};
use crate::{remote, service};
use anyhow::{Context as _, Result, anyhow, ensure};
use serde_json::Value;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

pub struct SetupOptions {
    pub host: String,
    pub remote_directory: Option<String>,
    pub start: bool,
    pub integrate_shell: bool,
    pub build_from_source: bool,
    pub assets_dir: Option<PathBuf>,
}

/// Installation writes a normalized example, which is not a chosen destination.
/// An explicit --remote-dir still overrides the remote-home default.
pub fn is_initial_example(config: &Config) -> bool {
    let example: Config = serde_json::from_slice(include_bytes!("config.example.json"))
        .expect("embedded example configuration must be valid");
    config == &example
}

trait RemoteSetup {
    fn destination(&self, ctx: &Context, host: &str, directory: Option<&str>) -> Result<Config>;
    fn deploy(&self, ctx: &Context, pending: &Path, options: &SetupOptions) -> Result<()>;
}

struct NativeRemoteSetup;

impl RemoteSetup for NativeRemoteSetup {
    fn destination(&self, ctx: &Context, host: &str, directory: Option<&str>) -> Result<Config> {
        config::check_destination(ctx, host, directory, true)
    }

    fn deploy(&self, ctx: &Context, pending: &Path, options: &SetupOptions) -> Result<()> {
        let mode = if options.build_from_source {
            remote::SetupMode::BuildFromSource
        } else {
            remote::SetupMode::Prebuilt
        };
        remote::setup_with_options(
            ctx,
            pending,
            options.integrate_shell,
            mode,
            options.assets_dir.as_deref(),
        )?;
        Ok(())
    }
}

fn cancelled(ctx: &Context) -> Result<()> {
    ensure!(!ctx.cancelled.load(Ordering::SeqCst), "setup cancelled");
    Ok(())
}

fn snapshot_service(ctx: &Context) -> Result<service::Snapshot> {
    service::check_legacy(ctx)?;
    let snapshot = service::snapshot(ctx, ctx.source.as_deref(), ctx.installed_root()?.as_deref())?;
    ensure!(
        snapshot.loaded || !service::monitor_running(ctx)?,
        "A foreground ClipBridge monitor is running. Stop it with Ctrl+C before setup."
    );
    Ok(snapshot)
}

fn unchanged_config(path: &Path, expected: Option<&[u8]>) -> Result<()> {
    ensure!(
        !path.is_symlink() && !path.parent().is_some_and(Path::is_symlink),
        "Configuration location changed during setup; the newer location was preserved. Rerun setup."
    );
    ensure!(
        service::read_optional(path)?.as_deref() == expected,
        "Configuration changed during setup; the newer file was preserved. Rerun setup."
    );
    Ok(())
}

fn commit_config(path: &Path, bytes: &[u8]) -> Result<(u64, u64)> {
    atomic_write(path, bytes, 0o600)?;
    let metadata = fs::symlink_metadata(path)?;
    Ok((metadata.dev(), metadata.ino()))
}

fn restore_config(
    path: &Path,
    original: Option<&[u8]>,
    original_mode: u32,
    committed: &[u8],
    identity: (u64, u64),
) -> Result<()> {
    let current = service::read_optional(path)?;
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        current.as_deref() == Some(committed) && (metadata.dev(), metadata.ino()) == identity,
        "Configuration changed during activation; the newer file was preserved. Restore the desired configuration before restarting ClipBridge."
    );
    service::restore_bytes(path, original, original_mode)
}

// Caller holds service.lock. Use the unlocked primitives so configuration is
// restored before any previous daemon is relaunched after an activation failure.
fn activate(
    ctx: &Context,
    path: &Path,
    start: bool,
    previous: &service::Snapshot,
) -> Result<String> {
    if previous.loaded || !start {
        service::stop_unlocked(ctx, false)?;
        service::wait_monitor_stopped(ctx)?;
    }
    cancelled(ctx)?;
    if start {
        let result = service::install_service(ctx, path, &ctx.executable)?;
        cancelled(ctx)?;
        Ok(format!("{result} Login startup is enabled."))
    } else {
        Ok("Background monitoring is stopped; login startup is disabled. Use clipbridge start or clipbridge run when ready.".into())
    }
}

pub fn execute(ctx: &Context, config_path: &Path, options: SetupOptions) -> Result<String> {
    execute_with(ctx, config_path, options, &NativeRemoteSetup)
}

fn execute_with(
    ctx: &Context,
    config_path: &Path,
    options: SetupOptions,
    remote_setup: &impl RemoteSetup,
) -> Result<String> {
    ensure!(ctx.uid != 0, "Run setup as your normal user, without sudo");
    config::validate_host(&options.host)?;
    let expanded = common::expand_path(config_path, &ctx.home);
    ensure!(
        !expanded.is_symlink() && !expanded.parent().is_some_and(Path::is_symlink),
        "configuration file/directory must not be a symlink"
    );
    let path = common::absolute(&expanded)?;
    let _setup_lock = lock(&ctx.cache().join("setup.lock"))
        .context("Another ClipBridge setup is running, or its lock is inaccessible")?;
    cancelled(ctx)?;
    let original = service::read_optional(&path)?;
    let original_mode = if original.is_some() {
        fs::metadata(&path)?.permissions().mode() & 0o777
    } else {
        0o600
    };
    let mut document: Value = match &original {
        Some(bytes) => serde_json::from_slice(bytes)
            .context("Existing configuration is invalid JSON; it was not changed")?,
        None => serde_json::json!({}),
    };
    let previous: Option<Config> = original
        .as_deref()
        .map(serde_json::from_slice)
        .transpose()
        .context("Existing configuration is invalid; it was not changed")?;
    if let Some(previous) = &previous {
        previous
            .validate()
            .context("Existing configuration is invalid; it was not changed")?;
    }
    let prior_service = {
        let _service_lock = service::operation_lock(ctx)?;
        snapshot_service(ctx)?
    };
    let same_host = previous
        .as_ref()
        .filter(|old| old.ssh_host == options.host && !is_initial_example(old));
    let directory = options
        .remote_directory
        .as_deref()
        .or_else(|| same_host.map(|old| old.remote_directory.as_str()));
    let mut chosen = remote_setup.destination(ctx, &options.host, directory)?;
    ensure!(
        chosen.ssh_host == options.host,
        "SSH destination changed during setup"
    );
    if let Some(previous) = same_host {
        chosen.ssh_aliases.clone_from(&previous.ssh_aliases);
    }
    chosen.remote_clipboard = false;
    chosen.validate()?;
    let object = document
        .as_object_mut()
        .context("Configuration must be a JSON object")?;
    object.insert("ssh_host".into(), Value::String(chosen.ssh_host.clone()));
    object.insert(
        "remote_directory".into(),
        Value::String(chosen.remote_directory.clone()),
    );
    if chosen.ssh_aliases.is_empty() {
        object.remove("ssh_aliases");
    } else {
        object.insert(
            "ssh_aliases".into(),
            serde_json::to_value(&chosen.ssh_aliases)?,
        );
    }
    object.insert("remote_clipboard".into(), Value::Bool(false));
    let pending = tempfile::Builder::new()
        .prefix("clipbridge-setup-")
        .tempdir()?;
    fs::set_permissions(pending.path(), fs::Permissions::from_mode(0o700))?;
    let pending_path = pending.path().join("config.json");
    atomic_write(&pending_path, &serde_json::to_vec_pretty(&document)?, 0o600)?;
    cancelled(ctx)?;
    remote_setup.deploy(ctx, &pending_path, &options).context(
        "Remote setup did not finish; the local configuration and service were preserved",
    )?;
    cancelled(ctx)?;
    document["remote_clipboard"] = Value::Bool(true);
    let mut committed = serde_json::to_vec_pretty(&document)?;
    committed.push(b'\n');

    let _service_lock = service::operation_lock(ctx)?;
    let current_service = snapshot_service(ctx)?;
    ensure!(
        current_service.data == prior_service.data
            && current_service.loaded == prior_service.loaded
            && current_service.config == prior_service.config,
        "Background service changed during setup; local configuration was preserved. Rerun setup."
    );
    unchanged_config(&path, original.as_deref())?;
    cancelled(ctx)?;
    let identity = commit_config(&path, &committed)?;
    let service_result = match activate(ctx, &path, options.start, &prior_service) {
        Ok(result) => result,
        Err(error) => {
            if let Err(recovery) = restore_config(
                &path,
                original.as_deref(),
                original_mode,
                &committed,
                identity,
            ) {
                return Err(anyhow!(
                    "Activation failed: {error:#}. Configuration recovery needs attention: {recovery:#}. The previous service was not relaunched."
                ));
            }
            if let Err(recovery) = service::restore(ctx, &prior_service) {
                return Err(anyhow!(
                    "Activation failed: {error:#}. Original configuration was restored, but service recovery needs attention: {recovery:#}"
                ));
            }
            return Err(error.context("Activation failed; the original configuration and service were restored. The remote helper remains installed"));
        }
    };
    let launch = if options.integrate_shell {
        "Open a new SSH shell, then run codex, claude or opencode normally. Existing CLI processes need relaunching."
    } else {
        "Shell integration was skipped. In the SSH terminal use clipbridge-remote run -- APPLICATION."
    };
    Ok(format!(
        "Setup complete for ssh {} (Linux/WSL).\nConfiguration: {}\n{service_result}\n{launch}\nKeep the matching SSH session open, copy a new image, and wait for remote image clipboard ready before pasting.",
        chosen.ssh_host,
        path.display(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{CommandOutput, CommandSpec, Runner};
    use crate::service::test_support::{FakeRunner, Fixture};
    use anyhow::bail;
    use std::cell::{Cell, RefCell};
    use std::sync::{Arc, Mutex, atomic::AtomicBool};

    #[derive(Default)]
    struct Remote {
        prepare_calls: Cell<usize>,
        deploy_calls: Cell<usize>,
        requested_directory: RefCell<Option<String>>,
        pending: RefCell<Option<Value>>,
        fail: bool,
        change_config: Option<(PathBuf, Vec<u8>)>,
        change_service: Option<Arc<FakeRunner>>,
        cancel: bool,
    }

    impl RemoteSetup for Remote {
        fn destination(&self, _: &Context, host: &str, directory: Option<&str>) -> Result<Config> {
            self.prepare_calls.set(self.prepare_calls.get() + 1);
            self.requested_directory
                .replace(directory.map(str::to_owned));
            Config::new(
                host,
                directory.unwrap_or("/home/example/.local/share/clipbridge/images"),
            )
        }

        fn deploy(&self, ctx: &Context, path: &Path, _: &SetupOptions) -> Result<()> {
            self.deploy_calls.set(self.deploy_calls.get() + 1);
            assert_eq!(fs::metadata(path)?.permissions().mode() & 0o777, 0o600);
            assert_eq!(
                fs::metadata(path.parent().unwrap())?.permissions().mode() & 0o777,
                0o700
            );
            self.pending
                .replace(Some(serde_json::from_slice(&fs::read(path)?)?));
            if let Some((path, bytes)) = &self.change_config {
                atomic_write(path, bytes, 0o600)?;
            }
            if let Some(runner) = &self.change_service {
                let definition = service::definition(ctx, &ctx.default_config(), &ctx.executable)?;
                atomic_write(&ctx.plist_path(), &definition, 0o600)?;
                runner.load_definition(&definition, "pid = 77");
            }
            if self.cancel {
                ctx.cancelled.store(true, Ordering::SeqCst);
            }
            if self.fail {
                bail!("remote dependencies are missing");
            }
            Ok(())
        }
    }

    fn options(host: &str, start: bool) -> SetupOptions {
        SetupOptions {
            host: host.into(),
            remote_directory: None,
            start,
            integrate_shell: true,
            build_from_source: false,
            assets_dir: None,
        }
    }

    struct ObservingRunner {
        delegate: Arc<FakeRunner>,
        config: PathBuf,
        bootstrapped_configs: Mutex<Vec<Option<Vec<u8>>>>,
        replace_before_bootstrap: Option<Vec<u8>>,
    }

    impl Runner for ObservingRunner {
        fn run(&self, command: &CommandSpec, cancelled: &AtomicBool) -> Result<CommandOutput> {
            if command.program == Path::new("/bin/launchctl")
                && command.args.first().is_some_and(|arg| arg == "bootstrap")
            {
                if let Some(bytes) = &self.replace_before_bootstrap {
                    atomic_write(&self.config, bytes, 0o600)?;
                }
                self.bootstrapped_configs
                    .lock()
                    .unwrap()
                    .push(service::read_optional(&self.config)?);
            }
            self.delegate.run(command, cancelled)
        }
    }

    #[test]
    fn successful_setup_preserves_unknown_fields_and_same_host_aliases_and_enables_startup() {
        let fixture = Fixture::new();
        let original = br#"{"ssh_host":"dev","remote_directory":"/tmp/custom/","ssh_aliases":["lan","backup"],"remote_clipboard":true,"custom":{"keep":42}}"#;
        atomic_write(&fixture.config, original, 0o600).unwrap();
        let remote = Remote::default();
        let message =
            execute_with(&fixture.ctx, &fixture.config, options("dev", true), &remote).unwrap();
        let saved: Value = serde_json::from_slice(&fs::read(&fixture.config).unwrap()).unwrap();
        assert_eq!(saved["custom"]["keep"], 42);
        assert_eq!(saved["ssh_aliases"], serde_json::json!(["lan", "backup"]));
        assert_eq!(saved["remote_directory"], "/tmp/custom");
        assert_eq!(saved["remote_clipboard"], true);
        assert_eq!(
            remote.pending.borrow().as_ref().unwrap()["remote_clipboard"],
            false
        );
        assert_eq!(
            remote.requested_directory.borrow().as_deref(),
            Some("/tmp/custom/")
        );
        assert_eq!(fixture.runner.action_count("bootstrap"), 1);
        assert!(fixture.ctx.plist_path().is_file());
        assert!(message.contains("Login startup is enabled"));
        assert!(message.contains("codex, claude or opencode"));
        assert!(!message.contains("clipbridge-setup-"));
    }

    #[test]
    fn unchanged_installer_example_uses_remote_home_unless_directory_is_explicit() {
        for explicit in [None, Some("/home/example/intentional")] {
            let fixture = Fixture::new();
            let example: Config =
                serde_json::from_slice(include_bytes!("config.example.json")).unwrap();
            example.save(&fixture.config).unwrap();
            assert!(is_initial_example(&Config::load(&fixture.config).unwrap()));
            let remote = Remote::default();
            let mut selected = options("dev-server", false);
            selected.remote_directory = explicit.map(str::to_owned);
            execute_with(&fixture.ctx, &fixture.config, selected, &remote).unwrap();
            assert_eq!(remote.requested_directory.borrow().as_deref(), explicit);
        }
        let mut custom = Config::new("dev-server", "/srv/images").unwrap();
        assert!(!is_initial_example(&custom));
        custom.remote_directory = "/home/example/.local/share/clipbridge/images".into();
        custom.remote_clipboard = true;
        assert!(!is_initial_example(&custom));
    }

    #[test]
    fn changing_host_clears_old_aliases_uses_remote_home_and_honors_no_start() {
        let fixture = Fixture::new();
        atomic_write(&fixture.config, br#"{"ssh_host":"dev","remote_directory":"/tmp/custom","ssh_aliases":["lan"],"keep":"yes"}"#, 0o600).unwrap();
        fixture.load_service();
        let remote = Remote::default();
        let mut selected = options("server", false);
        selected.integrate_shell = false;
        let message = execute_with(&fixture.ctx, &fixture.config, selected, &remote).unwrap();
        let saved: Value = serde_json::from_slice(&fs::read(&fixture.config).unwrap()).unwrap();
        assert_eq!(saved["ssh_host"], "server");
        assert_eq!(
            saved["remote_directory"],
            "/home/example/.local/share/clipbridge/images"
        );
        assert!(saved.get("ssh_aliases").is_none());
        assert_eq!(saved["keep"], "yes");
        assert!(remote.requested_directory.borrow().is_none());
        assert_eq!(fixture.runner.action_count("bootout"), 1);
        assert_eq!(fixture.runner.action_count("bootstrap"), 0);
        assert!(!fixture.ctx.plist_path().exists());
        assert!(message.contains("login startup is disabled"));
        assert!(message.contains("Shell integration was skipped"));
    }

    #[test]
    fn failed_remote_setup_preserves_configuration_and_running_service() {
        let fixture = Fixture::new();
        fixture.load_service();
        let original = fs::read(&fixture.config).unwrap();
        let plist = fs::read(fixture.ctx.plist_path()).unwrap();
        let remote = Remote {
            fail: true,
            ..Default::default()
        };
        let error = execute_with(
            &fixture.ctx,
            &fixture.config,
            options("new-host", true),
            &remote,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("remote dependencies are missing"));
        assert_eq!(fs::read(&fixture.config).unwrap(), original);
        assert_eq!(fs::read(fixture.ctx.plist_path()).unwrap(), plist);
        assert_eq!(fixture.runner.action_count("bootout"), 0);
        assert_eq!(fixture.runner.action_count("bootstrap"), 0);
    }

    #[test]
    fn configuration_changes_during_deployment_are_preserved_without_activation() {
        let fixture = Fixture::new();
        let replacement = br#"{"ssh_host":"personal","remote_directory":"/tmp/private"}"#.to_vec();
        let remote = Remote {
            change_config: Some((fixture.config.clone(), replacement.clone())),
            ..Default::default()
        };
        let error =
            execute_with(&fixture.ctx, &fixture.config, options("dev", true), &remote).unwrap_err();
        assert!(format!("{error:#}").contains("Configuration changed during setup"));
        assert_eq!(fs::read(&fixture.config).unwrap(), replacement);
        assert_eq!(fixture.runner.action_count("bootstrap"), 0);
    }

    #[test]
    fn startup_failure_restores_config_before_restarting_the_previous_service() {
        let fixture = Fixture::new();
        fixture.load_service();
        fs::set_permissions(&fixture.config, fs::Permissions::from_mode(0o640)).unwrap();
        let original = fs::read(&fixture.config).unwrap();
        let observer = Arc::new(ObservingRunner {
            delegate: fixture.runner.clone(),
            config: fixture.config.clone(),
            bootstrapped_configs: Mutex::new(Vec::new()),
            replace_before_bootstrap: None,
        });
        let mut ctx = fixture.ctx.clone();
        ctx.runner = observer.clone();
        fixture.runner.fail_once("bootstrap", "activation denied");
        let error = execute_with(
            &ctx,
            &fixture.config,
            options("new-host", true),
            &Remote::default(),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("original configuration and service were restored"));
        assert_eq!(fs::read(&fixture.config).unwrap(), original);
        assert_eq!(
            fs::metadata(&fixture.config).unwrap().permissions().mode() & 0o777,
            0o640
        );
        let snapshots = observer.bootstrapped_configs.lock().unwrap();
        assert_eq!(snapshots.len(), 2);
        assert_ne!(snapshots[0].as_deref(), Some(original.as_slice()));
        assert_eq!(snapshots[1].as_deref(), Some(original.as_slice()));
        assert!(
            fixture
                .runner
                .state
                .lock()
                .unwrap()
                .jobs
                .contains_key(service::LABEL)
        );
    }

    #[test]
    fn failed_first_activation_removes_only_the_configuration_created_by_setup() {
        let fixture = Fixture::new();
        fs::remove_file(&fixture.config).unwrap();
        fixture.runner.fail_once("bootstrap", "activation denied");
        assert!(
            execute_with(
                &fixture.ctx,
                &fixture.config,
                options("dev", true),
                &Remote::default()
            )
            .is_err()
        );
        assert!(!fixture.config.exists());
        assert!(!fixture.ctx.plist_path().exists());

        let replacement = br#"{"ssh_host":"edited","remote_directory":"/tmp/keep"}"#.to_vec();
        let observer = Arc::new(ObservingRunner {
            delegate: fixture.runner.clone(),
            config: fixture.config.clone(),
            bootstrapped_configs: Mutex::new(Vec::new()),
            replace_before_bootstrap: Some(replacement.clone()),
        });
        let mut ctx = fixture.ctx.clone();
        ctx.runner = observer;
        fixture.runner.fail_once("bootstrap", "activation denied");
        let error = execute_with(
            &ctx,
            &fixture.config,
            options("dev", true),
            &Remote::default(),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("newer file was preserved"));
        assert_eq!(fs::read(&fixture.config).unwrap(), replacement);
    }

    #[test]
    fn invalid_config_foreground_monitor_and_foreign_services_fail_before_remote_changes() {
        let fixture = Fixture::new();
        let valid = fs::read(&fixture.config).unwrap();
        for invalid in [
            b"{broken".as_slice(),
            br#"{"ssh_host":"bad;host","remote_directory":"/tmp/images"}"#,
        ] {
            atomic_write(&fixture.config, invalid, 0o600).unwrap();
            let remote = Remote::default();
            assert!(
                execute_with(&fixture.ctx, &fixture.config, options("dev", true), &remote).is_err()
            );
            assert_eq!(fs::read(&fixture.config).unwrap(), invalid);
            assert_eq!(remote.prepare_calls.get(), 0);
        }
        atomic_write(&fixture.config, &valid, 0o600).unwrap();
        let watcher = lock(&fixture.ctx.cache().join("watcher.lock")).unwrap();
        let remote = Remote::default();
        let error = execute_with(
            &fixture.ctx,
            &fixture.config,
            options("dev", false),
            &remote,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("foreground"));
        assert_eq!(remote.prepare_calls.get(), 0);
        drop(watcher);
        let foreign = fixture.ctx.home.join("other/clipbridge");
        atomic_write(&foreign, b"other program", 0o700).unwrap();
        let plist = service::definition(&fixture.ctx, &fixture.config, &foreign).unwrap();
        atomic_write(&fixture.ctx.plist_path(), &plist, 0o600).unwrap();
        fixture.runner.load_definition(&plist, "pid = 99");
        assert!(
            execute_with(&fixture.ctx, &fixture.config, options("dev", true), &remote).is_err()
        );
        assert_eq!(remote.prepare_calls.get(), 0);
        assert_eq!(fixture.runner.action_count("bootout"), 0);
    }

    #[test]
    fn cancellation_after_remote_deployment_leaves_local_state_unchanged() {
        let fixture = Fixture::new();
        let original = fs::read(&fixture.config).unwrap();
        let remote = Remote {
            cancel: true,
            ..Default::default()
        };
        let error = execute_with(
            &fixture.ctx,
            &fixture.config,
            options("new-host", true),
            &remote,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("setup cancelled"));
        assert_eq!(fs::read(&fixture.config).unwrap(), original);
        assert_eq!(fixture.runner.action_count("bootstrap"), 0);
    }

    #[test]
    fn service_changes_during_deployment_preserve_the_new_service_and_original_config() {
        let fixture = Fixture::new();
        let original = fs::read(&fixture.config).unwrap();
        let remote = Remote {
            change_service: Some(fixture.runner.clone()),
            ..Default::default()
        };
        let error = execute_with(
            &fixture.ctx,
            &fixture.config,
            options("dev", false),
            &remote,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("Background service changed during setup"));
        assert_eq!(fs::read(&fixture.config).unwrap(), original);
        assert!(
            fixture
                .runner
                .state
                .lock()
                .unwrap()
                .jobs
                .contains_key(service::LABEL)
        );
        assert_eq!(fixture.runner.action_count("bootout"), 0);
    }

    #[test]
    fn failed_no_start_restores_the_previous_config_and_login_service() {
        let fixture = Fixture::new();
        fixture.load_service();
        let original = fs::read(&fixture.config).unwrap();
        let plist = fs::read(fixture.ctx.plist_path()).unwrap();
        fixture.runner.fail_once("bootout", "stop denied");
        let error = execute_with(
            &fixture.ctx,
            &fixture.config,
            options("new-host", false),
            &Remote::default(),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("stop denied"));
        assert_eq!(fs::read(&fixture.config).unwrap(), original);
        assert_eq!(fs::read(fixture.ctx.plist_path()).unwrap(), plist);
        assert!(
            fixture
                .runner
                .state
                .lock()
                .unwrap()
                .jobs
                .contains_key(service::LABEL)
        );
    }
}
