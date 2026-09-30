# ClipBridge

Upload images from your Mac clipboard to a remote host over SSH, then paste the remote file path into your workflow. ClipBridge is initially intended for developers working with WSL or Linux servers.

## Project Status

The command-line toolkit is the first planned distribution format. Guided configuration and service commands are available from source; packaging and real-world installation validation are still in progress. There is no published installable release yet.

## Get Started

You need macOS, Python 3.9+, Swift Command Line Tools, and a working SSH alias with noninteractive authentication. From this checkout:

```sh
./clipbridge configure
./clipbridge doctor
```

Choose an SSH alias and accept the remote-home upload directory or enter your own. Setup checks the destination and saves your configuration; it does not enable monitoring.

Keep an interactive `ssh YOUR_ALIAS` session open. Start background monitoring, including at login:

```sh
./clipbridge start
./clipbridge status
```

Newly copied images from all applications are eligible. Successful uploads copy the remote file path back to the clipboard when its contents have not changed. Use `./clipbridge stop` to stop background uploads and disable startup at login, or `./clipbridge logs` to inspect results.

See the [toolkit guide](prototype/macos/README.md) for dependencies, foreground mode, configuration migration and known limitations. Older upload services must be stopped before starting this version. ClipBridge transfers files; it does not synchronize the remote operating system's clipboard.

## Development

```sh
make -C prototype/macos build
make -C prototype/macos test
```

Tests use mocked network/service commands and a private test pasteboard. They do not start monitoring or upload clipboard contents.

## Planning

The [Product Direction and Distribution Plan](docs/planning/product-direction-and-distribution.md) records the CLI-first direction and possible later interfaces. A menu bar app and cross-platform support depend on user feedback.

## License

This project is licensed under the [MIT License](LICENSE).
