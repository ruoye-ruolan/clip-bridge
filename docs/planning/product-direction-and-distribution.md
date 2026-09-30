# ClipBridge Product Direction and Release Plan

Last updated: 2026-09-30.

Status: Active plan. The first product is a macOS command-line toolkit. Source-based use is available; a packaged release is pending. Planned work below is not shipped functionality.

See the [project overview](../../README.md), [usage guide](../usage.md) and [implementation guide](../../prototype/macos/README.md) for current behavior and development instructions.

## Product Decision

ClipBridge serves developers who use a Mac to access Linux or WSL over SSH. It uploads newly copied clipboard images and returns the remote file path to the Mac clipboard. It does not synchronize the remote operating system's clipboard or restrict uploads to Xnip.

Release the existing Python and Swift implementation as a maintainable CLI toolkit. File-based configuration is the primary setup method; the setup wizard and diagnostics are optional conveniences. Reliable uploads, understandable status and straightforward service management take priority over a graphical interface.

The toolkit can remain the long-term product. A graphical interface and cross-platform support are outside the first release.

## What Exists Today

The root `clipbridge` command delegates to `prototype/macos/`, the source implementation underlying the CLI product.

- Configuration uses `~/.config/clipbridge/config.json`, with an explicit `ssh_host` alias and `remote_directory`. The latter is currently required. The wizard can resolve a remote-home default and save the resulting absolute path.
- `run` monitors in the foreground. `start` enables background monitoring and startup at login. Repeating `start` with the same configuration path preserves an existing running service; a cooperating foreground instance can finish queued uploads and hand over to the background service.
- `restart` applies changed settings or a different configuration to the background service. It can interrupt transfers. `stop` stops the background service and disables login startup; foreground monitoring uses Ctrl+C.
- `status` and `logs` expose service state and background records. `doctor` optionally checks tools, configuration, SSH access and an existing destination without starting monitoring.
- New images are uploaded sequentially while a matching SSH session is detected. Startup clipboard contents and images observed offline are skipped. Failed uploads retain local copies; successful uploads attempt to preserve newer clipboard content.

Current use requires a checkout, Python and macOS tools; Swift helpers are built locally. There is no published installer, bundled Python runtime, `install` command or `uninstall` command. The generated background service depends on the checkout and Python paths remaining available.

## Boundaries to Resolve

Session detection checks for a same-user `ssh ALIAS` process with an established TCP socket. This heuristic is not proof of authentication. IDE connections, multiplexing, jump hosts and remote-command sessions are not guaranteed. Define and test the supported connection patterns before advertising broader compatibility.

Direct configuration still requires an absolute remote directory. Uploads attempt to create it, while read-only `doctor` fails if it does not exist. Improve that diagnostic distinction. Making the field optional is a possible simplification, not current behavior.

Clipboard polling can miss rapid changes, and the final change-count check and write are not atomic. The upload queue is unbounded. Failed local files, remote images and logs have no automatic retention policy; interrupted transfers can leave partial remote files. Shutdown, handoff and restart need explicit expectations for queued and active uploads.

## First Release Work

| Workstream | Acceptance criteria |
| --- | --- |
| Upload reliability | Repeatable cases cover startup images, offline skipping, reconnection without deferred uploads, consecutive copies, authentication failures, interrupted transfers and preservation of newer clipboard contents. Define queue limits and what happens at capacity. |
| Service lifecycle | Validate real launchd startup, repeat `start`, foreground handoff, `restart`, logout/login and `stop`. Confirm one monitor runs and document interruption behavior. |
| Data lifecycle | Define failed-image retention, log cleanup, remote-file ownership and partial-transfer handling. Make manual recovery instructions clear. |
| Packaging | Choose runtime requirements and whether to distribute precompiled Swift helpers. Provide a versioned artifact with repeatable installation, upgrade, legacy migration and removal procedures that preserve settings as documented. |
| Independent use | Complete real SSH upload scenarios and a second-Mac setup from the published instructions without live assistance. Validate WSL separately before claiming WSL compatibility. |

Automated tests use mocked SSH and service operations, plus private-pasteboard integration checks. They provide regression evidence but do not replace the real-host and service acceptance checks above. Record actual results and remaining limits with the release.

## Later Decisions

| Direction | Evidence needed |
| --- | --- |
| Menu bar interface | Users repeatedly need visible upload status, convenient pause controls or graphical settings. Reuse the existing upload behavior where practical. |
| Multiple destinations or richer history | Daily usage demonstrates frequent switching or difficulty recovering uploaded paths. |
| Windows or Linux clients | Concrete demand and platform-specific clipboard, SSH and background-service validation justify the additional maintenance. |

Publish the CLI toolkit under the [MIT License](../../LICENSE) when its acceptance checks pass, then use installation and daily-use feedback to choose further work.
