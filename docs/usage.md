# ClipBridge Usage Guide

Last updated: 2026-09-30.

ClipBridge runs as an installed macOS command-line tool. It monitors newly copied images, uploads them over SSH and places the remote file path on the clipboard. It has no application window or menu bar icon. See the [installation guide](installation.md) to install from a checkout or extracted source package.

See the [project overview](../README.md), [development guide](../src/README.md) and [product plan](planning/product-direction-and-distribution.md) for other repository documentation.

## Requirements

- macOS with a logged-in graphical user session.
- Python 3.9 or later; no third-party Python packages are required.
- Installation/upgrades need `swiftc` and `make` from Xcode or Command Line Tools. Normal installed operation uses bundled helpers. Python must remain available.
- A Linux or WSL host with a literal SSH alias in `~/.ssh/config`.
- Noninteractive SSH authentication and a verified host key. ClipBridge cannot prompt for passwords or accept unknown host keys automatically.

First connect normally with `ssh YOUR_ALIAS` to verify the server identity and authentication. A key or SSH agent must also work for the background process. Stop any legacy clipboard uploader before starting ClipBridge; see [migration](#migration).

## Configure the Destination

Installation preserves your existing configuration or creates a sample at `~/.config/clipbridge/config.json`.

Edit `~/.config/clipbridge/config.json` in your preferred editor:

```json
{
  "ssh_host": "dev-server",
  "remote_directory": "/home/example/.local/share/clipbridge/images"
}
```

Replace both example values:

| Field | Meaning |
| --- | --- |
| `ssh_host` | Required: the exact alias used in `ssh ALIAS`, containing only letters, digits, underscores, dots and hyphens; it must begin with a letter or digit |
| `remote_directory` | Required: the destination directory on the remote host, writable by the SSH user |

The directory must be an absolute POSIX path. Supported characters are ASCII letters, digits, underscores, dots, slashes and hyphens. Spaces and `..` path components are rejected; `~`, `$HOME` and environment variables are not expanded. Use valid JSON with double quotes and no comments or trailing commas. Keep credentials in your SSH setup, outside this file.

**The remote directory does not have to exist before monitoring starts.** Each upload attempts to create it with `mkdir -p`; the SSH user must have permission to create it or write there. You can use an existing writable directory instead.

## Start and Use

Keep a matching interactive SSH session open in one terminal:

```sh
ssh dev-server
```

In another terminal:

```sh
clipbridge start
```

Replace `dev-server` with your configured alias. `start` uses the installed native helpers, launches a per-user background service and enables startup at login. You can close the command's terminal afterward; the matching SSH session must remain open for uploads.

Copy an image, wait for the upload, then paste its remote path, such as `/home/example/.local/share/clipbridge/images/shot-UUID.png`. Notifications are attempted; their visibility depends on macOS notification settings. Images from **all applications** can trigger uploads.

For a temporary foreground session with logs in the terminal, use `clipbridge run`. Stop it with Ctrl+C; queued uploads may delay exit. Foreground mode does not install a login service and cannot run alongside an existing monitor.

`doctor` is optional troubleshooting, not a prerequisite for `run` or `start`.

## Daily Commands

| Command | Behavior |
| --- | --- |
| `clipbridge start` | Start background monitoring, or report the existing service using the same configuration file; resume it if loaded without a running process |
| `clipbridge restart` | Restart background monitoring and reload the selected configuration |
| `clipbridge stop` | Stop the background service and disable login startup; foreground monitors are unaffected |
| `clipbridge status` | Show process/service state, not connectivity or upload health |
| `clipbridge logs` | Print the latest 50 background log lines, including uploaded paths |
| `clipbridge logs --follow` | Follow background logs; use `--lines NUMBER` to change the initial count |
| `clipbridge run` | Monitor in the foreground and log to the terminal |
| `clipbridge doctor` | Check tools, configuration, legacy service conflicts, SSH authentication and existing destination permissions |
| `clipbridge configure` | Run the optional setup wizard |

Repeating `start` does not interrupt a running background service or reload edits. After changing JSON, use `restart`; for foreground mode, stop and rerun it. A background restart may interrupt an active transfer and does not drain its upload queue.

Running `start` while a current `run` session is active requests a cooperative switch: the foreground monitor stops taking new clipboard events, finishes queued uploads and exits before the background service starts. The wait is limited to 180 seconds; on timeout, inspect the original terminal and `status` before retrying. Images copied during the switch may be skipped. Older foreground versions without handoff support need Ctrl+C once before `start`.

To select a configuration elsewhere:

```sh
clipbridge --config /absolute/path/to/config.json start
clipbridge --config /absolute/path/to/config.json restart
```

Runtime commands accept `--config` before or after the subcommand. Installation commands use `--prefix` as described in the [installation guide](installation.md). Relative paths resolve from the current directory. Switching a loaded service to another file requires `restart`. There is only one service per user: `stop`, `status` and `logs` address that service regardless of `--config`.

## Upload Behavior and Limits

- Startup ignores existing clipboard content. New PNG, TIFF or JPEG clipboard images are captured as PNG; text alone is ignored.
- Uploads require a same-user `ssh ALIAS` process with an established TCP socket. The alias must match the configured value. This is a heuristic, not proof of authentication; multiplexing, jump hosts, remote commands and IDE-managed connections are not guaranteed.
- Images copied without a matching session are skipped and are not uploaded after reconnecting. Each queued job checks the session again before starting; jobs skipped after disconnect are not retained for later upload.
- Captured images upload sequentially. The queue has no size limit, and polling can miss very rapid clipboard changes. Captures that become stale before extraction are skipped.
- On success, the path replaces the clipboard only if its change count still matches. Newer contents are preserved on a best-effort basis: checking and replacing are not atomic. Find a successful upload's path in the log if it was not copied.
- Failed uploads retain a local copy when available and log the error. There is no automatic retry or cleanup. Successful uploads remove their staging copies. Remote files remain indefinitely; interrupted transfers can leave partial remote files.

## Optional Setup Wizard

`clipbridge configure` lists literal aliases from `~/.ssh/config` and included files, then saves the same JSON configuration. Alias discovery supplies suggestions without evaluating `Match` rules; you can enter an alias manually. For a new target, accepting the directory default resolves `.local/share/clipbridge/images` under the **remote** user's home directory.

The wizard checks SSH access, creates the directory if needed and creates/removes a temporary write probe before saving. It does not start monitoring. Failed checks preserve existing settings; a successful replacement of invalid JSON first saves a `config.json.backup-*` file. Wizard-created configuration files have owner-only permissions.

Scripted setup is also available:

```sh
clipbridge configure --host dev-server
clipbridge configure --host dev-server --remote-dir /home/example/images
clipbridge configure --host dev-server --remote-dir /home/example/images --no-check
```

Replace the examples with your destination. `--no-check` saves offline and requires both an explicit `--host` and absolute `--remote-dir`.

## Troubleshooting

Run `clipbridge doctor` when configuration or uploads fail. It does not start monitoring or create the remote directory. A missing matching session is reported as waiting, even if the host is reachable.

Because `doctor` requires an existing directory, it can fail for a new destination that an upload would create. To check that destination in advance, create it yourself or use `configure`. For the example above:

```sh
ssh dev-server 'umask 077; mkdir -p -- /home/example/.local/share/clipbridge/images'
clipbridge doctor
```

Use your actual alias and absolute directory. For authentication errors, verify ordinary SSH access, host-key trust and key/agent availability. For missing uploads, check the exact alias, open session and logs. If a lifecycle command reports another operation in progress, wait for that command to finish before retrying.

## Files and Cleanup

| Location | Purpose |
| --- | --- |
| `~/.config/clipbridge/config.json` | User configuration |
| `~/Library/LaunchAgents/local.clipbridge.plist` | Generated background/login service |
| `~/Library/Logs/ClipBridge/clipbridge.log` | Background output and errors |
| `~/Library/Caches/clipbridge/auto/upload-*` | Upload staging and retained failures |
| `~/Library/Caches/clipbridge/auto/clipboard-*.png` | Captures awaiting upload |
| `~/Library/Caches/clipbridge/auto/` | Also holds monitor/service locks and temporary `monitor.json` / `handoff.json` control state |
| `~/.local/share/clipbridge/current/src/build/` | Installed native helper binaries |
| `~/.local/bin/clipbridge` | Installed command launcher |

The installed program does not depend on the checkout. Keep its managed prefix and Python interpreter available; reinstall with an available interpreter if Python is replaced. See [upgrade and removal](installation.md) before changing program files. `stop` preserves configuration, logs and uploaded files. Inspect retained images and remove unneeded cache files only after all foreground and background monitoring has stopped. Remote cleanup is also manual.

## Migration

For source-tree development, if the default user configuration is absent, the ignored `src/config.json` remains a fallback. Installation migrates this file when needed without copying it into program payloads. `configure` can copy its settings into the default user file while preserving the original. Custom locations other than the default user configuration path do not use this fallback.

Loaded legacy services `local.codex.xnip-wsl` and `local.clipbridge.prototype` block monitoring. ClipBridge does not modify them. Unload only the uploader you intend to replace, for example:

```sh
launchctl bootout "gui/$(id -u)/local.codex.xnip-wsl"
```

Move that uploader's plist out of `~/Library/LaunchAgents/` to prevent its next login startup. Stop standalone legacy processes in their original terminal. Preserve their configuration until migration is verified.
