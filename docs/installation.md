# Installation, Upgrade and Removal

Last updated: 2026-10-25.

ClipBridge 0.4.1 installs a native executable into your Mac user account, independently of the checkout or plugin cache. Its Linux companion is version 0.3.2 and supports ordinary Linux servers and WSL2.

Native Mac packages, Linux companion binaries and checksums are provided with [ClipBridge 0.4.1](https://github.com/ruoye-ruolan/clip-bridge/releases/tag/v0.4.1). Trusted local assets and source builds are supported as alternatives.

## Requirements

| Installation path | Mac requirements | Remote Linux/WSL requirements |
| --- | --- | --- |
| Native packages, default setup | Apple Silicon or Intel Mac, graphical login, system SSH/SCP and download tools | x86_64 or aarch64 Linux, SSH access, X11 utilities; no Cargo or GitHub access needed |
| Explicit source build | Rust 1.94+, Cargo and Apple's linker/SDK | Rust 1.94+, Cargo and a native linker for `--build-from-source` |

Both remote paths need `xvfb-run`, `Xvfb`, `xauth`, `xclip`, `sha256sum`, a POSIX shell, usual file utilities and an accessible `/proc`. No desktop session or SSH X11 forwarding is needed. See [Linux prerequisites](remote-images.md#prepare-the-linux-host). ClipBridge never installs remote system packages automatically.

Use a verified SSH alias with noninteractive authentication. Configuration, SSH keys and user data stay outside release packages. Run installation and setup as your normal user, without `sudo`.

## Native Installation

Download your Mac archive and its checksum from the [0.4.1 release](https://github.com/ruoye-ruolan/clip-bridge/releases/tag/v0.4.1).

| Mac | Archive |
| --- | --- |
| Apple Silicon | `clipbridge-0.4.1-aarch64-apple-darwin.tar.gz` |
| Intel | `clipbridge-0.4.1-x86_64-apple-darwin.tar.gz` |

For Apple Silicon:

```sh
shasum -a 256 -c clipbridge-0.4.1-aarch64-apple-darwin.tar.gz.sha256
tar -xzf clipbridge-0.4.1-aarch64-apple-darwin.tar.gz
cd clipbridge-0.4.1-aarch64-apple-darwin
./install.sh
export PATH="$HOME/.local/bin:$PATH"
clipbridge setup
```

Use the Intel filenames on Intel Macs. The `install.sh` inside a native bundle installs its compiled executable; it does not invoke Cargo. A bundle contains `bundle.json`, the executable, installation metadata, guides and the MIT license.

Installation preserves configuration and creates a sample only when no user configuration exists. Fresh installation does not start monitoring. `clipbridge setup` selects an SSH alias and image directory, installs the remote companion/integration, and applies your background-startup choice. See [one-time setup](usage.md#one-time-setup).

The standalone `install-release.sh` from the trusted matching tag or plugin automatically selects the Mac architecture, downloads and verifies that version's archive, and installs it:

```sh
sh ./install-release.sh --setup
```

Without `--setup`, it installs only. It is pinned to 0.4.1, refuses a downgrade of a newer managed Mac installation, and stops on missing assets or checksum failures. It does not silently compile source or select a different release.

To use another prefix, pass `--prefix "$HOME/tools"` to either installer. Add the resulting `bin` directory to PATH. The Mac installer does not edit shell startup files; remote shell integration is configured separately by setup.

## Local Assets and Offline Installation

Place the matching Mac archive and `.sha256` file in a trusted local directory:

```sh
sh ./install-release.sh --assets-dir /absolute/path/to/assets
export PATH="$HOME/.local/bin:$PATH"
clipbridge setup --host dev-server --start --assets-dir /absolute/path/to/assets
```

The setup command also needs the remote architecture's binary and matching checksum in that directory:

- `clipbridge-remote-0.3.2-x86_64-unknown-linux-musl`
- `clipbridge-remote-0.3.2-aarch64-unknown-linux-musl`

Supply the applicable binary and its `.sha256` sidecar. The directory is on the Mac; setup verifies the asset and uploads it over SSH. No remote download is needed. `--assets-dir` and `--build-from-source` are mutually exclusive.

For an interactive combined flow, `sh ./install-release.sh --assets-dir /absolute/path/to/assets --setup` forwards the same asset directory to the setup wizard.

For local development, `make native-package` creates the current Mac archive. Building the Linux assets is a separate target build; see [release packaging](../src/README.md#native-release-packaging). Use locally verified assets or explicit source setup when testing a development build or working without release-download access.

## Installed Layout

| Location | Purpose |
| --- | --- |
| `~/.local/bin/clipbridge` | Managed launcher for the native executable |
| `~/.local/share/clipbridge/releases/VERSION-ID/` | Executable, guides, example configuration, license and ownership inventory |
| `~/.local/share/clipbridge/current` | Link selecting the active payload |
| `~/.local/share/clipbridge/previous` | Previous successful payload retained for recovery |
| `~/.config/clipbridge/config.json` | User configuration outside the program |
| `~/Library/LaunchAgents/local.clipbridge.plist` | Background/login service, when enabled |

The checkout, extracted bundle and plugin cache are unnecessary for installed operation. Keep the managed installation prefix in place. Custom prefixes still share per-user configuration, logs, caches and one launchd service.

If an owned background service is already loaded, installation migrates/restarts it using its saved configuration. An unloaded login plist is updated without starting it. Stop a foreground monitor with Ctrl+C before installation. A service from another checkout is not silently replaced.

## Upgrade a Rust Installation

For an installed 0.4.0-or-newer command, use a trusted extracted native bundle or source checkout:

```sh
clipbridge upgrade --from /absolute/path/to/new-clipbridge
```

A native bundle is checked for its manifest, Mac architecture, version and executable digest before use; it needs no Cargo. A source checkout is compiled with locked dependencies before installation. `--from` accepts a directory, not a URL or archive, and does not fetch the latest release automatically.

To upgrade an older installation that does not understand native bundles, run `./install.sh` from the new verified native bundle, or use its standalone release installer. Reinstallation preserves configuration. Pass the same custom prefix when applicable.

After upgrading, use `clipbridge setup` to configure both hosts and the desired startup setting. To update only the configured remote companion/integration, use `clipbridge remote setup`; it does not reload the Mac monitor. Default setup downloads the companion attached to the Mac's exact release tag, even when the companion's own version differs.

Mac 0.4.1 retains companion 0.3.2. The helper accepts a running 0.3.1 backend through an explicit compatibility pair, so that backend and already integrated CLIs can remain running. Unknown backend versions are refused. If setup reports an incompatible backend, explicitly stop it with `clipbridge remote stop`, repeat setup, then relaunch remote CLIs.

OpenCode automatic shell integration was introduced in Mac 0.3.4 with companion 0.3.2. The exact older managed snippet upgrades in place; edited snippets are refused without modification, and custom aliases/functions are preserved. Reconnect SSH or source the updated snippet, then relaunch an OpenCode process that was started without integration.

Local installation stages and verifies a new payload before switching. Activation failure triggers an attempt to restore the previous release, launcher and service; recovery failures are reported with retained files. Readiness checks registration and the monitor lock, not an image transfer. A stopped service stays stopped. Active transfers may be interrupted during service replacement. There is no manual rollback command or guarantee against power loss.

## Build and Install from Source

Source installation needs Rust 1.94+, Cargo and Apple's linker/SDK. Install missing Apple tools with `xcode-select --install`. From a trusted checkout or extracted **source** archive:

```sh
./install.sh
export PATH="$HOME/.local/bin:$PATH"
clipbridge setup --build-from-source
```

The repository's `install.sh` builds the Mac executable. `./clipbridge install` is equivalent; source `./clipbridge` always performs an incremental release build, while the installed command does not.

`--build-from-source` explicitly selects remote source deployment. Cargo vendors locked dependencies on the Mac, uploads them with the embedded sources, and compiles on Linux with `--frozen`. Both hosts need Cargo and linkers; only the Mac needs registry access or cached dependencies. Normal operation needs no Rust toolchain on either host.

To rebuild only the configured companion:

```sh
clipbridge remote setup --build-from-source
```

Unsupported native release architectures need an explicit source build on a supported Linux toolchain; setup does not silently fall back to compiling.

## Migrate from Python 0.1

Run the new release installer or `./install.sh` from a trusted native bundle/source tree. The old Python `upgrade` command cannot install the current Rust payload. The installer recognizes the owned legacy installation, preserves configuration and replaces its launcher with the native command.

Source-local `src/config.json` can be migrated when the default user configuration is absent. An active configuration inside the old source/program directory is copied outside that payload before service migration. Existing files are preserved; the installer prints a `migrated-*.json` path when necessary. Keep using `--config` for a custom configuration path.

## Uninstall

```sh
clipbridge uninstall
```

This stops the installation's background service, disables login startup and removes its owned Mac program. Configuration, logs, retained images, remote uploads and the source checkout remain. Unknown or modified owned files are refused rather than deleted.

Stop foreground monitoring first. Mac removal does not stop or uninstall the remote companion: follow [remote stop/removal](remote-images.md#stop-disable-and-remove) separately. Removing the Claude Code plugin also leaves the native installation intact; see [plugin removal](plugins.md#updates-and-removal).

If the installed launcher cannot run, use `./clipbridge uninstall` from a trusted native bundle, or from a source checkout with a working build toolchain. Pass `--prefix` for the installation being removed. There is no automatic purge of user data.

## Build a Source Distribution

```sh
make package
```

This creates `dist/clipbridge-VERSION.tar.gz` and its `.sha256` file. The source archive contains both Rust crates and their tests, installation/packaging scripts, plugin metadata/skills, guides and the MIT license. It excludes personal configuration, logs, caches, screenshots, tutorial media and generated binaries. Source installation builds the Mac executable locally.

Native archives and Linux binaries are built separately. The GitHub workflow builds four target platforms and attaches verified artifacts to a draft release when a version tag is pushed; publication remains a separate step. Checksums detect corruption and mismatched files; obtain them and the assets from a trusted release. They are not signatures.
