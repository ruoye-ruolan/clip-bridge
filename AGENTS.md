# Repository Guidelines

Last updated: 2026-10-25.

## Project Structure & Module Organization

ClipBridge preserves Mac images, uploads copies over SSH and optionally publishes them to a private Linux clipboard. Applications, installation and packaging use Rust.

- `README.md`: overview; `docs/installation.md`, `docs/usage.md`, `docs/remote-images.md`: operating guides.
- `docs/planning/`: decisions and remaining validation.
- `Cargo.toml`, `Cargo.lock`, `VERSION`: Mac dependencies/version; `remote/` contains the Linux crate, manifest/lockfile and tests.
- `src/`: Mac application modules.
- `src/remote.rs`: embedded companion sources and SSH deployment/publication.
- `src/transport.rs`: streamed image transfer, remote staging and private SSH transport lifecycle.
- `remote/src/shell.rs`: managed Bash/Zsh integration and startup-file ownership checks.
- `src/bin/package.rs`: allowlisted source-archive generation.
- `tests/`: CLI/installation checks; unit tests live beside modules.

Keep current behavior in guides and proposals in planning. Update embedded sources, release inventories and package inputs when distributed files change.

## Build, Test, and Development Commands

Use Rust 1.94+ and Apple Command Line Tools on macOS. Remote setup needs Cargo on both hosts: vendor locally, build offline remotely. Remote operation needs Xvfb/xauth/xclip. From root:

- `make build`: compile `target/release/clipbridge` with locked dependencies.
- `make test`: test both crates, including private-pasteboard checks.
- `make check`: run both crates' rustfmt and Clippy with warnings denied.
- `make package`: generate an ignored `dist/` source archive and checksum.
- `git diff --check`: check whitespace.

`make -C src build` and `make -C src test` delegate to root. Source `./clipbridge` builds first; installed `clipbridge` runs its managed executable.

## Coding Style & Naming Conventions

Use rustfmt, four-space Rust indentation, Makefile tabs, `snake_case` functions/modules and `UpperCamelCase` types. Keep AppKit access on the main thread and the monitor clipboard interface read-only. Document unsafe boundaries; propagate contextual errors. Use lowercase hyphenated Markdown filenames and relative links.

## Testing Guidelines

Use descriptive `#[test]` names. Mock SSH, launchctl and X tools; isolate home, configuration, cache, installation and remote state. Clipboard checks use private pasteboards. Never start live services, modify general clipboards or submit real CLI prompts merely for tests. Installation/setup can change active services or remote files.

Cover ownership, rollback/cancellation, configuration preservation, ordered aliases and pinned routes, streamed byte counts/receipts, transport reuse/cleanup, queue bounds, publication failure, scoped environment, shell startup edits/conflicts, upgrade/removal and monitor-lock lifetime. No coverage threshold exists. Distinguish isolated checks from actual Linux/WSL/terminal validation.

## Commit & Pull Request Guidelines

Use Conventional Commits targeting `master`. Describe behavior, rationale, validation and limitations; link related issues.

## Security & Configuration

Exclude credentials, personal settings, screenshots, logs and build output from packages. Preserve host-key checks, validation, ownership markers and MIT licensing. Removal retains user data and refuses unfamiliar files. Never install remote system packages automatically.

SSH aliases in one configuration must reach the same machine, account and directory. Do not add broadcast uploads, cross-route job retries or connection probing to the session detector. Mac and companion releases use separate versions; accept older running backends only through explicitly tested version pairs.

Keep cached transports private and separate from user SSH sockets. Tool-owned connections must not authorize clipboard capture; close them before releasing the monitor lock.

Stage remote images only in an SSH-user-owned directory without group/other write permission. Preserve existing permissions; pin the validated directory and use relative staging paths.
