# ClipBridge

ClipBridge aims to upload images from your local clipboard to a remote host over SSH, then copy the remote file path back to your clipboard. It is initially intended for developers using a Mac to work with WSL or Linux servers over SSH.

## Project Status

This repository contains an early macOS developer prototype and product planning documents. The prototype combines Python with Swift clipboard helpers; it is not yet a standalone menu bar app. There is no installable release yet.

## Developer Prototype

See the [macOS prototype guide](prototype/macos/README.md) for requirements, configuration, running instructions, and known limitations.

```sh
make -C prototype/macos build
make -C prototype/macos test
```

These commands build the helpers and run tests without starting automatic uploads. Running the monitor requires explicit local SSH configuration. Stop any older clipboard-upload service before starting this version.

## Intended Workflow

1. Configure an SSH destination and a remote upload directory.
2. Enable clipboard image uploads.
3. Copy an image on your Mac.
4. Upload the image to the configured host over SSH.
5. Paste the resulting remote file path into your remote workflow.

ClipBridge transfers image files and returns their paths; it does not synchronize the remote operating system's clipboard.

## Planning

The [Product Direction and Distribution Plan](docs/planning/product-direction-and-distribution.md) compares a script toolkit, a native Mac menu bar app, and a cross-platform desktop app, and outlines a proposed roadmap. These are draft proposals and may change.

## License

This project is licensed under the [MIT License](LICENSE).
