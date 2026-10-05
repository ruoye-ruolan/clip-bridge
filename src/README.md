# macOS Toolkit Source Guide

Last updated: 2026-10-03.

ClipBridge's Mac application, installer, source packager and Linux companion are implemented in Rust. The Mac application calls AppKit through `objc2` for read-only clipboard access and macOS tools for SSH transfers and service management. The optional Linux companion uses Xvfb, xauth and xclip. There are no Python modules or Swift helper processes.

Use [`../clipbridge`](../clipbridge) during development and [`../install.sh`](../install.sh) to install a copy independent of the checkout. See the [installation guide](../docs/installation.md), [usage guide](../docs/usage.md) and [release plan](../docs/planning/product-direction-and-distribution.md).

## Source Layout

| File | Responsibility |
| --- | --- |
| `main.rs` | Command parsing, optional setup/diagnostics, logs and upgrade dispatch |
| `lib.rs` | Shared application modules |
| `common.rs` | OS command execution, cancellation, paths, private files and locks |
| `config.rs` | JSON validation, SSH alias discovery, remote checks and atomic saves |
| `clipboard.rs` | Native AppKit clipboard access, PNG conversion and private-pasteboard checks |
| `monitor.rs` | SSH session detection, image capture, bounded upload/notification queues and completion handling |
| `remote.rs` | Embedded companion sources, SSH deployment, remote diagnostics and image publication |
| `control.rs` | Instance-specific foreground handoff and monitor registration |
| `service.rs` | Serialized launchd start/restart/stop/status and ownership checks |
| `installer.rs` | Versioned installation, configuration migration, recovery and safe removal |
| `bin/package.rs` | Allowlisted source archive and checksum generation |
| `config.example.json` | Placeholder configuration |
| `Makefile` | Convenience delegation to root build/test commands |
| `../tests/` | CLI and installed-binary integration checks |
| `../remote/` | Separate Rust companion crate: private Linux clipboard server, scoped CLI wrapper and isolated tests |
| `../remote/src/shell.rs` | Managed Bash/Zsh startup integration for ordinary `codex` and `claude` launches |

Unit tests live alongside their modules. Cargo build output is ignored under `../target/`. User settings normally live at `~/.config/clipbridge/config.json`; ignored source-local `config.json` remains a migration fallback. Installed releases contain the compiled executable and supporting documents, not a source runtime.

## Execution Flow

1. Source `./clipbridge` builds the release executable with `--locked` and runs it. Installed `clipbridge` runs the managed binary directly. `start` manages the per-user launchd job.
2. The monitor validates configuration, acquires `watcher.lock`, registers its instance and baselines the clipboard so startup content is ignored.
3. The main thread polls AppKit. It captures new PNG, TIFF or JPEG clipboard images as PNG only when a matching SSH session is detected.
4. One upload worker rechecks the session, stages the image, creates the remote directory and transfers through system SSH/SCP. Up to 16 uploads may wait; a new capture is discarded and logged when the queue is full.
5. If `remote_clipboard` is enabled, the upload worker invokes the Linux companion to publish the uploaded PNG. Publication failure retains the local image and reports partial success. Completion never writes the Mac clipboard. A separate worker attempts notifications, with up to 16 pending notifications; overflow skips only the notification.
6. Remote setup, unless given `--no-shell`, installs Bash/Zsh functions for `codex` and `claude`. Each launch invokes `clipbridge-remote run -- COMMAND`, which obtains the current private backend's display/authorization environment and exposes its PNG clipboard to that child. The wrapper clears Wayland selection only for the child, leaving the surrounding shell untouched. The explicit `run` command remains available for other applications or shell conflicts.

The AppKit clipboard object stays on the main thread and exposes only observation/capture to the monitor. Shutdown and cooperative foreground handoff drain uploads, remote publication and notifications before releasing the monitor lock. Background service replacement can still interrupt work. Remote publication updates the last image only; it does not mirror Mac text or automatically attach anything to a CLI prompt.

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
| `make clean` | Remove Cargo build output |
| `git diff --check` | Check whitespace errors |

`make -C src build`, `test`, `check`, `package` and `clean` delegate to the root. `make -C src configure`, `doctor` and `run` invoke the CLI and may contact a real host or start monitoring; they are not automated validation steps. Cargo downloads locked dependencies as needed. No separate Swift compilation or Python runtime is required.

Keep `VERSION`, both `Cargo.toml` files and their package entries in `Cargo.lock` consistent. `src/remote.rs` embeds the companion manifest, lockfile and source, so rebuild the Mac binary after companion changes. Remote setup runs `cargo vendor --locked --respect-source-config` on the Mac and uploads the resulting dependency tree for a remote `cargo build --release --frozen`. This needs Cargo on both hosts but no remote registry access. The installer verifies source metadata against the compiled executable and records a release inventory; use `./install.sh` to rebuild before installation. Update package inputs and installed document inventories when distributed files change. Follow [Repository Guidelines](../AGENTS.md).

## Test Boundaries

- Configuration tests cover validation, atomic private saves, alias discovery and remote-command construction.
- Monitor tests use fake clipboard/command implementations for offline skipping, stale content, upload failures, queue capacity and shutdown/lock behavior.
- Service and installer tests mock launchctl and isolate paths, checking idempotent start, handoff, ownership, configuration preservation, cancellation and recovery.
- Installed-binary integration tests use temporary home/prefix directories and fake launchd, remove the fixture source, and execute the native command with no language tools on PATH.
- A subprocess runs the AppKit self-test on a private named pasteboard, covering PNG/TIFF/JPEG conversion and stale captures while verifying extraction preserves its contents. It does not use the general clipboard.
- Remote deployment tests mock SSH/SCP and check configuration enablement only after dependency checks succeed. Remote companion tests use temporary state and fake X utilities for startup, image publication, environment scoping, ownership and cleanup. Shell integration checks isolate home/startup files and verify managed edits, repeated installation/removal, conflict handling and argument forwarding.

These checks do not establish actual Ghostty/CLI attachment behavior, real Linux/WSL X11 integration, SSH upload, login/logout, second-Mac, Intel Mac or every macOS version's compatibility. Validate those separately before advertising support. Never start a live service, change general clipboards or submit a real application prompt merely to run regression checks.

## Implementation Boundaries

- `remote_directory` remains required in JSON. The optional wizard resolves a remote-home default.
- Uploads may create the destination; optional `doctor` only checks an existing directory.
- SSH session detection uses process/socket heuristics. Multiplexing, jump hosts and IDE-managed connections are not guaranteed.
- Polling can miss rapid changes. Full upload queues skip new captures, while notification overflow leaves upload results intact.
- Foreground handoff waits up to 180 seconds for draining; background stop/restart may interrupt transfers.
- The Linux companion requires Xvfb/xauth/xclip at runtime. Source setup requires Rust/Cargo on both hosts; normal operation does not. An incompatible running backend must be explicitly stopped before setup can finish, and existing CLIs must be relaunched. Bash/Zsh integration refreshes the connection on each launch; existing shells need a one-time reconnect after integration installation. Its clipboard is shared by wrapped applications for the same user; an old image remains until publication succeeds.
- Failed images, remote uploads and logs have no automatic retention policy. Interrupted transfers may leave partial remote files.

The original source import came from a local `codex-shot` prototype. Preserve legacy-install migration without restoring personal paths, credentials or obsolete Xnip-only filters.
