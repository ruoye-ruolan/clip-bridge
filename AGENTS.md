# Repository Guidelines

## Project Structure & Module Organization

ClipBridge uploads clipboard images from macOS to SSH hosts and returns remote file paths. This repository contains an early developer prototype, not a packaged menu bar application.

- `README.md`: public overview and current status.
- `docs/planning/product-direction-and-distribution.md`: draft product direction and roadmap.
- `prototype/macos/auto_upload.py`: configuration, session detection and uploads.
- `prototype/macos/swift/`: native clipboard helpers.
- `prototype/macos/tests/`: Python unit tests and Swift pasteboard integration tests.
- `prototype/macos/launchd/`: optional background service template.
- `LICENSE`: MIT license.

Keep proposals in `docs/planning/` and operational instructions in the prototype README. Distinguish planned features from working behavior.

## Build, Test, and Development Commands

Run from the repository root on macOS:

- `make -C prototype/macos build`: compile Swift helpers into ignored `build/`.
- `make -C prototype/macos test`: run Python and Swift tests without network uploads.
- `make -C prototype/macos test-python`: run the Python tests only.
- `make -C prototype/macos run`: start monitoring with local `config.json`; this uploads newly copied images when the session check passes.
- `git diff --check`: inspect tracked changes for whitespace errors.

Read the prototype setup instructions before running the monitor. Stop any previous uploader first. Do not start or install a background service merely to validate code.

## Coding Style & Naming Conventions

Use four-space indentation in Python and Swift, and tabs for Makefile recipes. Follow surrounding naming conventions: Python uses `snake_case`; Swift uses `lowerCamelCase`. No formatter or linter is configured.

Write clear English documentation with descriptive Markdown headings, fenced commands and relative links. Use lowercase, hyphen-separated document filenames. Never describe an unimplemented roadmap item as available.

## Testing Guidelines

Python uses standard-library `unittest`; name tests `test_*.py`. Mock SSH, SCP and general-clipboard access. Swift integration tests use a private named pasteboard, preserving the user's clipboard. Cover offline skipping, stale captures, failed-upload retention and clipboard replacement conditions when changing those paths.

No coverage threshold is configured. Report tests actually run and distinguish mocked transport tests from real SSH validation.

## Commit & Pull Request Guidelines

Use concise Conventional Commit messages, following existing `chore:` and `docs:` history; use `feat:`, `fix:` or `test:` as appropriate. Target `master`. Explain purpose, behavior changes and validation; link related issues and include screenshots for UI changes.

## Security & Configuration

Never commit credentials, personal hosts, screenshots, logs or compiled helpers. Keep destinations in ignored `prototype/macos/config.json`; publish placeholders only. Preserve argument validation and SSH host-key checks. Keep the MIT license.
