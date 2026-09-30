# ClipBridge

Last updated: 2026-09-30.

Copy an image on your Mac, upload it to a Linux or WSL host over SSH, and paste its remote file path into your workflow.

ClipBridge is a command-line toolkit configured with a JSON file. It runs in the foreground or as a per-user background service. The source is available for development and trials; there is no packaged release or installer yet.

## Quick Start

Requirements: macOS, Python 3.9+, Swift Command Line Tools with `make`, and a working SSH alias with noninteractive authentication and a verified host key. Run the commands below from this checkout.

Create a configuration file without replacing existing settings:

```sh
mkdir -p ~/.config/clipbridge
cp -n src/config.example.json ~/.config/clipbridge/config.json
chmod 600 ~/.config/clipbridge/config.json
```

Edit `~/.config/clipbridge/config.json`, replacing both example values:

```json
{
  "ssh_host": "dev-server",
  "remote_directory": "/home/example/.local/share/clipbridge/images"
}
```

`ssh_host` is the alias you use to connect. `remote_directory` is the image destination **on that remote host**. It is currently required and must be an absolute path; uploads attempt to create it if missing. See the [configuration reference](docs/usage.md) for accepted values.

Keep an interactive `ssh YOUR_ALIAS` session open. Enable background monitoring and startup at login:

```sh
./clipbridge start
```

Newly copied images from **all applications** are eligible. After a successful upload, ClipBridge checks whether the clipboard changed before replacing its content with the remote path. Existing images at startup and images copied while no matching SSH session is detected are skipped. This transfers image files; it does not synchronize the remote operating system's clipboard.

Neither the `configure` wizard nor `doctor` is required before starting. They are optional setup and diagnostic tools.

## Everyday Commands

| Command | Action |
| --- | --- |
| `./clipbridge start` | Start background monitoring, switch from a cooperating foreground monitor, or report the existing background instance |
| `./clipbridge run` | Monitor in the foreground; stop with Ctrl+C |
| `./clipbridge restart` | Reload settings by restarting background monitoring |
| `./clipbridge stop` | Stop the background service and disable startup at login |
| `./clipbridge status` | Show monitor/service state |
| `./clipbridge logs --follow` | Follow background upload logs |
| `./clipbridge doctor` | Optionally check tools, SSH access and existing directory permissions |

Repeated `start` with the same configuration does not restart an active background service. Configuration edits require `restart`, or stopping and rerunning foreground mode. See the [user guide](docs/usage.md) for handoff behavior, custom configuration paths, migration and troubleshooting.

## Repository Guide

| File or directory | Purpose |
| --- | --- |
| [docs/usage.md](docs/usage.md) | Configuration, commands, runtime behavior and troubleshooting |
| [src/README.md](src/README.md) | Source layout, build commands, tests and implementation boundaries |
| [docs/planning/product-direction-and-distribution.md](docs/planning/product-direction-and-distribution.md) | Product decisions, release requirements and future options |
| [AGENTS.md](AGENTS.md) | Contributor and coding-agent guidelines |
| [clipbridge](clipbridge) | Repository command-line entry point |

## Development

```sh
make -C src build
make -C src test
```

Tests use mocked SSH/launchd operations, isolated subprocesses and a private test pasteboard. They do not upload your clipboard or install a background service. Release preparation still includes full real-host transfer and service-lifecycle checks, plus installation on another Mac.

## License

[MIT](LICENSE).
