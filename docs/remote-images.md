# Image Paste in Local and SSH CLIs

Last updated: 2026-10-25.

ClipBridge 0.4.0 leaves the Mac clipboard unchanged. Local applications continue receiving the original image. For an SSH CLI, enable the Linux companion and reconnect SSH once to load its Bash/Zsh integration. Then run `codex`, `claude` or `opencode` normally. The companion provides a private X11 image clipboard without changing Ghostty shortcuts or inserting a file path into the terminal.

Versions before 0.3 replaced the Mac clipboard with an uploaded file path, which terminal text paste could transmit directly. Image paste needs the remote CLI to read actual image data. Version 0.3 introduced an explicit launch wrapper to supply that connection; 0.3.1 invokes it automatically through shell integration.

The implementation and isolated regression tests are available. The real X11 image path has also been verified on WSL2; full attachment rendering and shortcut delivery in actual Ghostty, Codex, Claude Code and OpenCode sessions still need acceptance testing. CLI versions must support reading an image from the supplied Linux clipboard.

Mac 0.4.0 adds one-time Mac-side setup and prebuilt companion deployment. It retains companion 0.3.2, including OpenCode integration and compatibility with a running 0.3.1 clipboard backend. The same path supports ordinary Linux servers and WSL2; no remote desktop session is required.

## Validation Record

On 2026-10-25, Mac toolkit 0.4.0 and companion 0.3.2 passed 186 automated tests, formatting and Clippy checks. Native builds completed for Apple Silicon and Intel macOS, plus statically linked x86_64 and aarch64 Linux companions. Claude Code's strict validator accepted the marketplace, plugin manifest and skills.

The standalone installer upgraded the source Mac using local release assets and only system tools on PATH. The unified setup then deployed the x86_64 native companion to WSL2 without Cargo, preserving configuration values and the existing 0.3.1 backend/Xvfb processes. Separate isolated native-binary checks exercised version, dependencies and stopped status without starting a clipboard backend. Ordinary Linux uses the same platform path, but a separate non-WSL server, Linux ARM64 runtime and Intel Mac runtime have not yet been tested. These checks do not establish actual agent attachment rendering.

On 2026-10-25, Mac toolkit 0.3.4 and companion 0.3.2 passed 153 automated tests, formatting and Clippy checks. New regression checks cover OpenCode argument and exit-status forwarding, scoped clipboard settings, alias/function conflicts, exact legacy snippet upgrades, and reuse of a compatible 0.3.1 backend while rejecting unknown versions.

The helper and managed shell snippet were upgraded on WSL2 while the existing 0.3.1 backend and Xvfb processes stayed running. A fresh Zsh session loaded the `opencode` function, the explicit wrapper launched OpenCode 2.1.4's version command, and the private X11 clipboard advertised `image/png`. These deployment checks did not submit a prompt or verify attachment rendering in the OpenCode UI.

On 2026-10-24, Mac toolkit 0.3.3 and companion 0.3.1 passed 148 automated tests, formatting and Clippy checks. A 262,548-byte synthetic PNG was transferred to a private WSL2 test directory: the old three-connection path took 17.904 seconds on its successful trial and timed out on another; the streamed path took 5.511 seconds for its first connection and 1.910 / 2.560 seconds when reused. This was a small, network-dependent transport comparison: publication was disabled for the new path, and the old path used a read-only helper status call as its final step. It does not measure clipboard publication or CLI rendering. All four successful files matched the source SHA-256 and were removed afterward.

A separate same-user process read a synthetic file through the uploader's procfs working-directory reference before and after a directory rename on WSL2. No real clipboard or model prompt was used in these checks.

On 2026-10-12, the Mac toolkit 0.3.2 and unchanged companion 0.3.1 passed 126 automated tests plus formatting and Clippy checks. Multi-alias tests cover route selection, consistent upload/publication routing, disconnect handling without reassignment, and configuration changes during setup.

On 2026-09-30, both crates passed 104 automated tests, formatting and Clippy checks. On a WSL2 Linux host, a separate private Xvfb backend advertised `image/png`, and three xclip reads returned the synthetic PNG byte-for-byte. A temporary Rust probe using arboard 3.6.1 also read and decoded the same 1×1 RGBA image correctly three times with `WAYLAND_DISPLAY` unset. The temporary backend, image and remote probe files were removed afterward.

These checks verify the image data path used by Linux clipboard readers. They do not establish the attachment UI or keyboard behavior of every CLI version. No personal clipboard image or model prompt was used in these checks.

On 2026-10-03, version 0.3.1 passed 92 Mac-toolkit tests and 23 companion tests. Shell integration checks use real Bash/Zsh with isolated home directories and fake application commands to verify startup hooks, arguments, exit status, configuration preservation and removal. An additional Dash check verifies that a shared `.profile` remains usable without loading Bash/Zsh-specific code.

## Prepare the Linux Host

Default setup supports x86_64 and aarch64 Linux, including WSL2. On the remote account, provide:

- Working SSH authentication and a verified host key.
- `xvfb-run`, `Xvfb`, `xauth`, `xclip` and `sha256sum`.
- A POSIX shell, ordinary file utilities and an accessible Linux `/proc` filesystem.
- The desired CLI (`codex`, `claude` or `opencode`) on the interactive shell's PATH, with its own image-input dependencies.

Choose an image directory owned and writable by the SSH account, without group/other write permission. `clipbridge setup` defaults to `.local/share/clipbridge/images` under the remote account's home and creates it privately if needed. Existing directory permissions are preserved. See [destination configuration](usage.md#configure-the-destination).

Setup checks the required utilities before installing a companion. If any are missing, an administrator must install the distribution's packages. On Debian/Ubuntu, for example:

```sh
sudo apt-get install xvfb xauth xclip coreutils
```

For Fedora/RHEL, use `dnf`; for Alpine, use `apk` to install packages providing the same programs. Package names and availability depend on the distribution and enabled repositories. ClipBridge prints dependency guidance and never runs a system package manager automatically.

These requirements apply equally to a headless Linux server and WSL2. The companion creates an authenticated private virtual X display; no existing desktop or SSH X11 forwarding is needed. Default setup needs neither Cargo nor direct GitHub access on the remote host.

## Enable Image Publication

Run initial setup **on the Mac whose clipboard you want to synchronize**. After [installing the Mac program](installation.md), run:

```sh
clipbridge setup
```

Choose an SSH alias, accept the remote-home image directory or provide another, and choose whether to start background monitoring at login. For explicit choices:

```sh
clipbridge setup --host dev-server --start
```

This checks the destination and Linux architecture, downloads the matching prebuilt companion and SHA-256 sidecar on the Mac, uploads the verified binary over SSH, installs Bash/Zsh integration, enables remote publication and applies the selected startup setting. Downloads are pinned to the Mac toolkit's exact GitHub release tag. ClipBridge 0.4.0 uses the companion assets attached to its [release](https://github.com/ruoye-ruolan/clip-bridge/releases/tag/v0.4.0); see [offline assets](installation.md#local-assets-and-offline-installation) for local development or restricted download access.

Use `--no-start` instead of `--start` to leave monitoring stopped and disable existing managed login startup. Use `--no-shell` to keep explicit wrapper launches; it does not remove hooks already installed. See [setup options and failure handling](usage.md#one-time-setup).

### Update an Already Configured Companion

To install/update only the configured companion and its shell integration, run on the Mac:

```sh
clipbridge remote setup
```

This command uses the existing JSON destination and prebuilt assets by default. It enables `remote_clipboard` after successful checks, but does not restart the Mac monitor. Run `clipbridge restart` if that flag changed; stop and rerun a foreground monitor instead when appropriate. A custom configuration can be selected with `clipbridge --config /absolute/path/config.json remote setup`.

Companion 0.3.2 accepts its own backend and the explicitly compatible 0.3.1 backend. A running 0.3.1 backend and its image can remain active during this update. Exact unmodified older shell snippets upgrade safely; edited snippets and unknown files are refused without modification. Reload integration and relaunch agents started without it. Already integrated agents need no relaunch when the backend remains unchanged.

If setup reports an incompatible backend, explicitly stop it, repeat setup and relaunch remote CLIs:

```sh
clipbridge remote stop
clipbridge remote setup
```

A newly published image or integrated CLI launch starts the backend when needed. Setup itself does not start a private X server. Native helper activation checks the staged binary and attempts to restore the previous command if promotion fails. A later local configuration/startup failure can leave the successfully deployed helper installed; inspect the reported recovery result before retrying.

### Source and Offline Deployment

For a locally supplied companion binary and checksum, run:

```sh
clipbridge remote setup --assets-dir /absolute/path/to/assets
```

The asset directory is on the Mac. For an explicit source build:

```sh
clipbridge remote setup --build-from-source
```

Source deployment requires Rust 1.94+/Cargo and linkers on both hosts. The Mac vendors locked dependencies and uploads them with embedded sources; Linux builds with `--frozen` without registry access. Only the Mac needs registry access or cached dependencies. The source path is separate from native activation and does not provide its rollback guarantees. The flags are mutually exclusive; download failure does not silently select a source build.

## Start the Remote CLI

After the first setup, exit existing CLI processes and reconnect SSH once. In Ghostty, open a normal SSH session and keep it open:

```sh
ssh dev-server
```

Replace the alias with your configured `ssh_host` or an entry in `ssh_aliases`. All listed aliases must reach the same machine and account; see [multiple network routes](usage.md#multiple-network-routes). Run the desired command **inside that remote shell**:

```sh
codex
# Or:
claude
# Or:
opencode
```

Arguments work as usual, for example `codex resume`. The integration defines shell functions for these three command names, leaving their installed executables unchanged. Each launch obtains the current private clipboard connection and sets `DISPLAY` and `XAUTHORITY` only for the application, while clearing its `WAYLAND_DISPLAY`. The parent shell's display settings remain unchanged.

To keep the current remote Bash/Zsh session, load the installed snippet once before launching the CLI:

```sh
. "$HOME/.local/share/clipbridge-remote/shell/init.sh"
```

If you use tmux, run that command inside the existing pane or create a new pane. Reconnecting SSH alone does not reload an existing tmux shell. Direct `ssh dev-server COMMAND` invocations are outside the current session detector's supported pattern.

### Manual Launch and Custom Shells

The explicit wrapper remains available for other applications, unsupported shells, or an existing `codex`/`claude`/`opencode` alias or function that the integration preserves:

```sh
~/.local/bin/clipbridge-remote run -- codex
# Or:
~/.local/bin/clipbridge-remote run -- claude
# Or:
~/.local/bin/clipbridge-remote run -- opencode
```

Arguments after the application name are passed directly to it. If the remote shell cannot find the application, use its actual absolute path, for example `~/.local/bin/clipbridge-remote run -- ~/.local/bin/codex` when that is where Codex is installed. Calling an application by its absolute path bypasses the shell function, so use the explicit wrapper in that case.

Existing aliases and functions are left in place with a warning. Update your own custom command to invoke the wrapper if appropriate. Existing CLI processes cannot acquire new clipboard environment settings; exit and relaunch them once.

## Manage Shell Integration

Run these commands **on the remote host**:

```sh
~/.local/bin/clipbridge-remote shell install
~/.local/bin/clipbridge-remote shell uninstall
```

Installation adds a marked block to the selected startup files and stores the snippet at `~/.local/share/clipbridge-remote/shell/init.sh`. It chooses Bash or Zsh from the remote `SHELL` environment variable unless `--shell` is supplied:

- Zsh: `${ZDOTDIR:-$HOME}/.zshrc`. `ZDOTDIR` must be exported and absolute to be detected.
- Bash: `~/.bashrc` plus the first existing `~/.bash_profile`, `~/.bash_login` or `~/.profile`; it creates `.profile` if none exists.
- `--rc-file`: only the given absolute file. Use this when your shell chooses a custom startup location that remote setup cannot detect.

Existing startup files are backed up before changes. Repeated installation is safe, and an exact older managed snippet can be upgraded without duplicating startup blocks. Removal preserves other startup content, CLI executables and `shell/backup-*` files. With no options, `shell uninstall` removes all tracked hooks; supply the same `--shell` or `--rc-file` options to remove only selected hooks. Edited snippets, symlinked files and unfamiliar managed files are refused.

For a custom setup, select the shell and startup file explicitly:

```sh
~/.local/bin/clipbridge-remote shell install --shell zsh --rc-file /absolute/path/to/.zshrc
```

For manually managed Bash/Zsh configuration, `~/.local/bin/clipbridge-remote shell init` prints the integration script without editing startup files. Installing or sourcing integration does not start a clipboard backend; launching an integrated command does.

Reconnect SSH after removing integration to clear functions from the current shell. Removing integration does not stop image uploads or the private clipboard backend.

## Copy and Paste

1. Copy a new screenshot or other image on the Mac. Images present when monitoring started are skipped.
2. Paste into a local CLI as usual; ClipBridge does not consume or replace the image.
3. For the remote CLI, wait for **remote image clipboard ready** in the notification or `clipbridge logs --follow`.
4. Use the remote CLI's image-paste action, such as `Ctrl+V` where configured, and verify that an image attachment appears before sending your prompt.

An upload path in the log is the remote file's storage location. The companion reads that file as PNG and publishes image data. The CLI renders its own attachment indicator; ClipBridge does not generate an `[Image #N]` placeholder itself.

Remote publication is asynchronous. The private clipboard holds the last successfully published image, so pasting before the ready message can paste an older image. Copying text on the Mac does not clear the remote image. All applications launched through the integration or wrapper for the same remote user share that private clipboard. A failed publication retains a local copy and reports the failure; the previous remote image may remain available.

The first transfer still needs a connection; later images can reuse it within the 60-second idle window. Logs record capture-to-completion time, including queue wait and publication. See [transfer speed](usage.md#transfer-speed) for the remaining delays and network-change behavior. Transfer completion is reported only after the received byte count is verified and the remote file is finalized; remote readiness additionally requires successful publication.

## Troubleshooting

Run these commands on the Mac:

```sh
clipbridge remote doctor
clipbridge remote status
clipbridge logs --follow
```

`remote doctor` checks remote dependencies; ordinary `doctor` checks the SSH destination. `remote status` does not start the backend. A freshly set-up companion can correctly report stopped until its first publication or integrated application launch starts it. After the backend has been stopped/restarted, exit and relaunch the CLI so it receives the current display settings, then copy a new image. A new integrated launch refreshes the connection automatically.

`remote` commands use one selected route for the entire operation: the preferred matching interactive session, or the primary `ssh_host` when none is detected. They print that alias and do not probe alternatives on failure. After changing networks, close stale SSH sessions and reconnect through a working configured alias, then copy a new image.

If attachments do not appear, confirm the following:

- The Mac monitor has reloaded a configuration containing `"remote_clipboard": true`.
- A matching ordinary `ssh ALIAS` session remains open and the latest image has reached the remote-ready state.
- The remote shell loaded the integration before launching the CLI, including inside tmux if used. Run `type codex`, `type claude` or `type opencode`: a managed launch uses a shell function. If it resolves directly to a binary or a different custom function, reconnect/reload the integration or use the explicit wrapper.
- The CLI receives its image-paste shortcut; terminal text paste is a different operation.
- The remote CLI supports the companion's X11 image clipboard in that environment. WSL-specific Windows clipboard behavior needs separate verification.

If OpenCode shows no response to its image-paste shortcut while `remote status` reports `image ready`, first check `type opencode`. A process launched before the integration was loaded does not receive the private clipboard environment retroactively: exit it, source the snippet above and launch `opencode` again. Using an absolute executable path bypasses the shell function, so use the explicit wrapper for that form.

Ghostty 1.3.0's release notes describe OSC 5522 parsing without implementation. This integration therefore uses SSH transfer and a remote clipboard, rather than relying on that terminal image-clipboard protocol. See [Ghostty release notes](https://ghostty.org/docs/install/release-notes/1-3-0). Clipboard behavior also depends on the application; consult the [Codex clipboard implementation](https://github.com/openai/codex/blob/main/codex-rs/tui/src/clipboard_paste.rs) and [Claude Code keyboard reference](https://code.claude.com/docs/en/interactive-mode) when diagnosing version-specific behavior.

## Stop, Disable and Remove

`clipbridge remote stop` stops the private backend and its image owner. A later enabled upload, integrated CLI launch or explicit `clipbridge-remote run` starts it again. For a persistent pause, set `"remote_clipboard": false` in the Mac configuration, reload monitoring, then stop the backend:

```sh
clipbridge restart
clipbridge remote stop
```

Stop and rerun a foreground monitor instead of `restart` if you want to stay in foreground mode. These actions preserve remote uploaded files. To stop all uploads as well, use local `clipbridge stop` or Ctrl+C for foreground monitoring.

Remote installation uses `~/.local/share/clipbridge-remote/` and a symlink at `~/.local/bin/clipbridge-remote`. Runtime state defaults to `~/.local/state/clipbridge-remote/`. Mac uninstallation does not remove those remote files. There is no complete remote uninstall command yet. First run `~/.local/bin/clipbridge-remote shell uninstall` on the remote host to remove the managed startup integration. Then disable publication, stop the backend, and inspect these managed locations before removing them manually. Keep uploaded images unless you explicitly want to delete them.
