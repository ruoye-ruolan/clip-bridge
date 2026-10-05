# ClipBridge Product Direction and Release Plan

Last updated: 2026-10-03.

Status: Active plan for 0.3.1. The macOS toolkit and Linux clipboard companion are implemented in Rust. Local clipboard preservation and remote image publication have isolated regression checks; actual terminal/CLI attachment and broader release validation remain pending. Planned work below is not shipped functionality.

See the [project overview](../../README.md), [usage guide](../usage.md) and [implementation guide](../../src/README.md) for current behavior and development instructions.

## Product Decision

ClipBridge serves developers who need image input in both local Mac applications and remote Linux/WSL CLIs. Preserve the original Mac clipboard image while uploading a copy over SSH. An optional private Linux clipboard makes the image available to remote CLIs. Bash/Zsh integration keeps ordinary `codex` and `claude` commands usable after one-time setup. Remote file paths remain implementation/storage details and log records; they are never substituted for the Mac clipboard image. Uploads are not limited to Xnip.

Maintain the product as a native Rust CLI toolkit. File-based configuration is the primary setup method; the setup wizard and diagnostics are optional conveniences. Reliable uploads, understandable status and straightforward service management take priority over a graphical interface.

The toolkit can remain the long-term product. A graphical interface and cross-platform support are outside the first release.

## What Exists Today

The root `clipbridge` wrapper builds and runs the Rust executable from `src/`. Installed operation uses one native executable, including direct AppKit clipboard access; Python and Swift helpers are no longer part of the current runtime.

- Configuration uses `~/.config/clipbridge/config.json`, with an explicit `ssh_host` alias and `remote_directory`. The latter is currently required. The wizard can resolve a remote-home default and save the resulting absolute path.
- Optional `remote_clipboard` defaults to false. `remote setup` vendors locked dependencies on the Mac, deploys embedded sources/dependencies and builds offline with `--frozen` on Linux. It requires Rust/Cargo on both hosts, checks X tools and backend compatibility, installs managed Bash/Zsh integration before enabling the flag, and requires a local monitor reload. Incompatible old backends require explicit stop and remote CLI relaunch.
- The companion in `remote/` supervises an authenticated private Xvfb/xclip clipboard. Managed Bash/Zsh functions invoke `clipbridge-remote run -- COMMAND` for ordinary `codex`/`claude` launches, scoping the current display environment to that CLI. Existing aliases/functions are preserved, and explicit `run` remains the fallback. Both crates are Rust; Linux runtime dependencies include Xvfb, xauth and xclip. System package installation is explicit and never performed by the toolkit.
- `run` monitors in the foreground. `start` enables background monitoring and startup at login. Repeating `start` with the same configuration path preserves an existing running service; a cooperating foreground instance can finish queued uploads and hand over to the background service.
- `restart` applies changed settings or a different configuration to the background service. It can interrupt transfers. `stop` stops the background service and disables login startup; foreground monitoring uses Ctrl+C.
- `status` and `logs` expose service state and background records. `doctor` optionally checks tools, configuration, SSH access and an existing destination without starting monitoring.
- New images are uploaded sequentially while a matching SSH session is detected. Startup clipboard contents and images observed offline are skipped. The Mac clipboard is always unchanged. Failed transfers or remote clipboard publication retain local copies; readiness is reported only after enabled remote publication succeeds.
- Uploads have a limit of 16 pending jobs. A full queue discards only the new capture and logs the skip. Notifications have a separate limit of 16 pending messages; notification overflow does not block upload completion or discard upload results.

The toolkit provides `install.sh`, `install`, `upgrade --from PATH`, `uninstall` and `--version`. Source installation builds the executable, creates an owned user-prefix payload and launcher, and preserves configuration outside the program. Upgrades compile the selected new source and invoke its installer, stage a new version before switching, restore an already loaded service and attempt rollback on failure. Removal preserves configuration, logs and images. `make package` creates a versioned source archive and checksum. Installed operation needs neither the checkout nor a language toolchain; source installation/upgrades require Rust 1.94+, Cargo and Apple's linker/SDK.

Migration from Python 0.1 uses `./install.sh` from the new source tree. The Rust installer accepts existing ownership markers and JSON settings; the old Python upgrade command cannot install the new source layout. Rust 0.2 users upgrade normally; remote image input needs the separate [companion setup](../remote-images.md).

## Boundaries to Resolve

Session detection checks for a same-user `ssh ALIAS` process with an established TCP socket. This heuristic is not proof of authentication. IDE connections, multiplexing, jump hosts and remote-command sessions are not guaranteed. Define and test the supported connection patterns before advertising broader compatibility.

Direct configuration still requires an absolute remote directory. Uploads attempt to create it, while read-only `doctor` fails if it does not exist. Improve that diagnostic distinction. Making the field optional is a possible simplification, not current behavior.

Clipboard polling can miss rapid changes. Queue capacity is bounded, but real-world bursts still need validation. Failed local files, remote images and logs have no automatic retention policy; interrupted transfers can leave partial remote files. Foreground shutdown/handoff drains workers while retaining the monitor lock; background replacement may interrupt transfers. Validate those expectations under real service conditions.

The remote clipboard contains the last successful image, not a mirror of every Mac clipboard change. Users must wait for remote readiness before image paste. Copying text does not clear the remote image, and publication failure can leave an older image available. Existing shells need a one-time reconnect after integration installation. Existing CLIs need relaunching after private backend restart; each new integrated launch obtains the current connection. All wrapped CLIs for one remote user share the image. Test actual Codex and Claude Code versions, Ghostty shortcut delivery, tmux and WSL-specific clipboard paths before claiming broad support.

## First Release Work

| Workstream | Acceptance criteria |
| --- | --- |
| Upload reliability | Validate startup images, offline skipping, reconnection without deferred uploads, consecutive copies, authentication failures, interrupted transfers, queue saturation and preservation of newer clipboard contents against real hosts. |
| Image attachments | In actual Ghostty sessions, verify the same test image can be attached locally and in remote Codex/Claude Code launched through shell integration or the explicit wrapper without path substitution. Cover delayed/failed publication, repeated paste, backend restart, Bash/Zsh startup modes, alias/function conflicts and tmux. Validate Linux and WSL separately. |
| Service lifecycle | Validate real launchd startup, repeat `start`, foreground handoff, `restart`, logout/login and `stop`. Confirm one monitor runs and document interruption behavior. |
| Data lifecycle | Define failed-image retention, log cleanup, remote-file ownership and partial-transfer handling. Make manual recovery instructions clear. |
| Packaging | Validate the Rust source installer/archive on another Mac, including Python-to-Rust migration, failed upgrades and removal. Decide supported macOS versions/architectures and whether to offer precompiled, signed distribution artifacts. |
| Remote provisioning | Validate userspace source deployment, missing dependencies, companion upgrades and cleanup on Linux. Add explicit remote removal/upgrade recovery before presenting it as equivalent to the Mac install lifecycle. |
| Independent use | Complete real SSH upload scenarios and a second-Mac setup from the published instructions without live assistance. Validate WSL separately before claiming WSL compatibility. |

Automated Rust tests use mocked SSH and service operations, private-pasteboard checks, and native-binary installation/upgrade/removal in temporary prefixes. Companion tests use temporary state and fake X tools for lifecycle, repeated image reads, errors and environment scoping. Installed-command tests remove the fixture source and execute with no language tools on PATH. Private pasteboard checks cover PNG/TIFF/JPEG extraction and unchanged contents. They provide regression evidence but do not replace actual X11, CLI, real-host and service acceptance checks. Record actual results and remaining limits with the release.

## Later Decisions

| Direction | Evidence needed |
| --- | --- |
| Menu bar interface | Users repeatedly need visible upload status, convenient pause controls or graphical settings. Reuse the existing upload behavior where practical. |
| Multiple destinations or richer history | Daily usage demonstrates frequent switching or difficulty recovering uploaded paths. |
| Windows or Linux clients | Concrete demand and platform-specific clipboard, SSH and background-service validation justify the additional maintenance. |

Publish the CLI toolkit under the [MIT License](../../LICENSE) when its acceptance checks pass, then use installation and daily-use feedback to choose further work.
