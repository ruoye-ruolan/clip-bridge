"""Manage ClipBridge's per-user launchd service without touching older uploaders."""

import fcntl
from contextlib import contextmanager
import os
from pathlib import Path
import plistlib
import re
import subprocess
import sys
import tempfile
from xml.parsers.expat import ExpatError

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


@contextmanager
def _operation_lock():
    """Serialize service changes so one failed startup cannot undo another."""
    CACHE.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (CACHE / 'service.lock').open('a') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise RuntimeError('Another ClipBridge service command is in progress. Try again shortly.') from error
        yield


def _check_legacy_services():
    legacy = legacy_services()
    if legacy:
        raise RuntimeError(
            'An older uploader is loaded: ' + ', '.join(legacy)
            + '. Stop it manually before starting ClipBridge; it has not been changed.')


def check_foreground_available():
    """Reject duplicate monitors before foreground or background startup."""
    if _service_info() is not None:
        raise RuntimeError(
            'ClipBridge is already loaded. Run clipbridge stop first, then start '
            'again to use the selected configuration.')
    _check_legacy_services()
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


def _validate_config(config_path):
    config_path = Path(config_path).expanduser().resolve(strict=True)
    configuration.load_config(config_path)
    return config_path


def _build_helpers():
    build = _run(['make', '-C', str(ROOT), 'build'], timeout=180)
    if build.returncode != 0:
        raise RuntimeError('Could not build clipboard helpers: ' + _details(build))


def _loaded_config_path():
    """Read the selected config from the plist managed by this service."""
    try:
        with _plist_path().open('rb') as stream:
            plist = plistlib.load(stream)
        arguments = plist['ProgramArguments']
        if not isinstance(arguments, list) or not all(isinstance(item, str) for item in arguments):
            raise ValueError('ProgramArguments must be a list of strings')
        selected = Path(arguments[arguments.index('--config') + 1])
        if not selected.is_absolute():
            raise ValueError('service config path is not absolute')
        return selected.resolve()
    except (OSError, ValueError, KeyError, IndexError, TypeError, ExpatError, plistlib.InvalidFileException) as error:
        raise RuntimeError(
            'Cannot verify the loaded service configuration. Run clipbridge restart '
            'with the selected --config to reload it.') from error


def _use_loaded_service(info, config_path):
    if _loaded_config_path() != config_path:
        raise RuntimeError(
            'ClipBridge is already loaded with a different configuration. '
            'Run clipbridge restart with the selected --config to switch configurations.')
    if re.search(r'^\s*pid = \d+\s*$', info, re.MULTILINE):
        return _description(info) + ' Use clipbridge restart to reload changed settings.'
    # Without -k, kickstart never interrupts a process launchd has just started.
    result = _run([LAUNCHCTL, 'kickstart', _target()])
    if result.returncode != 0:
        raise RuntimeError('Could not resume ClipBridge: ' + _details(result))
    info = _service_info()
    if info is None:
        raise RuntimeError('ClipBridge is no longer loaded. Run clipbridge start again.')
    return _description(info)


def _request_foreground_handoff():
    # Imported here so configuration and read-only status need no control socket.
    import monitor_control

    return monitor_control.request_handoff(CACHE, timeout=180)


def _start(config_path, helpers_built=False):
    info = _service_info()
    if info is not None:
        return _use_loaded_service(info, config_path)
    _check_legacy_services()
    if not helpers_built:
        _build_helpers()
    if _monitor_running():
        handed_off = _request_foreground_handoff()
        if not handed_off and _monitor_running():
            raise RuntimeError(
                'An older or unmanaged ClipBridge monitor is running. Press Ctrl+C once '
                'in its terminal, then run clipbridge start again.')
    _check_monitor_lock()
    return _install_service(config_path)


def start(config_path):
    """Ensure the selected background service is running, without duplicating it."""
    config_path = _validate_config(config_path)
    with _operation_lock():
        return _start(config_path)


def _install_service(config_path):
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


def _stop():
    if _service_info() is not None:
        result = _run([LAUNCHCTL, 'bootout', _target()])
        # Also allow another process to have unloaded it between print and bootout.
        if _service_info() is not None:
            detail = _details(result) if result.returncode else 'service is still loaded'
            raise RuntimeError('Could not stop ClipBridge: ' + detail)
    _plist_path().unlink(missing_ok=True)
    return ('ClipBridge background service is stopped; automatic startup is disabled. '
            'Foreground monitors, if any, are unaffected.')


def stop():
    """Unload only this service and remove its login plist; safe to repeat."""
    with _operation_lock():
        return _stop()


def restart(config_path):
    """Apply selected settings with one serialized background-service restart."""
    config_path = _validate_config(config_path)
    with _operation_lock():
        # A bad config, legacy service or compiler must not stop a working service.
        _check_legacy_services()
        _build_helpers()
        _stop()
        return _start(config_path, helpers_built=True)


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
