---
name: setup
description: Install and configure ClipBridge on a Mac and its Linux or WSL2 SSH destination.
disable-model-invocation: true
argument-hint: "[SSH_ALIAS]"
---

# Set up ClipBridge

Use this workflow only when the user requests ClipBridge installation or configuration. It configures the Mac that owns the clipboard and one remote Linux account. It does not install or launch an AI agent, capture a screenshot, or submit a prompt.

1. Run `uname -s` on the host where this skill is executing. If it is not `Darwin`, explain that initial setup must run on the source Mac. A plugin running on Linux or WSL cannot read a Mac clipboard. Do not install the Mac program there or change remote shell files independently.
2. On the Mac, use the bundled installer from this plugin:

   ```sh
   sh "${CLAUDE_PLUGIN_ROOT}/install-release.sh"
   ```

   It installs the version pinned to this plugin into the managed user prefix and preserves configuration. The executable and login service must never run from the plugin cache. Do not use `sudo`, pipe downloaded code into a shell, bypass a checksum failure, or silently fall back to compiling source. If this version's release assets are unavailable, report the exact failure and stop setup; do not fetch an unrelated executable or overwrite an existing installation manually.
3. Run `"$HOME/.local/bin/clipbridge" hosts --json`. These are local SSH configuration suggestions, not connectivity probes. Reuse a host explicitly provided in the request; otherwise let the user choose an alias. A host must start with an ASCII letter or digit and contain only letters, digits, dots, underscores or hyphens. Treat arguments as data and never interpolate unvalidated text into a shell command.
4. Use the remote account's default image directory unless the user requests another absolute directory. Ask whether to enable background monitoring and login startup, unless the user already supplied that preference. Explain that `--no-start` leaves monitoring stopped and disables existing managed login startup.
5. Run the managed CLI with explicit choices, for example:

   ```sh
   "$HOME/.local/bin/clipbridge" setup --host dev-server --start
   ```

   Replace `dev-server` with the validated chosen alias. Use `--no-start` when selected, and add `--remote-dir` only for a requested custom directory. This operation configures both hosts, selects a Linux executable for the remote architecture, installs Bash/Zsh integration, and applies the startup choice. Ordinary Linux servers and WSL2 use the same path; do not assume the remote alias is `wsl` or that it runs Ubuntu.
6. If remote system dependencies are missing, show the CLI's installation guidance and ask the user to have an administrator install them. Never run a package manager automatically. Do not request passwords or disable SSH host-key checking. Preserve existing aliases, functions, unrelated settings and unknown files when a conflict is reported.
7. Report the actual result. Explain that a new remote shell, or sourcing `~/.local/share/clipbridge-remote/shell/init.sh`, loads integration for `codex`, `claude` and `opencode`. Existing agent processes need restarting when they were launched without integration. Keep an ordinary matching interactive SSH session open, copy a new image, wait for remote readiness, then use the agent's image-paste action. Let the user verify the real attachment UI; do not claim it was tested by setup.

The installation, configuration and image data remain independent of plugin updates. Removing this plugin does not uninstall the native programs; use the documented ClipBridge removal commands when explicitly requested.
