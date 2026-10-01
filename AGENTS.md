# Repository Guidelines

Last updated: 2026-09-30.

## Project Structure & Module Organization

ClipBridge is a macOS CLI that uploads clipboard images over SSH. Its application, installer and packaging tool are written in Rust; installed operation needs no language toolchain.

- `README.md`: overview and quick start.
- `docs/installation.md`, `docs/usage.md`: installation and daily operation.
- `docs/planning/`: product decisions and remaining release validation.
- `Cargo.toml`, `Cargo.lock`, `VERSION`: dependencies and matching package version.
- `clipbridge`, `install.sh`: source build/run and installation wrappers.
- `src/`: configuration, clipboard, monitor, control, service and installer modules.
- `src/bin/package.rs`: allowlisted source-archive generation.
- `tests/`: CLI and isolated installed-binary integration tests; unit tests live beside modules.

Keep current behavior in user guides and proposals in planning. Update release inventories and package inputs when changing distributed files.

## Build, Test, and Development Commands

Use Rust 1.94+ and Apple Command Line Tools on macOS. Run from the repository root:

- `make build`: compile `target/release/clipbridge` with locked dependencies.
- `make test`: run Rust unit, integration and private-pasteboard checks.
- `make check`: run rustfmt and Clippy with warnings denied.
- `make package`: generate an ignored `dist/` source archive and checksum.
- `git diff --check`: check whitespace.

`make -C src build` and `make -C src test` delegate to the root. Source `./clipbridge` builds before execution; installed `clipbridge` executes the managed binary.

## Coding Style & Naming Conventions

Use rustfmt, four-space Rust indentation, tabs in Makefiles, `snake_case` functions/modules and `UpperCamelCase` types. Keep macOS clipboard access on the main thread. Document unsafe boundaries and propagate errors with context. Use lowercase hyphen-separated Markdown filenames and relative links.

## Testing Guidelines

Use `#[test]` and descriptive behavior-based names. Mock SSH and launchctl; isolate home, configuration, cache and installation paths. Clipboard integration checks use private named pasteboards. Never start live services or upload personal clipboard contents merely to validate code. Installation can migrate a loaded service and is not a build-only command.

Cover ownership checks, rollback/cancellation, configuration preservation, bounded queues, upgrade/removal safety and monitor-lock lifetime. No coverage threshold is configured. Distinguish isolated tests from real-host/login validation.

## Commit & Pull Request Guidelines

Use Conventional Commits and target `master`. Explain behavior, rationale, validation and remaining limitations; link related issues.

## Security & Configuration

Never package credentials, personal configuration, screenshots, logs or generated source outputs. Preserve SSH host-key checks, input validation, ownership markers and the MIT license. Uninstallation must retain user data and refuse unfamiliar files.
