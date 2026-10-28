# ClipBridge

Last updated: 2026-10-25.

Copy a screenshot on your Mac and paste it as an image into a Linux or WSL2 CLI over SSH. ClipBridge keeps the original Mac clipboard image and publishes a copy to a private remote image clipboard.

**ClipBridge 0.4.1** bundles Linux companion 0.3.2. Install a native Mac package or use the Claude Code plugin for guided setup; neither path needs Rust or Cargo. Source builds and local assets remain available for [development and offline installation](docs/installation.md).

## Install and Set Up on Your Mac

Download the matching Mac archive and its `.sha256` file from [ClipBridge 0.4.1](https://github.com/ruoye-ruolan/clip-bridge/releases/tag/v0.4.1):

- Apple Silicon: `clipbridge-0.4.1-aarch64-apple-darwin.tar.gz`
- Intel: `clipbridge-0.4.1-x86_64-apple-darwin.tar.gz`

For Apple Silicon, run from the download directory:

```sh
shasum -a 256 -c clipbridge-0.4.1-aarch64-apple-darwin.tar.gz.sha256
tar -xzf clipbridge-0.4.1-aarch64-apple-darwin.tar.gz
cd clipbridge-0.4.1-aarch64-apple-darwin
./install.sh
export PATH="$HOME/.local/bin:$PATH"
clipbridge setup
```

Use the Intel filename on Intel Macs. Setup asks for your SSH alias, defaults the image directory to the remote account's home, and lets you choose background monitoring and login startup. It installs the Linux companion and Bash/Zsh integration from the Mac. Ordinary Linux servers and WSL2 use the same setup; neither needs Cargo or direct access to GitHub.

The remote host needs `xvfb-run`, `Xvfb`, `xauth` and `xclip`. A desktop session is unnecessary. If dependencies are missing, setup prints instructions for your administrator; it does not install system packages. See [installation](docs/installation.md) and [Linux prerequisites](docs/remote-images.md#prepare-the-linux-host).

## Set Up Through Claude Code

Open Claude Code **on the Mac whose clipboard you want to sync**, then add and install the plugin:

```text
/plugin marketplace add https://github.com/ruoye-ruolan/clip-bridge
/plugin install clipbridge@clipbridge
/clipbridge:setup
```

Complete the plugin install flow before invoking setup. The plugin guides installation and configuration; the native program runs independently of the plugin cache. It also provides `/clipbridge:status` and `/clipbridge:doctor`. See the [plugin guide](docs/plugins.md) for scope, updates and removal.

## Use It

Open a new SSH shell after setup and keep it connected:

```sh
ssh dev-server
```

Inside that remote shell, start the desired CLI normally:

```sh
codex
# Or:
claude
# Or:
opencode
```

Copy a new image on the Mac, wait for **remote image clipboard ready**, then use the CLI's image-paste shortcut. Local image paste remains available. Existing CLI processes launched without integration need relaunching; existing aliases/functions are preserved. See [remote image use](docs/remote-images.md) for shell reloads, custom commands and terminal-specific validation limits.

Only new images copied while a matching ordinary SSH session is open are eligible. For multiple routes to the same server, configure [alternative SSH aliases](docs/usage.md#multiple-network-routes). The tool does not reconnect SSH automatically.

## Everyday Commands

| Command | Action |
| --- | --- |
| `clipbridge setup` | Configure the Mac, remote companion and startup choice |
| `clipbridge hosts --json` | List local SSH aliases without connecting |
| `clipbridge start` | Enable background monitoring and login startup |
| `clipbridge stop` | Stop background monitoring and disable login startup |
| `clipbridge status` | Show monitor state |
| `clipbridge logs --follow` | Follow capture and upload results |
| `clipbridge doctor` | Diagnose SSH and destination access |
| `clipbridge remote status` | Show remote clipboard state |
| `clipbridge uninstall` | Remove the Mac program/service while preserving user data |

Images use one SSH request and can reuse a private connection for 60 seconds of inactivity. See [transfer speed](docs/usage.md#transfer-speed), [configuration](docs/usage.md) and [upgrade/removal](docs/installation.md) for details.

## Repository Guide

- [Installation and release packages](docs/installation.md)
- [Linux/WSL clipboard setup](docs/remote-images.md)
- [Claude Code plugin](docs/plugins.md)
- [Chinese video tutorial](https://github.com/ruoye-ruolan/clip-bridge/tree/v0.4.0/tutorial) — 2:52, 1080p; covers the older 0.3.4 source-installation flow. Use the instructions above for 0.4.0 native/plugin setup.
- [Rust development guide](src/README.md) and [contributor guidelines](AGENTS.md)
- [Remaining validation and future plans](docs/planning/product-direction-and-distribution.md)

Source development uses `make build`, `make check` and `make test`. `make package` creates a source archive; `make native-package` packages the current Mac build. Neither publishes a release. Native clipboard tests use private pasteboards, and service/network tests use isolated fixtures.

[MIT License](LICENSE).
