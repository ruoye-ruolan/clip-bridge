# ClipBridge Product Direction and Distribution Plan

Status: Draft. This document explores product direction, distribution options, and a proposed roadmap. It is not an installation guide or a commitment to ship the features described below.

ClipBridge uploads images from the local clipboard to a remote host over SSH, then copies the remote file path back to the local clipboard. Its initial audience is developers who use a Mac to access WSL or Linux servers over SSH.

This document compares three distribution options: a script toolkit, a Mac menu bar app, and a cross-platform desktop app. The recommended approach is to validate installation and configuration with the script toolkit, make the Mac menu bar app the primary product, and pursue cross-platform support once demand is clear. The technology choices and roadmap below are proposals; product development and public distribution have not started.

## Current Prototype

A working local prototype combines Python, Swift helper programs, and a macOS background service. The prototype has not yet been included in this repository. It performs the following steps:

1. Watches for newly copied clipboard images, ignoring existing content at startup.
2. Checks for an established `ssh wsl` session.
3. Uploads the image to a remote directory using SSH and SCP.
4. Replaces the clipboard with the remote path after a successful upload, provided the clipboard has not changed in the meantime.
5. Skips images while disconnected without uploading them later, and retains a local image copy if an upload fails.

The workflow supports images copied by Xnip and other applications. There is no reliable Xnip source marker in the clipboard, so it cannot be advertised as uploading only Xnip screenshots. It transfers an image file and returns its path; it does not synchronize image data with the remote operating system's clipboard.

The prototype still contains machine-specific paths and a remote user directory. SSH session detection also targets the current command-line workflow. Compatibility with all terminals, connection managers, SSH multiplexing configurations, and remote development tools has not been established.

## Comparison of Distribution Options

| Dimension | Script toolkit | Mac menu bar app | Cross-platform desktop app |
| --- | --- | --- | --- |
| Primary audience | Developers comfortable with the terminal | Mac users and developers | Mac, Windows, and Linux users |
| Installation | Download the toolkit and run an installer command | Download the app and move it to Applications | Download the installer for the operating system |
| Configuration | Configuration file and command line | Graphical settings | A consistent settings interface across platforms |
| Proposed implementation | Existing scripts and precompiled helpers | Swift, SwiftUI, and AppKit | Tauri or Electron as candidates, with platform adapters |
| Release artifacts | Source code, release archives, installer, and uninstaller | `.app`, `.dmg`, and source code | Installers for each platform and source code |
| Relative development effort | Low | Medium | High |
| Relative maintenance effort | Low to medium, mostly environment differences | Medium, focused on macOS | High, requiring validation on three operating systems |
| Recommended role | Initial validation and developer edition | Primary product | Later expansion |

Effort levels are comparative assessments, not delivery estimates. No cross-platform framework has been selected. Clipboard access, background operation, and SSH integration should be validated before making that decision.

## Option One Script Toolkit

Package the existing prototype as a configurable open-source command-line tool with installation and removal support. This is the quickest way to let other developers try it.

Suggested commands include `install`, `uninstall`, `start`, `stop`, `status`, and `doctor` for installation, removal, background service management, and connection diagnostics. A public release must not depend on absolute paths from the developer's machine or require ordinary users to compile Swift helpers themselves.

Configuration should cover at least the SSH host alias, remote directory, automatic startup, and upload trigger policy. Reuse each user's SSH configuration and authentication method by default. Do not bundle personal keys or host details. Resolve the remote destination through configuration or the remote user's home directory rather than hard-coding a username.

Release artifacts should include a versioned archive, installation and removal instructions, a sample configuration, log locations, and troubleshooting guidance. Installation should be safe to repeat, upgrades should preserve user settings, and removal should stop the background service.

This option reuses the existing implementation and enables quick feedback. Its main drawback is the onboarding burden of terminal commands, permissions, and runtime environment differences.

## Option Two Mac Menu Bar App

Integrate the upload functionality into a standalone native Mac app. Users can view status and change settings from the menu bar without managing several scripts. This is the recommended primary product.

Use Swift, SwiftUI, and AppKit for the interface, clipboard monitoring, and state management. Continue using the system SSH and SCP tools with the user's existing connection configuration. The app should manage background operation and launch at login. Migration should disable the prototype's background service to prevent duplicate monitoring and uploads.

The first release should include:

- Initial setup: enter or select an SSH alias, configure the remote directory, and test the connection.
- An automatic upload switch, disabled by default until the user understands its scope and enables it.
- Connection policies: upload only while an SSH session is established, with an optional mode that uploads whenever the host is reachable.
- Menu bar states: paused, waiting for a connection, uploading, succeeded, and failed.
- Recent upload history: copy a path again, inspect failures, and retry manually.
- A launch-at-login switch and access to logs.
- A clear explanation that monitoring covers newly copied images from all applications and that successful uploads replace clipboard content with a file path.

The core workflow is: install the app → configure an SSH destination → enable automatic uploads → copy an image → receive an upload notification → paste the remote path.

Distribute a `.dmg` through GitHub Releases. For direct distribution to general users, plan for Developer ID signing and Apple notarization. The signing identity, developer account, and release credentials must be arranged before publishing. Apple supports distribution outside the Mac App Store and provides signing and notarization workflows for that purpose. [Apple macOS distribution guidance](https://developer.apple.com/macos/distribution/)

This option offers a more complete installation, configuration, and status experience while keeping maintenance focused on macOS. It requires additional work on the native interface, application lifecycle, and release process.

## Option Three Cross-Platform Desktop App

Provide a similar experience on Mac, Windows, and Linux. The product can serve the broader workflow of transferring clipboard images from a local computer to an SSH host, beyond WSL or any particular screenshot tool.

Separate shared configuration, upload tasks, history, and state logic from platform-specific clipboard, tray, startup, and SSH session detection modules. Tauri and Electron are potential frameworks; validate the required capabilities before choosing one.

The following areas need implementation and testing on each platform:

- Clipboard image formats and access mechanisms.
- System tray behavior, background operation, and automatic startup.
- SSH configuration locations, client availability, and connection detection.
- Linux desktop environment and display protocol differences.
- The distinction between accessing local WSL on Windows and connecting to remote WSL over SSH.
- Packaging, signing, upgrades, and removal for each operating system.

A cross-platform interface framework does not eliminate these operating system differences. Pursue this option once there is clear demand from non-Mac users; it is not the recommended starting point for the first release.

## Requirements Shared by All Public Releases

| Area | Work required before release |
| --- | --- |
| Portable configuration | Remove personal usernames, host addresses, and fixed installation paths; provide configuration examples |
| Connection detection | Define supported SSH session types and distinguish an established session from host reachability |
| Upload scope | Explain that clipboard images from all applications are eligible and provide an accessible pause control |
| Clipboard protection | Leave the clipboard unchanged after failed uploads and preserve content copied during an upload |
| Concurrency and disconnection | Prevent duplicate uploads and define behavior for consecutive copies, network interruptions, and retries |
| Local files | Document retained image copies and log locations, with cleanup and retention policies |
| Installation and upgrades | Support removal, preserve settings during upgrades, and run only one monitoring instance |
| Validation | Cover offline skipping, no deferred upload on reconnection, rapid consecutive copies, image formats, and authentication failures |
| Open-source preparation | Use the MIT license, remove personal configuration and credentials, and provide contribution and issue-reporting guidance |

ClipBridge is a provisional name. Existing projects, trademarks, and domain availability still need to be checked before release. This document makes no claim that the name is available.

## Recommended Roadmap

1. Package the script toolkit with configurable settings, installation and removal, diagnostics, and validation on another machine. Acceptance criterion: a user can follow the documentation on another Mac and complete a first upload.
2. Build the Mac menu bar app, initially supporting a single SSH destination and the complete automatic upload workflow. Acceptance criterion: users can configure, pause, and troubleshoot uploads without editing scripts.
3. Complete signing, notarization, and release packaging, then publish a downloadable version through GitHub Releases. GitHub Releases supports binary attachments and release notes. [GitHub Releases documentation](https://docs.github.com/en/repositories/releasing-projects-on-github)
4. Use feedback to prioritize multiple destinations, updates, and retry improvements before deciding whether to invest in a cross-platform version.

These options can evolve in stages rather than becoming three separate products developed at once. The immediate priority is reusable upload logic and a consistent configuration model that the menu bar app can build on.

## License

This project is licensed under the [MIT License](../../LICENSE).
