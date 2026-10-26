---
name: doctor
description: Diagnose ClipBridge setup and image-paste failures on macOS, Linux servers or WSL2.
disable-model-invocation: true
---

# Diagnose ClipBridge

Identify the current operating system with `uname -s`. Use existing installed programs; this workflow does not install dependencies or change configuration.

On the Mac:

```sh
"$HOME/.local/bin/clipbridge" doctor
"$HOME/.local/bin/clipbridge" remote doctor
"$HOME/.local/bin/clipbridge" remote status
"$HOME/.local/bin/clipbridge" logs --lines 20
```

On the remote Linux/WSL host:

```sh
"$HOME/.local/bin/clipbridge-remote" doctor
"$HOME/.local/bin/clipbridge-remote" status
```

Classify the failure as local capture, SSH transport, remote publication, or agent startup/input. If the remote clipboard is ready but pasting fails, ask the user to run `type opencode` (or their agent's command name) in the actual interactive shell. Integration should resolve to a shell function. Loading the integration does not change the environment of an already running agent; it must be relaunched. Do not hard-code a display number, modify global display settings, or infer that all terminals and agent versions support the same shortcut.

Keep logs bounded and omit private host addresses and image filenames from any public report. Never read or upload image contents for diagnostics. Never stop a live clipboard service, modify SSH trust, install system packages, or submit a real agent prompt just to test. If a repair is needed, explain the concrete change and follow the user's existing authorization and host approval policy.
