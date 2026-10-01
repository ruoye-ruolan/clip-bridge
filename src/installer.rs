//! Transactional, user-owned installation of the already compiled Rust executable.
use crate::common::{
    CommandSpec, Context, INSTALL_MARKER, RELEASE_MARKER, VERSION, absolute, atomic_write,
    expand_path, private_dir, shell_quote, unique_id,
};
use crate::config::Config;
use crate::service;
use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

struct Locations {
    root: PathBuf,
    launcher: PathBuf,
}

fn locations(ctx: &Context, prefix: Option<&Path>) -> Result<Locations> {
    let prefix = if let Some(prefix) = prefix {
        absolute(&expand_path(prefix, &ctx.home))?
    } else if let Some(root) = ctx.installed_root()? {
        root.parent()
            .and_then(Path::parent)
            .context("Invalid installation location")?
            .to_owned()
    } else {
        ctx.home.join(".local")
    };
    if prefix == Path::new("/") {
        bail!("Choose a user installation prefix, not the filesystem root");
    }
    Ok(Locations {
        root: prefix.join("share/clipbridge"),
        launcher: prefix.join("bin/clipbridge"),
    })
}

fn parse_marker(path: &Path) -> Result<Value> {
    let data = service::read_optional(path)?
        .with_context(|| format!("Missing ownership marker: {}", path.display()))?;
    let value: Value = serde_json::from_slice(&data).context("Invalid ownership marker JSON")?;
    if value["product"] != "clipbridge" || value["schema"] != 1 {
        bail!("Unknown marker product/schema: {}", path.display());
    }
    Ok(value)
}

fn existing(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn real_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        bail!(
            "Refusing non-directory or symlink directory: {}",
            path.display()
        );
    }
    Ok(())
}

fn link_target(path: &Path) -> Result<Option<PathBuf>> {
    if !existing(path)? {
        return Ok(None);
    }
    if !fs::symlink_metadata(path)?.file_type().is_symlink() {
        bail!("Expected a managed release link: {}", path.display());
    }
    let target = fs::read_link(path)?;
    let parts: Vec<_> = target.components().collect();
    if parts.len() != 2
        || parts[0] != Component::Normal("releases".as_ref())
        || !matches!(parts[1], Component::Normal(_))
    {
        bail!("Release link escapes the installation: {}", path.display());
    }
    let release = path
        .parent()
        .context("Release link lacks parent")?
        .join(&target);
    real_directory(&release)?;
    parse_marker(&release.join(RELEASE_MARKER))?;
    Ok(Some(target))
}

fn read_marker(root: &Path) -> Result<Option<Value>> {
    if !existing(root)? {
        return Ok(None);
    }
    real_directory(root)?;
    let marker = parse_marker(&root.join(INSTALL_MARKER)).with_context(|| {
        format!(
            "Installation directory is unmanaged, not owned by ClipBridge: {}",
            root.display()
        )
    })?;
    real_directory(&root.join("releases"))?;
    link_target(&root.join("current"))?;
    link_target(&root.join("previous"))?;
    Ok(Some(marker))
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn check_launcher(path: &Path, marker: Option<&Value>) -> Result<()> {
    if let Some(data) = service::read_optional(path)?
        && marker.and_then(|m| m["launcher_sha256"].as_str()) != Some(hash(&data).as_str())
    {
        bail!(
            "Existing command is unmanaged or modified; it will not be overwritten: {}",
            path.display()
        );
    }
    if let Some(parent) = path.parent()
        && existing(parent)?
    {
        real_directory(parent)?;
    }
    Ok(())
}

fn replace_link(path: &Path, target: Option<&Path>) -> Result<()> {
    let Some(target) = target else {
        return service::remove_file(path);
    };
    let temporary = path.with_file_name(format!(".clipbridge-link-{}", unique_id()));
    symlink(target, &temporary)?;
    let result = fs::rename(&temporary, path);
    if result.is_err() {
        service::remove_file(&temporary)?;
    }
    result.with_context(|| format!("Could not switch release link {}", path.display()))
}

fn cancelled(ctx: &Context) -> Result<()> {
    if ctx.cancelled.load(Ordering::Relaxed) {
        bail!("Operation cancelled");
    }
    Ok(())
}

fn verify_source(source: &Path) -> Result<()> {
    for name in [
        "Cargo.toml",
        "Cargo.lock",
        "VERSION",
        "LICENSE",
        "src/config.example.json",
    ] {
        service::read_optional(&source.join(name))?
            .with_context(|| format!("Missing source file: {name}"))?;
    }
    if fs::read_to_string(source.join("VERSION"))?.trim() != VERSION {
        bail!(
            "Source VERSION does not match this compiled binary ({VERSION}); rebuild the selected source"
        );
    }
    if fs::read(source.join("Cargo.toml"))? != include_bytes!("../Cargo.toml")
        || fs::read(source.join("Cargo.lock"))? != include_bytes!("../Cargo.lock")
    {
        bail!(
            "Source Cargo.toml/Cargo.lock does not match this compiled binary; rebuild the selected source"
        );
    }
    Ok(())
}

fn copy_payload(
    source: &Path,
    stage: &Path,
    relative: &Path,
    files: &mut BTreeSet<String>,
) -> Result<()> {
    let bytes = service::read_optional(&source.join(relative))?
        .with_context(|| format!("Missing source payload: {}", relative.display()))?;
    atomic_write(&stage.join(relative), &bytes, 0o600)?;
    files.insert(relative.to_string_lossy().into_owned());
    Ok(())
}

fn copy_docs(
    source: &Path,
    stage: &Path,
    relative: &Path,
    files: &mut BTreeSet<String>,
) -> Result<()> {
    if !source.join(relative).exists() {
        return Ok(());
    }
    real_directory(&source.join(relative))?;
    for entry in fs::read_dir(source.join(relative))? {
        let entry = entry?;
        let item = relative.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            bail!("Refusing symlink documentation: {}", item.display());
        }
        if kind.is_dir() {
            copy_docs(source, stage, &item, files)?;
        } else if kind.is_file() && item.extension().and_then(|s| s.to_str()) == Some("md") {
            copy_payload(source, stage, &item, files)?;
        }
    }
    Ok(())
}

fn prepare_release(ctx: &Context, source: &Path, stage: &Path) -> Result<()> {
    verify_source(source)?;
    let mut files = BTreeSet::new();
    let binary =
        service::read_optional(&ctx.executable)?.context("Compiled executable is unavailable")?;
    atomic_write(&stage.join("clipbridge"), &binary, 0o755)?;
    files.insert("clipbridge".into());
    for name in ["VERSION", "LICENSE", "src/config.example.json"] {
        copy_payload(source, stage, Path::new(name), &mut files)?;
    }
    for name in ["README.md", "AGENTS.md", "src/README.md"] {
        if source.join(name).exists() {
            copy_payload(source, stage, Path::new(name), &mut files)?;
        }
    }
    copy_docs(source, stage, Path::new("docs"), &mut files)?;
    let version = ctx.exec(
        CommandSpec::new(stage.join("clipbridge"))
            .args(["--version"])
            .timeout(20),
    )?;
    if version.stdout.trim() != format!("clipbridge {VERSION}") {
        bail!("Staged executable version does not match {VERSION}");
    }
    ctx.exec(
        CommandSpec::new(stage.join("clipbridge"))
            .args(["--help"])
            .timeout(20),
    )?;
    atomic_write(
        &stage.join(RELEASE_MARKER),
        &serde_json::to_vec_pretty(&json!({
            "product": "clipbridge", "schema": 1, "version": VERSION, "runtime": "rust", "files": files,
        }))?,
        0o600,
    )?;
    cancelled(ctx)
}

fn persistent_config(
    ctx: &Context,
    source: &Path,
    root: &Path,
    selected: Option<&Path>,
) -> Result<PathBuf> {
    let default = ctx.default_config();
    if let Some(selected) = selected {
        let selected = absolute(selected)?;
        if selected.starts_with(absolute(source)?) || selected.starts_with(absolute(root)?) {
            let destination = if default.exists() {
                default.with_file_name(format!("migrated-{}.json", &unique_id()[..8]))
            } else {
                default
            };
            Config::load(&selected)?;
            atomic_write(&destination, &fs::read(&selected)?, 0o600)?;
            return absolute(&destination);
        }
        return Ok(selected);
    }
    if !default.exists() {
        let legacy = source.join("src/config.json");
        let origin = if legacy.is_file() {
            legacy
        } else {
            source.join("src/config.example.json")
        };
        Config::load(&origin)?.save(&default)?;
    }
    absolute(&default)
}

fn wait_healthy(ctx: &Context, config: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let selected = absolute(config)?;
    loop {
        cancelled(ctx)?;
        let process = service::loaded_info(ctx)?.and_then(|info| service::pid(&info));
        let state = service::read_optional(&ctx.cache().join("monitor.json"))?
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        if let (Some(pid), Some(state)) = (process, state)
            && state["pid"].as_u64() == Some(u64::from(pid))
            && state["config"].as_str() == selected.to_str()
            && service::monitor_running(ctx)?
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "Installed monitor did not become ready; see {}",
                ctx.log_path().display()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn legacy_payload(relative: &Path) -> bool {
    let value = relative.to_string_lossy();
    const FILES: &[&str] = &[
        "clipbridge",
        "install.sh",
        "VERSION",
        "LICENSE",
        "README.md",
        "AGENTS.md",
        "src/auto_upload.py",
        "src/cli.py",
        "src/configuration.py",
        "src/installer.py",
        "src/monitor_control.py",
        "src/runtime.py",
        "src/service.py",
        "src/Makefile",
        "src/README.md",
        "src/config.example.json",
        "src/swift/clipboard-watch.swift",
        "src/swift/clipboard-path.swift",
        "src/build/clipboard-watch",
        "src/build/clipboard-path",
        "src/build/.directory",
        "docs/installation.md",
        "docs/usage.md",
        "docs/planning/product-direction-and-distribution.md",
    ];
    if FILES.contains(&value.as_ref()) {
        return true;
    }
    if let Some(name) = value.strip_prefix("src/__pycache__/") {
        let modules = [
            "auto_upload",
            "cli",
            "configuration",
            "installer",
            "monitor_control",
            "runtime",
            "service",
        ];
        return modules
            .iter()
            .any(|module| name.starts_with(&format!("{module}.cpython-")))
            && name.ends_with(".pyc")
            && !name.contains('/');
    }
    false
}

fn release_files(root: &Path, relative: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = relative.join(entry.file_name());
        if kind.is_symlink() {
            bail!(
                "Unmanaged symlink in release: {}",
                root.join(&path).display()
            );
        }
        if kind.is_dir() {
            release_files(root, &path, files)?;
        } else if kind.is_file() {
            files.push(path);
        } else {
            bail!(
                "Unmanaged special file in release: {}",
                root.join(path).display()
            );
        }
    }
    Ok(())
}

fn safe_release(release: &Path, preserved_config: Option<&Path>) -> Result<()> {
    real_directory(release)?;
    let marker = parse_marker(&release.join(RELEASE_MARKER))?;
    let inventory = match marker.get("files") {
        Some(Value::Array(items)) => Some(
            items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::to_owned)
                        .context("Invalid release file inventory")
                })
                .collect::<Result<BTreeSet<_>>>()?,
        ),
        None if marker.get("runtime").is_none() => None,
        _ => bail!("Invalid release file inventory: {}", release.display()),
    };
    let mut files = Vec::new();
    release_files(release, Path::new(""), &mut files)?;
    for relative in files {
        let file = release.join(&relative);
        if relative == Path::new(RELEASE_MARKER) || preserved_config == Some(file.as_path()) {
            continue;
        }
        let allowed = inventory
            .as_ref()
            .map(|names| names.contains(relative.to_string_lossy().as_ref()))
            .unwrap_or_else(|| legacy_payload(&relative));
        if !allowed {
            bail!(
                "Unmanaged release data will not be deleted: {}",
                file.display()
            );
        }
    }
    Ok(())
}

struct PriorInstall {
    marker: Option<Vec<u8>>,
    launcher: Option<Vec<u8>>,
    current: Option<PathBuf>,
    previous: Option<PathBuf>,
    service: service::Snapshot,
}

fn rollback(
    ctx: &Context,
    locations: &Locations,
    prior: &PriorInstall,
    service_changed: bool,
) -> Result<()> {
    let mut errors = Vec::new();
    if service_changed && let Err(error) = service::stop_unlocked(ctx, true) {
        errors.push(error.to_string());
    }
    for (name, target) in [("current", &prior.current), ("previous", &prior.previous)] {
        if let Err(error) = replace_link(&locations.root.join(name), target.as_deref()) {
            errors.push(error.to_string());
        }
    }
    if let Err(error) =
        service::restore_bytes(&locations.launcher, prior.launcher.as_deref(), 0o755)
    {
        errors.push(error.to_string());
    }
    if let Err(error) = service::restore_bytes(
        &locations.root.join(INSTALL_MARKER),
        prior.marker.as_deref(),
        0o600,
    ) {
        errors.push(error.to_string());
    }
    if service_changed && let Err(error) = service::restore(ctx, &prior.service) {
        errors.push(error.to_string());
    }
    if !errors.is_empty() {
        bail!("{}", errors.join("; "));
    }
    Ok(())
}

pub fn install(
    ctx: &Context,
    source: &Path,
    prefix: Option<&Path>,
    upgrade: bool,
) -> Result<String> {
    if !cfg!(target_os = "macos") {
        bail!("Installation currently requires macOS");
    }
    if ctx.uid == 0 {
        bail!("Run the installer as your normal user, without sudo");
    }
    let source = source
        .canonicalize()
        .context("Cannot find selected source")?;
    let paths = locations(ctx, prefix)?;
    let _lock = service::operation_lock(ctx)?;
    let marker = read_marker(&paths.root)?;
    if upgrade && marker.is_none() {
        bail!(
            "ClipBridge is not installed at {}; run ./install.sh first",
            paths.root.display()
        );
    }
    check_launcher(&paths.launcher, marker.as_ref())?;
    service::check_legacy(ctx)?;
    private_dir(paths.root.parent().context("Installation has no parent")?)?;
    let stage = tempfile::Builder::new()
        .prefix(".clipbridge-stage-")
        .tempdir_in(paths.root.parent().context("Missing root parent")?)?;
    prepare_release(ctx, &source, stage.path())?;
    let prior = PriorInstall {
        service: service::snapshot(ctx, Some(&source), Some(&paths.root))?,
        marker: service::read_optional(&paths.root.join(INSTALL_MARKER))?,
        launcher: service::read_optional(&paths.launcher)?,
        current: link_target(&paths.root.join("current"))?,
        previous: link_target(&paths.root.join("previous"))?,
    };
    if prior.service.loaded {
        Config::load(
            prior
                .service
                .config
                .as_deref()
                .context("Loaded service has no configuration")?,
        )?;
    }
    if !prior.service.loaded && service::monitor_running(ctx)? {
        bail!(
            "Stop foreground monitoring with Ctrl+C before installing, upgrading or uninstalling"
        );
    }
    let selected = persistent_config(ctx, &source, &paths.root, prior.service.config.as_deref())?;
    let release_id = format!("{VERSION}-{}", &unique_id()[..12]);
    let release = paths.root.join("releases").join(&release_id);
    let launcher_bytes = format!(
        "#!/bin/sh\n# Managed by ClipBridge installer v1\nexec {} \"$@\"\n",
        shell_quote(&paths.root.join("current/clipbridge").to_string_lossy())
    )
    .into_bytes();
    let mut service_changed = false;
    let result = (|| {
        cancelled(ctx)?;
        private_dir(&paths.root.join("releases"))?;
        fs::rename(stage.path(), &release)?;
        if prior.service.loaded {
            service_changed = true;
            service::stop_unlocked(ctx, false)?;
            service::wait_monitor_stopped(ctx)?;
        }
        cancelled(ctx)?;
        replace_link(
            &paths.root.join("current"),
            Some(&PathBuf::from("releases").join(&release_id)),
        )?;
        replace_link(&paths.root.join("previous"), prior.current.as_deref())?;
        atomic_write(&paths.launcher, &launcher_bytes, 0o755)?;
        atomic_write(
            &paths.root.join(INSTALL_MARKER),
            &serde_json::to_vec_pretty(&json!({
                "product":"clipbridge", "schema":1, "launcher_sha256":hash(&launcher_bytes),
            }))?,
            0o600,
        )?;
        cancelled(ctx)?;
        if prior.service.loaded {
            service::install_service(ctx, &selected, &paths.root.join("current/clipbridge"))?;
            wait_healthy(ctx, &selected)?;
        } else if prior.service.data.is_some() {
            service_changed = true;
            atomic_write(
                &ctx.plist_path(),
                &service::definition(ctx, &selected, &paths.root.join("current/clipbridge"))?,
                0o600,
            )?;
        }
        cancelled(ctx)
    })();
    if let Err(error) = result {
        if let Err(recovery) = rollback(ctx, &paths, &prior, service_changed) {
            bail!(
                "Installation failed: {error:#}. Recovery needs attention: {recovery:#}. Preserved files at {}",
                paths.root.display()
            );
        }
        if release.exists() {
            fs::remove_dir_all(&release).context("Could not clean up failed new release")?;
        }
        if marker.is_none() && paths.root.exists() {
            fs::remove_dir(paths.root.join("releases"))?;
            fs::remove_dir(&paths.root)?;
        }
        return Err(error);
    }
    // Prune only payloads with known contents. Unfamiliar files are retained.
    for entry in fs::read_dir(paths.root.join("releases"))? {
        let entry = entry?;
        let previous = prior.current.as_ref().and_then(|p| p.file_name());
        if entry.file_name() == release.file_name().context("Missing release name")?
            || Some(entry.file_name().as_os_str()) == previous
        {
            continue;
        }
        if safe_release(&entry.path(), None).is_ok()
            && let Err(error) = fs::remove_dir_all(entry.path())
        {
            eprintln!("Installed successfully; old release cleanup needs attention: {error}");
        }
    }
    let state = if prior.service.loaded {
        "Background service restored with the new version."
    } else {
        "Monitoring was not started."
    };
    Ok(format!(
        "Installed ClipBridge {VERSION} at {}\nCommand: {}\nConfiguration: {}\n{state}\nAdd {} to PATH if needed.",
        paths.root.display(),
        paths.launcher.display(),
        selected.display(),
        paths
            .launcher
            .parent()
            .context("Missing command directory")?
            .display()
    ))
}

pub fn uninstall(ctx: &Context, prefix: Option<&Path>) -> Result<String> {
    let paths = locations(ctx, prefix)?;
    let _lock = service::operation_lock(ctx)?;
    let Some(marker) = read_marker(&paths.root)? else {
        return Ok(format!(
            "ClipBridge is not installed at {}",
            paths.root.display()
        ));
    };
    check_launcher(&paths.launcher, Some(&marker))?;
    let allowed = ["releases", "current", "previous", INSTALL_MARKER];
    for entry in fs::read_dir(&paths.root)? {
        let entry = entry?;
        if !allowed.iter().any(|name| entry.file_name() == *name) {
            bail!(
                "Unrecognized files in installation directory; preserved {}",
                paths.root.display()
            );
        }
    }
    let old = service::snapshot(ctx, None, Some(&paths.root))?;
    if !old.loaded && service::monitor_running(ctx)? {
        bail!("Stop foreground monitoring with Ctrl+C before uninstalling");
    }
    let internal = old
        .config
        .as_ref()
        .filter(|path| path.starts_with(&paths.root));
    for entry in fs::read_dir(paths.root.join("releases"))? {
        safe_release(&entry?.path(), internal.map(PathBuf::as_path))?;
    }
    let preserved = internal
        .map(|path| persistent_config(ctx, &paths.root, &paths.root, Some(path)))
        .transpose()?;
    let launcher = service::read_optional(&paths.launcher)?;
    // Rename the whole installation before deleting it, allowing pre-delete errors to roll back.
    let tombstone = paths
        .root
        .with_file_name(format!(".clipbridge-uninstall-{}", unique_id()));
    let mut moved = false;
    let result = (|| {
        cancelled(ctx)?;
        service::stop_unlocked(ctx, false)?;
        service::wait_monitor_stopped(ctx)?;
        cancelled(ctx)?;
        fs::rename(&paths.root, &tombstone)?;
        moved = true;
        service::remove_file(&paths.launcher)?;
        cancelled(ctx)
    })();
    if let Err(error) = result {
        let recovery = (|| -> Result<()> {
            if moved {
                fs::rename(&tombstone, &paths.root)?;
            }
            service::restore_bytes(&paths.launcher, launcher.as_deref(), 0o755)?;
            service::restore(ctx, &old)
        })();
        recovery.map_err(|cleanup| anyhow!("{error:#}. Uninstall recovery failed: {cleanup:#}"))?;
        return Err(error);
    }
    fs::remove_dir_all(&tombstone).with_context(|| {
        format!(
            "Command and service removed; retained program files could not be removed at {}",
            tombstone.display()
        )
    })?;
    let detail = preserved
        .map(|path| format!("\nMoved active configuration to: {}", path.display()))
        .unwrap_or_default();
    Ok(format!(
        "ClipBridge uninstalled. Configuration, logs, retained images and remote uploads were preserved.{detail}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::test_support::Fixture;
    use plist::{Dictionary, Value as PlistValue};
    use std::os::unix::fs::PermissionsExt;

    fn fixture() -> Fixture {
        let fixture = Fixture::new();
        for (name, bytes) in [
            ("Cargo.toml", include_bytes!("../Cargo.toml").as_slice()),
            ("Cargo.lock", include_bytes!("../Cargo.lock").as_slice()),
            ("VERSION", VERSION.as_bytes()),
            ("LICENSE", b"MIT license".as_slice()),
            (
                "src/config.example.json",
                br#"{"ssh_host":"dev","remote_directory":"/tmp/images"}"#.as_slice(),
            ),
        ] {
            atomic_write(&fixture.source.join(name), bytes, 0o600).unwrap();
        }
        fixture
    }

    fn prefix(f: &Fixture) -> PathBuf {
        f.ctx.home.join("prefix")
    }
    fn root(f: &Fixture) -> PathBuf {
        prefix(f).join("share/clipbridge")
    }
    fn launcher(f: &Fixture) -> PathBuf {
        prefix(f).join("bin/clipbridge")
    }
    fn install_fixture(f: &Fixture) -> Result<String> {
        install(&f.ctx, &f.source, Some(&prefix(f)), false)
    }
    fn python_service(f: &Fixture, source: &Path, config: &Path, loaded: bool) {
        let mut definition = Dictionary::new();
        definition.insert("Label".into(), service::LABEL.into());
        definition.insert(
            "ProgramArguments".into(),
            PlistValue::Array(vec![
                "/usr/bin/python3".into(),
                "-u".into(),
                source
                    .join("src/auto_upload.py")
                    .to_string_lossy()
                    .into_owned()
                    .into(),
                "--config".into(),
                config.to_string_lossy().into_owned().into(),
            ]),
        );
        let mut bytes = Vec::new();
        PlistValue::Dictionary(definition)
            .to_writer_xml(&mut bytes)
            .unwrap();
        atomic_write(&f.ctx.plist_path(), &bytes, 0o600).unwrap();
        if loaded {
            f.runner.load_definition(&bytes, "pid = 123\n");
        }
    }

    #[test]
    fn installs_only_native_payload_without_starting_or_replacing_config() {
        let f = fixture();
        let before = fs::read(&f.config).unwrap();
        fs::write(
            f.source.join("src/config.json"),
            br#"{"private":"never package"}"#,
        )
        .unwrap();
        install_fixture(&f).unwrap();
        let current = root(&f).join("current").canonicalize().unwrap();
        assert_eq!(
            fs::read(current.join("clipbridge")).unwrap(),
            b"fake executable"
        );
        assert!(!current.join("src/config.json").exists());
        assert!(!current.join("src/auto_upload.py").exists());
        assert_eq!(before, fs::read(&f.config).unwrap());
        assert_eq!(
            fs::metadata(current.join("clipbridge"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(f.runner.action_count("bootstrap"), 0);
        let wrapper = fs::read_to_string(launcher(&f)).unwrap();
        assert!(!wrapper.contains("python"));
        assert!(!wrapper.contains(&*f.source.to_string_lossy()));
        assert!(wrapper.contains("current/clipbridge"));
    }

    #[test]
    fn repeat_install_keeps_previous_release_and_prunes_known_older_release() {
        let f = fixture();
        install_fixture(&f).unwrap();
        let first = root(&f).join("current").canonicalize().unwrap();
        install_fixture(&f).unwrap();
        assert_eq!(root(&f).join("previous").canonicalize().unwrap(), first);
        install_fixture(&f).unwrap();
        assert!(!first.exists());
        assert_eq!(fs::read_dir(root(&f).join("releases")).unwrap().count(), 2);
    }

    #[test]
    fn fingerprint_mismatch_does_not_change_installation() {
        let f = fixture();
        install_fixture(&f).unwrap();
        let old = fs::read_link(root(&f).join("current")).unwrap();
        fs::write(f.source.join("Cargo.lock"), "altered lock").unwrap();
        assert!(
            install_fixture(&f)
                .unwrap_err()
                .to_string()
                .contains("does not match")
        );
        assert_eq!(old, fs::read_link(root(&f).join("current")).unwrap());
        assert_eq!(f.runner.action_count("bootout"), 0);
    }

    #[test]
    fn foreign_and_modified_launchers_are_preserved() {
        let f = fixture();
        atomic_write(&launcher(&f), b"another command", 0o755).unwrap();
        assert!(install_fixture(&f).is_err());
        assert_eq!(fs::read(launcher(&f)).unwrap(), b"another command");
        assert!(!root(&f).exists());
        fs::remove_file(launcher(&f)).unwrap();
        install_fixture(&f).unwrap();
        fs::write(launcher(&f), "modified").unwrap();
        assert!(uninstall(&f.ctx, Some(&prefix(&f))).is_err());
        assert!(root(&f).exists());
    }

    #[test]
    fn corrupt_markers_and_foreign_directory_are_preserved() {
        let f = fixture();
        atomic_write(&root(&f).join("personal"), b"keep", 0o600).unwrap();
        assert!(install_fixture(&f).is_err());
        assert_eq!(fs::read(root(&f).join("personal")).unwrap(), b"keep");
        fs::remove_file(root(&f).join("personal")).unwrap();
        fs::remove_dir(root(&f)).unwrap();
        install_fixture(&f).unwrap();
        fs::write(root(&f).join(INSTALL_MARKER), "{}").unwrap();
        assert!(install_fixture(&f).is_err());
        assert!(uninstall(&f.ctx, Some(&prefix(&f))).is_err());
        assert!(launcher(&f).exists());
    }

    #[test]
    fn upgrade_requires_existing_owned_installation() {
        let f = fixture();
        assert!(
            install(&f.ctx, &f.source, Some(&prefix(&f)), true)
                .unwrap_err()
                .to_string()
                .contains("not installed")
        );
        assert!(!root(&f).exists());
    }

    #[test]
    fn migration_reuses_python_schema_and_replaces_python_launcher() {
        let f = fixture();
        let old = root(&f).join("releases/0.1.0-old");
        private_dir(&old.join("src")).unwrap();
        atomic_write(
            &old.join(RELEASE_MARKER),
            br#"{"product":"clipbridge","schema":1,"version":"0.1.0","python":"/usr/bin/python3"}"#,
            0o600,
        )
        .unwrap();
        atomic_write(&old.join("src/auto_upload.py"), b"old uploader", 0o600).unwrap();
        symlink("releases/0.1.0-old", root(&f).join("current")).unwrap();
        let wrapper = b"#!/bin/sh\nexec python3 old-clipbridge";
        atomic_write(&launcher(&f), wrapper, 0o755).unwrap();
        atomic_write(
            &root(&f).join(INSTALL_MARKER),
            &serde_json::to_vec(
                &json!({"product":"clipbridge","schema":1,"launcher_sha256":hash(wrapper)}),
            )
            .unwrap(),
            0o600,
        )
        .unwrap();
        python_service(&f, &root(&f).join("current"), &f.config, true);
        f.runner.state.lock().unwrap().ready = true;
        let before = fs::read(&f.config).unwrap();
        install(&f.ctx, &f.source, Some(&prefix(&f)), true).unwrap();
        assert_eq!(root(&f).join("previous").canonicalize().unwrap(), old);
        assert_eq!(before, fs::read(&f.config).unwrap());
        assert!(!fs::read_to_string(launcher(&f)).unwrap().contains("python"));
        let data = fs::read_to_string(f.ctx.plist_path()).unwrap();
        assert!(data.contains("monitor"));
        assert!(!data.contains("auto_upload.py"));
        assert_eq!(f.runner.action_count("bootout"), 1);
    }

    #[test]
    fn source_python_service_and_source_config_are_migrated() {
        let f = fixture();
        let local = f.source.join("src/config.json");
        fs::copy(&f.config, &local).unwrap();
        python_service(&f, &f.source, &local, true);
        f.runner.state.lock().unwrap().ready = true;
        install_fixture(&f).unwrap();
        let selected = service::snapshot(&f.ctx, None, Some(&root(&f)))
            .unwrap()
            .config
            .unwrap();
        assert!(selected.starts_with(f.config.parent().unwrap()));
        assert_eq!(fs::read(selected).unwrap(), fs::read(&f.config).unwrap());
        assert!(local.exists());
    }

    #[test]
    fn unloaded_login_plist_is_migrated_without_starting() {
        let f = fixture();
        python_service(&f, &f.source, &f.config, false);
        install_fixture(&f).unwrap();
        assert_eq!(f.runner.action_count("bootstrap"), 0);
        assert!(
            fs::read_to_string(f.ctx.plist_path())
                .unwrap()
                .contains("monitor")
        );
    }

    #[test]
    fn foreign_service_is_not_changed_by_install_or_uninstall() {
        let f = fixture();
        python_service(&f, &f.ctx.home.join("other-source"), &f.config, true);
        let before = fs::read(f.ctx.plist_path()).unwrap();
        assert!(
            install_fixture(&f)
                .unwrap_err()
                .to_string()
                .contains("not owned")
        );
        assert_eq!(before, fs::read(f.ctx.plist_path()).unwrap());
        assert_eq!(f.runner.action_count("bootout"), 0);
    }

    #[test]
    fn activation_failure_restores_links_launcher_and_running_service() {
        let f = fixture();
        install_fixture(&f).unwrap();
        python_service(&f, &root(&f).join("current"), &f.config, true);
        let current = fs::read_link(root(&f).join("current")).unwrap();
        let wrapper = fs::read(launcher(&f)).unwrap();
        let plist = fs::read(f.ctx.plist_path()).unwrap();
        f.runner.fail_once("bootstrap", "new monitor failed");
        assert!(
            install_fixture(&f)
                .unwrap_err()
                .to_string()
                .contains("new monitor failed")
        );
        assert_eq!(current, fs::read_link(root(&f).join("current")).unwrap());
        assert_eq!(wrapper, fs::read(launcher(&f)).unwrap());
        assert_eq!(plist, fs::read(f.ctx.plist_path()).unwrap());
        assert!(
            f.runner
                .state
                .lock()
                .unwrap()
                .jobs
                .contains_key(service::LABEL)
        );
        assert_eq!(fs::read_dir(root(&f).join("releases")).unwrap().count(), 1);
    }

    #[test]
    fn cancellation_rolls_back_first_install_and_source_service() {
        let f = fixture();
        python_service(&f, &f.source, &f.config, true);
        let old = fs::read(f.ctx.plist_path()).unwrap();
        f.runner.state.lock().unwrap().cancel_bootstrap = true;
        assert!(install_fixture(&f).is_err());
        assert_eq!(old, fs::read(f.ctx.plist_path()).unwrap());
        assert!(
            f.runner
                .state
                .lock()
                .unwrap()
                .jobs
                .contains_key(service::LABEL)
        );
        assert!(!root(&f).exists());
        assert!(!launcher(&f).exists());
    }

    #[test]
    fn failed_recovery_preserves_new_release_and_reports_both_errors() {
        let f = fixture();
        python_service(&f, &f.source, &f.config, true);
        f.runner.fail_once("bootstrap", "activation denied");
        f.runner.fail_once("bootstrap", "recovery denied");
        let error = install_fixture(&f).unwrap_err().to_string();
        assert!(error.contains("activation denied"));
        assert!(error.contains("Recovery needs attention"));
        assert!(error.contains("recovery denied"));
        assert_eq!(fs::read_dir(root(&f).join("releases")).unwrap().count(), 1);
    }

    #[test]
    fn readiness_probe_failure_restores_previous_installation_and_service() {
        let f = fixture();
        install_fixture(&f).unwrap();
        python_service(&f, &root(&f).join("current"), &f.config, true);
        let current = fs::read_link(root(&f).join("current")).unwrap();
        let old_plist = fs::read(f.ctx.plist_path()).unwrap();
        f.runner.state.lock().unwrap().fail_readiness_once = true;
        assert!(
            install_fixture(&f)
                .unwrap_err()
                .to_string()
                .contains("readiness inspection denied")
        );
        assert_eq!(current, fs::read_link(root(&f).join("current")).unwrap());
        assert_eq!(old_plist, fs::read(f.ctx.plist_path()).unwrap());
        assert!(
            f.runner
                .state
                .lock()
                .unwrap()
                .jobs
                .contains_key(service::LABEL)
        );
    }

    #[test]
    fn unknown_files_inside_a_marked_release_are_never_deleted() {
        let f = fixture();
        install_fixture(&f).unwrap();
        let first = root(&f).join("current").canonicalize().unwrap();
        fs::write(first.join("personal.txt"), "keep").unwrap();
        install_fixture(&f).unwrap();
        install_fixture(&f).unwrap();
        assert_eq!(fs::read(first.join("personal.txt")).unwrap(), b"keep");
        assert!(uninstall(&f.ctx, Some(&prefix(&f))).is_err());
        assert!(launcher(&f).exists());
    }

    #[test]
    fn escaped_or_symlinked_release_marker_is_rejected() {
        let f = fixture();
        install_fixture(&f).unwrap();
        fs::remove_file(root(&f).join("current")).unwrap();
        symlink("/tmp", root(&f).join("current")).unwrap();
        assert!(
            uninstall(&f.ctx, Some(&prefix(&f)))
                .unwrap_err()
                .to_string()
                .contains("escapes")
        );
        assert!(launcher(&f).exists());
    }

    #[test]
    fn uninstall_preserves_config_logs_and_images() {
        let f = fixture();
        install_fixture(&f).unwrap();
        let config = fs::read(&f.config).unwrap();
        atomic_write(&f.ctx.log_path(), b"keep log", 0o600).unwrap();
        atomic_write(&f.ctx.cache().join("image.png"), b"keep image", 0o600).unwrap();
        uninstall(&f.ctx, Some(&prefix(&f))).unwrap();
        assert_eq!(config, fs::read(&f.config).unwrap());
        assert_eq!(fs::read(f.ctx.log_path()).unwrap(), b"keep log");
        assert_eq!(
            fs::read(f.ctx.cache().join("image.png")).unwrap(),
            b"keep image"
        );
        assert!(!root(&f).exists());
        assert!(!launcher(&f).exists());
        assert!(
            uninstall(&f.ctx, Some(&prefix(&f)))
                .unwrap()
                .contains("not installed")
        );
    }

    #[test]
    fn uninstall_migrates_active_configuration_out_of_payload() {
        let f = fixture();
        install_fixture(&f).unwrap();
        let active = root(&f)
            .join("current")
            .canonicalize()
            .unwrap()
            .join("src/config.json");
        fs::copy(&f.config, &active).unwrap();
        python_service(&f, &root(&f).join("current"), &active, true);
        let text = uninstall(&f.ctx, Some(&prefix(&f))).unwrap();
        assert!(text.contains("Moved active configuration"));
        assert!(
            fs::read_dir(f.config.parent().unwrap())
                .unwrap()
                .any(|entry| entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("migrated-"))
        );
        assert!(!root(&f).exists());
    }

    #[test]
    fn active_foreground_monitor_blocks_install_and_uninstall() {
        let f = fixture();
        install_fixture(&f).unwrap();
        let _held = crate::common::lock(&f.ctx.cache().join("watcher.lock")).unwrap();
        assert!(
            install_fixture(&f)
                .unwrap_err()
                .to_string()
                .contains("Stop foreground")
        );
        assert!(
            uninstall(&f.ctx, Some(&prefix(&f)))
                .unwrap_err()
                .to_string()
                .contains("Stop foreground")
        );
        assert!(launcher(&f).exists());
    }

    #[test]
    fn uninstall_stop_failure_restores_original_files() {
        let f = fixture();
        install_fixture(&f).unwrap();
        python_service(&f, &root(&f).join("current"), &f.config, true);
        let old = fs::read(f.ctx.plist_path()).unwrap();
        f.runner.fail_once("bootout", "denied");
        assert!(uninstall(&f.ctx, Some(&prefix(&f))).is_err());
        assert_eq!(old, fs::read(f.ctx.plist_path()).unwrap());
        assert!(root(&f).exists());
        assert!(launcher(&f).exists());
    }

    #[test]
    fn uninstall_other_prefix_never_stops_callers_loaded_service() {
        let f = fixture();
        install_fixture(&f).unwrap();
        let other_prefix = f.ctx.home.join("other-prefix");
        install(&f.ctx, &f.source, Some(&other_prefix), false).unwrap();
        let mut installed = f.ctx.clone();
        installed.executable = root(&f).join("current/clipbridge").canonicalize().unwrap();
        installed.source = None;
        let plist = service::definition(&installed, &f.config, &installed.executable).unwrap();
        atomic_write(&f.ctx.plist_path(), &plist, 0o600).unwrap();
        f.runner.load_definition(&plist, "pid = 123\n");

        let error = uninstall(&installed, Some(&other_prefix)).unwrap_err();
        assert!(error.to_string().contains("not owned"));
        assert_eq!(f.runner.action_count("bootout"), 0);
        assert_eq!(fs::read(f.ctx.plist_path()).unwrap(), plist);
        assert!(root(&f).exists());
        assert!(other_prefix.join("share/clipbridge").exists());
        assert!(other_prefix.join("bin/clipbridge").exists());
        // A plist replaced with B's definition must not disguise A's loaded job.
        let other_binary = other_prefix.join("share/clipbridge/current/clipbridge");
        let other_plist = service::definition(&installed, &f.config, &other_binary).unwrap();
        atomic_write(&f.ctx.plist_path(), &other_plist, 0o600).unwrap();
        assert!(
            uninstall(&installed, Some(&other_prefix))
                .unwrap_err()
                .to_string()
                .contains("do not match")
        );
        assert_eq!(f.runner.action_count("bootout"), 0);
        assert!(other_prefix.join("bin/clipbridge").exists());
        atomic_write(&f.ctx.plist_path(), &plist, 0o600).unwrap();
        // The same installed caller may still operate on its own prefix.
        service::stop(&installed).unwrap();
    }
}
