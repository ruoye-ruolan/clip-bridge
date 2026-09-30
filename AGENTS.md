# Repository Guidelines

Last updated: 2026-09-30.

## Project Structure & Module Organization

ClipBridge is a macOS CLI toolkit that uploads clipboard images over SSH and returns remote paths. Source trials are supported; release packaging is pending.

- `README.md`: project overview and minimal startup flow.
- `docs/usage.md`: authoritative user-facing configuration, commands and troubleshooting.
- `docs/planning/product-direction-and-distribution.md`: decisions and future release work.
- `clipbridge`: command-line entry point.
- `src/`: Python CLI, configuration, service, handoff and upload modules.
- `src/swift/`: native clipboard helpers.
- `src/tests/`: unit, subprocess and private-pasteboard tests.
- `src/README.md`: source map and development instructions.

Keep operating instructions in the user guide and proposals in planning. Do not describe proposed functionality as implemented.

## Build, Test, and Development Commands

Run from the repository root:

- `make -C src build`: compile helpers into ignored `build/`.
- `make -C src test`: run Python and Swift checks.
- `make -C src test-python`: run Python tests only.
- `git diff --check`: check whitespace.

File-based configuration is the primary setup flow. `configure` and `doctor` are optional. `run` enables foreground monitoring; `start`, `restart`, `stop` and `status` manage background operation. Never start services or upload personal clipboard contents merely to validate code.

## Coding Style & Naming Conventions

Use four spaces in Python and Swift and tabs for Makefile recipes. Use Python `snake_case`, Swift `lowerCamelCase`, and lowercase hyphen-separated documentation filenames. No formatter or linter is configured.

Write clear English Markdown with descriptive headings, fenced commands and relative links. Keep user instructions aligned with actual command behavior.

## Testing Guidelines

Use standard-library `unittest` and `test_*.py` filenames. Mock SSH, SCP and launchctl. Use temporary config/cache directories and harmless processes for lifecycle tests; use private named pasteboards for Swift checks.

Cover failed uploads, stale clipboard content, repeated startup, handoff, cancellation and lock ownership when changing those paths. No coverage threshold is configured. Distinguish automated checks from real-host validation; documentation-only edits need link and command checks, not live service operations.

## Commit & Pull Request Guidelines

Use concise Conventional Commit messages, such as `docs: clarify configuration` or `fix: preserve monitor lock`. Target `master`. Describe the resulting behavior, reason and validation. Link related issues when applicable.

## Security & Configuration

Never commit credentials, personal destinations, screenshots, logs or generated helpers. Default user settings live at `~/.config/clipbridge/config.json`; the ignored source-local file is a legacy fallback. Preserve input validation, SSH host-key checks, single-monitor locking and the MIT license.
