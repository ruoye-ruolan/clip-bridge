# ClipBridge

Upload images from your Mac clipboard to a remote host over SSH, then paste the remote file path into your workflow. ClipBridge is initially intended for developers working with WSL or Linux servers.

## Project Status

The command-line toolkit is the first planned distribution format. File-based configuration and service commands are available from source; packaging and real-world installation validation are still in progress. There is no published installable release yet.

## Get Started

You need macOS, Python 3.9+, Swift Command Line Tools, and a working SSH alias with noninteractive authentication. From this checkout:

Create your local configuration without overwriting an existing file:

```sh
mkdir -p ~/.config/clipbridge
cp -n prototype/macos/config.example.json ~/.config/clipbridge/config.json
chmod 600 ~/.config/clipbridge/config.json
```

Edit `~/.config/clipbridge/config.json` in your preferred editor:

```json
{
  "ssh_host": "dev-server",
  "remote_directory": "/home/example/.local/share/clipbridge/images"
}
```

Replace `dev-server` with your SSH alias and the directory with an absolute path on that server. Ensure the remote directory exists and is writable; the [toolkit guide](prototype/macos/README.md#configure-with-a-file) includes a setup example. Keep an interactive `ssh YOUR_ALIAS` session open, then check and try foreground monitoring:

```sh
./clipbridge doctor  # optional diagnostics
./clipbridge run
```

Stop foreground monitoring with Ctrl+C, or run `./clipbridge start` in another terminal to switch it to background monitoring and enable startup at login. Repeating `start` while the same background service is running returns its status without starting another instance. Use `./clipbridge restart` after editing settings. The optional `./clipbridge configure` wizard can still generate the same file.

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
