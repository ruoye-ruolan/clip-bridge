---
name: status
description: Inspect ClipBridge monitor and remote clipboard status without starting services or reading image contents.
disable-model-invocation: true
---

# ClipBridge status

Run `uname -s` to identify the current host. Do not install or upgrade anything for a status request.

On the source Mac, use the managed executable:

```sh
"$HOME/.local/bin/clipbridge" status
"$HOME/.local/bin/clipbridge" remote status
```

On Linux or WSL, inspect only the installed companion:

```sh
"$HOME/.local/bin/clipbridge-remote" status
```

If the command is absent, explain which component is missing and direct initial setup to the Mac. A running Mac monitor does not prove that an image has reached the remote clipboard. Distinguish stopped, running without an image, and image-ready states. Preserve the selected route and report connection errors without probing other hosts. Do not run `start`, `ensure`, `publish`, `run`, or a real agent to make the status appear healthy. Do not display clipboard contents, credentials, or entire environment dumps.
