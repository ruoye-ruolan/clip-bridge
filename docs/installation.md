# Installation, Upgrade and Removal

Last updated: 2026-09-30.

ClipBridge 0.2 uses a native Rust executable. It installs into your user account without `sudo`, independently of the original checkout or extracted source package.

## Requirements

Building, installing from source and upgrading from source require macOS, Rust 1.94+ with Cargo, and the linker/SDK supplied by Xcode or Apple Command Line Tools. Install missing Apple tools with `xcode-select --install`. Cargo may download the dependencies recorded in `Cargo.lock` on the first build.

Normal installed operation does not need Python, Swift, Rust or Cargo. The executable calls macOS system tools, including SSH/SCP and launchctl, and accesses the clipboard through AppKit. A graphical login session is required for monitoring. The source installer builds for the current Mac; precompiled cross-machine release archives are not provided yet.

## Install

From a trusted checkout or extracted ClipBridge source package:

```sh
./install.sh
```

This builds the Rust executable with locked dependencies and installs that compiled version. `./clipbridge install` is equivalent when run from the source tree. The source `./clipbridge` wrapper always performs an incremental release build before running; the installed command does not build anything.

The installer creates a sample `~/.config/clipbridge/config.json` only when no user configuration is present. It can also migrate the source-local `src/config.json`. Existing user configuration is not overwritten. Edit the example values before first use; see [configuration](usage.md#configure-the-destination).

Ensure the command directory is on your shell's PATH:

```sh
export PATH="$HOME/.local/bin:$PATH"
clipbridge --version
```

If needed, add that export to your shell startup file, such as `~/.zshrc`. The installer does not edit shell startup files. The absolute command `~/.local/bin/clipbridge` also works without a PATH change.

Fresh installation does not enable clipboard monitoring. After configuration, keep the matching interactive SSH session open and run:

```sh
clipbridge start
```

If this source checkout or an existing managed installation already has a loaded background service, installation migrates/restarts it using its saved configuration. An unloaded login plist is updated without starting it. Foreground monitoring must be stopped with Ctrl+C before installation. A service from a different checkout is not silently replaced; stop it explicitly first.

## Migrate from Python 0.1

For the first upgrade from Python 0.1 to Rust 0.2, run the installer **from the new Rust source directory**:

```sh
./install.sh
```

The old Python `clipbridge upgrade` command expects the old source layout and cannot install this Rust payload. The new installer recognizes the existing managed installation, preserves configuration and replaces the Python launcher with a native-binary launcher. A previously loaded managed service is restored with the new executable. The prior payload is retained for recovery.

After a successful migration, Python and the old Swift helpers are no longer used by the current version.

## Installed Layout

| Location | Purpose |
| --- | --- |
| `~/.local/bin/clipbridge` | Small managed shell launcher for the native executable |
| `~/.local/share/clipbridge/releases/VERSION-ID/clipbridge` | Native Rust executable for one installed release |
| `~/.local/share/clipbridge/releases/VERSION-ID/` | Executable, documentation, example configuration, license and ownership inventory |
| `~/.local/share/clipbridge/current` | Link selecting the active payload |
| `~/.local/share/clipbridge/previous` | Previous successful payload retained after an upgrade |
| `~/.config/clipbridge/config.json` | User settings, outside versioned program files |
| `~/Library/LaunchAgents/local.clipbridge.plist` | Background service, when enabled |

The original source directory may be moved or deleted after successful installation. Keep the managed installation prefix in place while using the command or service.

For another installation prefix:

```sh
./install.sh --prefix "$HOME/tools"
```

This places the command under `~/tools/bin` and the program under `~/tools/share/clipbridge`. Configuration, logs, caches and launchd service remain per-user. Add the chosen `bin` directory to PATH. Custom prefixes do not create independent concurrent monitors.

## Upgrade a Rust Installation

Obtain the newer trusted source checkout or extract its source archive. Then run:

```sh
clipbridge upgrade --from /absolute/path/to/new-clipbridge
```

Alternatively, run `./install.sh` from the newer source. Reinstallation preserves existing configuration. For a custom prefix, pass the same `--prefix` to the source installer; installed `clipbridge upgrade` already knows its own prefix.

Upgrades are local and explicit. The command does not fetch code from GitHub or choose a latest release. `--from` must name an extracted source directory, not a tarball or URL. Cargo and the Apple build tools must be available for this operation.

The upgrade compiles the selected new source, then invokes that version's installer. It stages and checks the executable before stopping the existing service, switches the active release and restores a previously loaded monitor. Readiness requires monitor registration and acquisition of its upload lock; it does not verify an SSH transfer. A stopped service stays stopped. Active background transfers may be interrupted during replacement.

If activation fails or is cancelled, the installer attempts to restore the old release, launcher and service state. Recovery failures are reported explicitly with preserved program files for inspection. The current and immediately preceding successful payloads are retained; older recognized payloads are cleaned up. There is no manual rollback command yet. These recovery checks are not a guarantee against power loss during installation.

Configuration inside a source/program directory is copied to the user configuration directory before that active service is migrated. Existing files are not overwritten; a `migrated-*.json` filename is used if necessary. The installer prints the selected path. Custom configuration outside the source/program directory remains at its existing location; keep passing `--config` when it differs from the default.

## Uninstall

```sh
clipbridge uninstall
```

This stops the installation's background service, disables its login startup and removes its managed command and program payloads. It preserves user configuration, logs, retained local images, remote uploads and the original source checkout.

Stop a foreground monitor before uninstalling. An active configuration inside the program directory is copied outside it before removal. The uninstaller checks ownership and release inventories, refusing unfamiliar files or a modified launcher rather than deleting them.

If the installed launcher cannot run, use a trusted source checkout with a working Rust build toolchain:

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

The Rust packaging tool creates `dist/clipbridge-VERSION.tar.gz` and a matching `.sha256` checksum. The archive contains source, Cargo manifests/lockfile, installation wrappers, tests, documentation and the MIT license. It excludes personal configuration, caches, logs and generated binaries. Extraction produces a directory containing `install.sh`; installation builds the native executable locally.

`VERSION` and the version in `Cargo.toml` must agree. `clipbridge --version` reports the compiled version. Checksums detect corruption; they are not signatures or proof of publisher identity. Building a source package does not publish a GitHub Release.
