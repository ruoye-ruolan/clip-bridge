//! Per-user launchd lifecycle. All system commands pass through the injected runner.
use crate::common::{CommandSpec, Context, absolute, atomic_write, lock, lock_held};
use crate::config::Config;
use anyhow::{Context as _, Result, anyhow, bail};
use plist::{Dictionary, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

pub const LABEL: &str = "local.clipbridge";
const LEGACY: &[&str] = &["local.codex.xnip-wsl", "local.clipbridge.prototype"];
const LAUNCHCTL: &str = "/bin/launchctl";

fn command(ctx: &Context, args: &[&str], cleanup: bool) -> Result<crate::common::CommandOutput> {
    let mut spec = CommandSpec::new(LAUNCHCTL)
        .args(args)
        .timeout(20)
        .unchecked();
    if cleanup {
        spec = spec.uncancellable();
    }
    ctx.exec(spec)
}

fn domain(ctx: &Context) -> String {
    format!("gui/{}", ctx.uid)
}

fn target(ctx: &Context, label: &str) -> String {
    format!("{}/{label}", domain(ctx))
}

fn details(output: &crate::common::CommandOutput) -> String {
    if !output.stderr.trim().is_empty() {
        output.stderr.trim().to_owned()
    } else if !output.stdout.trim().is_empty() {
        output.stdout.trim().to_owned()
    } else {
        format!("exit code {}", output.code)
    }
}

fn info_for(ctx: &Context, label: &str, cleanup: bool) -> Result<Option<String>> {
    let output = command(ctx, &["print", &target(ctx, label)], cleanup)?;
    if output.code == 0 {
        return Ok(Some(output.stdout));
    }
    let message = output.stderr.to_lowercase();
    if output.code == 113
        && (message.contains("could not find service")
            || message.contains("could not find specified service")
            || message
                .lines()
                .any(|line| line.contains("service") && line.contains("not found")))
    {
        return Ok(None);
    }
    bail!("Could not inspect {label}: {}", details(&output))
}

pub(crate) fn loaded_info(ctx: &Context) -> Result<Option<String>> {
    info_for(ctx, LABEL, false)
}

pub fn legacy_services(ctx: &Context) -> Result<Vec<String>> {
    let mut found = Vec::new();
    for label in LEGACY {
        if info_for(ctx, label, false)?.is_some() {
            found.push((*label).to_owned());
        }
    }
    Ok(found)
}

pub(crate) fn check_legacy(ctx: &Context) -> Result<()> {
    let found = legacy_services(ctx)?;
    if !found.is_empty() {
        bail!(
            "An older uploader is loaded: {}. Stop it manually before starting ClipBridge.",
            found.join(", ")
        );
    }
    Ok(())
}

pub fn operation_lock(ctx: &Context) -> Result<crate::common::FileLock> {
    lock(&ctx.cache().join("service.lock")).context(
        "Another ClipBridge service command is in progress, or its lock is inaccessible. Try again shortly",
    )
}

pub(crate) fn monitor_running(ctx: &Context) -> Result<bool> {
    lock_held(&ctx.cache().join("watcher.lock"))
}

fn check_monitor_lock(ctx: &Context) -> Result<()> {
    if monitor_running(ctx)? {
        bail!("Another ClipBridge monitor is running. Stop it first.");
    }
    Ok(())
}

pub fn check_foreground_available(ctx: &Context) -> Result<()> {
    if loaded_info(ctx)?.is_some() {
        bail!("ClipBridge is already loaded. Run clipbridge stop before using run.");
    }
    check_legacy(ctx)?;
    check_monitor_lock(ctx)
}

pub(crate) fn pid(info: &str) -> Option<u32> {
    info.lines().find_map(|line| {
        line.trim()
            .strip_prefix("pid = ")
            .and_then(|value| value.parse::<u32>().ok())
    })
}

fn description(ctx: &Context, info: &str) -> String {
    if let Some(pid) = pid(info) {
        return format!("ClipBridge is running (PID {pid}).");
    }
    let last = info
        .lines()
        .find_map(|line| line.trim().strip_prefix("last exit code = "));
    let detail = last
        .map(|value| format!(" Last exit code: {value}."))
        .unwrap_or_default();
    format!(
        "ClipBridge is loaded but not running.{detail} Check {}.",
        ctx.log_path().display()
    )
}

fn validate_config(path: &Path) -> Result<PathBuf> {
    let path = path
        .canonicalize()
        .context("Cannot read selected configuration")?;
    Config::load(&path)?;
    Ok(path)
}

pub(crate) fn definition(ctx: &Context, config: &Path, executable: &Path) -> Result<Vec<u8>> {
    let executable = absolute(executable)?;
    let mut plist = Dictionary::new();
    plist.insert("Label".into(), LABEL.into());
    plist.insert(
        "ProgramArguments".into(),
        Value::Array(vec![
            executable.to_string_lossy().into_owned().into(),
            "--config".into(),
            config.to_string_lossy().into_owned().into(),
            "monitor".into(),
        ]),
    );
    let directory = executable.parent().context("Executable has no directory")?;
    plist.insert(
        "WorkingDirectory".into(),
        directory.to_string_lossy().into_owned().into(),
    );
    plist.insert("RunAtLoad".into(), true.into());
    plist.insert("KeepAlive".into(), true.into());
    plist.insert("ProcessType".into(), "Background".into());
    plist.insert("ThrottleInterval".into(), 10u64.into());
    plist.insert("Umask".into(), 0o077u64.into());
    for key in ["StandardOutPath", "StandardErrorPath"] {
        plist.insert(
            key.into(),
            ctx.log_path().to_string_lossy().into_owned().into(),
        );
    }
    let mut bytes = Vec::new();
    Value::Dictionary(plist).to_writer_xml(&mut bytes)?;
    Ok(bytes)
}

pub(crate) fn remove_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            bail!("Refusing non-regular managed file: {}", path.display())
        }
        Ok(_) => Ok(Some(fs::read(path)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn restore_bytes(path: &Path, data: Option<&[u8]>, mode: u32) -> Result<()> {
    if let Some(data) = data {
        atomic_write(path, data, mode)
    } else {
        remove_file(path)
    }
}

fn plist_arguments(data: &[u8]) -> Result<Vec<String>> {
    let value = Value::from_reader(std::io::Cursor::new(data))?;
    let plist = value
        .as_dictionary()
        .context("Service plist must be a dictionary")?;
    if plist.get("Label").and_then(Value::as_string) != Some(LABEL) {
        bail!("Service plist belongs to another label");
    }
    plist
        .get("ProgramArguments")
        .and_then(Value::as_array)
        .context("Service has no ProgramArguments")?
        .iter()
        .map(|arg| {
            arg.as_string()
                .map(str::to_owned)
                .context("Invalid service argument")
        })
        .collect()
}

fn verify_loaded_definition(info: &str, expected: &[String]) -> Result<()> {
    let mut program = None;
    let mut arguments = None;
    let mut lines = info.lines();
    while let Some(line) = lines.next() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("program = ") {
            if program.replace(value).is_some() {
                bail!("Cannot verify loaded service: duplicate program field");
            }
        } else if line == "arguments = {" {
            if arguments.is_some() {
                bail!("Cannot verify loaded service: duplicate arguments block");
            }
            let mut values = Vec::new();
            let mut closed = false;
            for argument in lines.by_ref() {
                let argument = argument.trim();
                if argument == "}" {
                    closed = true;
                    break;
                }
                values.push(argument.to_owned());
            }
            if !closed {
                bail!("Cannot verify loaded service: incomplete arguments block");
            }
            arguments = Some(values);
        }
    }
    if program != expected.first().map(String::as_str) || arguments.as_deref() != Some(expected) {
        bail!(
            "Loaded service program or arguments do not match its login plist; refusing to change an unverified service. Restore the matching plist or stop it through its original installation."
        );
    }
    Ok(())
}

fn config_from_args(args: &[String]) -> Result<PathBuf> {
    let index = args
        .iter()
        .position(|arg| arg == "--config")
        .context("Service config missing")?;
    let path = Path::new(args.get(index + 1).context("Service config path missing")?);
    if !path.is_absolute() {
        bail!("Service config path is not absolute");
    }
    absolute(path)
}

/// Recognize exact supported command shapes, including the Python 0.1 service.
fn service_program(args: &[String]) -> Result<PathBuf> {
    let program = if args.len() == 4 && args[1] == "--config" && args[3] == "monitor" {
        Path::new(&args[0])
    } else if args.len() == 5 && args[1] == "-u" && args[3] == "--config" {
        let script = Path::new(&args[2]);
        if script.file_name().and_then(|s| s.to_str()) != Some("auto_upload.py") {
            bail!("Unrecognized legacy service program");
        }
        script
    } else {
        bail!("Unrecognized service command");
    };
    if !program.is_absolute() {
        bail!("Service program is not absolute");
    }
    absolute(program)
}

fn inside_release(program: &Path, root: &Path) -> bool {
    let Ok(relative) = program.strip_prefix(root) else {
        return false;
    };
    let parts: Vec<_> = relative.components().collect();
    let plain: Vec<_> = parts
        .iter()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect();
    (plain.len() == 3 && plain[0] == "releases" && plain[2] == "clipbridge")
        || (plain.len() == 4
            && plain[0] == "releases"
            && plain[2] == "src"
            && plain[3] == "auto_upload.py")
        || (plain.len() == 2 && plain[0] == "current" && plain[1] == "clipbridge")
        || (plain.len() == 3
            && plain[0] == "current"
            && plain[1] == "src"
            && plain[2] == "auto_upload.py")
}

fn check_owned(
    ctx: &Context,
    args: &[String],
    source: Option<&Path>,
    root: Option<&Path>,
) -> Result<()> {
    let program = service_program(args)?;
    // A root-only snapshot belongs to the requested installation (not necessarily
    // the calling executable). In particular, uninstall --prefix B from A must
    // never accept and stop A's service merely because it is the caller.
    let mut owned = (source.is_some() || root.is_none()) && program == absolute(&ctx.executable)?;
    if let Some(source) = source {
        let source = absolute(source)?;
        owned |= program == absolute(&source.join("src/auto_upload.py"))?;
        for profile in ["debug", "release"] {
            owned |= program == absolute(&source.join("target").join(profile).join("clipbridge"))?;
        }
    }
    if let Some(root) = root {
        owned |= inside_release(&program, &absolute(root)?);
    }
    if !owned {
        bail!(
            "Service is not owned by this installation/source; stop it from its original installation before continuing"
        );
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct Snapshot {
    pub data: Option<Vec<u8>>,
    pub loaded: bool,
    pub config: Option<PathBuf>,
}

pub(crate) fn snapshot(
    ctx: &Context,
    source: Option<&Path>,
    root: Option<&Path>,
) -> Result<Snapshot> {
    let info = loaded_info(ctx)?;
    let loaded = info.is_some();
    let data = read_optional(&ctx.plist_path())?;
    if loaded && data.is_none() {
        bail!("Loaded service has no readable plist; stop it before continuing");
    }
    let config = if let Some(bytes) = &data {
        let args = plist_arguments(bytes).context("Cannot verify the service plist")?;
        check_owned(ctx, &args, source, root)?;
        if let Some(info) = &info {
            verify_loaded_definition(info, &args)?;
        }
        let config = config_from_args(&args)?;
        Some(config)
    } else {
        None
    };
    Ok(Snapshot {
        data,
        loaded,
        config,
    })
}

fn context_snapshot(ctx: &Context) -> Result<Snapshot> {
    snapshot(ctx, ctx.source.as_deref(), ctx.installed_root()?.as_deref())
}

fn unload(ctx: &Context, cleanup: bool) -> Result<()> {
    if info_for(ctx, LABEL, cleanup)?.is_some() {
        let output = command(ctx, &["bootout", &target(ctx, LABEL)], cleanup)?;
        if info_for(ctx, LABEL, cleanup)?.is_some() {
            bail!("Could not stop ClipBridge: {}", details(&output));
        }
    }
    Ok(())
}

pub(crate) fn stop_unlocked(ctx: &Context, cleanup: bool) -> Result<()> {
    unload(ctx, cleanup)?;
    remove_file(&ctx.plist_path())
}

pub(crate) fn restore(ctx: &Context, snapshot: &Snapshot) -> Result<()> {
    unload(ctx, true)?;
    restore_bytes(&ctx.plist_path(), snapshot.data.as_deref(), 0o600)?;
    if snapshot.loaded {
        let path = ctx.plist_path();
        let output = command(
            ctx,
            &["bootstrap", &domain(ctx), &path.to_string_lossy()],
            true,
        )?;
        if output.code != 0 || info_for(ctx, LABEL, true)?.is_none() {
            bail!(
                "Could not restore previous background service: {}",
                details(&output)
            );
        }
    }
    Ok(())
}

pub(crate) fn install_service(ctx: &Context, config: &Path, executable: &Path) -> Result<String> {
    let previous = read_optional(&ctx.plist_path())?;
    fs::create_dir_all(ctx.log_path().parent().context("Log path has no parent")?)?;
    let bytes = definition(ctx, config, executable)?;
    let result = (|| {
        atomic_write(&ctx.plist_path(), &bytes, 0o600)?;
        let path = ctx.plist_path();
        let output = command(
            ctx,
            &["bootstrap", &domain(ctx), &path.to_string_lossy()],
            false,
        )?;
        if output.code != 0 {
            bail!("Could not start ClipBridge: {}", details(&output));
        }
        let info =
            loaded_info(ctx)?.context("launchd accepted the plist but ClipBridge is not loaded")?;
        verify_loaded_definition(&info, &plist_arguments(&bytes)?)?;
        Ok(description(ctx, &info))
    })();
    if let Err(error) = result {
        let mut cleanup_errors = Vec::new();
        if let Err(cleanup) = unload(ctx, true) {
            cleanup_errors.push(cleanup.to_string());
        }
        if let Err(cleanup) = restore_bytes(&ctx.plist_path(), previous.as_deref(), 0o600) {
            cleanup_errors.push(cleanup.to_string());
        }
        if !cleanup_errors.is_empty() {
            bail!(
                "{error:#}. Startup cleanup also failed: {}. Inspect clipbridge status.",
                cleanup_errors.join("; ")
            );
        }
        return Err(error);
    }
    result
}

fn start_unlocked(ctx: &Context, config: &Path) -> Result<String> {
    if let Some(info) = loaded_info(ctx)? {
        let snapshot = context_snapshot(ctx)?;
        if snapshot.config.as_deref() != Some(config) {
            bail!(
                "ClipBridge is already loaded with a different configuration. Run clipbridge restart with the selected --config."
            );
        }
        if pid(&info).is_some() {
            return Ok(format!(
                "{} Use clipbridge restart to reload changed settings.",
                description(ctx, &info)
            ));
        }
        let output = command(ctx, &["kickstart", &target(ctx, LABEL)], false)?;
        if output.code != 0 {
            bail!("Could not resume ClipBridge: {}", details(&output));
        }
        return Ok(description(
            ctx,
            &loaded_info(ctx)?
                .context("ClipBridge is no longer loaded. Run clipbridge start again")?,
        ));
    }
    check_legacy(ctx)?;
    // Refuse to replace another checkout's login configuration, even if unloaded.
    context_snapshot(ctx)?;
    if monitor_running(ctx)?
        && !crate::control::request_handoff(ctx, Duration::from_secs(180))?
        && monitor_running(ctx)?
    {
        bail!(
            "An older or unmanaged ClipBridge monitor is running. Press Ctrl+C once in its terminal, then run clipbridge start again."
        );
    }
    check_monitor_lock(ctx)?;
    install_service(ctx, config, &ctx.executable)
}

pub fn start(ctx: &Context, config: &Path) -> Result<String> {
    let config = validate_config(config)?;
    let _lock = operation_lock(ctx)?;
    start_unlocked(ctx, &config)
}

pub fn stop(ctx: &Context) -> Result<String> {
    let _lock = operation_lock(ctx)?;
    context_snapshot(ctx)?;
    stop_unlocked(ctx, false)?;
    Ok("ClipBridge background service is stopped; automatic startup is disabled. Foreground monitors, if any, are unaffected.".into())
}

pub(crate) fn wait_monitor_stopped(ctx: &Context) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while monitor_running(ctx)? {
        if ctx.cancelled.load(Ordering::Relaxed) {
            bail!("Operation cancelled");
        }
        if Instant::now() >= deadline {
            bail!("Previous monitor still holds its lock; retry after it finishes stopping");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

pub fn restart(ctx: &Context, config: &Path) -> Result<String> {
    let config = validate_config(config)?;
    let _lock = operation_lock(ctx)?;
    check_legacy(ctx)?;
    let old = context_snapshot(ctx)?;
    if !old.loaded {
        return start_unlocked(ctx, &config);
    }
    let result = (|| {
        stop_unlocked(ctx, false)?;
        wait_monitor_stopped(ctx)?;
        install_service(ctx, &config, &ctx.executable)
    })();
    result.or_else(|error| {
        restore(ctx, &old)
            .map_err(|cleanup| anyhow!("{error:#}. Restart recovery failed: {cleanup:#}"))?;
        Err(error)
    })
}

pub fn status(ctx: &Context) -> Result<String> {
    if let Some(info) = loaded_info(ctx)? {
        return Ok(description(ctx, &info));
    }
    if monitor_running(ctx)? {
        return Ok("A foreground or unmanaged ClipBridge monitor is running. No ClipBridge background service is loaded.".into());
    }
    if ctx.plist_path().exists() {
        return Ok(format!(
            "ClipBridge is stopped. A login plist remains at {}; run clipbridge stop to remove it.",
            ctx.plist_path().display()
        ));
    }
    Ok("ClipBridge is stopped.".into())
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::common::{CommandOutput, Runner};
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::{Arc, Mutex, atomic::AtomicBool};

    #[derive(Default)]
    pub struct FakeState {
        pub jobs: BTreeMap<String, String>,
        pub definitions: BTreeMap<String, Vec<String>>,
        pub calls: Vec<Vec<String>>,
        pub failures: BTreeMap<String, VecDeque<&'static str>>,
        pub cancel_bootstrap: bool,
        pub fail_readiness_once: bool,
        pub prints_until_failure: Option<u8>,
        pub ready: bool,
        pub watcher: Option<crate::common::FileLock>,
    }

    pub struct FakeRunner {
        pub home: PathBuf,
        pub state: Mutex<FakeState>,
    }

    impl FakeRunner {
        pub fn load_definition(&self, plist: &[u8], status: &str) {
            let mut state = self.state.lock().unwrap();
            state.jobs.insert(LABEL.into(), status.into());
            state
                .definitions
                .insert(LABEL.into(), plist_arguments(plist).unwrap());
        }
        pub fn fail_once(&self, command: &str, message: &'static str) {
            self.state
                .lock()
                .unwrap()
                .failures
                .entry(command.into())
                .or_default()
                .push_back(message);
        }
        pub fn action_count(&self, command: &str) -> usize {
            self.state
                .lock()
                .unwrap()
                .calls
                .iter()
                .filter(|args| args.first().is_some_and(|s| s == command))
                .count()
        }
    }

    impl Runner for FakeRunner {
        fn run(&self, spec: &CommandSpec, cancelled: &AtomicBool) -> Result<CommandOutput> {
            let args: Vec<String> = spec
                .args
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect();
            if spec.program != Path::new(LAUNCHCTL) {
                if args == ["--version"] {
                    return Ok(CommandOutput {
                        stdout: format!("clipbridge {}\n", crate::common::VERSION),
                        ..Default::default()
                    });
                }
                assert_eq!(args, ["--help"]);
                return Ok(CommandOutput {
                    stdout: "ClipBridge help".into(),
                    ..Default::default()
                });
            }
            let mut state = self.state.lock().unwrap();
            state.calls.push(args.clone());
            if let Some(reason) = state
                .failures
                .get_mut(&args[0])
                .and_then(VecDeque::pop_front)
            {
                return Ok(CommandOutput {
                    code: 1,
                    stderr: reason.into(),
                    ..Default::default()
                });
            }
            match args[0].as_str() {
                "print" => {
                    if let Some(remaining) = state.prints_until_failure.as_mut() {
                        *remaining -= 1;
                        if *remaining == 0 {
                            state.prints_until_failure = None;
                            return Ok(CommandOutput {
                                code: 1,
                                stderr: "readiness inspection denied".into(),
                                ..Default::default()
                            });
                        }
                    }
                    let label = args[1].rsplit('/').next().unwrap();
                    match state.jobs.get(label) {
                        Some(info) => Ok(CommandOutput {
                            stdout: state
                                .definitions
                                .get(label)
                                .map(|args| {
                                    format!(
                                        "{info}\n program = {}\n arguments = {{\n\t{}\n }}\n",
                                        args[0],
                                        args.join("\n\t"),
                                    )
                                })
                                .unwrap_or_else(|| info.clone()),
                            ..Default::default()
                        }),
                        None => Ok(CommandOutput {
                            code: 113,
                            stderr: format!("Could not find service {label}"),
                            ..Default::default()
                        }),
                    }
                }
                "bootstrap" => {
                    if state.fail_readiness_once {
                        state.fail_readiness_once = false;
                        state.prints_until_failure = Some(2);
                    }
                    state
                        .jobs
                        .insert(LABEL.into(), "state = running\n pid = 123\n".into());
                    let bytes = fs::read(&args[2])?;
                    state
                        .definitions
                        .insert(LABEL.into(), plist_arguments(&bytes)?);
                    if state.ready {
                        let selected = config_from_args(&plist_arguments(&bytes)?)?;
                        let cache = self.home.join("Library/Caches/clipbridge/auto");
                        state.watcher = Some(lock(&cache.join("watcher.lock"))?);
                        atomic_write(
                            &cache.join("monitor.json"),
                            &serde_json::to_vec(&serde_json::json!({
                                "pid":123, "config":selected, "version":1, "mode":"background", "token":"fake",
                            }))?,
                            0o600,
                        )?;
                    }
                    if state.cancel_bootstrap {
                        state.cancel_bootstrap = false;
                        cancelled.store(true, Ordering::SeqCst);
                        bail!("operation cancelled");
                    }
                    Ok(CommandOutput::default())
                }
                "bootout" => {
                    state.jobs.remove(LABEL);
                    state.definitions.remove(LABEL);
                    state.watcher = None;
                    Ok(CommandOutput::default())
                }
                "kickstart" => {
                    state.jobs.insert(LABEL.into(), "pid = 456\n".into());
                    Ok(CommandOutput::default())
                }
                _ => panic!("Unexpected fake command: {args:?}"),
            }
        }
    }

    pub struct Fixture {
        pub _temporary: tempfile::TempDir,
        pub ctx: Context,
        pub runner: Arc<FakeRunner>,
        pub source: PathBuf,
        pub config: PathBuf,
    }

    impl Fixture {
        pub fn new() -> Self {
            let temporary = tempfile::Builder::new()
                .prefix("clipbridge Rust & ")
                .tempdir()
                .unwrap();
            let home = temporary.path().canonicalize().unwrap();
            let source = home.join("source");
            let executable = source.join("target/release/clipbridge");
            atomic_write(&executable, b"fake executable", 0o755).unwrap();
            let runner = Arc::new(FakeRunner {
                home: home.clone(),
                state: Mutex::new(FakeState::default()),
            });
            let ctx = Context {
                home,
                executable,
                source: Some(source.clone()),
                cancelled: Arc::new(AtomicBool::new(false)),
                runner: runner.clone(),
                uid: 501,
            };
            let config = ctx.default_config();
            atomic_write(
                &config,
                br#"{"ssh_host":"dev","remote_directory":"/tmp/images"}"#,
                0o600,
            )
            .unwrap();
            Self {
                _temporary: temporary,
                ctx,
                runner,
                source,
                config,
            }
        }
        pub fn load_service(&self) {
            let data = definition(&self.ctx, &self.config, &self.ctx.executable).unwrap();
            atomic_write(&self.ctx.plist_path(), &data, 0o600).unwrap();
            self.runner.load_definition(&data, "pid = 52\n");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::Fixture;
    use super::*;

    #[test]
    fn start_registers_single_rust_binary_and_repeats_without_restarting() {
        let fixture = Fixture::new();
        assert!(
            start(&fixture.ctx, &fixture.config)
                .unwrap()
                .contains("PID 123")
        );
        let bytes = fs::read(fixture.ctx.plist_path()).unwrap();
        let args = plist_arguments(&bytes).unwrap();
        assert_eq!(
            args,
            [
                fixture.ctx.executable.to_string_lossy(),
                "--config".into(),
                fixture.config.to_string_lossy(),
                "monitor".into()
            ]
        );
        assert!(
            start(&fixture.ctx, &fixture.config)
                .unwrap()
                .contains("restart to reload")
        );
        assert_eq!(fixture.runner.action_count("bootstrap"), 1);
        assert_eq!(fixture.runner.action_count("bootout"), 0);
        assert_eq!(bytes, fs::read(fixture.ctx.plist_path()).unwrap());
    }

    #[test]
    fn invalid_configuration_preserves_running_service() {
        let fixture = Fixture::new();
        fixture.load_service();
        fs::write(&fixture.config, "{}").unwrap();
        assert!(restart(&fixture.ctx, &fixture.config).is_err());
        assert!(
            fixture
                .runner
                .state
                .lock()
                .unwrap()
                .jobs
                .contains_key(LABEL)
        );
        assert_eq!(fixture.runner.action_count("bootout"), 0);
    }

    #[test]
    fn different_configuration_requires_restart() {
        let fixture = Fixture::new();
        fixture.load_service();
        let other = fixture.source.join("other.json");
        fs::copy(&fixture.config, &other).unwrap();
        assert!(
            start(&fixture.ctx, &other)
                .unwrap_err()
                .to_string()
                .contains("different configuration")
        );
        assert_eq!(fixture.runner.action_count("bootout"), 0);
    }

    #[test]
    fn waiting_service_resumes_without_killing_it() {
        let fixture = Fixture::new();
        fixture.load_service();
        fixture
            .runner
            .state
            .lock()
            .unwrap()
            .jobs
            .insert(LABEL.into(), "state = waiting\n".into());
        assert!(
            start(&fixture.ctx, &fixture.config)
                .unwrap()
                .contains("PID 456")
        );
        assert_eq!(fixture.runner.action_count("kickstart"), 1);
        assert_eq!(fixture.runner.action_count("bootout"), 0);
    }

    #[test]
    fn legacy_uploader_is_untouched() {
        let fixture = Fixture::new();
        fixture
            .runner
            .state
            .lock()
            .unwrap()
            .jobs
            .insert(LEGACY[0].into(), "pid = 4\n".into());
        assert!(
            start(&fixture.ctx, &fixture.config)
                .unwrap_err()
                .to_string()
                .contains("Stop it manually")
        );
        assert_eq!(fixture.runner.action_count("bootstrap"), 0);
    }

    #[test]
    fn startup_failure_and_cancellation_restore_previous_login_plist() {
        for cancel in [false, true] {
            let fixture = Fixture::new();
            fixture.load_service();
            fixture.runner.state.lock().unwrap().jobs.clear();
            let old = fs::read(fixture.ctx.plist_path()).unwrap();
            if cancel {
                fixture.runner.state.lock().unwrap().cancel_bootstrap = true;
            } else {
                fixture.runner.fail_once("bootstrap", "activation denied");
            }
            assert!(start(&fixture.ctx, &fixture.config).is_err());
            assert_eq!(old, fs::read(fixture.ctx.plist_path()).unwrap());
            assert!(
                !fixture
                    .runner
                    .state
                    .lock()
                    .unwrap()
                    .jobs
                    .contains_key(LABEL)
            );
        }
    }

    #[test]
    fn failed_restart_restores_old_service_even_when_cancelled() {
        for cancel in [false, true] {
            let fixture = Fixture::new();
            fixture.load_service();
            let old = fs::read(fixture.ctx.plist_path()).unwrap();
            if cancel {
                fixture.runner.state.lock().unwrap().cancel_bootstrap = true;
            } else {
                fixture.runner.fail_once("bootstrap", "activation denied");
            }
            assert!(restart(&fixture.ctx, &fixture.config).is_err());
            assert_eq!(old, fs::read(fixture.ctx.plist_path()).unwrap());
            assert!(
                fixture
                    .runner
                    .state
                    .lock()
                    .unwrap()
                    .jobs
                    .contains_key(LABEL)
            );
        }
    }

    #[test]
    fn foreign_service_is_never_stopped() {
        let fixture = Fixture::new();
        atomic_write(
            &fixture.ctx.plist_path(),
            &definition(
                &fixture.ctx,
                &fixture.config,
                Path::new("/another/clipbridge"),
            )
            .unwrap(),
            0o600,
        )
        .unwrap();
        fixture
            .runner
            .state
            .lock()
            .unwrap()
            .jobs
            .insert(LABEL.into(), "pid = 2\n".into());
        assert!(
            stop(&fixture.ctx)
                .unwrap_err()
                .to_string()
                .contains("not owned")
        );
        assert_eq!(fixture.runner.action_count("bootout"), 0);
    }

    #[test]
    fn status_is_read_only_and_propagates_inspection_errors() {
        let fixture = Fixture::new();
        assert_eq!(status(&fixture.ctx).unwrap(), "ClipBridge is stopped.");
        assert!(!fixture.ctx.cache().exists());
        fixture.runner.fail_once("print", "Could not find domain");
        assert!(
            status(&fixture.ctx)
                .unwrap_err()
                .to_string()
                .contains("Could not find domain")
        );
    }

    #[test]
    fn unmanaged_foreground_monitor_blocks_bootstrap() {
        let fixture = Fixture::new();
        let _held = lock(&fixture.ctx.cache().join("watcher.lock")).unwrap();
        assert!(
            start(&fixture.ctx, &fixture.config)
                .unwrap_err()
                .to_string()
                .contains("Ctrl+C")
        );
        assert!(!fixture.ctx.plist_path().exists());
        assert_eq!(fixture.runner.action_count("bootstrap"), 0);
    }

    #[test]
    fn concurrent_service_operations_are_rejected() {
        let fixture = Fixture::new();
        let _held = operation_lock(&fixture.ctx).unwrap();
        assert!(start(&fixture.ctx, &fixture.config).is_err());
        assert!(stop(&fixture.ctx).is_err());
        assert_eq!(fixture.runner.action_count("print"), 0);
    }

    #[test]
    fn stop_is_repeatable_and_does_not_touch_other_labels() {
        let fixture = Fixture::new();
        fixture.load_service();
        fixture
            .runner
            .state
            .lock()
            .unwrap()
            .jobs
            .insert(LEGACY[0].into(), "pid = 4\n".into());
        stop(&fixture.ctx).unwrap();
        stop(&fixture.ctx).unwrap();
        assert!(!fixture.ctx.plist_path().exists());
        assert!(
            fixture
                .runner
                .state
                .lock()
                .unwrap()
                .jobs
                .contains_key(LEGACY[0])
        );
    }

    #[test]
    fn failed_stop_preserves_login_plist() {
        let fixture = Fixture::new();
        fixture.load_service();
        fixture.runner.fail_once("bootout", "permission denied");
        assert!(
            stop(&fixture.ctx)
                .unwrap_err()
                .to_string()
                .contains("permission denied")
        );
        assert!(fixture.ctx.plist_path().exists());
    }

    #[test]
    fn stop_does_not_require_existing_or_valid_configuration() {
        for missing in [false, true] {
            let fixture = Fixture::new();
            fixture.load_service();
            if missing {
                fs::remove_file(&fixture.config).unwrap();
            } else {
                fs::write(&fixture.config, "invalid JSON").unwrap();
            }
            stop(&fixture.ctx).unwrap();
            assert!(
                !fixture
                    .runner
                    .state
                    .lock()
                    .unwrap()
                    .jobs
                    .contains_key(LABEL)
            );
            assert!(!fixture.ctx.plist_path().exists());
        }
    }

    #[test]
    fn loaded_job_must_match_disk_plist_before_service_changes() {
        let fixture = Fixture::new();
        fixture.load_service();
        let other_config = fixture.source.join("another config.json");
        fs::copy(&fixture.config, &other_config).unwrap();
        let changed = definition(&fixture.ctx, &other_config, &fixture.ctx.executable).unwrap();
        atomic_write(&fixture.ctx.plist_path(), &changed, 0o600).unwrap();
        for result in [
            start(&fixture.ctx, &other_config),
            restart(&fixture.ctx, &other_config),
            stop(&fixture.ctx),
        ] {
            assert!(result.unwrap_err().to_string().contains("do not match"));
        }
        assert_eq!(fixture.runner.action_count("bootout"), 0);
        assert_eq!(fixture.runner.action_count("bootstrap"), 0);
        assert_eq!(fs::read(fixture.ctx.plist_path()).unwrap(), changed);
    }

    #[test]
    fn loaded_definition_parser_preserves_spaces_and_rejects_incomplete_output() {
        let expected = vec![
            "/a directory/clipbridge".into(),
            "--config".into(),
            "/a directory/config.json".into(),
            "monitor".into(),
        ];
        let info = format!(
            "program = {}\narguments = {{\n {}\n}}\npid = 52\n",
            expected[0],
            expected.join("\n ")
        );
        verify_loaded_definition(&info, &expected).unwrap();
        assert!(verify_loaded_definition("pid = 52\n", &expected).is_err());
        assert!(
            verify_loaded_definition(
                "program = /a directory/clipbridge\narguments = {\n",
                &expected
            )
            .is_err()
        );
    }
}
