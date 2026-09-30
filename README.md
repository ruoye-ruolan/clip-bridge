# ClipBridge

Last updated: 2026-09-30.

Copy an image on your Mac, upload it to a Linux or WSL host over SSH, and paste its remote file path into your workflow.

ClipBridge is a user-installable command-line toolkit with JSON configuration, foreground/background operation, upgrades and removal. Installation builds the native helpers and copies the program out of the source checkout. Python 3.9+ remains required at runtime.

## Install

On macOS with Python 3.9+, `make` and Swift Command Line Tools, run from a trusted checkout or extracted source package:

```sh
./install.sh
export PATH="$HOME/.local/bin:$PATH"
clipbridge --version
```

If needed, add the PATH export to your shell startup file for future terminals. No `sudo` is needed. Fresh installation does not start monitoring; an already loaded service from this checkout or a managed installation is migrated with its existing configuration.

## Configure and Start

Edit `~/.config/clipbridge/config.json`. Installation preserves an existing file or creates a sample:

```json
{
  "ssh_host": "dev-server",
  "remote_directory": "/home/example/.local/share/clipbridge/images"
}
```

Replace both values. `ssh_host` is your SSH alias, using noninteractive authentication and a verified host key. `remote_directory` is a required absolute destination on that remote host; uploads attempt to create it if needed.

Keep a matching interactive `ssh YOUR_ALIAS` session open, then:

```sh
clipbridge start
```

Newly copied images from **all applications** are eligible. After uploading, ClipBridge checks whether the clipboard changed before replacing it with the remote path. Existing images at startup and images observed without a matching session are skipped. This transfers files; it does not synchronize the remote operating system's clipboard. The setup wizard and `doctor` diagnostics are optional.

## Everyday Commands

| Command | Action |
| --- | --- |
| `clipbridge start` | Start background monitoring or report the existing instance; switch from a cooperating foreground monitor |
| `clipbridge run` | Monitor in the foreground; stop with Ctrl+C |
| `clipbridge restart` | Reload settings by restarting background monitoring |
| `clipbridge stop` | Stop the background service and disable login startup |
| `clipbridge status` | Show monitor/service state |
| `clipbridge logs --follow` | Follow background logs |
| `clipbridge doctor` | Optionally check tools, SSH access and existing directory permissions |

Repeated `start` does not interrupt a running service using the same configuration file. See the [usage guide](docs/usage.md) for handoff behavior, custom configuration and troubleshooting.

## Upgrade and Uninstall

```sh
clipbridge upgrade --from /path/to/new-source-or-extracted-package
clipbridge uninstall
```

Upgrade builds the new version before switching, preserves configuration and attempts to restore the previous version if activation fails. Uninstall removes the managed program and service, keeping configuration, logs and images. See the [installation guide](docs/installation.md) for runtime requirements, custom prefixes and recovery behavior.

## Repository Guide

| File or directory | Purpose |
| --- | --- |
| [docs/installation.md](docs/installation.md) | Installation, upgrades, removal and source distributions |
| [docs/usage.md](docs/usage.md) | Configuration, commands, runtime behavior and troubleshooting |
| [src/README.md](src/README.md) | Source layout, build commands, tests and implementation boundaries |
| [Product plan](docs/planning/product-direction-and-distribution.md) | Release requirements and future options |
| [AGENTS.md](AGENTS.md) | Contributor and coding-agent guidelines |

## Development and Packaging

```sh
make build
make test
make package
```

`make package` produces a versioned source archive and checksum under `dist/`; it does not publish a release. Tests isolate service/network operations and use a private test pasteboard. Installation lifecycle tests compile real helpers in temporary prefixes and verify operation after deleting the source tree. Remote transfer reliability and broader machine/OS coverage remain release-validation work.

## License

[MIT](LICENSE).
