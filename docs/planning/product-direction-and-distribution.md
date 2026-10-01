# ClipBridge Product Direction and Release Plan

Last updated: 2026-09-30.

Status: Active plan. The macOS command-line toolkit now uses Rust throughout its application, installer and source packager. User installation and versioned source packaging are implemented; broader release validation is pending. Planned work below is not shipped functionality.

See the [project overview](../../README.md), [usage guide](../usage.md) and [implementation guide](../../src/README.md) for current behavior and development instructions.

## Product Decision

ClipBridge serves developers who use a Mac to access Linux or WSL over SSH. It uploads newly copied clipboard images and returns the remote file path to the Mac clipboard. It does not synchronize the remote operating system's clipboard or restrict uploads to Xnip.

Maintain the product as a native Rust CLI toolkit. File-based configuration is the primary setup method; the setup wizard and diagnostics are optional conveniences. Reliable uploads, understandable status and straightforward service management take priority over a graphical interface.

The toolkit can remain the long-term product. A graphical interface and cross-platform support are outside the first release.

## What Exists Today

The root `clipbridge` wrapper builds and runs the Rust executable from `src/`. Installed operation uses one native executable, including direct AppKit clipboard access; Python and Swift helpers are no longer part of the current runtime.

- Configuration uses `~/.config/clipbridge/config.json`, with an explicit `ssh_host` alias and `remote_directory`. The latter is currently required. The wizard can resolve a remote-home default and save the resulting absolute path.
- `run` monitors in the foreground. `start` enables background monitoring and startup at login. Repeating `start` with the same configuration path preserves an existing running service; a cooperating foreground instance can finish queued uploads and hand over to the background service.
- `restart` applies changed settings or a different configuration to the background service. It can interrupt transfers. `stop` stops the background service and disables login startup; foreground monitoring uses Ctrl+C.
- `status` and `logs` expose service state and background records. `doctor` optionally checks tools, configuration, SSH access and an existing destination without starting monitoring.
- New images are uploaded sequentially while a matching SSH session is detected. Startup clipboard contents and images observed offline are skipped. Failed uploads retain local copies; successful uploads attempt to preserve newer clipboard content.
- Uploads have a limit of 16 pending jobs. A full queue discards only the new capture and logs the skip. Notifications have a separate limit of 16 pending messages; notification overflow does not block clipboard updates or discard upload results.

The toolkit provides `install.sh`, `install`, `upgrade --from PATH`, `uninstall` and `--version`. Source installation builds the executable, creates an owned user-prefix payload and launcher, and preserves configuration outside the program. Upgrades compile the selected new source and invoke its installer, stage a new version before switching, restore an already loaded service and attempt rollback on failure. Removal preserves configuration, logs and images. `make package` creates a versioned source archive and checksum. Installed operation needs neither the checkout nor a language toolchain; source installation/upgrades require Rust 1.94+, Cargo and Apple's linker/SDK.

The first Python 0.1 to Rust 0.2 migration uses `./install.sh` from the new source tree. The Rust installer accepts existing ownership markers and JSON settings; the old Python upgrade command cannot install the new source layout.

## Boundaries to Resolve

Session detection checks for a same-user `ssh ALIAS` process with an established TCP socket. This heuristic is not proof of authentication. IDE connections, multiplexing, jump hosts and remote-command sessions are not guaranteed. Define and test the supported connection patterns before advertising broader compatibility.

Direct configuration still requires an absolute remote directory. Uploads attempt to create it, while read-only `doctor` fails if it does not exist. Improve that diagnostic distinction. Making the field optional is a possible simplification, not current behavior.

Clipboard polling can miss rapid changes, and the final change-count check and write are not atomic. Queue capacity is now bounded, but real-world bursts still need validation. Failed local files, remote images and logs have no automatic retention policy; interrupted transfers can leave partial remote files. Foreground shutdown/handoff drains workers while retaining the monitor lock; background replacement may interrupt transfers. Validate those expectations under real service conditions.

## First Release Work

| Workstream | Acceptance criteria |
| --- | --- |
| Upload reliability | Validate startup images, offline skipping, reconnection without deferred uploads, consecutive copies, authentication failures, interrupted transfers, queue saturation and preservation of newer clipboard contents against real hosts. |
| Service lifecycle | Validate real launchd startup, repeat `start`, foreground handoff, `restart`, logout/login and `stop`. Confirm one monitor runs and document interruption behavior. |
| Data lifecycle | Define failed-image retention, log cleanup, remote-file ownership and partial-transfer handling. Make manual recovery instructions clear. |
| Packaging | Validate the Rust source installer/archive on another Mac, including Python-to-Rust migration, failed upgrades and removal. Decide supported macOS versions/architectures and whether to offer precompiled, signed distribution artifacts. |
| Independent use | Complete real SSH upload scenarios and a second-Mac setup from the published instructions without live assistance. Validate WSL separately before claiming WSL compatibility. |

Automated Rust tests use mocked SSH and service operations, private-pasteboard checks, and native-binary installation/upgrade/removal in temporary prefixes. Installed-command tests remove the fixture source and execute with no language tools on PATH. Private pasteboard checks cover PNG/TIFF/JPEG and stale clipboard contents. They provide regression evidence but do not replace the real-host and service acceptance checks above. Record actual results and remaining limits with the release.

## Later Decisions

| Direction | Evidence needed |
| --- | --- |
| Menu bar interface | Users repeatedly need visible upload status, convenient pause controls or graphical settings. Reuse the existing upload behavior where practical. |
| Multiple destinations or richer history | Daily usage demonstrates frequent switching or difficulty recovering uploaded paths. |
| Windows or Linux clients | Concrete demand and platform-specific clipboard, SSH and background-service validation justify the additional maintenance. |

Publish the CLI toolkit under the [MIT License](../../LICENSE) when its acceptance checks pass, then use installation and daily-use feedback to choose further work.
