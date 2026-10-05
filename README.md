# ClipBridge

Last updated: 2026-10-03.

Copy an image on your Mac and keep pasting it as an image locally. ClipBridge uploads a copy over SSH and can make it available to a remote CLI through a private Linux image clipboard.

ClipBridge 0.3.1 is implemented in Rust, including native macOS clipboard access and the optional Linux companion. The installed Mac program is one native executable with no language runtime requirement. Remote image pasting additionally needs Xvfb, xauth and xclip on the SSH host; see [remote image setup](docs/remote-images.md).

## Install or Migrate from Python 0.1

Source installation requires macOS, Rust 1.94+ with Cargo, and Apple's Command Line Tools/linker SDK. From a trusted checkout or extracted source package:

```sh
./install.sh
export PATH="$HOME/.local/bin:$PATH"
clipbridge --version
```

If needed, add the PATH export to your shell startup file. No `sudo` is needed. Existing configuration is preserved, and an already loaded service from this checkout or the managed installation is migrated. Fresh installation does not enable monitoring.

**For migration from Python 0.1, run this new `install.sh`.** The older installed Python `upgrade` command cannot install a Rust-only source tree. Existing Rust installations use `clipbridge upgrade --from PATH`.

## Configure and Start

Edit `~/.config/clipbridge/config.json`. Installation preserves an existing file or creates a sample:

```json
{
  "ssh_host": "dev-server",
  "remote_directory": "/home/example/.local/share/clipbridge/images"
}
```

Replace both values. `ssh_host` is your SSH alias, with noninteractive authentication and a verified host key. `remote_directory` is a required absolute path on that remote host; uploads attempt to create it if needed.

Keep a matching interactive `ssh YOUR_ALIAS` session open, then:

```sh
clipbridge start
```

Newly copied images from **all applications** are eligible. ClipBridge never changes the Mac clipboard, so local image paste remains available. Startup clipboard contents and images observed without a matching session are skipped. With the configuration above, images are uploaded only; paths appear in logs instead of replacing the clipboard. The setup wizard and `doctor` are optional.

## Paste Images in a Remote CLI

Prepare the remote Linux dependencies and Rust 1.94+/Cargo on both hosts as described in the [remote image guide](docs/remote-images.md). Setup vendors dependencies on the Mac, then builds offline on Linux. On your Mac:

```sh
clipbridge remote setup
clipbridge restart
```

Setup installs shell integration for remote Bash or Zsh. Reconnect SSH once after setup, then start the desired CLI normally inside an ordinary `ssh dev-server` session:

```sh
codex
# Or:
claude
```

Arguments work as usual, for example `codex resume`. The shell integration obtains the private clipboard connection for each launch without changing the parent shell's display settings. Existing aliases/functions are preserved; see the [manual fallback](docs/remote-images.md#manual-launch-and-custom-shells) if you see a conflict warning or your shell is unsupported.

Copy a new image and wait for the remote-ready notification or log entry before using the CLI's image-paste shortcut. Exit existing CLI processes once and relaunch them after reconnecting. Ghostty keybindings are unchanged. Real Ghostty/CLI attachment acceptance testing remains pending.

## Everyday Commands

| Command | Action |
| --- | --- |
| `clipbridge start` | Start background monitoring or report the existing instance; switch from a cooperating foreground monitor |
| `clipbridge run` | Monitor in the foreground; Ctrl+C stops intake and drains work |
| `clipbridge restart` | Restart background monitoring with changed settings |
| `clipbridge stop` | Stop the background service and disable login startup |
| `clipbridge status` | Show monitor/service state |
| `clipbridge logs --follow` | Follow background logs |
| `clipbridge doctor` | Optionally check SSH access and existing directory permissions |
| `clipbridge remote doctor` | Check remote image clipboard dependencies |
| `clipbridge remote status` | Show remote helper state |

Repeated `start` does not interrupt a service using the same configuration file. See the [usage guide](docs/usage.md) for handoff behavior, queue limits, custom configuration and troubleshooting.

## Upgrade and Uninstall

```sh
clipbridge upgrade --from /path/to/new-rust-source
clipbridge uninstall
```

Upgrade compiles the selected source before invoking its installer, preserves configuration and attempts recovery if activation fails. Uninstall removes the owned program and service while keeping configuration, logs and images. See the [installation guide](docs/installation.md) for prerequisites, custom prefixes and recovery limits.

## Repository Guide

| File or directory | Purpose |
| --- | --- |
| [docs/installation.md](docs/installation.md) | Installation, migration, upgrades, removal and source distributions |
| [docs/usage.md](docs/usage.md) | Configuration, commands, runtime behavior and troubleshooting |
| [docs/remote-images.md](docs/remote-images.md) | Linux companion setup and image pasting in SSH CLI sessions |
| [src/README.md](src/README.md) | Rust modules, build commands, tests and implementation boundaries |
| [Product plan](docs/planning/product-direction-and-distribution.md) | Remaining release validation and future options |
| [AGENTS.md](AGENTS.md) | Contributor and coding-agent guidelines |

## Development and Packaging

```sh
make build
make check
make test
make package
```

The checks cover both Rust crates. Native clipboard tests use a private named pasteboard; remote tests use isolated state and fake X tools. Service and network tests are isolated; installation tests verify the executable after deleting their source fixture. `make package` creates an allowlisted source archive and checksum under `dist/`, without publishing a release.

## License

[MIT](LICENSE).
