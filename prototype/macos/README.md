# ClipBridge macOS Prototype

This is a source import of the local Python/Swift clipboard uploader, with configurable destinations and reproducible helper builds. It is a developer prototype, not the planned menu bar app or an installable release.

## Requirements

- macOS with a logged-in graphical user session.
- Python 3.9 or later (standard library only).
- Xcode or Command Line Tools providing `swiftc` and `make`.
- System SSH/SCP and a Linux or WSL SSH destination with a writable directory.
- An SSH config alias with noninteractive authentication and a verified host key.

Verified locally with Python 3.14.7, Swift 6.4, and macOS 27.0.1 on Apple silicon. Other versions and Intel Macs have not been validated.

## Build and Test

From the repository root:

```sh
make -C prototype/macos build
make -C prototype/macos test
```

Builds go into ignored `build/`. Python tests mock network and clipboard commands. The Swift integration test creates a private named pasteboard, checks image capture and stale-image rejection, then releases it. It does not modify the general clipboard or upload files.

## Configure and Run

1. Copy `prototype/macos/config.example.json` to `prototype/macos/config.json` (ignored by Git).
2. Set `ssh_host` to your alias from `~/.ssh/config`. Set `remote_directory` to the actual absolute directory on that host; `/home/example/...` is only a placeholder. The directory accepts ASCII letters, digits, underscores, dots, slashes and hyphens, with no `..` components. Tilde expansion, spaces and shell expressions are unsupported.
3. Confirm SSH authentication and the server's host key through your normal terminal workflow. Keep an interactive `ssh YOUR_ALIAS` session open while testing.
4. Stop any earlier clipboard-upload service before starting this copy. The original prototype used the LaunchAgent label `local.codex.xnip-wsl`; if you have that service, unload it first:

   ```sh
   launchctl bootout "gui/$(id -u)/local.codex.xnip-wsl"
   ```

5. Start the foreground monitor:

   ```sh
   make -C prototype/macos run
   ```

Starting this command enables monitoring of newly copied images from **all applications**. Copy a test image; a successful upload normally replaces it with the remote path. Logs appear in the terminal. Use Ctrl+C to stop; an upload already running or queued may finish during shutdown.

Existing clipboard content is ignored at startup. Offline images are skipped. Captures that become stale before extraction are skipped. Uploads run sequentially, and queued captures are discarded if the session check fails when their upload starts. A change-count check preserves newer clipboard content in ordinary use.

## Files and Background Operation

- `auto_upload.py`: configuration, session detection, upload queue and notifications.
- `swift/clipboard-watch.swift`: polls the clipboard every 0.3 seconds and extracts PNG images.
- `swift/clipboard-path.swift`: conditionally writes the remote path.
- `tests/`: Python unit tests and a Swift pasteboard integration test.
- `launchd/local.clipbridge.prototype.plist.example`: optional background service template.

For background use, copy the template to a `.plist`, replace all placeholders with absolute paths (including the Python executable and log file), and create the log directory first. Use XML escaping for special characters in paths. Validate with `plutil -lint PATH_TO_PLIST`, place it in `~/Library/LaunchAgents/`, and load it with `launchctl bootstrap "gui/$(id -u)" PATH_TO_PLIST`. Stop the foreground copy before loading. Unload with `launchctl bootout "gui/$(id -u)/local.clipbridge.prototype"`; remove the installed plist to prevent future loading. `KeepAlive` restarts the monitor after exit. Moving the checkout requires updating the plist.

A per-user lock prevents concurrent instances of this imported version. It does not detect the original external service. Importing or testing this repository does not install, start, or replace any LaunchAgent.

Failed uploads retain local images under `~/Library/Caches/clipbridge/auto/upload-*`. Successful uploads remove their temporary local copy. There is no automatic retention policy or retry command; inspect logs and remove unwanted retained files manually while stopped. Remote files are not automatically removed, and interrupted SCP transfers may leave partial files.

## Known Limitations

Session detection looks for a same-user `ssh ALIAS` process with an established TCP socket. This is a heuristic, not proof of completed authentication. Remote-command sessions, SSH multiplexing, jump hosts and IDE-managed connections are not guaranteed to work.

Clipboard polling can miss rapid changes. The upload queue is unbounded. The change-count check and clipboard write are not atomic, leaving a small race window. No menu bar UI, pause switch, history browser, automatic updater or packaged runtime is included. Full end-to-end SSH uploads and another-machine installation remain to be verified for this imported version.

## Import Scope

Imported from the active `codex-shot` prototype: `auto_upload.py`, `clipboard-watch.swift`, `clipboard-path.swift`, clipboard tests and the LaunchAgent structure. Personal paths became local configuration; notifications use the ClipBridge name. Old folder-watching code, the unused Xnip-only `UploadPolicy.swift`, its obsolete tests, the standalone `clipboard-image` utility, backups and compiled binaries were not imported. The original local installation remains separate.
