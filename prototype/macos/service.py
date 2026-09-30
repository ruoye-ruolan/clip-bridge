"""Manage ClipBridge's per-user launchd service without touching older uploaders."""

import fcntl
import os
from pathlib import Path
import plistlib
import re
import subprocess
import sys
import tempfile

import configuration


HOME = Path.home()
ROOT = Path(__file__).resolve().parent
CACHE = HOME / 'Library/Caches/clipbridge/auto'
LABEL = 'local.clipbridge'
LEGACY_LABELS = ('local.codex.xnip-wsl', 'local.clipbridge.prototype')
LAUNCHCTL = '/bin/launchctl'


def log_path():
    return HOME / 'Library/Logs/ClipBridge/clipbridge.log'


def _plist_path():
    return HOME / 'Library/LaunchAgents' / (LABEL + '.plist')


def _domain():
    return 'gui/' + str(os.getuid())


def _target(label=LABEL):
    return _domain() + '/' + label


def _run(args, timeout=15):
    try:
        return subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    except (OSError, subprocess.SubprocessError) as error:
        raise RuntimeError('Could not run ' + args[0] + ': ' + str(error)) from error


def _details(result):
    return (result.stderr or result.stdout or 'exit code ' + str(result.returncode)).strip()


def _service_info(label=LABEL):
    result = _run([LAUNCHCTL, 'print', _target(label)])
    if result.returncode == 0:
        return result.stdout
    # A missing service is normal; a missing GUI domain or permission error is not.
    if result.returncode == 113 and re.search(
            r'could not find (?:specified )?service|service[^\n]*not found',
            result.stderr, re.IGNORECASE):
        return None
    raise RuntimeError('Could not inspect ' + label + ': ' + _details(result))


def legacy_services():
    """Return loaded older uploaders, including jobs waiting to restart."""
    return [label for label in LEGACY_LABELS if _service_info(label) is not None]


def _check_monitor_lock():
    CACHE.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (CACHE / 'watcher.lock').open('a') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise RuntimeError('Another ClipBridge monitor is running. Stop it first.') from error
        # Closing releases the lock; never hold it while launchd starts.


def _monitor_running():
    """Inspect an existing monitor lock without creating files during status."""
    try:
        lock = (CACHE / 'watcher.lock').open('r')
    except FileNotFoundError:
        return False
    with lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return True
    return False


def check_foreground_available():
    """Reject duplicate monitors before foreground or background startup."""
    if _service_info() is not None:
        raise RuntimeError(
            'ClipBridge is already loaded. Run clipbridge stop first, then start '
            'again to use the selected configuration.')
    legacy = legacy_services()
    if legacy:
        raise RuntimeError(
            'An older uploader is loaded: ' + ', '.join(legacy)
            + '. Stop it manually before starting ClipBridge; it has not been changed.')
    _check_monitor_lock()


def _atomic_write(path, content):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as stream:
            temporary = Path(stream.name)
            stream.write(content)
        temporary.replace(path)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def _description(info):
    pid = re.search(r'^\s*pid = (\d+)\s*$', info, re.MULTILINE)
    if pid:
        return 'ClipBridge is running (PID ' + pid.group(1) + ').'
    exit_code = re.search(r'^\s*last exit code = (\S+)\s*$', info, re.MULTILINE)
    detail = ' Last exit code: ' + exit_code.group(1) + '.' if exit_code else ''
    return 'ClipBridge is loaded but not running.' + detail + ' Check ' + str(log_path()) + '.'


def start(config_path):
    """Build helpers, install a plist, and bootstrap the current user's job."""
    config_path = Path(config_path).expanduser().resolve(strict=True)
    configuration.load_config(config_path)
    check_foreground_available()
    build = _run(['make', '-C', str(ROOT), 'build'], timeout=180)
    if build.returncode != 0:
        raise RuntimeError('Could not build clipboard helpers: ' + _details(build))

    path = _plist_path()
    previous = path.read_bytes() if path.exists() else None
    log_path().parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    plist = {
        'Label': LABEL,
        'ProgramArguments': [str(Path(sys.executable).resolve()), '-u',
                             str(ROOT / 'auto_upload.py'), '--config', str(config_path)],
        'WorkingDirectory': str(ROOT),
        'RunAtLoad': True,
        'KeepAlive': True,
        'ProcessType': 'Background',
        'ThrottleInterval': 10,
        'Umask': 0o077,
        'StandardOutPath': str(log_path()),
        'StandardErrorPath': str(log_path()),
    }
    try:
        _atomic_write(path, plistlib.dumps(plist))
        result = _run([LAUNCHCTL, 'bootstrap', _domain(), str(path)])
        if result.returncode != 0:
            raise RuntimeError('Could not start ClipBridge: ' + _details(result))
        info = _service_info()
        if info is None:
            raise RuntimeError('launchd accepted the plist but ClipBridge is not loaded.')
    except BaseException as error:
        # Cancellation must roll back the login plist just like a command failure.
        # Re-raise the original exception only when rollback has been verified.
        cleanup_errors = []
        try:
            if _service_info() is not None:
                result = _run([LAUNCHCTL, 'bootout', _target()])
                if _service_info() is not None:
                    detail = _details(result) if result.returncode else 'service is still loaded'
                    cleanup_errors.append('Could not unload the failed service: ' + detail)
        except BaseException as cleanup:
            cleanup_errors.append(str(cleanup) or type(cleanup).__name__)
        try:
            if previous is None:
                path.unlink(missing_ok=True)
            else:
                _atomic_write(path, previous)
        except BaseException as cleanup:
            cleanup_errors.append('Could not restore the login plist at ' + str(path)
                                  + ': ' + (str(cleanup) or type(cleanup).__name__))
        if cleanup_errors:
            reason = 'Startup cancelled.' if isinstance(error, KeyboardInterrupt) else str(error)
            raise RuntimeError(reason + ' Cleanup also failed: ' + '; '.join(cleanup_errors)
                               + '. Inspect clipbridge status and stop the service manually.') from error
        raise
    return _description(info)


def stop():
    """Unload only this service and remove its login plist; safe to repeat."""
    if _service_info() is not None:
        result = _run([LAUNCHCTL, 'bootout', _target()])
        # Also allow another process to have unloaded it between print and bootout.
        if _service_info() is not None:
            detail = _details(result) if result.returncode else 'service is still loaded'
            raise RuntimeError('Could not stop ClipBridge: ' + detail)
    _plist_path().unlink(missing_ok=True)
    return ('ClipBridge background service is stopped; automatic startup is disabled. '
            'Foreground monitors, if any, are unaffected.')


def status():
    """Report launchd state, not SSH reachability or upload success."""
    info = _service_info()
    if info is not None:
        return _description(info)
    if _monitor_running():
        return ('A foreground or unmanaged ClipBridge monitor is running. '
                'No ClipBridge background service is loaded.')
    if _plist_path().exists():
        return ('ClipBridge is stopped. A login plist remains at ' + str(_plist_path())
                + '; run clipbridge stop to remove it.')
    return 'ClipBridge is stopped.'
