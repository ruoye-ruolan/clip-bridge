# ClipBridge Product Direction and Release Plan

Last updated: 2026-10-25.

Status: Current release is Mac 0.4.1 with Linux companion 0.3.2, which also accepts a running 0.3.1 backend. Native packages, Mac-side setup and the Claude Code plugin provide the onboarding flow. Actual terminal/CLI attachment and broader platform validation remain distinct acceptance work. Planned work below is not shipped functionality.

See the [project overview](../../README.md), [usage guide](../usage.md) and [implementation guide](../../src/README.md) for current behavior and development instructions.

## Product Decision

ClipBridge serves developers who need image input in both local Mac applications and remote Linux/WSL CLIs. Preserve the original Mac clipboard image while uploading a copy over SSH. An optional private Linux clipboard makes the image available to remote CLIs. Bash/Zsh integration keeps ordinary `codex`, `claude` and `opencode` commands usable after one-time setup. Remote file paths remain implementation/storage details and log records; they are never substituted for the Mac clipboard image. Uploads are not limited to Xnip.

Maintain the product as a native Rust CLI toolkit. Native installation and one-time setup on the source Mac are the primary onboarding path. JSON remains the configuration format and supports direct editing. The optional Claude Code plugin guides the same native setup instead of owning a separate synchronization runtime. Reliable uploads, understandable status and straightforward service management take priority over a graphical interface.

The toolkit can remain the long-term product. A graphical interface and cross-platform support are outside the first release.

## What Exists Today

The root `clipbridge` wrapper builds and runs the Rust executable from `src/`. Installed operation uses one native executable, including direct AppKit clipboard access; Python and Swift helpers are no longer part of the current runtime.

- Configuration uses `~/.config/clipbridge/config.json`, with an explicit `ssh_host`, optional ordered `ssh_aliases` and required stored `remote_directory`. `setup` chooses the host, resolves a remote-home default, deploys the companion and applies an explicit startup choice. It stages local settings until remote success and attempts local recovery on activation failure. `hosts --json` lists SSH aliases offline; `configure` remains a destination-only wizard.
- Optional `remote_clipboard` defaults to false in manual configuration; `setup` enables it after successful deployment. Default remote setup detects Linux architecture, downloads a version-pinned executable/checksum on the Mac and transfers it over SSH. Ordinary Linux servers and WSL2 need no remote Cargo or GitHub access. Missing X tools are reported for administrator installation. Native activation validates staged bytes and attempts rollback before reporting failure. Explicit `--build-from-source` retains Mac vendoring and offline remote compilation, requiring Cargo on both hosts. Incompatible running backends require explicit stop and remote CLI relaunch.
- The companion in `remote/` supervises an authenticated private Xvfb/xclip clipboard. Managed Bash/Zsh functions invoke `clipbridge-remote run -- COMMAND` for ordinary `codex`/`claude`/`opencode` launches, scoping the current display environment to that CLI. Existing aliases/functions are preserved, and explicit `run` remains the fallback. Exact legacy snippets can upgrade without duplicating shell hooks. Companion 0.3.2 can continue using a running 0.3.1 backend, so adding OpenCode does not require restarting Xvfb. Both crates are Rust; Linux runtime dependencies include Xvfb, xauth and xclip. System package installation is explicit and never performed by the toolkit.
- `run` monitors in the foreground. `start` enables background monitoring and startup at login. Repeating `start` with the same configuration path preserves an existing running service; a cooperating foreground instance can finish queued uploads and hand over to the background service.
- `restart` applies changed settings or a different configuration to the background service. It can interrupt transfers. `stop` stops the background service and disables login startup; foreground monitoring uses Ctrl+C.
- `status` and `logs` expose service state and background records. `doctor` optionally checks tools, configuration, SSH access and an existing destination without starting monitoring.
- All configured aliases must reach the same machine/account and directory. Session selection prefers the primary alias, then alternatives in order. Captures keep their selected route; queued jobs are skipped if it disconnects, without switching routes. Diagnostics and remote management commands use one selected route throughout, falling back to the primary only when no session is detected.
- New images are uploaded sequentially while a matching SSH session is detected. Startup clipboard contents and images observed offline are skipped. The Mac clipboard is always unchanged. Failed transfers or remote clipboard publication retain local copies; readiness is reported only after enabled remote publication succeeds.
- Each image streams through one SSH command that validates an SSH-user-owned destination without group/other write permission, stages the image relative to that directory, checks its byte count, atomically finalizes it and optionally publishes it. Existing directory permissions are preserved. The worker reuses private connections for the same alias with a 60-second idle expiry. First/expired connections still need authentication. Failures invalidate the affected cache without retrying or changing routes; shutdown closes owned sockets before releasing the monitor lock. Logs record capture-to-completion time, including queue wait.
- Uploads have a limit of 16 pending jobs. A full queue discards only the new capture and logs the skip. Notifications have a separate limit of 16 pending messages; notification overflow does not block upload completion or discard upload results.

The toolkit provides native bundles, the pinned `install-release.sh`, source `install.sh`, `upgrade --from PATH` and `uninstall`. Native installation verifies the archive and payload and needs no Cargo. Source installation compiles with Rust 1.94+ and Apple's linker/SDK. Both create an owned user-prefix payload/launcher and preserve external configuration. Upgrades accept an extracted native bundle or trusted source checkout, stage before switching and attempt local recovery on failure. Removal preserves user data. `make package` creates source archives; native packaging targets Apple Silicon/Intel Macs and x86_64/aarch64 Linux musl. The CI workflow builds four platforms and attaches artifacts to a draft release without publishing it.

The Claude Code marketplace contains setup/status/doctor skills. Setup runs on the source Mac, installs outside the plugin cache and asks for missing host/startup choices. Removing the plugin leaves the native installation intact. The plugin does not bypass dependency checks, SSH trust or user authorization. Public installation instructions are in the [plugin guide](../plugins.md).

Migration from Python 0.1 uses the new installer; its old `upgrade` command cannot install the Rust payload. Pre-0.4 Rust installations can use the new native bundle's installer. Existing ownership markers and JSON settings are retained; remote image input is configured by [Mac-side setup](../usage.md#one-time-setup).

## Boundaries to Resolve

Session detection checks for a same-user `ssh ALIAS` process with an established TCP socket, held by the process or by a descendant such as a `ProxyCommand` helper. This heuristic is not proof of authentication or current reachability after a network change; a stale socket can appear established until its session is closed. Alias configuration does not implement network probing, SSH reconnection or arbitrary-IP failover. IDE connections, user-managed multiplexed interactive sessions, other jump-host setups and remote-command sessions are not guaranteed. ClipBridge's private upload transports do not satisfy session detection. Define and test the supported connection patterns before advertising broader compatibility.

Stored configuration still requires an absolute remote directory, but one-time setup resolves its remote-home default automatically. Uploads attempt to create it, while read-only `doctor` fails if it does not exist. Improve that diagnostic distinction without changing the ownership and permission checks.

Clipboard polling can miss rapid changes. Queue capacity is bounded, but real-world bursts still need validation. Failed local files, finalized remote images and logs have no automatic retention policy. Remote staging cleanup is best effort if a process is abruptly terminated; incomplete files are not finalized or published. Foreground shutdown/handoff drains workers and closes owned transports while retaining the monitor lock; background replacement may interrupt transfers. Validate those expectations under real service conditions.

The remote clipboard contains the last successful image, not a mirror of every Mac clipboard change. Users must wait for remote readiness before image paste. Copying text does not clear the remote image, and publication failure can leave an older image available. Existing shells need a one-time reconnect or snippet reload after integration installation. Existing CLIs need relaunching after private backend restart or when first adopting the wrapper; each new integrated launch obtains the current connection. All wrapped CLIs for one remote user share the image. Test actual Codex, Claude Code and OpenCode versions, Ghostty shortcut delivery, tmux and WSL-specific clipboard paths before claiming broad support.

## Release Acceptance Work

| Workstream | Acceptance criteria |
| --- | --- |
| Upload reliability | Validate startup images, offline skipping, reconnection without deferred uploads, alternative-alias selection and pinned-route disconnects, consecutive copies, authentication failures, interrupted transfers, queue saturation and preservation of newer clipboard contents against real hosts. |
| Transfer speed | Compare initial and reused-connection transfers with synthetic data on LAN and public networks; record file size and network conditions. Verify idle expiry, failed connection replacement, staging cleanup and monitor shutdown. Do not infer CLI attachment latency solely from transport timings. |
| Image attachments | In actual Ghostty sessions, verify the same test image can be attached locally and in remote Codex/Claude Code/OpenCode launched through shell integration or the explicit wrapper without path substitution. Cover delayed/failed publication, repeated paste, backend restart, Bash/Zsh startup modes, legacy snippet upgrades, alias/function conflicts and tmux. Validate Linux and WSL separately. |
| Service lifecycle | Validate real launchd startup, repeat `start`, foreground handoff, `restart`, logout/login and `stop`. Confirm one monitor runs and document interruption behavior. |
| Data lifecycle | Define failed-image retention, log cleanup, remote-file ownership and partial-transfer handling. Make manual recovery instructions clear. |
| Packaging | Validate native archives on Apple Silicon and Intel Macs, including bootstrap checksums, older-install migration, native upgrades, rollback and removal. Run the four-platform workflow before publishing assets; determine supported macOS versions and signing/notarization policy. |
| Remote provisioning | Validate prebuilt deployment on ordinary Linux servers and WSL2, both Linux architectures, missing dependencies, incompatible backends and activation rollback. Retain explicit source-build coverage. Complete remote removal beyond shell-hook uninstall before presenting it as equivalent to the Mac install lifecycle. |
| Plugin onboarding | Verify marketplace installation and namespaced skills on the Mac, absent assets/dependencies, explicit startup choices, plugin updates and removal without losing the native installation. Verify that the pinned asset names resolve to the intended published release. |
| Independent use | Complete real SSH upload scenarios and a second-Mac setup from the published instructions without live assistance. Validate WSL separately before claiming WSL compatibility. |

Automated Rust tests use mocked downloads, SSH and service operations, private-pasteboard checks, setup failure/recovery fixtures, and native-binary installation/upgrade/removal in temporary prefixes. Companion tests use temporary state and fake X tools for lifecycle, repeated image reads, errors and environment scoping. Installed-command tests remove the fixture source and execute with no language tools on PATH. Private pasteboard checks cover PNG/TIFF/JPEG extraction and unchanged contents. They provide regression evidence but do not replace actual X11, CLI, real-host and service acceptance checks. Record actual results and remaining limits with the release.

## Later Decisions

| Direction | Evidence needed |
| --- | --- |
| Menu bar interface | Users repeatedly need visible upload status, convenient pause controls or graphical settings. Reuse the existing upload behavior where practical. |
| Separate destinations or richer history | Alternative routes to one destination are supported. Daily usage must justify independent destination configurations or richer uploaded-image history. |
| Windows or Linux clients | Concrete demand and platform-specific clipboard, SSH and background-service validation justify the additional maintenance. |

For subsequent releases, review platform checks and draft attachments before publishing under the [MIT License](../../LICENSE). Keep release notes short and direct installation details to the README, then use onboarding and daily-use feedback to choose further work.
