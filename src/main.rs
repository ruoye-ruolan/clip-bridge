use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand};
use clipbridge::{
    common::{self, CommandSpec, Context},
    config::{self, Config},
    monitor, release, remote, service, setup,
};
use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::Ordering, mpsc};
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "clipbridge",
    version,
    about = "Upload macOS clipboard images to an SSH host"
)]
struct Cli {
    #[arg(
        long,
        global = true,
        help = "Configuration file (default: ~/.config/clipbridge/config.json)"
    )]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    #[command(about = "Install this compiled version from a trusted source or native bundle")]
    Install {
        #[arg(long)]
        prefix: Option<PathBuf>,
        #[arg(long, hide = true)]
        source: Option<PathBuf>,
        #[arg(long, hide = true)]
        require_existing: bool,
    },
    #[command(about = "Install an extracted native bundle or build a trusted source checkout")]
    Upgrade {
        #[arg(long = "from")]
        source: PathBuf,
        #[arg(long)]
        prefix: Option<PathBuf>,
    },
    #[command(about = "Remove installed program/service; preserve config, logs and images")]
    Uninstall {
        #[arg(long)]
        prefix: Option<PathBuf>,
    },
    #[command(about = "Optional guided SSH destination setup")]
    Configure {
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        remote_dir: Option<String>,
        #[arg(long)]
        no_check: bool,
    },
    #[command(about = "List SSH aliases from local configuration without connecting")]
    Hosts {
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Configure this Mac and a Linux/WSL host in one guided flow")]
    Setup {
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        remote_dir: Option<String>,
        #[arg(
            long,
            conflicts_with = "no_start",
            help = "Start monitoring and enable login startup"
        )]
        start: bool,
        #[arg(
            long,
            help = "Leave monitoring stopped and disable existing managed login startup"
        )]
        no_start: bool,
        #[arg(long)]
        no_shell: bool,
        #[arg(
            long,
            help = "Explicitly build the remote companion from source instead of downloading it"
        )]
        build_from_source: bool,
        #[arg(
            long,
            conflicts_with = "build_from_source",
            help = "Use verified release assets from a local directory"
        )]
        assets_dir: Option<PathBuf>,
    },
    #[command(about = "Check tools, SSH access and existing destination permissions")]
    Doctor,
    #[command(about = "Manage the SSH host image clipboard bridge")]
    Remote {
        #[command(subcommand)]
        command: RemoteAction,
    },
    #[command(about = "Ensure background monitoring and login startup are enabled")]
    Start,
    #[command(about = "Restart background monitoring and reload settings")]
    Restart,
    #[command(about = "Stop background monitoring and disable login startup")]
    Stop,
    #[command(about = "Show monitor/service status")]
    Status,
    #[command(about = "Monitor in the foreground; Ctrl+C stops after draining uploads")]
    Run,
    #[command(about = "Show background upload logs")]
    Logs {
        #[arg(long, default_value_t = 50)]
        lines: usize,
        #[arg(long)]
        follow: bool,
    },
    #[command(hide = true)]
    Monitor,
    #[command(hide = true)]
    ClipboardSelfTest,
}

#[derive(Subcommand)]
enum RemoteAction {
    #[command(about = "Set up image clipboard support on the configured SSH host")]
    Setup {
        #[arg(
            long,
            help = "Keep manual wrapper startup instead of installing shell integration"
        )]
        no_shell: bool,
        #[arg(
            long,
            help = "Vendor locally and compile offline on the remote Linux host"
        )]
        build_from_source: bool,
        #[arg(
            long,
            conflicts_with = "build_from_source",
            help = "Use verified release assets from a local directory"
        )]
        assets_dir: Option<PathBuf>,
    },
    #[command(about = "Check remote image clipboard support")]
    Doctor,
    #[command(about = "Show remote image clipboard status")]
    Status,
    #[command(about = "Stop remote image clipboard support")]
    Stop,
}

fn prompt(ctx: &Context, message: &str) -> Result<String> {
    print!("{message}");
    io::stdout().flush()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let read = io::stdin().read_line(&mut line).map(|n| (n, line));
        // Cancellation intentionally drops the receiver; no settings are saved.
        let _ = tx.send(read);
    });
    loop {
        if ctx.cancelled.load(Ordering::SeqCst) {
            bail!("setup cancelled");
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(result) => {
                let (n, line) = result?;
                if n == 0 {
                    bail!("setup cancelled: input ended");
                }
                return Ok(line.trim().to_owned());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => bail!("setup input ended unexpectedly"),
        }
    }
}

fn choose_host(ctx: &Context, previous: Option<&Config>) -> Result<String> {
    let aliases = config::discover_ssh_aliases(&ctx.home.join(".ssh/config"), &ctx.home);
    for (index, alias) in aliases.iter().enumerate() {
        println!("  {}. {alias}", index + 1);
    }
    let default = previous
        .map(|config| config.ssh_host.as_str())
        .or_else(|| (aliases.len() == 1).then(|| aliases[0].as_str()))
        .unwrap_or("");
    loop {
        let input = prompt(ctx, &format!("SSH alias (name or number) [{default}]: "))?;
        let mut host = if input.is_empty() {
            default.to_owned()
        } else {
            input
        };
        if let Ok(index) = host.parse::<usize>()
            && index > 0
            && index <= aliases.len()
        {
            host = aliases[index - 1].clone();
        }
        match config::validate_host(&host) {
            Ok(()) => return Ok(host),
            Err(error) => println!("{error}"),
        }
    }
}

struct SetupInput {
    host: Option<String>,
    remote_dir: Option<String>,
    start: bool,
    no_start: bool,
    no_shell: bool,
    build_from_source: bool,
    assets_dir: Option<PathBuf>,
}

fn guided_setup(ctx: &Context, path: &Path, input: SetupInput) -> Result<()> {
    let wizard = input.host.is_none() || !(input.start || input.no_start);
    if wizard && !io::stdin().is_terminal() {
        bail!("setup requires a terminal, or --host ALIAS and either --start or --no-start");
    }
    let previous = if path.exists() {
        Some(Config::load(path)?)
    } else {
        None
    };
    let host = match input.host {
        Some(host) => {
            config::validate_host(&host)?;
            host
        }
        None => choose_host(
            ctx,
            previous
                .as_ref()
                .filter(|old| !setup::is_initial_example(old)),
        )?,
    };
    let mut directory = input.remote_dir.or_else(|| {
        previous
            .as_ref()
            .filter(|old| old.ssh_host == host && !setup::is_initial_example(old))
            .map(|old| old.remote_directory.clone())
    });
    if wizard {
        let default = directory
            .as_deref()
            .unwrap_or("remote home/.local/share/clipbridge/images");
        let answer = prompt(ctx, &format!("Remote image directory [{default}]: "))?;
        if !answer.is_empty() {
            directory = Some(answer);
        }
    }
    let start = if input.start || input.no_start {
        input.start
    } else {
        loop {
            match prompt(ctx, "Start monitoring and enable login startup? [Y/n]: ")?
                .to_ascii_lowercase()
                .as_str()
            {
                "" | "y" | "yes" => break true,
                "n" | "no" => break false,
                _ => println!("Enter yes or no."),
            }
        }
    };
    let options = setup::SetupOptions {
        host,
        remote_directory: directory,
        start,
        integrate_shell: !input.no_shell,
        build_from_source: input.build_from_source,
        assets_dir: input
            .assets_dir
            .map(|path| common::expand_path(&path, &ctx.home)),
    };
    println!("{}", setup::execute(ctx, path, options)?);
    Ok(())
}

fn hosts(ctx: &Context, path: &Path, json: bool) -> Result<()> {
    let aliases = config::discover_ssh_aliases(&ctx.home.join(".ssh/config"), &ctx.home);
    if json {
        let configured = Config::load(path)
            .ok()
            .filter(|config| !setup::is_initial_example(config))
            .map(|config| config.ssh_host);
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({"aliases": aliases, "configured_host": configured})
            )?
        );
    } else {
        for alias in aliases {
            println!("{alias}");
        }
    }
    Ok(())
}

fn configure(
    ctx: &Context,
    explicit: Option<&Path>,
    host: Option<String>,
    directory: Option<String>,
    no_check: bool,
) -> Result<()> {
    let path = explicit
        .map(|p| common::expand_path(p, &ctx.home))
        .unwrap_or_else(|| ctx.default_config());
    let existing = config::selected_path(ctx, explicit);
    let mut corrupt = None;
    let previous = if existing.exists() {
        match Config::load(&existing) {
            Ok(config) => Some(config),
            Err(error) => {
                eprintln!(
                    "Existing configuration is invalid: {error:#}. It will be preserved until setup succeeds."
                );
                if existing == path {
                    corrupt = Some(fs::read(&existing)?);
                }
                None
            }
        }
    } else {
        None
    };
    let interactive = host.is_none();
    if interactive && !io::stdin().is_terminal() {
        bail!("interactive setup requires a terminal; use configure --host YOUR_ALIAS");
    }
    if no_check && (host.is_none() || directory.is_none()) {
        bail!("--no-check requires --host and an absolute --remote-dir");
    }
    println!("Set up an SSH destination. Monitoring will not be started.");
    let host = if let Some(host) = host {
        config::validate_host(&host)?;
        host
    } else {
        choose_host(ctx, previous.as_ref())?
    };
    let mut directory = directory.or_else(|| {
        previous
            .as_ref()
            .filter(|p| p.ssh_host == host)
            .map(|p| p.remote_directory.clone())
    });
    if interactive {
        let default = directory
            .as_deref()
            .unwrap_or("remote home/.local/share/clipbridge/images");
        let input = prompt(ctx, &format!("Remote directory [Enter: {default}]: "))?;
        if !input.is_empty() {
            directory = Some(input);
        }
    }
    let mut new = if no_check {
        println!("SSH connection and destination permissions were not checked.");
        Config::new(host, directory.context("missing remote directory")?)?
    } else {
        println!("Checking SSH and preparing the destination...");
        config::check_destination(ctx, &host, directory.as_deref(), true)?
    };
    new.remote_clipboard = previous
        .as_ref()
        .is_some_and(|old| old.ssh_host == new.ssh_host && old.remote_clipboard);
    if let Some(old) = previous.as_ref().filter(|old| old.ssh_host == new.ssh_host) {
        new.ssh_aliases.clone_from(&old.ssh_aliases);
    }
    if ctx.cancelled.load(Ordering::SeqCst) {
        bail!("setup cancelled");
    }
    if let Some(bytes) = corrupt {
        let file_name = path
            .file_name()
            .context("configuration path has no filename")?
            .to_string_lossy();
        let backup = path.with_file_name(format!("{file_name}.backup-{}", common::unique_id()));
        common::atomic_write(&backup, &bytes, 0o600)?;
        println!("Preserved invalid configuration: {}", backup.display());
    }
    new.save(&path)?;
    println!(
        "Saved configuration: {}\nDestination: {}:{}",
        path.display(),
        new.ssh_host,
        new.remote_directory
    );
    if previous
        .as_ref()
        .is_some_and(|old| old.remote_clipboard && old.ssh_host != new.ssh_host)
    {
        println!(
            "SSH target changed; run clipbridge remote setup to enable its image clipboard support."
        );
    }
    println!(
        "Monitoring covers images from all applications. Start with clipbridge start or clipbridge run."
    );
    println!("Restart an existing monitor to apply changed settings.");
    Ok(())
}
fn doctor(ctx: &Context, path: &Path) -> Result<()> {
    for executable in [
        "/usr/bin/ssh",
        "/usr/bin/scp",
        "/usr/bin/osascript",
        "/usr/sbin/lsof",
        "/bin/ps",
    ] {
        if !Path::new(executable).is_file() {
            bail!("required macOS tool is missing: {executable}");
        }
    }
    let config = Config::load(path)?;
    let legacy = service::legacy_services(ctx)?;
    if !legacy.is_empty() {
        bail!(
            "older uploaders are loaded: {}. Stop them before starting ClipBridge",
            legacy.join(", ")
        );
    }
    let active = monitor::active_host(ctx, &config);
    let host = active.as_deref().unwrap_or(&config.ssh_host);
    println!("SSH route: {host}");
    config::check_destination(ctx, host, Some(&config.remote_directory), false)?;
    println!(
        "Configuration: {}\nSSH authentication and destination permissions: OK",
        path.display()
    );
    if active.is_some() {
        println!("Matching SSH session detected: {host} (process/socket heuristic).");
    } else {
        println!(
            "Waiting for a matching interactive SSH session. Configured aliases: {}",
            config.ssh_hosts().collect::<Vec<_>>().join(", ")
        );
    }
    println!("Doctor finished. No monitoring was started.");
    Ok(())
}
fn logs(ctx: &Context, count: usize, follow: bool) -> Result<()> {
    if !(1..=10000).contains(&count) {
        bail!("--lines must be between 1 and 10000");
    }
    let path = ctx.log_path();
    if !path.exists() {
        println!(
            "No background log yet: {}. Foreground logs appear in the terminal.",
            path.display()
        );
        return Ok(());
    }
    if follow {
        let mut child = std::process::Command::new("/usr/bin/tail")
            .args(["-n", &count.to_string(), "-F"])
            .arg(&path)
            .spawn()?;
        loop {
            if let Some(status) = child.try_wait()? {
                if !status.success() {
                    bail!("tail exited with {status}");
                }
                break;
            }
            if ctx.cancelled.load(Ordering::SeqCst) {
                if let Err(error) = child.kill()
                    && child.try_wait()?.is_none()
                {
                    return Err(error).context("could not stop log follower");
                }
                child.wait()?;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    } else {
        let mut lines = VecDeque::new();
        for line in io::BufReader::new(File::open(path)?).lines() {
            lines.push_back(line?);
            if lines.len() > count {
                lines.pop_front();
            }
        }
        for line in lines {
            println!("{line}");
        }
    }
    Ok(())
}
fn upgrade(ctx: &Context, source: &Path, prefix: Option<&Path>) -> Result<()> {
    let source = common::expand_path(source, &ctx.home)
        .canonicalize()
        .context("upgrade source directory is missing")?;
    if !source.join("Cargo.toml").is_file() {
        bail!("--from must name an extracted native bundle or Rust source directory");
    }
    let prefix = match prefix {
        Some(prefix) => common::absolute(&common::expand_path(prefix, &ctx.home))?,
        None => ctx
            .installed_root()?
            .and_then(|p| p.parent().and_then(Path::parent).map(Path::to_owned))
            .unwrap_or_else(|| ctx.home.join(".local")),
    };
    if !prefix
        .join("share/clipbridge/.clipbridge-install.json")
        .is_file()
    {
        bail!("ClipBridge is not installed at this prefix; use install.sh first");
    }
    let binary = if source.join("bundle.json").exists() {
        release::validate_native_bundle(&source)?;
        println!("Installing the verified native bundle; no compiler is needed...");
        source.join("clipbridge")
    } else {
        println!("Building the selected source before upgrading...");
        ctx.exec(
            CommandSpec::new("cargo")
                .args(["build", "--release", "--locked", "--manifest-path"])
                .args([source.join("Cargo.toml").as_os_str()])
                .args(["--target-dir"])
                .args([source.join("target").as_os_str()])
                .timeout(600),
        )?;
        source.join("target/release/clipbridge")
    };
    if ctx.cancelled.load(Ordering::SeqCst) {
        bail!("upgrade cancelled before installation");
    }
    let output = ctx.exec(
        CommandSpec::new(binary)
            .args(["install", "--source"])
            .args([source.as_os_str()])
            .args(["--prefix"])
            .args([prefix.as_os_str()])
            .args(["--require-existing"])
            .timeout(600)
            .cooperative(),
    )?;
    print!("{}", output.stdout);
    Ok(())
}
fn execute(cli: Cli, ctx: &Context) -> Result<()> {
    let path = config::selected_path(ctx, cli.config.as_deref());
    match cli.command {
        Action::Install {
            source,
            prefix,
            require_existing,
        } => {
            let source = source
                .or_else(|| ctx.source.clone())
                .context("install from a trusted source directory using ./install.sh")?;
            if source.join("bundle.json").exists() {
                release::validate_native_bundle(&source)?;
            }
            println!(
                "{}",
                clipbridge::installer::install(ctx, &source, prefix.as_deref(), require_existing)?
            );
        }
        Action::Upgrade { source, prefix } => upgrade(ctx, &source, prefix.as_deref())?,
        Action::Uninstall { prefix } => println!(
            "{}",
            clipbridge::installer::uninstall(ctx, prefix.as_deref())?
        ),
        Action::Configure {
            host,
            remote_dir,
            no_check,
        } => configure(ctx, cli.config.as_deref(), host, remote_dir, no_check)?,
        Action::Hosts { json } => hosts(ctx, &path, json)?,
        Action::Setup {
            host,
            remote_dir,
            start,
            no_start,
            no_shell,
            build_from_source,
            assets_dir,
        } => {
            guided_setup(
                ctx,
                &path,
                SetupInput {
                    host,
                    remote_dir,
                    start,
                    no_start,
                    no_shell,
                    build_from_source,
                    assets_dir,
                },
            )?;
        }
        Action::Doctor => doctor(ctx, &path)?,
        Action::Remote { command } => println!(
            "{}",
            match command {
                RemoteAction::Setup {
                    no_shell,
                    build_from_source,
                    assets_dir,
                } => remote::setup_with_options(
                    ctx,
                    &path,
                    !no_shell,
                    if build_from_source {
                        remote::SetupMode::BuildFromSource
                    } else {
                        remote::SetupMode::Prebuilt
                    },
                    assets_dir
                        .as_ref()
                        .map(|path| common::expand_path(path, &ctx.home))
                        .as_deref()
                )?,
                RemoteAction::Doctor => remote::doctor(ctx, &path)?,
                RemoteAction::Status => remote::status(ctx, &path)?,
                RemoteAction::Stop => remote::stop(ctx, &path)?,
            }
        ),
        Action::Start => println!("{}", service::start(ctx, &path)?),
        Action::Restart => println!("{}", service::restart(ctx, &path)?),
        Action::Stop => println!("{}", service::stop(ctx)?),
        Action::Status => println!("{}", service::status(ctx)?),
        Action::Run => {
            service::check_foreground_available(ctx)?;
            monitor::run(ctx, &path, true)?;
        }
        Action::Monitor => monitor::run(ctx, &path, false)?,
        Action::Logs { lines, follow } => logs(ctx, lines, follow)?,
        Action::ClipboardSelfTest => clipbridge::clipboard::self_test()?,
    }
    Ok(())
}
fn main() {
    let cli = Cli::parse();
    if matches!(cli.command, Action::ClipboardSelfTest) {
        match clipbridge::clipboard::self_test() {
            Ok(()) => {}
            Err(error) => {
                eprintln!("Error: {error:#}");
                std::process::exit(1);
            }
        }
        return;
    }
    let context = match Context::discover() {
        Ok(ctx) => ctx,
        Err(error) => {
            eprintln!("Error: {error:#}");
            std::process::exit(1);
        }
    };
    let cancelled = Arc::clone(&context.cancelled);
    if let Err(error) = ctrlc::set_handler(move || {
        cancelled.store(true, Ordering::SeqCst);
    }) {
        eprintln!("Error: could not install shutdown handler: {error}");
        std::process::exit(1);
    }
    if let Err(error) = execute(cli, &context) {
        eprintln!("Error: {error:#}");
        std::process::exit(if context.cancelled.load(Ordering::SeqCst) {
            130
        } else {
            1
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clipbridge::common::{CommandOutput, Runner};
    use std::sync::atomic::AtomicBool;

    struct NoCommands;
    impl Runner for NoCommands {
        fn run(&self, _: &CommandSpec, _: &AtomicBool) -> Result<CommandOutput> {
            panic!("Offline configuration must not invoke external commands");
        }
    }

    #[test]
    fn reconfigure_preserves_remote_clipboard_only_for_same_host() {
        for host in ["original", "different"] {
            let home = tempfile::tempdir().unwrap();
            let ctx = Context {
                home: home.path().to_owned(),
                executable: home.path().join("clipbridge"),
                source: None,
                uid: 501,
                cancelled: Arc::new(AtomicBool::new(false)),
                runner: Arc::new(NoCommands),
            };
            let path = ctx.default_config();
            let mut initial = Config::new("original", "/tmp/original").unwrap();
            initial.remote_clipboard = true;
            initial.ssh_aliases = vec!["original.local".into()];
            initial.save(&path).unwrap();
            configure(&ctx, None, Some(host.into()), Some("/tmp/new".into()), true).unwrap();
            let saved = Config::load(&path).unwrap();
            assert_eq!(saved.ssh_host, host);
            assert_eq!(saved.remote_directory, "/tmp/new");
            assert_eq!(saved.remote_clipboard, host == "original");
            assert_eq!(
                saved.ssh_aliases,
                if host == "original" {
                    vec!["original.local".to_owned()]
                } else {
                    Vec::new()
                }
            );
        }
    }

    #[test]
    fn remote_subcommands_accept_global_config() {
        for action in ["setup", "doctor", "status", "stop"] {
            let cli = Cli::try_parse_from([
                "clipbridge",
                "remote",
                action,
                "--config",
                "/tmp/settings.json",
            ])
            .unwrap();
            assert!(matches!(cli.command, Action::Remote { .. }));
            assert_eq!(cli.config, Some(PathBuf::from("/tmp/settings.json")));
        }
        assert!(Cli::try_parse_from(["clipbridge", "remote"]).is_err());
        assert!(matches!(
            Cli::try_parse_from(["clipbridge", "remote", "setup", "--no-shell"])
                .unwrap()
                .command,
            Action::Remote {
                command: RemoteAction::Setup { no_shell: true, .. }
            }
        ));
    }

    #[test]
    fn setup_parser_requires_unambiguous_startup_and_distribution_options() {
        for command in [
            vec!["clipbridge", "setup"],
            vec!["clipbridge", "remote", "setup"],
        ] {
            let mut conflicting = command.clone();
            conflicting.extend(["--build-from-source", "--assets-dir", "/tmp/assets"]);
            assert!(Cli::try_parse_from(conflicting).is_err());
            let mut native = command;
            native.extend(["--assets-dir", "/tmp/assets"]);
            assert!(Cli::try_parse_from(native).is_ok());
        }
        assert!(
            Cli::try_parse_from([
                "clipbridge",
                "setup",
                "--host",
                "linux-prod",
                "--start",
                "--no-start"
            ])
            .is_err()
        );
        for option in ["--start", "--no-start"] {
            assert!(
                Cli::try_parse_from(["clipbridge", "setup", "--host", "linux-prod", option])
                    .is_ok()
            );
        }
    }

    #[test]
    fn native_upgrade_runs_the_candidate_installer_without_building_source() {
        use sha2::{Digest, Sha256};
        use std::sync::Mutex;
        #[derive(Default)]
        struct NativeInstaller(Mutex<Vec<CommandSpec>>);
        impl Runner for NativeInstaller {
            fn run(&self, spec: &CommandSpec, _: &AtomicBool) -> Result<CommandOutput> {
                self.0.lock().unwrap().push(spec.clone());
                Ok(CommandOutput::default())
            }
        }
        let home = tempfile::tempdir().unwrap();
        let bundle = home.path().join("native bundle");
        fs::create_dir_all(bundle.join("src")).unwrap();
        for name in [
            "Cargo.toml",
            "Cargo.lock",
            "VERSION",
            "LICENSE",
            "src/config.example.json",
        ] {
            fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR")).join(name),
                bundle.join(name),
            )
            .unwrap();
        }
        let executable = bundle.join("clipbridge");
        fs::write(&executable, b"candidate native executable").unwrap();
        fs::write(
            bundle.join("bundle.json"),
            serde_json::to_vec(&serde_json::json!({
                "product": "clipbridge", "schema": 1, "version": env!("CARGO_PKG_VERSION"),
                "target": format!("{}-apple-darwin", std::env::consts::ARCH),
                "binary_sha256": format!("{:x}", Sha256::digest(fs::read(&executable).unwrap()))
            }))
            .unwrap(),
        )
        .unwrap();
        let prefix = home.path().join("installed prefix");
        fs::create_dir_all(prefix.join("share/clipbridge")).unwrap();
        fs::write(
            prefix.join("share/clipbridge/.clipbridge-install.json"),
            b"{}",
        )
        .unwrap();
        let runner = Arc::new(NativeInstaller::default());
        let ctx = Context {
            home: home.path().to_owned(),
            executable: home.path().join("old-clipbridge"),
            source: None,
            uid: 501,
            cancelled: Arc::new(AtomicBool::new(false)),
            runner: runner.clone(),
        };
        upgrade(&ctx, &bundle, Some(&prefix)).unwrap();
        let calls = runner.0.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].program,
            bundle.canonicalize().unwrap().join("clipbridge")
        );
        assert_eq!(calls[0].args[0], "install");
        assert!(calls[0].args.iter().any(|arg| arg == "--require-existing"));
        assert!(!calls[0].args.iter().any(|arg| arg == "build"));
    }
}
