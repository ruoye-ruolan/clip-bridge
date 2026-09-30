"""Validate, discover and save ClipBridge's explicit SSH destination."""
import glob
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import tempfile

DEFAULT_CONFIG = Path.home() / '.config/clipbridge/config.json'
LEGACY_CONFIG = Path(__file__).resolve().parent / 'config.json'
SSH_OPTIONS = ['-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes',
               '-o', 'ConnectTimeout=8', '-o', 'ServerAliveInterval=10',
               '-o', 'ServerAliveCountMax=2']
ALIAS_PATTERN = r'[A-Za-z0-9][A-Za-z0-9_.-]*'


def validate_host(host):
    """Reject flags, user@host targets and characters that are not literal aliases."""
    if not isinstance(host, str) or not re.fullmatch(ALIAS_PATTERN, host):
        raise ValueError('ssh_host must be an SSH config alias (letters, digits, _, . or -)')
    return host


def validate_config(host, remote):
    """Accept only an SSH alias and an absolute, shell-safe POSIX directory."""
    validate_host(host)
    if (not isinstance(remote, str) or not re.fullmatch(r'/[A-Za-z0-9_./-]*', remote)
            or '..' in remote.split('/')):
        raise ValueError('remote_directory must be an absolute POSIX path without spaces or shell characters')
    return host, remote.rstrip('/') or '/'


def load_config(path):
    """Read the same two-key configuration supported by the original monitor."""
    with Path(path).open(encoding='utf-8') as stream:
        value = json.load(stream)
    if not isinstance(value, dict):
        raise ValueError('Configuration must be a JSON object')
    return validate_config(value.get('ssh_host', ''), value.get('remote_directory', ''))


def _create_private_parent(path):
    if path.is_symlink():
        raise ValueError('Configuration directory must not be a symlink')
    if not path.exists():
        _create_private_parent(path.parent)
        try:
            path.mkdir(mode=0o700)
        except FileExistsError:
            # A concurrent setup may have created it; check again before writing.
            if path.is_symlink() or not path.is_dir():
                raise ValueError('Configuration directory must be a regular directory')
    elif not path.is_dir():
        raise ValueError('Configuration directory must be a regular directory')


def save_config(path, host, remote):
    """Replace the configuration atomically, preserving it if writing fails."""
    host, remote = validate_config(host, remote)
    path = Path(path).expanduser()
    if path.is_symlink():
        raise ValueError('Configuration file must not be a symlink')
    _create_private_parent(path.parent)
    descriptor, temporary = tempfile.mkstemp(prefix='.config-', suffix='.json', dir=path.parent)
    try:
        with os.fdopen(descriptor, 'w', encoding='utf-8') as stream:
            os.fchmod(stream.fileno(), 0o600)
            json.dump({'ssh_host': host, 'remote_directory': remote}, stream, indent=2)
            stream.write('\n')
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        Path(temporary).unlink(missing_ok=True)


def discover_ssh_aliases(path=None):
    """Return literal Host hints, without evaluating Match or executing commands."""
    source = Path(path).expanduser() if path is not None else Path.home() / '.ssh/config'
    ssh_directory = source.parent
    aliases = set()
    visited = set()

    def read_config(filename):
        try:
            resolved = filename.resolve()
        except (OSError, RuntimeError):
            return
        if resolved in visited:
            return
        visited.add(resolved)
        try:
            lines = filename.read_text(encoding='utf-8').splitlines()
        except (OSError, UnicodeError):
            return
        for line in lines:
            directive = re.match(r'^\s*([A-Za-z][A-Za-z0-9]*)\s*(?:=\s*)?(.*)$', line)
            if not directive:
                continue
            key, value = directive.groups()
            try:
                arguments = shlex.split(value, comments=True)
            except ValueError:
                continue
            if key.lower() == 'host':
                aliases.update(alias for alias in arguments if re.fullmatch(ALIAS_PATTERN, alias))
            elif key.lower() == 'include':
                for pattern in arguments:
                    include = Path(pattern).expanduser()
                    if not include.is_absolute():
                        include = ssh_directory / include
                    for match in sorted(glob.glob(str(include))):
                        read_config(Path(match))

    read_config(source)
    return sorted(aliases)


def _ssh(host, command):
    return subprocess.run(['/usr/bin/ssh'] + SSH_OPTIONS + [host, command],
                          capture_output=True, text=True, timeout=35, check=True)


def resolve_remote_directory(host):
    """Resolve the default from the SSH host's HOME, not the Mac's username."""
    validate_host(host)
    home = _ssh(host, 'printf \'%s\\n\' "$HOME"').stdout.removesuffix('\n')
    _, home = validate_config(host, home)
    return home.rstrip('/') + '/.local/share/clipbridge/images'


def check_destination(host, remote=None, *, create=True):
    """Check SSH and directory access; setup may create it and probe writing."""
    validate_config(host, remote if remote is not None else '/')
    if remote is None:
        remote = resolve_remote_directory(host)
    host, remote = validate_config(host, remote)
    quoted = shlex.quote(remote)
    command = f'test -d {quoted} && test -w {quoted} && test -x {quoted}'
    if create:
        probe = shlex.quote(remote.rstrip('/') + '/.clipbridge-check.XXXXXXXX')
        command = (f'umask 077; mkdir -p -- {quoted} && {command} && '
                   f'probe=$(mktemp {probe}) && trap \'rm -f -- "$probe"\' 0 HUP INT TERM')
    _ssh(host, command)
    return remote
