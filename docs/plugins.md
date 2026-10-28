# Claude Code Plugin

Last updated: 2026-10-25.

The ClipBridge plugin provides guided setup, status and diagnostics. The native Mac program and Linux companion perform image synchronization; the plugin does not replace them or implement image paste inside an agent.

The plugin installs the native Mac package pinned to [ClipBridge 0.4.1](https://github.com/ruoye-ruolan/clip-bridge/releases/tag/v0.4.1). For offline or development installation, use [trusted local assets](installation.md#local-assets-and-offline-installation).

## Install on the Source Mac

Run Claude Code on the Mac whose clipboard you want to synchronize. In its session:

```text
/plugin marketplace add https://github.com/ruoye-ruolan/clip-bridge
/plugin install clipbridge@clipbridge
```

Complete the plugin installation panel and select your intended scope. These commands follow Claude Code's [marketplace installation flow](https://code.claude.com/docs/en/discover-plugins). The repository is the `clipbridge` marketplace, and its plugin is also named `clipbridge`.

Then invoke:

```text
/clipbridge:setup
```

You can provide the SSH alias as an argument, such as `/clipbridge:setup dev-server`. The skill installs the pinned native Mac release into the managed user prefix, lists SSH aliases, and asks for the host and startup preference when they were not already supplied. It invokes `clipbridge setup` with explicit choices. Setup handles ordinary Linux servers and WSL2; the remote account and distribution are not assumed.

Initial setup must run on the source Mac. Invoking the plugin from a Linux or WSL agent cannot read the Mac clipboard and does not install the Mac program there. The remote host still needs the [X11 utilities](remote-images.md#prepare-the-linux-host); the skill reports missing dependencies rather than running a package manager.

## Commands

| Skill | Behavior |
| --- | --- |
| `/clipbridge:setup` | Install/configure both hosts and apply the chosen login-startup setting |
| `/clipbridge:status` | Inspect installed monitor/clipboard state without starting services or reading images |
| `/clipbridge:doctor` | Diagnose SSH, configuration, remote publication and agent startup without changing them |

On Linux or WSL, the status and doctor skills inspect the installed companion only. Missing components are reported; those skills do not install them. Skill names use Claude Code's [plugin namespace](https://code.claude.com/docs/en/plugins).

After setup, reconnect SSH or source `~/.local/share/clipbridge-remote/shell/init.sh` in the remote interactive shell. Relaunch an agent that was started without integration. Keep a matching SSH session open, copy a new image, wait for remote readiness and verify the attachment before sending a prompt.

## Updates and Removal

Use Claude Code's plugin management interface to update or remove the plugin. Updating plugin files alone does not upgrade the installed native program; invoke setup again when adopting the plugin's newer pinned release. The installer refuses to downgrade a newer managed Mac installation.

The executable and login service live in ClipBridge's [managed installation](installation.md#installed-layout), never in the plugin cache. Removing the plugin leaves those programs, configuration and data intact. Use `clipbridge uninstall` for the Mac program and the [separate remote removal procedure](remote-images.md#stop-disable-and-remove) when desired.

No plugin command should bypass checksum failures, modify SSH host-key trust, install system packages automatically, or submit a real agent prompt merely to test image input.
