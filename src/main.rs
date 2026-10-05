use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand};
use clipbridge::{
    common::{self, CommandSpec, Context},
    config::{self, Config},
    monitor, remote, service,
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
    #[command(about = "Install this compiled version from a trusted source tree")]
    Install {
        #[arg(long)]
        prefix: Option<PathBuf>,
        #[arg(long, hide = true)]
        source: Option<PathBuf>,
        #[arg(long, hide = true)]
        require_existing: bool,
    },
    #[command(about = "Build and install a newer trusted source checkout")]
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
        let aliases = config::discover_ssh_aliases(&ctx.home.join(".ssh/config"), &ctx.home);
        for (i, alias) in aliases.iter().enumerate() {
            println!("  {}. {alias}", i + 1);
        }
        let default = previous
            .as_ref()
            .map(|c| c.ssh_host.as_str())
            .or_else(|| (aliases.len() == 1).then(|| aliases[0].as_str()))
            .unwrap_or("");
        loop {
            let input = prompt(ctx, &format!("SSH alias (name or number) [{default}]: "))?;
            let mut chosen = if input.is_empty() {
                default.to_owned()
            } else {
                input
            };
            if let Ok(index) = chosen.parse::<usize>()
                && index > 0
                && index <= aliases.len()
            {
                chosen = aliases[index - 1].clone();
            }
            match config::validate_host(&chosen) {
                Ok(()) => break chosen,
                Err(error) => println!("{error}"),
            }
        }
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
    config::check_destination(ctx, &config.ssh_host, Some(&config.remote_directory), false)?;
    println!(
        "Configuration: {}\nSSH authentication and destination permissions: OK",
        path.display()
    );
    if monitor::connected(ctx, &config.ssh_host) {
        println!("Matching SSH session detected (process/socket heuristic).");
    } else {
        println!(
            "Waiting for a matching interactive SSH session; open: ssh {}",
            config.ssh_host
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
        bail!("--from must name an extracted Rust source directory");
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
    println!("Building the selected source before upgrading...");
    ctx.exec(
        CommandSpec::new("cargo")
            .args(["build", "--release", "--locked", "--manifest-path"])
            .args([source.join("Cargo.toml").as_os_str()])
            .args(["--target-dir"])
            .args([source.join("target").as_os_str()])
            .timeout(600),
    )?;
    if ctx.cancelled.load(Ordering::SeqCst) {
        bail!("upgrade cancelled before installation");
    }
    let output = ctx.exec(
        CommandSpec::new(source.join("target/release/clipbridge"))
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
        Action::Doctor => doctor(ctx, &path)?,
        Action::Remote { command } => println!(
            "{}",
            match command {
                RemoteAction::Setup { no_shell } =>
                    remote::setup_with_shell(ctx, &path, !no_shell)?,
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
            initial.save(&path).unwrap();
            configure(&ctx, None, Some(host.into()), Some("/tmp/new".into()), true).unwrap();
            let saved = Config::load(&path).unwrap();
            assert_eq!(saved.ssh_host, host);
            assert_eq!(saved.remote_directory, "/tmp/new");
            assert_eq!(saved.remote_clipboard, host == "original");
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
                command: RemoteAction::Setup { no_shell: true }
            }
        ));
    }
}
