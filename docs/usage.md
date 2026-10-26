# ClipBridge Usage Guide

Last updated: 2026-10-25.

ClipBridge monitors newly copied Mac images and uploads copies over SSH while leaving the Mac clipboard unchanged. Optional remote image support makes an uploaded image available to a CLI launched through the Linux companion. It has no application window or menu bar icon. See [installation](installation.md) and [remote image setup](remote-images.md).

See the [project overview](../README.md), [development guide](../src/README.md) and [product plan](planning/product-direction-and-distribution.md) for other repository documentation.

## Requirements

- macOS with a logged-in graphical user session.
- The installed Rust executable; no Python, Swift or Rust toolchain is needed for normal operation.
- Source installation/upgrades need Rust 1.94+, Cargo and Apple's linker/SDK from Xcode or Command Line Tools. See [installation](installation.md) for the first upgrade from Python 0.1.
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
| `ssh_host` | Required: primary alias used in `ssh ALIAS`; letters, digits, underscores, dots and hyphens only, beginning with a letter or digit |
| `ssh_aliases` | Optional array, default `[]`: other SSH aliases for the same machine, account and remote directory, in preference order; aliases must be valid and unique across both fields |
| `remote_directory` | Required: an absolute directory owned and writable by the SSH user, with no group/other write permission |
| `remote_clipboard` | Optional boolean, default `false`: publish uploaded PNGs to the remote companion's private image clipboard; `remote setup` enables it after checking dependencies |

The directory must be an absolute POSIX path. Supported characters are ASCII letters, digits, underscores, dots, slashes and hyphens. Spaces and `..` path components are rejected; `~`, `$HOME` and environment variables are not expanded. Use valid JSON with double quotes and no comments or trailing commas. Keep credentials in your SSH setup, outside this file.

**The remote directory does not have to exist before monitoring starts.** Each upload attempts to create it under `umask 077`; the SSH user must have permission to create it. An existing directory must be owned and writable by that user and must not be writable by its group or other users. ClipBridge does not change permissions on an existing directory. A dedicated path under the remote user's home, as in the example, is preferable to a shared directory.

## Multiple Network Routes

Version 0.3.2 can recognize several SSH aliases for **one remote machine and account**. Put addresses in `~/.ssh/config` under separate `Host` aliases, then list those aliases in the ClipBridge JSON:

```json
{
  "ssh_host": "dev-server",
  "ssh_aliases": ["dev-server.local", "dev-server-lan"],
  "remote_directory": "/home/example/.local/share/clipbridge/images",
  "remote_clipboard": true
}
```

Use only aliases you have configured and verified. All entries must reach the same account, uploaded files and companion; this is not a list of separate destinations. Use `remote_clipboard: true` only after [remote setup](remote-images.md). IPv6 addresses belong in an SSH `HostName` setting behind an alias, not directly in this JSON. If only an address changes, keeping the existing `Host` alias and updating its `HostName` requires no ClipBridge configuration change.

After editing the JSON, run `clipbridge restart`. Connect with the alias that works on the current network, such as `ssh dev-server.local`. ClipBridge prefers an open session for `ssh_host`, then the first matching entry in `ssh_aliases`. Each new image keeps the selected route for its upload and publication. It is never broadcast to all aliases or reassigned to another route after capture.

This list does not probe addresses, establish SSH sessions or reconnect after a network change. Close stale SSH sessions and connect again through a working alias before copying a new image. An old TCP connection may temporarily still appear established after switching networks, causing it to win the preference order until closed. Images skipped while disconnected are not replayed.

`doctor` and `remote setup/doctor/status/stop` print the selected route and use it throughout that command. They choose the preferred matching session, or `ssh_host` when none is detected; they do not try each alias after a connection failure.

## Start and Use

Keep a matching interactive SSH session open in one terminal:

```sh
ssh dev-server
```

In another terminal:

```sh
clipbridge start
```

Replace `dev-server` with your primary alias or an entry in `ssh_aliases`. `start` launches the native executable as a per-user background service and enables startup at login. You can close the command's terminal afterward; the matching SSH session must remain open for uploads.

Copy an image and paste it into a local application as usual. The Mac clipboard remains unchanged before, during and after upload. Paths such as `/home/example/.local/share/clipbridge/images/shot-UUID.png` are storage locations shown in logs, not clipboard replacements. Notifications are attempted; their visibility depends on macOS notification settings. Images from **all applications** can trigger uploads.

For image attachments in an SSH CLI, complete [remote setup](remote-images.md), reconnect SSH once to load the Bash/Zsh integration, then run `codex`, `claude` or `opencode` normally inside the SSH terminal. Arguments such as `codex resume` work as usual. For custom shells or existing alias/function conflicts, use the [manual launch](remote-images.md#manual-launch-and-custom-shells). Wait for **remote image clipboard ready** before image paste. The remote clipboard holds the last successfully published image; copying Mac text does not clear it.

OpenCode integration is included in Mac 0.3.4 with companion 0.3.2. Existing installations need `clipbridge remote setup` after the Mac upgrade, then a new SSH shell or a reload of the managed snippet. OpenCode processes started without the wrapper must be relaunched. See the [upgrade instructions](installation.md#upgrade-a-rust-installation).

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
| `clipbridge remote setup` | Use Cargo on both hosts to vendor on the Mac/build offline on Linux, check dependencies/backend compatibility, install remote Bash/Zsh integration and enable `remote_clipboard`; does not restart monitoring |
| `clipbridge remote doctor` | Check the companion's Xvfb, xauth and xclip dependencies |
| `clipbridge remote status` | Show the companion's running/image state |
| `clipbridge remote stop` | Stop the remote private clipboard backend; a later upload or integrated CLI launch can start it again |

`clipbridge remote setup --no-shell` keeps manual wrapper launches and skips shell installation. It does not remove previously installed integration.

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
- Uploads require a same-user `ssh ALIAS` process with an established TCP socket. The alias must match `ssh_host` or an entry in `ssh_aliases`. This is a heuristic, not proof of authentication; user-managed multiplexed sessions, jump hosts, remote commands and IDE-managed connections are not guaranteed. ClipBridge's own cached connections do not count as interactive sessions.
- Images copied without a matching session are skipped and are not uploaded after reconnecting. Each queued job checks its captured route again before starting; if that route has disconnected, the job is skipped even if another configured route is open. Skipped jobs are not retained for later upload.
- Captured images upload sequentially, with room for 16 pending uploads in addition to the active transfer. When full, the queue skips the newly captured image, removes that capture and logs the skip; already queued images remain. Polling can miss very rapid clipboard changes. Captures that become stale before extraction are skipped.
- ClipBridge never writes to or clears the Mac clipboard. Local image paste and newer copied content remain untouched. Successful upload paths are available in logs.
- With `remote_clipboard: true`, each completed transfer is published to the Linux companion before reporting remote readiness. A publication failure is reported separately from a transfer failure and retains the local copy. Until a later publication succeeds, the remote clipboard may still contain an older image. This is asynchronous image publication, not a full clipboard mirror.
- Failed uploads retain a local copy when available and log the error. There is no automatic retry or retention policy for these copies or completed remote images. The remote transfer writes a private temporary file, verifies its byte count and renames it into place before publication. Cleanup removes temporary files when it can run; an abruptly terminated remote process can leave temporary files behind.
- Notifications run separately with room for 16 pending notifications. A full notification queue skips the new notification without blocking upload completion or changing the upload result; logs remain available.

## Transfer Speed

Version 0.3.3 sends each PNG and invokes remote publication in one SSH request, instead of separate directory, SCP and publication connections. The sequential upload worker keeps private connections by alias, with a 60-second idle expiry. A first image, or an image sent after expiry, still needs an SSH handshake. Closely spaced images can reuse that connection. Captured PNG bytes are transferred unchanged; ClipBridge does not resize images to obtain the speed improvement.

The cache is created only when an eligible image is uploaded. It does not open a connection when monitoring starts, replace the interactive SSH session requirement, or edit your SSH or proxy configuration. A failure invalidates the affected cached connection; cleanup errors are reported, and the current image is not retried through another alias. A later new image can establish a fresh connection. Normal monitor shutdown closes its owned connections before releasing the monitor lock.

Image uploads allow up to 30 seconds for SSH connection establishment and 150 seconds for the complete command, including transfer and publication. These limits bound failed or stalled work; they are not an expected transfer time. Setup and diagnostic commands use their existing time limits and do not use this private upload cache.

Logs include `capture-to-completion N ms`, covering alias selection, image conversion, queue wait, transfer and publication. It excludes the initial clipboard polling delay (up to about 300 ms) and notification delivery. File size, connection establishment, network throughput and queued images can still affect when an image is ready. Continue waiting for the remote-ready notification or log entry before pasting.

After switching networks, close stale SSH sessions and reconnect through a working configured alias. If a new image reports a transport failure, inspect the log before copying it again. The cached upload connection does not make a broken network route usable.

## Optional Setup Wizard

`clipbridge configure` lists literal aliases from `~/.ssh/config` and included files, then saves the same JSON configuration. Alias discovery supplies suggestions without evaluating `Match` rules; you can enter an alias manually. For a new target, accepting the directory default resolves `.local/share/clipbridge/images` under the **remote** user's home directory.

The wizard checks SSH access, creates the directory if needed and creates/removes a temporary write probe before saving. It does not start monitoring. Failed checks preserve existing settings; a successful replacement of invalid JSON first saves a `config.json.backup-*` file. Wizard-created configuration files have owner-only permissions. Reconfiguration preserves `ssh_aliases` and remote clipboard enablement when `ssh_host` stays the same. Changing `ssh_host` clears the alternative aliases and disables remote publication until `remote setup` succeeds for that host. Edit `ssh_aliases` directly in JSON; the wizard does not manage that list.

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

Use your actual alias and absolute directory. For authentication errors, verify ordinary SSH access, host-key trust and key/agent availability. For missing uploads after a network change, check the allowed aliases, close stale sessions and reconnect through a working route; see [multiple network routes](#multiple-network-routes). Check logs for the selected route and transfer result. If a lifecycle command reports another operation in progress, wait for that command to finish before retrying.

If the destination is refused because of ownership or write permissions, choose a dedicated directory owned by your remote SSH account and update `remote_directory`, then run `clipbridge restart`. Do not point uploads at a shared writable directory. Existing files and directory permissions are preserved; coordinate any changes to a shared location with its owner instead of broadly changing its permissions.

If local pastes still contain paths, confirm the active monitor is version 0.3 or newer and stop older uploaders, then copy the image again. Already replaced clipboard contents cannot be restored by an upgrade. For remote attachment failures, see [remote troubleshooting](remote-images.md#troubleshooting); ordinary `doctor` does not check the remote clipboard.

## Files and Cleanup

| Location | Purpose |
| --- | --- |
| `~/.config/clipbridge/config.json` | User configuration |
| `~/Library/LaunchAgents/local.clipbridge.plist` | Generated background/login service |
| `~/Library/Logs/ClipBridge/clipbridge.log` | Background output and errors |
| `~/Library/Caches/clipbridge/auto/upload-*` | Upload staging and retained failures |
| `~/Library/Caches/clipbridge/auto/clipboard-*.png` | Captures awaiting upload |
| `~/Library/Caches/clipbridge/auto/` | Also holds monitor/service locks and temporary `monitor.json` / `handoff.json` control state |
| `/private/tmp/cb-ssh-*/` | Private upload control sockets; owned connections are closed during normal monitor shutdown |
| `~/.local/share/clipbridge/current/clipbridge` | Installed native Rust executable |
| `~/.local/bin/clipbridge` | Installed command launcher |

The installed program does not depend on the checkout or a language toolchain. Keep its managed installation prefix in place. See [upgrade and removal](installation.md) before changing program files. `stop` preserves configuration, logs and uploaded files. Inspect retained images and remove unneeded cache files only after all foreground and background monitoring has stopped. Remote cleanup is also manual.

## Migration

For Python 0.1 installations, run `./install.sh` from the new Rust source directory for the first migration. The old Python upgrade command cannot use the Rust source layout. Existing JSON settings remain compatible; see [migration instructions](installation.md#migrate-from-python-01).

For source-tree development, if the default user configuration is absent, the ignored `src/config.json` remains a fallback. Installation migrates this file when needed without copying it into program payloads. `configure` can copy its settings into the default user file while preserving the original. Custom locations other than the default user configuration path do not use this fallback.

Loaded legacy services `local.codex.xnip-wsl` and `local.clipbridge.prototype` block monitoring. ClipBridge does not modify them. Unload only the uploader you intend to replace, for example:

```sh
launchctl bootout "gui/$(id -u)/local.codex.xnip-wsl"
```

Move that uploader's plist out of `~/Library/LaunchAgents/` to prevent its next login startup. Stop standalone legacy processes in their original terminal. Preserve their configuration until migration is verified.
