# ClipBridge macOS Command-Line Toolkit

This developer toolkit uploads newly copied images to an SSH host and returns their remote paths. Edit a JSON configuration file and use `clipbridge` commands to check it and manage the service. There is no packaged release yet; run it from this checkout.

## Requirements

- macOS with a logged-in graphical user session.
- Python 3.9 or later (standard library only).
- Xcode or Command Line Tools providing `swiftc` and `make`.
- A Linux or WSL SSH destination, configured with a literal alias in `~/.ssh/config`.
- Noninteractive SSH authentication, such as a key available to the SSH agent, and a verified host key.

If tools are missing, install Command Line Tools with `xcode-select --install`. Before setup, connect using your normal `ssh YOUR_ALIAS` command to verify the server identity and authentication. ClipBridge will not accept unknown host keys automatically or prompt for an SSH password.

Development checks have run with Python 3.14.7, Swift 6.4 and macOS 27.0.1 on Apple silicon. Other versions and Intel Macs remain unverified.

## Configure with a File

From the repository root, copy the example without overwriting existing settings:

```sh
mkdir -p ~/.config/clipbridge
cp -n prototype/macos/config.example.json ~/.config/clipbridge/config.json
chmod 600 ~/.config/clipbridge/config.json
```

Open `~/.config/clipbridge/config.json` in your preferred editor. It contains two fields:

```json
{
  "ssh_host": "dev-server",
  "remote_directory": "/home/example/.local/share/clipbridge/images"
}
```

| Field | Value |
| --- | --- |
| `ssh_host` | The literal SSH alias you use in `ssh ALIAS`, as configured in `~/.ssh/config` |
| `remote_directory` | An absolute directory on that remote host, owned by or writable to the SSH user |

Replace both example values. Use valid JSON with double quotes and no comments or trailing commas. Direct file configuration requires an explicit absolute remote path; it does not expand `~`, `$HOME` or environment variables. No credentials belong in this file: authentication continues to use your SSH configuration and agent.

Create the remote directory before running `doctor`. For the sample above, the command would be:

```sh
ssh dev-server 'umask 077; mkdir -p -- /home/example/.local/share/clipbridge/images'
```

Substitute your actual alias and directory in that command too. Then keep an interactive `ssh YOUR_ALIAS` session open and run:

```sh
./clipbridge doctor
./clipbridge run
```

`doctor` checks without changing the remote directory. `run` compiles helpers as needed and starts foreground monitoring, with logs in the terminal. Copy a test image and paste the resulting remote path. Monitoring includes newly copied images from **all applications**. Use Ctrl+C to stop.

To run in the background and automatically at login, stop the foreground copy first, then:

```sh
./clipbridge start
./clipbridge status
```

The toolkit generates the LaunchAgent automatically. Use `./clipbridge stop` to stop background uploads and disable startup at login. That command does not stop a foreground monitor.

Settings are loaded at startup, with no automatic reload. After editing, restart foreground monitoring, or run `./clipbridge stop` followed by `./clipbridge start` for the background service.

For a configuration stored elsewhere, specify it consistently:

```sh
./clipbridge --config /absolute/path/to/config.json doctor
./clipbridge --config /absolute/path/to/config.json run
```

## Optional Setup Wizard

If you prefer guided setup, run `./clipbridge configure`. It writes the same configuration file, lists literal SSH aliases from your SSH configuration and included files, and lets you choose a number or enter an alias. For a new target, Enter selects `.local/share/clipbridge/images` under the remote user's home directory.

The wizard checks SSH access, creates the directory if necessary and creates/removes a temporary write probe before saving locally. It never starts clipboard monitoring. Editing the file directly does not require running this wizard.

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

`doctor` checks an existing remote directory without creating it. An absent interactive SSH session is reported as waiting, because reachability and an active session are different conditions. Create a missing destination over SSH as shown above, or use the optional `configure` wizard.

## Configuration and Migration

Settings live in `~/.config/clipbridge/config.json`, outside the checkout. Edit the file directly or rerun the optional `configure` wizard, then restart an existing monitor to apply changes. The wizard saves atomically with owner-only permissions. If an existing configuration is invalid, successful setup preserves its bytes in an owner-only `config.json.backup-*` file before replacing it.

For scripted setup:

```sh
./clipbridge configure --host dev-server
./clipbridge configure --host dev-server --remote-dir /home/example/images
```

Replace the sample alias and path with your destination. To save without network access, pass both `--host`, an absolute `--remote-dir`, and `--no-check`. Run `doctor` before starting once access is available.

All commands accept `--config PATH` before or after the subcommand. Relative paths use the current working directory. `stop`, `status` and `logs` still manage the single per-user service; selecting another config does not create an independent service. Explicit directories currently support ASCII letters, digits, underscores, dots, slashes and hyphens. Spaces, `..`, tilde expansion and shell expressions are unsupported. Enter accepts the displayed default; a new SSH target defaults to its remote home directory.

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
