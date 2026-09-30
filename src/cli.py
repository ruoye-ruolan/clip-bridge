"""Guided setup and daily commands for the macOS ClipBridge toolkit."""
import argparse
from collections import deque
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile

from auto_upload import connected
from configuration import (
    DEFAULT_CONFIG, LEGACY_CONFIG, check_destination, discover_ssh_aliases,
    load_config, save_config, validate_config, validate_host,
)
import service
import runtime

ROOT = Path(__file__).resolve().parent


def config_option(parser, default=argparse.SUPPRESS):
    parser.add_argument('--config', type=Path, default=default,
                        help='configuration file (default: ~/.config/clipbridge/config.json)')


def parser_for_cli():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--version', action='version', version='ClipBridge ' + runtime.version())
    config_option(parser, DEFAULT_CONFIG)
    commands = parser.add_subparsers(dest='command', required=True)
    install = commands.add_parser('install', help='install this source tree into a user prefix')
    install.add_argument('--prefix', type=Path, help='installation prefix (default: ~/.local)')
    upgrade = commands.add_parser('upgrade', help='upgrade from a trusted local checkout or extracted source package')
    upgrade.add_argument('--from', dest='source', type=Path, required=True)
    upgrade.add_argument('--prefix', type=Path, help='installation prefix to upgrade')
    uninstall = commands.add_parser('uninstall', help='remove the installed program; preserve config, logs and images')
    uninstall.add_argument('--prefix', type=Path, help='installation prefix to remove')
    setup = commands.add_parser('configure', help='choose a destination and save configuration')
    setup.add_argument('--host', help='SSH config alias; omit for the interactive wizard')
    setup.add_argument('--remote-dir', help='absolute remote directory; default: remote home/.local/share/clipbridge/images')
    setup.add_argument('--no-check', action='store_true', help='save offline; requires --host and an absolute --remote-dir')
    for name, help_text in (
        ('doctor', 'check local tools, SSH authentication and destination permissions'),
        ('start', 'build helpers and enable background uploads, including at login'),
        ('restart', 'restart background monitoring and reload configuration'),
        ('stop', 'stop background uploads and disable launch at login'),
        ('status', 'show background service status'),
        ('run', 'build helpers and monitor in the foreground'),
    ):
        command = commands.add_parser(name, help=help_text)
        config_option(command)
    config_option(setup)
    logs = commands.add_parser('logs', help='show recent background service logs')
    logs.add_argument('--lines', type=int, default=50)
    logs.add_argument('--follow', action='store_true')
    config_option(logs)
    return parser


def existing_config(path):
    if path.exists():
        return path
    if path == DEFAULT_CONFIG and LEGACY_CONFIG.exists():
        return LEGACY_CONFIG
    return path


def choose_host(previous):
    aliases = discover_ssh_aliases()
    if aliases:
        print('SSH aliases:')
        for index, alias in enumerate(aliases, 1):
            print(f'  {index}. {alias}')
    default = previous or (aliases[0] if len(aliases) == 1 else '')
    suffix = f' [{default}]' if default else ''
    while True:
        value = input(f'SSH alias (name or number){suffix}: ').strip() or default
        if value.isdigit() and 1 <= int(value) <= len(aliases):
            value = aliases[int(value) - 1]
        try:
            return validate_host(value)
        except ValueError as error:
            print(error)


def configure(args):
    path = args.config.expanduser()
    previous = existing_config(path)
    old_host, old_remote = None, None
    invalid_content = None
    if previous.exists():
        try:
            old_host, old_remote = load_config(previous)
        except ValueError as error:
            print(f'Existing configuration is invalid: {previous}: {error}')
            print('Choose fresh settings. The existing file will be preserved until setup succeeds.')
            if previous == path:
                invalid_content = previous.read_bytes()
    if args.host is None and not sys.stdin.isatty():
        raise ValueError('Interactive setup requires a terminal; use configure --host YOUR_ALIAS instead.')
    print('Set up an SSH destination. This does not start clipboard monitoring.')
    host = validate_host(args.host) if args.host is not None else choose_host(old_host)
    remote = args.remote_dir
    if remote is None and host == old_host:
        remote = old_remote
    if args.host is None:
        default = remote or 'remote home/.local/share/clipbridge/images'
        remote = input(f'Remote directory [Enter: {default}]: ').strip() or remote
    if args.no_check:
        if args.host is None or not args.remote_dir:
            raise ValueError('--no-check requires --host and an absolute --remote-dir.')
        host, remote = validate_config(host, remote)
        print('SSH connection and directory permissions were not checked.')
    else:
        print(f'Checking SSH access to {host} and preparing the upload directory...')
        remote = check_destination(host, remote)
    if invalid_content is not None:
        descriptor, backup = tempfile.mkstemp(prefix=path.name + '.backup-', dir=path.parent)
        with os.fdopen(descriptor, 'wb') as stream:
            stream.write(invalid_content)
        print(f'Preserved invalid configuration: {backup}')
    save_config(path, host, remote)
    if previous != path:
        print(f'Copied settings from {previous}; the original file was preserved.')
    print(f'Saved configuration: {path}\nDestination: {host}:{remote}')
    print('Monitoring covers images from all applications and may replace the clipboard with a remote path.')
    entry = 'clipbridge' if runtime.is_installed() else './clipbridge'
    command = entry if path == DEFAULT_CONFIG else f'{entry} --config {shlex.quote(str(path))}'
    print(f'Next: keep an interactive SSH session open, then run {command} start (or {command} run).')
    print(f'If ClipBridge is already running, use {command} restart to apply changes.')


def check_local_tools():
    if sys.platform != 'darwin':
        raise RuntimeError('Clipboard monitoring currently requires macOS.')
    required = ('ssh', 'scp', 'sips', 'osascript', 'lsof', 'ps')
    missing = [name for name in required if not shutil.which(name)]
    if missing:
        raise RuntimeError('Missing tools: ' + ', '.join(missing))
    helpers = all(os.access(ROOT / 'build' / name, os.X_OK)
                  for name in ('clipboard-watch', 'clipboard-path'))
    if not helpers:
        if runtime.is_installed():
            runtime.require_helpers()
        if not shutil.which('make') or not shutil.which('swiftc'):
            raise RuntimeError('Install Xcode Command Line Tools (xcode-select --install) to build clipboard helpers.')
        print('Clipboard helpers will be compiled on the first start or run.')
    print('Local tools: available')
    return True


def doctor(path):
    check_local_tools()
    host, remote = load_config(path)
    print(f'Configuration: {path}\nDestination: {host}:{remote}')
    legacy = service.legacy_services()
    if legacy:
        raise RuntimeError('An older uploader is loaded: ' + ', '.join(legacy)
                           + '. Stop it before using ClipBridge; see the migration guide.')
    check_destination(host, remote, create=False)
    print('SSH authentication and directory permissions: OK')
    if connected(host):
        print('Matching SSH session: detected (process/socket heuristic).')
    else:
        print(f'Waiting for a matching interactive SSH session; open: ssh {host}')
    print('Doctor finished. No monitoring was started.')


def run_foreground(path):
    load_config(path)
    service.check_foreground_available()
    check_local_tools()
    service._build_helpers()
    print('Monitoring newly copied images from all applications. Press Ctrl+C to stop.', flush=True)
    os.execv(sys.executable, [sys.executable, '-u', str(ROOT / 'auto_upload.py'),
                             '--config', str(path.resolve()), '--foreground'])


def show_logs(args):
    if not 1 <= args.lines <= 10000:
        raise ValueError('--lines must be between 1 and 10000.')
    path = service.log_path()
    if not path.exists():
        print(f'No background log yet: {path}. Foreground mode logs to the terminal.')
        return
    if args.follow:
        subprocess.run(['/usr/bin/tail', '-n', str(args.lines), '-F', str(path)], check=True)
    else:
        with path.open(encoding='utf-8', errors='replace') as stream:
            print(''.join(deque(stream, maxlen=args.lines)), end='')


def main(argv=None):
    try:
        args = parser_for_cli().parse_args(argv)
        path = existing_config(args.config.expanduser())
        if args.command in ('install', 'upgrade', 'uninstall'):
            import installer
            if args.command == 'uninstall':
                print(installer.uninstall(prefix=args.prefix))
            else:
                source = args.source if args.command == 'upgrade' else ROOT.parent
                print(installer.install(source, prefix=args.prefix, upgrade=args.command == 'upgrade'))
        elif args.command == 'configure':
            configure(args)
        elif args.command == 'doctor':
            doctor(path)
        elif args.command == 'start':
            print(service.start(path))
        elif args.command == 'restart':
            print(service.restart(path))
        elif args.command == 'stop':
            print(service.stop())
        elif args.command == 'status':
            print(service.status())
        elif args.command == 'run':
            run_foreground(path)
        else:
            show_logs(args)
        return 0
    except SystemExit as error:
        return error.code
    except (KeyboardInterrupt, EOFError):
        print('\nCancelled.', file=sys.stderr)
        return 130
    except FileNotFoundError as error:
        print(f'Error: {error}. Run ./clipbridge configure if configuration is missing.', file=sys.stderr)
        return 1
    except subprocess.CalledProcessError as error:
        detail = (error.stderr or error.stdout or '').strip()
        print(f'Error: {Path(error.cmd[0]).name} failed (exit {error.returncode}). {detail}', file=sys.stderr)
        if Path(error.cmd[0]).name == 'ssh':
            print('First verify the host key with ssh YOUR_ALIAS and configure key/agent authentication; then retry.', file=sys.stderr)
        return 1
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f'Error: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
