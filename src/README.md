# macOS Toolkit Source Guide

Last updated: 2026-10-25.

ClipBridge's Mac application, installer, source packager and Linux companion are implemented in Rust. The Mac application calls AppKit through `objc2` for read-only clipboard access and macOS tools for SSH transfers and service management. The optional Linux companion uses Xvfb, xauth and xclip. There are no Python modules or Swift helper processes.

Use [`../clipbridge`](../clipbridge) during development and [`../install.sh`](../install.sh) to install a copy independent of the checkout. See the [installation guide](../docs/installation.md), [usage guide](../docs/usage.md) and [release plan](../docs/planning/product-direction-and-distribution.md).

## Source Layout

| File | Responsibility |
| --- | --- |
| `main.rs` | Command parsing, guided setup, offline SSH alias listing, diagnostics and upgrade dispatch |
| `setup.rs` | Local configuration/service transaction around remote deployment and startup selection |
| `release.rs` | Fixed-version asset downloads, checksums, Linux target selection and native bundle validation |
| `lib.rs` | Shared application modules |
| `common.rs` | OS command execution with validated file-backed stdin, cancellation, paths, private files and locks |
| `config.rs` | JSON validation, ordered allowed SSH aliases, route-specific settings, remote checks and atomic saves |
| `clipboard.rs` | Native AppKit clipboard access, PNG conversion and private-pasteboard checks |
| `monitor.rs` | SSH session detection/route selection, image capture, bounded upload/notification queues and completion handling |
| `transport.rs` | Streamed image transfer, atomic remote staging, commit receipts and private SSH connection reuse/cleanup |
| `remote.rs` | Prebuilt/source companion deployment, embedded sources, remote diagnostics and image publication |
| `control.rs` | Instance-specific foreground handoff and monitor registration |
| `service.rs` | Serialized launchd start/restart/stop/status and ownership checks |
| `installer.rs` | Versioned installation, configuration migration, recovery and safe removal |
| `bin/package.rs` | Allowlisted source archive and checksum generation |
| `config.example.json` | Placeholder configuration |
| `Makefile` | Convenience delegation to root build/test commands |
| `../tests/` | CLI and installed-binary integration checks |
| `../remote/` | Separate Rust companion crate: private Linux clipboard server, scoped CLI wrapper and isolated tests |
| `../remote/src/shell.rs` | Managed Bash/Zsh startup integration for ordinary `codex`, `claude` and `opencode` launches |
| `../install-release.sh` | Pinned native Mac bootstrap with architecture and checksum validation |
| `../scripts/package-native.sh` | Native Mac archives and Linux executable/checksum packaging |
| `../.github/workflows/release.yml` | Four-platform builds and draft-only release attachment |
| `../.claude-plugin/`, `../skills/` | Claude Code marketplace metadata and setup/status/doctor skills |

Unit tests live alongside their modules. Cargo build output is ignored under `../target/`. User settings normally live at `~/.config/clipbridge/config.json`; ignored source-local `config.json` remains a migration fallback. Installed releases contain the compiled executable and supporting documents, not a source runtime.

## Execution Flow

1. Source `./clipbridge` builds the release executable with `--locked` and runs it. Installed `clipbridge` runs the managed binary directly. `start` manages the per-user launchd job.
2. The monitor validates configuration, acquires `watcher.lock`, registers its instance and baselines the clipboard so startup content is ignored.
3. The main thread polls AppKit. It captures new PNG, TIFF or JPEG clipboard images as PNG only when a matching SSH session is detected. Selection prefers `ssh_host`, then `ssh_aliases` in order, and pins the chosen alias to that capture.
4. One upload worker rechecks the captured route and stages the local image. Its private transport cache streams that file as stdin to one SSH command. The remote script verifies an SSH-user-owned directory without group/other write permission, pins it as the working directory, then uses relative staging paths. It checks the received byte count and atomically renames the temporary file into place. If the captured route disconnected, the worker skips the job instead of retargeting it. Up to 16 uploads may wait; a new capture is discarded and logged when the queue is full.
5. The same remote command emits a per-image commit receipt and, when `remote_clipboard` is enabled, invokes the Linux companion to publish the PNG. An acknowledged upload followed by publication failure retains the local image and reports partial success. Missing confirmation is treated as failure, even if the process exits successfully. Completion never writes the Mac clipboard. A separate worker attempts notifications, with up to 16 pending notifications; overflow skips only the notification.
6. `setup` stages a candidate configuration, deploys the remote component, then commits local settings and applies `--start` or `--no-start`. Activation failure attempts to restore local configuration/service state. A successfully deployed remote helper can remain installed. Default deployment downloads a checksum-verified Linux executable on the Mac; `--build-from-source` explicitly selects vendoring and offline remote compilation.
7. Remote setup, unless given `--no-shell`, installs Bash/Zsh functions for `codex`, `claude` and `opencode`. Each launch invokes `clipbridge-remote run -- COMMAND`, which obtains the current private backend's display/authorization environment and exposes its PNG clipboard to that child. The wrapper clears Wayland selection only for the child, leaving the surrounding shell untouched. The explicit `run` command remains available for other applications or shell conflicts. An exact legacy managed snippet upgrades in place; edited snippets are refused without modification.

The AppKit clipboard object stays on the main thread and exposes only observation/capture to the monitor. Shutdown and cooperative foreground handoff drain uploads, remote publication and notifications and close owned transport sockets before releasing the monitor lock. Background service replacement can still interrupt work. Remote publication updates the last image only; it does not mirror Mac text or automatically attach anything to a CLI prompt.

Transport state belongs to the sequential upload worker and is created lazily after an image passes the pinned-session check. Each alias uses an owner-only temporary directory and a `ControlPath` containing `%C`, so effective host/user/port/jump settings select distinct sockets. The first real transfer establishes the connection; subsequent transfers can reuse it, with `ControlPersist=60`. Failures invalidate that alias's cache without retrying the image. Cleanup targets only owned sockets and does not use the user's control socket or change `~/.ssh/config`. Tool transport processes cannot satisfy the interactive-session detector.

The image-transfer command allows a 30-second handshake and 150 seconds overall. File-backed stdin is opened without following symlinks and checked for regular-file type and expected length before spawning the child. Cleanup uses a local SSH control operation with a three-second limit per owned socket. `capture-to-completion N ms` includes queue time and publication, but excludes initial polling and notification delivery.

## Build and Development Commands

Use macOS, Rust 1.94+ with Cargo, and Apple's linker/SDK from Xcode or Command Line Tools. Run from the repository root:

```sh
make build
make test
make check
```

| Command | Purpose |
| --- | --- |
| `make build` | Build `target/release/clipbridge` with locked dependencies |
| `make test` | Run both crates' Rust tests, CLI/installer checks and private-pasteboard checks |
| `make check` | Check both crates' rustfmt formatting and Clippy with warnings denied |
| `make package` | Run the Rust source packager and create `dist/` artifacts |
| `make native-package` | Build/package the current Mac architecture as a native archive |
| `make clean` | Remove Cargo build output |
| `git diff --check` | Check whitespace errors |

`make -C src build`, `test`, `check`, `package` and `clean` delegate to the root. `make -C src configure`, `doctor` and `run` invoke the CLI and may contact a real host or start monitoring; they are not automated validation steps. Cargo downloads locked dependencies as needed. No separate Swift compilation or Python runtime is required.

Keep root `VERSION`, root `Cargo.toml`, the Mac package entry in root `Cargo.lock`, the bootstrap pin and plugin manifest aligned at 0.4.0. The companion has a separate manifest/lockfile version, currently 0.3.2. That client accepts its own backend and the explicitly tested 0.3.1 pair. Unknown backend versions remain incompatible.

`src/remote.rs` embeds companion sources for `--build-from-source`; rebuild the Mac binary after companion changes. Default setup downloads the remote architecture's executable from the fixed official tag matching the Mac version, then verifies the checksum and remote staged bytes. The source path vendors locally and builds with `--frozen` remotely, requiring Cargo on both hosts. Source installation uses the repository's `./install.sh`; a native bundle's generated `install.sh` installs its existing executable. Update every allowlist and installed document inventory when distributed files change. Follow [Repository Guidelines](../AGENTS.md).

## Native Release Packaging

The native targets are `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`. Mac archives include the executable, a `bundle.json` manifest, generated installer and installation metadata/guides. Linux assets are static musl companion executables with independent version names. Every asset has a `.sha256` sidecar.

For the current Mac:

```sh
make native-package
```

To package an already built target explicitly:

```sh
scripts/package-native.sh --target aarch64-apple-darwin --binary target/release/clipbridge
```

On an appropriate Linux build host with the Rust target and musl linker installed:

```sh
cargo build --manifest-path remote/Cargo.toml --release --locked --target x86_64-unknown-linux-musl
scripts/package-native.sh --target x86_64-unknown-linux-musl --binary remote/target/x86_64-unknown-linux-musl/release/clipbridge-remote
```

The packaging script does not build or install a binary. Keep platform and binary paths matched. Validate local artifacts using `install-release.sh --assets-dir` and `clipbridge setup --assets-dir`; these commands install/configure programs and are not isolated tests by default.

The GitHub workflow builds all four targets. A manual dispatch retains build artifacts; a version-tag push also creates or updates a draft release after verifying checksums. It refuses to modify a published release and never publishes automatically. Review platform validation, attachment contents and short release notes before publication. Default downloads use the exact tag matching the compiled Mac version; development builds without matching published assets must use local assets or explicit source setup.

The Claude Code plugin runs the bootstrap from its cache but installs the native program into the managed user prefix. Services must never reference the plugin cache. Its skills have explicit setup, read-only status and diagnostic roles; see the [plugin guide](../docs/plugins.md).

## Test Boundaries

- Configuration tests cover validation, unique ordered aliases, atomic private saves, alias discovery and remote-command construction. Setup tests isolate staged configuration, concurrent changes, startup selection, cancellation and recovery; release tests verify asset names, digests, architecture and native bundle metadata.
- Monitor tests use fake clipboard/command implementations for route selection/pinning, offline skipping, stale content, upload failures, queue capacity and shutdown/lock behavior.
- Transport tests use isolated files and mocked commands for file-backed stdin, byte-count checks, staging/commit receipts, publication errors, connection reuse and owned-socket cleanup. They do not upload personal images or start a remote backend.
- Service and installer tests mock launchctl and isolate paths, checking idempotent start, handoff, ownership, configuration preservation, cancellation and recovery.
- Installed-binary integration tests use temporary home/prefix directories and fake launchd, remove the fixture source, and execute the native command with no language tools on PATH.
- A subprocess runs the AppKit self-test on a private named pasteboard, covering PNG/TIFF/JPEG conversion and stale captures while verifying extraction preserves its contents. It does not use the general clipboard.
- Remote deployment tests mock SSH/SCP/downloads and check architecture selection, dependency preflight, staged binary verification, activation recovery and configuration enablement. Remote companion tests use temporary state and fake X utilities for startup, image publication, environment scoping, ownership, version compatibility and cleanup. Shell integration checks isolate home/startup files and verify managed edits, legacy snippet upgrades, repeated installation/removal, conflicts and argument forwarding for all three commands.

These checks do not establish actual Ghostty/CLI attachment behavior, real Linux/WSL X11 integration, SSH upload, login/logout, second-Mac, Intel Mac or every macOS version's compatibility. Validate those separately before advertising support. Never start a live service, change general clipboards or submit a real application prompt merely to run regression checks.

## Implementation Boundaries

- `remote_directory` remains required in JSON. It must be owned and writable by the SSH user and not group/other-writable. Existing permissions are not changed automatically. The optional wizard resolves a remote-home default.
- Uploads may create the destination; optional `doctor` only checks an existing directory.
- SSH aliases must reach one machine/account and directory. Monitoring selects existing interactive sessions; it does not probe networks or reconnect. Old TCP sockets may appear established after network changes until their sessions are closed. User-managed multiplexed interactive sessions, jump hosts and IDE-managed connections are not guaranteed; the tool's private upload transports do not count as interactive sessions.
- `doctor` and remote management commands pin the preferred detected alias, falling back to the primary alias only when no session is detected. Connection failure does not trigger another route selection.
- Polling can miss rapid changes. Full upload queues skip new captures, while notification overflow leaves upload results intact.
- Foreground handoff waits up to 180 seconds for draining; background stop/restart may interrupt transfers.
- The Linux companion requires Xvfb/xauth/xclip at runtime. Default prebuilt setup requires neither remote Cargo nor GitHub access; explicit source setup requires Rust/Cargo and linkers on both hosts. An incompatible running backend must be explicitly stopped before setup can finish, and existing CLIs must be relaunched. Bash/Zsh integration refreshes the connection on each launch; existing shells need a one-time reconnect after integration installation. Its clipboard is shared by wrapped applications for the same user; an old image remains until publication succeeds.
- Failed local images, finalized remote uploads and logs have no automatic retention policy. Remote cleanup traps remove staging files when they can run; abrupt termination can leave temporary files. Incomplete transfers are not renamed into final image paths or published.

The original source import came from a local `codex-shot` prototype. Preserve legacy-install migration without restoring personal paths, credentials or obsolete Xnip-only filters.
