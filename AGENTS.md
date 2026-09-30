# Repository Guidelines

Last updated: 2026-09-30.

## Project Structure & Module Organization

ClipBridge is an installable macOS CLI toolkit for uploading clipboard images over SSH. Python remains an external runtime; installation compiles Swift helpers.

- `README.md`: overview and quick start.
- `docs/installation.md`: installation, upgrades, removal and packaging.
- `docs/usage.md`: configuration, commands and troubleshooting.
- `docs/planning/`: product decisions and remaining release work.
- `clipbridge`, `install.sh`, `VERSION`: command entry, installer entry and package version.
- `src/`: Python runtime, service, configuration, installer and handoff modules.
- `src/swift/`, `src/tests/`: native helpers and tests.
- `tools/package.py`: source-archive generation using an explicit file list.

Keep current behavior in user guides and proposals in planning. Update `installer.payload_files` when adding runtime files required by installed copies.

## Build, Test, and Development Commands

Run from the repository root:

- `make build`: compile source helpers.
- `make test`: Python and private-pasteboard checks.
- `make -C src test-python`: Python tests only.
- `make package`: generate ignored `dist/` source archive and checksum.
- `git diff --check`: check whitespace.

Use isolated prefixes and mocked service operations to test installation. Never start live services or upload personal clipboard contents merely to validate code. Installation may migrate an already loaded service; do not treat it as a harmless build command.

## Coding Style & Naming Conventions

Use four spaces in Python and Swift, tabs in Makefiles, Python `snake_case`, Swift `lowerCamelCase`, and lowercase hyphen-separated documentation filenames. No formatter or linter is configured.

Keep Python compatible with 3.9+. Write clear English Markdown with relative links and commands that distinguish source `./clipbridge` from installed `clipbridge` usage.

## Testing Guidelines

Use standard-library `unittest` and `test_*.py`. Mock SSH and launchctl; isolate config/cache/install paths. Swift checks use private named pasteboards. Installer integration tests may compile real helpers in temporary directories.

Cover ownership checks, failure/cancellation rollback, configuration preservation, upgrade state, uninstall safety and single-monitor locking when modifying lifecycle code. No coverage threshold is configured. Distinguish isolated checks from real-host/login validation.

## Commit & Pull Request Guidelines

Use Conventional Commit messages and target `master`. Explain the resulting behavior, reason and validation. Link related issues when applicable.

## Security & Configuration

Never package personal config, credentials, screenshots, logs or source build outputs. Preserve input validation, SSH host-key checks, ownership markers and the MIT license. User configuration lives outside installed payloads; removal must preserve user data and refuse unfamiliar files.
