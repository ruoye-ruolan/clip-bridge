# ClipBridge

Last updated: 2026-09-30.

Copy an image on your Mac, upload it to a Linux or WSL host over SSH, and paste its remote file path into your workflow.

ClipBridge 0.2 is implemented entirely in Rust, including native macOS clipboard access. The installed program is one native executable: it needs no Python, Swift helpers or Rust toolchain at runtime. It uses the system SSH/SCP clients and your existing SSH configuration.

## Install or Migrate from Python 0.1

Source installation requires macOS, Rust 1.94+ with Cargo, and Apple's Command Line Tools/linker SDK. From a trusted checkout or extracted source package:

```sh
./install.sh
export PATH="$HOME/.local/bin:$PATH"
clipbridge --version
```

If needed, add the PATH export to your shell startup file. No `sudo` is needed. Existing configuration is preserved, and an already loaded service from this checkout or the managed installation is migrated. Fresh installation does not enable monitoring.

**For the first Python 0.1 → Rust 0.2 migration, run this new `install.sh`.** The older installed Python `upgrade` command cannot install a Rust-only source tree. Once migrated, future Rust upgrades use `clipbridge upgrade --from PATH`.

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

Newly copied images from **all applications** are eligible. After upload, ClipBridge checks for newer clipboard content before writing the remote path. Startup clipboard contents and images observed without a matching session are skipped. This transfers image files; it does not synchronize the remote operating system's clipboard. The setup wizard and `doctor` are optional.

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

The checks run rustfmt, Clippy and Rust tests. Native clipboard tests use a private named pasteboard. Service and network tests are isolated; installation tests copy the real executable and verify it after deleting their source fixture. `make package` creates an allowlisted source archive and checksum under `dist/`, without publishing a release.

## License

[MIT](LICENSE).
