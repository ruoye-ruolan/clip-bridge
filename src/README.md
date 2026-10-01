# macOS Toolkit Source Guide

Last updated: 2026-09-30.

ClipBridge's application, installer and source packager are implemented in Rust. The application is one native executable; it calls AppKit through `objc2` for clipboard access and macOS tools for SSH transfers and service management. There are no Python modules or Swift helper processes.

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
| `control.rs` | Instance-specific foreground handoff and monitor registration |
| `service.rs` | Serialized launchd start/restart/stop/status and ownership checks |
| `installer.rs` | Versioned installation, configuration migration, recovery and safe removal |
| `bin/package.rs` | Allowlisted source archive and checksum generation |
| `config.example.json` | Placeholder configuration |
| `Makefile` | Convenience delegation to root build/test commands |
| `../tests/` | CLI and installed-binary integration checks |

Unit tests live alongside their modules. Cargo build output is ignored under `../target/`. User settings normally live at `~/.config/clipbridge/config.json`; ignored source-local `config.json` remains a migration fallback. Installed releases contain the compiled executable and supporting documents, not a source runtime.

## Execution Flow

1. Source `./clipbridge` builds the release executable with `--locked` and runs it. Installed `clipbridge` runs the managed binary directly. `start` manages the per-user launchd job.
2. The monitor validates configuration, acquires `watcher.lock`, registers its instance and baselines the clipboard so startup content is ignored.
3. The main thread polls AppKit. It captures new PNG, TIFF or JPEG clipboard images as PNG only when a matching SSH session is detected.
4. One upload worker rechecks the session, stages the image, creates the remote directory and transfers through system SSH/SCP. Up to 16 uploads may wait; a new capture is discarded and logged when the queue is full.
5. Completion returns to the main thread, which checks the clipboard change count before copying the remote path. A separate worker attempts notifications, with up to 16 pending notifications; overflow skips only the notification.

The AppKit clipboard object stays on the main thread. Shutdown and cooperative foreground handoff stop intake and drain workers before releasing the monitor lock. Background service replacement can still interrupt work. Change-count comparison and clipboard replacement are not atomic.

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
| `make test` | Run Rust unit, CLI, installer and private-pasteboard checks |
| `make check` | Check rustfmt formatting and run Clippy with warnings denied |
| `make package` | Run the Rust source packager and create `dist/` artifacts |
| `make clean` | Remove Cargo build output |
| `git diff --check` | Check whitespace errors |

`make -C src build`, `test`, `check`, `package` and `clean` delegate to the root. `make -C src configure`, `doctor` and `run` invoke the CLI and may contact a real host or start monitoring; they are not automated validation steps. Cargo downloads locked dependencies as needed. No separate Swift compilation or Python runtime is required.

Keep `VERSION`, `Cargo.toml` and the root package entry in `Cargo.lock` consistent. The installer verifies source metadata against the compiled executable and records a release inventory; use `./install.sh` to rebuild before installation. The packager's explicit inputs exclude personal settings and generated output. Follow [Repository Guidelines](../AGENTS.md).

## Test Boundaries

- Configuration tests cover validation, atomic private saves, alias discovery and remote-command construction.
- Monitor tests use fake clipboard/command implementations for offline skipping, stale content, upload failures, queue capacity and shutdown/lock behavior.
- Service and installer tests mock launchctl and isolate paths, checking idempotent start, handoff, ownership, configuration preservation, cancellation and recovery.
- Installed-binary integration tests use temporary home/prefix directories and fake launchd, remove the fixture source, and execute the native command with no language tools on PATH.
- A subprocess runs the AppKit self-test on a private named pasteboard, covering PNG/TIFF/JPEG conversion, stale captures and conditional path writing. It does not use the general clipboard.

These checks do not establish real SSH upload, login/logout, second-Mac, Intel Mac or every macOS version's compatibility. Validate those separately before advertising support. Never start a live service or upload personal clipboard contents simply to run regression checks.

## Implementation Boundaries

- `remote_directory` remains required in JSON. The optional wizard resolves a remote-home default.
- Uploads may create the destination; optional `doctor` only checks an existing directory.
- SSH session detection uses process/socket heuristics. Multiplexing, jump hosts and IDE-managed connections are not guaranteed.
- Polling can miss rapid changes. Full upload queues skip new captures, while notification overflow leaves upload results intact.
- Foreground handoff waits up to 180 seconds for draining; background stop/restart may interrupt transfers.
- Failed images, remote uploads and logs have no automatic retention policy. Interrupted transfers may leave partial remote files.

The original source import came from a local `codex-shot` prototype. Preserve legacy-install migration without restoring personal paths, credentials or obsolete Xnip-only filters.
