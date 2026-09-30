# ClipBridge macOS Command-Line Toolkit

This developer toolkit uploads newly copied images to an SSH host and returns their remote paths. Configuration and service management use a single `clipbridge` command. There is no packaged release yet; run it from this checkout.

## Requirements

- macOS with a logged-in graphical user session.
- Python 3.9 or later (standard library only).
- Xcode or Command Line Tools providing `swiftc` and `make`.
- A Linux or WSL SSH destination, configured with a literal alias in `~/.ssh/config`.
- Noninteractive SSH authentication, such as a key available to the SSH agent, and a verified host key.

If tools are missing, install Command Line Tools with `xcode-select --install`. Before setup, connect using your normal `ssh YOUR_ALIAS` command to verify the server identity and authentication. ClipBridge will not accept unknown host keys automatically or prompt for an SSH password.

Development checks have run with Python 3.14.7, Swift 6.4 and macOS 27.0.1 on Apple silicon. Other versions and Intel Macs remain unverified.

## Quick Start

From the repository root:

```sh
./clipbridge configure
./clipbridge doctor
```

The wizard lists literal SSH aliases from your SSH configuration and included files. Choose a number or enter an alias manually. Press Enter for the default destination: `.local/share/clipbridge/images` under the **remote** user's home directory. Setup checks SSH access, creates the directory if necessary and creates/removes a temporary write probe before saving locally. It never starts clipboard monitoring.

Keep an interactive `ssh YOUR_ALIAS` session open. To enable background uploads and automatic startup at login:

```sh
./clipbridge start
./clipbridge status
```

The first start compiles the Swift helpers and generates the LaunchAgent automatically. Copy a test image, then paste the resulting remote path. Monitoring includes images from **all applications**. A successful upload can replace the clipboard with a file path.

To stop background uploads and disable startup at login:

```sh
./clipbridge stop
```

Use `./clipbridge run` instead of `start` for foreground monitoring; stop it with Ctrl+C. Foreground logs appear in that terminal. `stop` manages the background service only.

## Commands

| Command | Purpose |
| --- | --- |
| `./clipbridge configure` | Choose or change the SSH destination; preserves the saved configuration if checks fail |
| `./clipbridge doctor` | Check tools, configuration, old-service conflicts, authentication and directory permissions without starting monitoring |
| `./clipbridge start` | Build helpers, start the background monitor and enable startup at login |
| `./clipbridge stop` | Unload the background monitor and remove its login plist; keep configuration and uploads |
| `./clipbridge status` | Show monitor/service state; this is not a connectivity or upload-health check |
| `./clipbridge logs` | Show the last 50 background log lines |
| `./clipbridge logs --follow` | Follow the background log |
| `./clipbridge run` | Build helpers and monitor in the foreground |

`doctor` checks an existing remote directory without creating it. An absent interactive SSH session is reported as waiting, because reachability and an active session are different conditions. Run `configure` to create a missing destination.

## Configuration and Migration

Settings live in `~/.config/clipbridge/config.json`, outside the checkout. The wizard saves atomically with owner-only permissions. Rerun `configure` to change settings, then stop/start an existing monitor to apply them. If an existing configuration is invalid, successful setup preserves its bytes in an owner-only `config.json.backup-*` file before replacing it.

For scripted setup:

```sh
./clipbridge configure --host dev-server
./clipbridge configure --host dev-server --remote-dir /home/example/images
```

Replace the sample alias and path with your destination. To save without network access, pass both `--host`, an absolute `--remote-dir`, and `--no-check`. Run `doctor` before starting once access is available.

All commands accept `--config PATH` for another configuration file, before or after the subcommand. Explicit directories currently support ASCII letters, digits, underscores, dots, slashes and hyphens. Spaces, `..`, tilde expansion and shell expressions are unsupported. Enter accepts the displayed default; a new SSH target defaults to its remote home directory.

The earlier `prototype/macos/config.json` is still recognized when the default user configuration does not exist. `configure` uses those settings as defaults and saves a new user configuration without deleting the old file. Custom locations other than the default user configuration path do not use this fallback.

Older uploaders must be stopped before starting this version. The toolkit detects loaded `local.codex.xnip-wsl` and `local.clipbridge.prototype` services and refuses to run alongside them. It does not change those services. If migrating from either one, unload its exact label; for example:

```sh
launchctl bootout "gui/$(id -u)/local.codex.xnip-wsl"
```

Remove or move that older service's plist out of `~/Library/LaunchAgents/` if it should no longer start at login. New instances also check a per-user monitor lock. Keep the checkout at a stable path while its background service is installed. After moving it or replacing the Python installation, run `stop`, then `start` from the new location.

## Files and Logs

- `auto_upload.py`: clipboard events, session detection and sequential uploads.
- `configuration.py`: validation, alias discovery, remote checks and private config saving.
- `cli.py` and `service.py`: command interface and launchd management.
- `swift/`: native clipboard helpers; generated binaries live in ignored `build/`.
- `tests/`: unit tests and a private-pasteboard integration test.
- `~/Library/LaunchAgents/local.clipbridge.plist`: generated background service.
- `~/Library/Logs/ClipBridge/clipbridge.log`: background log, including uploaded paths.
- `~/Library/Caches/clipbridge/auto/`: upload staging and failed-image retention.

Failed transfers retain images under `upload-*`; successful transfers remove their temporary local copies. There is no automatic cleanup policy or retry command. Inspect logs and clean retained files manually while stopped. Remote files are not automatically removed, and interrupted SCP transfers may leave partial files.

## Development and Validation

```sh
make -C prototype/macos build
make -C prototype/macos test
```

Tests do not upload files, install services or touch the general clipboard. Python tests mock SSH and launchctl and exercise configuration, migration, error handling and service lifecycle. Swift tests use a private named pasteboard for image capture and stale-image rejection. The remote setup shell fragment is also tested in a temporary local directory.

Real SSH uploads and launchd startup under the new commands still require end-to-end testing, followed by installation on another Mac, before a release.

## Prototype Boundaries

Existing clipboard content is ignored on startup. Offline images are skipped rather than deferred. Captures that become stale before extraction are skipped. Uploads run sequentially, rechecking the session when each job begins.

Session detection looks for a same-user `ssh ALIAS` process with an established TCP socket. It is a heuristic, not proof of authentication. SSH multiplexing, jump hosts, remote-command sessions and IDE-managed connections are not guaranteed. Alias discovery provides suggestions without evaluating `Match` rules; manual entry remains available.

Clipboard polling can miss rapid changes. The upload queue is unbounded. Checking the clipboard change count and writing a path are not atomic, leaving a small race window. Foreground shutdown may finish already queued uploads. No GUI, history browser, automatic updater or bundled Python runtime is included.

The original import came from the active `codex-shot` Python/Swift prototype. Obsolete folder-watching code, Xnip-only source filtering, backups and compiled binaries were excluded. The original local installation remains separate.
