# Installation, Upgrade and Removal

Last updated: 2026-09-30.

ClipBridge installs into your user account without `sudo`. The installed command and background service run from a managed program directory, independently of the original checkout or extracted source package.

## Requirements

Installation and upgrades require macOS, Python 3.9+, `make` and `swiftc` from Xcode or Command Line Tools. Install missing Apple tools with `xcode-select --install`. There are no third-party Python dependencies.

The installer builds native helpers for the current Mac. Normal installed operation uses these compiled helpers and does not invoke `make` or `swiftc`. Python remains an external runtime dependency: keep the selected Python installation available. A bundled Python runtime and cross-machine precompiled binaries are not provided.

## Install

From a trusted checkout or extracted ClipBridge source package:

```sh
./install.sh
```

The equivalent command is `./clipbridge install`. To choose a Python interpreter explicitly:

```sh
CLIPBRIDGE_PYTHON=/path/to/python3 ./install.sh
```

The installer creates a sample `~/.config/clipbridge/config.json` only when no user configuration is present. It can also migrate the source-local `src/config.json`. Existing user configuration is not overwritten. Edit the example values before first use; see [configuration](usage.md#configure-the-destination).

Ensure the command directory is on your shell's PATH:

```sh
export PATH="$HOME/.local/bin:$PATH"
clipbridge --version
```

If needed, add that export to your shell startup file, such as `~/.zshrc`, for future terminals. The installer does not edit shell startup files. The absolute command `~/.local/bin/clipbridge` also works without a PATH change.

Fresh installation does not enable clipboard monitoring. After configuration, keep the matching interactive SSH session open and run:

```sh
clipbridge start
```

If this source checkout or an existing managed installation already has a loaded background service, installation migrates/restarts it using its saved configuration. An unloaded login plist is updated without starting it. Foreground monitoring must be stopped with Ctrl+C before installation. A service from a different checkout is not silently replaced; stop it explicitly first.

## Installed Layout

| Location | Purpose |
| --- | --- |
| `~/.local/bin/clipbridge` | Managed launcher using the selected Python interpreter |
| `~/.local/share/clipbridge/releases/VERSION-ID/` | Self-contained program payload, documentation and compiled helpers |
| `~/.local/share/clipbridge/current` | Link selecting the active payload |
| `~/.local/share/clipbridge/previous` | Previous successful payload retained after an upgrade |
| `~/.config/clipbridge/config.json` | User settings, outside versioned program files |
| `~/Library/LaunchAgents/local.clipbridge.plist` | Background service, when enabled |

The original source directory may be moved or deleted after successful installation. Do not move the managed prefix or remove its Python interpreter while it is in use. Installing again with an available interpreter updates the launcher and service paths.

For another installation prefix:

```sh
./install.sh --prefix "$HOME/tools"
```

This places the command under `~/tools/bin` and the program under `~/tools/share/clipbridge`. Configuration, logs, caches and launchd service remain per-user. Add the chosen `bin` directory to PATH. Custom prefixes do not create independent concurrent monitors.

## Upgrade

Obtain the newer trusted source checkout or extract its source archive. Then run:

```sh
clipbridge upgrade --from /absolute/path/to/new-clipbridge
```

Alternatively, run `./install.sh` again from the newer source. Reinstallation is supported and preserves existing configuration. For a custom prefix, pass the same `--prefix` when using a source-tree installer; installed `clipbridge upgrade` already knows its own prefix.

Upgrades are local and explicit. This command does not fetch code from GitHub, choose a latest release or execute downloaded installation scripts. `--from` must name an extracted source directory, not a tarball or URL.

The upgrade builds and checks the new payload before stopping the existing service. It then switches the active release, updates the launcher, and restores a previously loaded monitor. A readiness check requires that monitor to register and acquire its upload lock; it does not verify an SSH transfer. A stopped service stays stopped. Active background transfers may be interrupted during replacement.

If activation fails or is cancelled, the installer attempts to restore the old release, launcher and service state. Recovery failures are reported explicitly with preserved program files for inspection. The current and immediately preceding successful payloads are retained; older recognized payloads are cleaned up. There is no manual rollback command yet—rerun an older trusted source installer if needed. These exception-recovery checks are not a guarantee against power loss during installation.

Configuration files stored inside a source/program directory are copied to the user configuration directory before that active service is migrated. Existing files are not overwritten; a `migrated-*.json` filename is used if necessary. The installer prints the selected configuration path. Custom configuration outside the source/program directory remains at its existing location; keep passing `--config` if it differs from the default.

## Uninstall

```sh
clipbridge uninstall
```

This stops the installation's background service, disables its login startup, removes its managed command and program payloads, and preserves:

- User configuration, including external custom configuration files.
- Logs and local retained images.
- Every file previously uploaded to a remote host.
- The original source checkout or extracted package.

Stop a foreground monitor before uninstalling. An active configuration inside the program directory is copied outside it before removal. The uninstaller refuses unfamiliar or modified command files and unmanaged program data rather than deleting them.

If the installed launcher cannot run, use a trusted source checkout with a working Python interpreter:

```sh
./clipbridge uninstall
# For a custom prefix:
./clipbridge uninstall --prefix "$HOME/tools"
```

There is no automatic purge of configuration, logs or images. Inspect those separately before manual cleanup.

## Build a Source Distribution

From the repository root:

```sh
make package
```

This creates `dist/clipbridge-VERSION.tar.gz` and a matching `.sha256` checksum. The archive contains the installer, source, tests, documentation and MIT license. It excludes personal configuration, environment files, caches, logs and generated binaries. Extraction produces a directory containing `install.sh`; installation compiles helpers locally.

The package version comes from the root `VERSION` file; `clipbridge --version` shows the active version. Checksums detect corruption; they are not signatures or proof of publisher identity. Building a package does not publish a GitHub Release.
