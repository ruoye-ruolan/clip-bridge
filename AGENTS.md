# Repository Guidelines

## Project Structure & Module Organization

ClipBridge is a macOS CLI toolkit for uploading clipboard images over SSH and returning remote paths. Packaging is pending.

- `README.md`: public overview and current status.
- `docs/planning/product-direction-and-distribution.md`: draft product direction and roadmap.
- `clipbridge`: command-line entry point.
- `prototype/macos/`: configuration, CLI/service management and upload modules.
- `prototype/macos/swift/`: native clipboard helpers.
- `prototype/macos/tests/`: Python unit tests and Swift pasteboard integration tests.
- `LICENSE`: MIT license.

Keep proposals in `docs/planning/` and operating instructions in the prototype README. Distinguish plans from working behavior.

## Build, Test, and Development Commands

Run from the repository root on macOS:

- `make -C prototype/macos build`: compile Swift helpers into ignored `build/`.
- `make -C prototype/macos test`: run Python and Swift tests without network uploads.
- `make -C prototype/macos test-python`: run the Python tests only.
- `./clipbridge configure`: guided setup; checks SSH and creates/checks the destination.
- `./clipbridge doctor`: read-only connection and environment diagnostics.
- `./clipbridge start` / `stop` / `status`: background service controls.
- `./clipbridge run`: foreground monitoring with the saved user configuration.
- `git diff --check`: inspect tracked changes for whitespace errors.

Read setup instructions and stop previous uploaders before monitoring. Never start or install services merely to validate code.

## Coding Style & Naming Conventions

Use four-space indentation in Python and Swift, and tabs for Makefile recipes. Follow surrounding naming conventions: Python uses `snake_case`; Swift uses `lowerCamelCase`. No formatter or linter is configured.

Write English documentation with descriptive headings, fenced commands and relative links. Use lowercase, hyphen-separated filenames.

## Testing Guidelines

Python uses standard-library `unittest`; name tests `test_*.py`. Mock SSH, SCP, launchctl and general-clipboard access. Use temporary configuration paths. Swift integration tests use a private named pasteboard, preserving the user's clipboard. Cover offline skipping, stale captures, failed-upload retention and clipboard replacement conditions when changing those paths.

No coverage threshold is configured. Report tests actually run and distinguish mocked transport tests from real SSH validation.

## Commit & Pull Request Guidelines

Use concise Conventional Commit messages, following existing `chore:` and `docs:` history; use `feat:`, `fix:` or `test:` as appropriate. Target `master`. Explain purpose, behavior changes and validation; link related issues and include screenshots for UI changes.

## Security & Configuration

Never commit credentials, personal hosts, screenshots, logs or compiled helpers. Keep destinations in `~/.config/clipbridge/config.json`; the ignored prototype-local file remains a migration fallback. Publish placeholders only. Preserve argument validation and SSH host-key checks. Keep the MIT license.
