# Image Paste in Local and SSH CLIs

Last updated: 2026-10-24.

ClipBridge 0.3.3 leaves the Mac clipboard unchanged. Local applications continue receiving the original image. For an SSH CLI, enable the Linux companion and reconnect SSH once to load its Bash/Zsh integration. Then run `codex` or `claude` normally. The companion provides a private X11 image clipboard without changing Ghostty shortcuts or inserting a file path into the terminal.

Versions before 0.3 replaced the Mac clipboard with an uploaded file path, which terminal text paste could transmit directly. Image paste needs the remote CLI to read actual image data. Version 0.3 introduced an explicit launch wrapper to supply that connection; 0.3.1 invokes it automatically through shell integration.

The implementation and isolated regression tests are available. The real X11 image path has also been verified on WSL2; full attachment rendering and shortcut delivery in actual Ghostty, Codex and Claude Code sessions still need acceptance testing. CLI versions must support reading an image from the supplied Linux clipboard.

Mac 0.3.2 added alternative SSH aliases. Mac 0.3.3 streams each image and publishes it in one SSH request, with private connection reuse for subsequent images. The Linux companion remains 0.3.1; an existing compatible companion does not need reinstallation for these updates.

## Validation Record

On 2026-10-24, Mac toolkit 0.3.3 and companion 0.3.1 passed 148 automated tests, formatting and Clippy checks. A 262,548-byte synthetic PNG was transferred to a private WSL2 test directory: the old three-connection path took 17.904 seconds on its successful trial and timed out on another; the streamed path took 5.511 seconds for its first connection and 1.910 / 2.560 seconds when reused. This was a small, network-dependent transport comparison: publication was disabled for the new path, and the old path used a read-only helper status call as its final step. It does not measure clipboard publication or CLI rendering. All four successful files matched the source SHA-256 and were removed afterward.

A separate same-user process read a synthetic file through the uploader's procfs working-directory reference before and after a directory rename on WSL2. No real clipboard or model prompt was used in these checks.

On 2026-10-12, the Mac toolkit 0.3.2 and unchanged companion 0.3.1 passed 126 automated tests plus formatting and Clippy checks. Multi-alias tests cover route selection, consistent upload/publication routing, disconnect handling without reassignment, and configuration changes during setup.

On 2026-09-30, both crates passed 104 automated tests, formatting and Clippy checks. On a WSL2 Linux host, a separate private Xvfb backend advertised `image/png`, and three xclip reads returned the synthetic PNG byte-for-byte. A temporary Rust probe using arboard 3.6.1 also read and decoded the same 1×1 RGBA image correctly three times with `WAYLAND_DISPLAY` unset. The temporary backend, image and remote probe files were removed afterward.

These checks verify the image data path used by Linux clipboard readers. They do not establish the attachment UI or keyboard behavior of every CLI version. No personal clipboard image or model prompt was used in these checks.

On 2026-10-03, version 0.3.1 passed 92 Mac-toolkit tests and 23 companion tests. Shell integration checks use real Bash/Zsh with isolated home directories and fake application commands to verify startup hooks, arguments, exit status, configuration preservation and removal. An additional Dash check verifies that a shared `.profile` remains usable without loading Bash/Zsh-specific code.

## Prepare the Linux Host

On the configured SSH host, provide:

- Rust 1.94+, Cargo and a native linker for companion installation/upgrades.
- `xvfb-run`, `Xvfb`, `xauth` and `xclip` for operation.
- Standard shell utilities and an accessible Linux `/proc` filesystem for image publication.
- The CLI you want to run, such as `codex` or `claude`.

Choose an image directory owned and writable by the SSH account, with no group/other write permission. Uploads create a missing directory privately, but preserve an existing directory's permissions and refuse an unsafe destination. See [destination configuration](usage.md#configure-the-destination).

The Mac also needs Rust 1.94+ and Cargo for `remote setup`, even when the Mac executable is already installed. Setup uses the Mac's Cargo cache/configuration to prepare locked dependencies; the Mac needs registry access if they are not cached. It transfers those dependencies with the source, allowing the remote build to run without crates.io access. Normal monitoring and companion operation need no Rust toolchain on either host.

For Debian/Ubuntu, an administrator can install the operating-system dependencies with:

```sh
sudo apt-get install xvfb xclip xauth
```

This command runs **on the remote host**. ClipBridge never executes `sudo` or installs system packages automatically. No existing graphical desktop or SSH X11 forwarding is needed: the companion creates its own authenticated virtual display.

## Enable Image Publication

Install or upgrade the Mac toolkit first, and configure `ssh_host` and `remote_directory` as described in [usage](usage.md#configure-the-destination). Then run **on the Mac**:

```sh
clipbridge remote setup
clipbridge restart
```

`remote setup` extracts the embedded companion source into a temporary directory and runs `cargo vendor --locked --respect-source-config` on the Mac. It uploads the source and vendored dependencies over SSH/SCP, runs `cargo build --release --frozen` offline on the SSH host, and installs a user-owned command. It then checks the X tools and any running backend's compatibility, installs remote Bash/Zsh integration and sets `"remote_clipboard": true` in the selected local JSON configuration.

Setup preserves other JSON fields and does not start or restart monitoring. To keep manual wrapper launches or use an unsupported shell, run `clipbridge remote setup --no-shell`. This skips shell installation for that setup; it does not remove an existing integration. If dependencies are missing, install them remotely and rerun setup. The helper may already have been deployed, but the failed setup does not enable synchronization or change an existing configuration flag.

For a custom configuration, use the same path for both commands:

```sh
clipbridge --config /absolute/path/config.json remote setup
clipbridge --config /absolute/path/config.json restart
```

To use foreground monitoring instead, stop the old foreground process and run `clipbridge run` again after setup. Rerun `remote setup` to deploy the companion bundled with a newer Mac toolkit; setup again requires Cargo on both hosts. Updating its executable does not transparently replace an already running backend. If setup reports an incompatible backend, run on the Mac:

```sh
clipbridge remote stop
clipbridge remote setup
clipbridge restart
```

Then reconnect SSH, relaunch remote CLIs and copy a new image. Companion deployment does not have the Mac installer's versioned rollback mechanism.

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
```

Arguments work as usual, for example `codex resume`. The integration defines shell functions for these two command names, leaving their installed executables unchanged. Each launch obtains the current private clipboard connection and sets `DISPLAY` and `XAUTHORITY` only for the application, while clearing its `WAYLAND_DISPLAY`. The parent shell's display settings remain unchanged.

To keep the current remote Bash/Zsh session, load the installed snippet once before launching the CLI:

```sh
. "$HOME/.local/share/clipbridge-remote/shell/init.sh"
```

If you use tmux, run that command inside the existing pane or create a new pane. Reconnecting SSH alone does not reload an existing tmux shell. Direct `ssh dev-server COMMAND` invocations are outside the current session detector's supported pattern.

### Manual Launch and Custom Shells

The explicit wrapper remains available for other applications, unsupported shells, or an existing `codex`/`claude` alias or function that the integration preserves:

```sh
~/.local/bin/clipbridge-remote run -- codex
# Or:
~/.local/bin/clipbridge-remote run -- claude
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

Existing startup files are backed up before changes. Repeated installation is safe. Removal preserves other startup content, CLI executables and `shell/backup-*` files. With no options, `shell uninstall` removes all tracked hooks; supply the same `--shell` or `--rc-file` options to remove only selected hooks. Symlinked or unfamiliar managed files are refused.

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
- The remote shell loaded the integration before launching the CLI, including inside tmux if used. Run `type codex` or `type claude`: a managed launch uses a shell function. If it resolves directly to a binary or a different custom function, reconnect/reload the integration or use the explicit wrapper.
- The CLI receives its image-paste shortcut; terminal text paste is a different operation.
- The remote CLI supports the companion's X11 image clipboard in that environment. WSL-specific Windows clipboard behavior needs separate verification.

Ghostty 1.3.0's release notes describe OSC 5522 parsing without implementation. This integration therefore uses SSH transfer and a remote clipboard, rather than relying on that terminal image-clipboard protocol. See [Ghostty release notes](https://ghostty.org/docs/install/release-notes/1-3-0). Clipboard behavior also depends on the application; consult the [Codex clipboard implementation](https://github.com/openai/codex/blob/main/codex-rs/tui/src/clipboard_paste.rs) and [Claude Code keyboard reference](https://code.claude.com/docs/en/interactive-mode) when diagnosing version-specific behavior.

## Stop, Disable and Remove

`clipbridge remote stop` stops the private backend and its image owner. A later enabled upload, integrated CLI launch or explicit `clipbridge-remote run` starts it again. For a persistent pause, set `"remote_clipboard": false` in the Mac configuration, reload monitoring, then stop the backend:

```sh
clipbridge restart
clipbridge remote stop
```

Stop and rerun a foreground monitor instead of `restart` if you want to stay in foreground mode. These actions preserve remote uploaded files. To stop all uploads as well, use local `clipbridge stop` or Ctrl+C for foreground monitoring.

Remote installation uses `~/.local/share/clipbridge-remote/` and a symlink at `~/.local/bin/clipbridge-remote`. Runtime state defaults to `~/.local/state/clipbridge-remote/`. Mac uninstallation does not remove those remote files. There is no complete remote uninstall command yet. First run `~/.local/bin/clipbridge-remote shell uninstall` on the remote host to remove the managed startup integration. Then disable publication, stop the backend, and inspect these managed locations before removing them manually. Keep uploaded images unless you explicitly want to delete them.
