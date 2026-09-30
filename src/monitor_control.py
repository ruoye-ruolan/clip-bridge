"""Cooperative foreground-to-launchd handoff, without sending process signals."""

from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path
import tempfile
import time
import uuid


VERSION = 1
POLL_INTERVAL = 0.1


def _read_json(path):
    try:
        with path.open(encoding='utf-8') as stream:
            value = json.load(stream)
    except (FileNotFoundError, ValueError, UnicodeError):
        return None
    return value if isinstance(value, dict) else None


def _atomic_write(path, value):
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode='w', encoding='utf-8',
                                         dir=path.parent, delete=False) as stream:
            temporary = Path(stream.name)
            os.fchmod(stream.fileno(), 0o600)
            json.dump(value, stream)
            stream.write('\n')
        temporary.replace(path)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def _remove_matching(path, token, request_id=None):
    current = _read_json(path)
    if (current is not None and current.get('token') == token
            and (request_id is None or current.get('request_id') == request_id)):
        path.unlink(missing_ok=True)


def _lock_available(cache):
    """Inspect the existing lock inode; never create or unlink it here."""
    try:
        lock = (cache / 'watcher.lock').open('r')
    except FileNotFoundError:
        return True
    with lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return False
    return True


class MonitorRegistration:
    def __init__(self, cache, config_path, foreground):
        self.cache = Path(cache)
        self.token = uuid.uuid4().hex
        self.foreground = foreground
        self.state = {'version': VERSION, 'token': self.token, 'pid': os.getpid(),
                      'config': str(Path(config_path).expanduser().resolve()),
                      'mode': 'foreground' if foreground else 'background'}

    def handoff_requested(self):
        if not self.foreground:
            return False
        request = _read_json(self.cache / 'handoff.json')
        return bool(request and request.get('version') == VERSION
                    and request.get('token') == self.token
                    and isinstance(request.get('request_id'), str)
                    and request['request_id'])


@contextmanager
def monitor_registration(cache, config_path, *, foreground):
    """Register while the caller holds watcher.lock, including upload draining."""
    registration = MonitorRegistration(cache, config_path, foreground)
    _atomic_write(registration.cache / 'monitor.json', registration.state)
    try:
        yield registration
    finally:
        _remove_matching(registration.cache / 'handoff.json', registration.token)
        _remove_matching(registration.cache / 'monitor.json', registration.token)


def request_handoff(cache, timeout=180):
    """Ask the current foreground instance to drain and wait for its lock release.

    False means there is no supported foreground instance, or it was replaced.
    Cancellation and timeouts withdraw this caller's request; an instance already
    draining may still finish its shutdown. No monitor is ever forcibly stopped.
    """
    cache = Path(cache)
    state = _read_json(cache / 'monitor.json')
    if (not state or state.get('version') != VERSION or state.get('mode') != 'foreground'
            or not isinstance(state.get('token'), str) or not state['token']):
        return False
    if _lock_available(cache):
        return False

    token = state['token']
    request_id = uuid.uuid4().hex
    request_path = cache / 'handoff.json'
    deadline = time.monotonic() + timeout
    try:
        _atomic_write(request_path, {'version': VERSION, 'token': token, 'request_id': request_id})
        while True:
            current = _read_json(cache / 'monitor.json')
            if current is not None and current.get('token') != token:
                return False
            if _lock_available(cache):
                return True
            if time.monotonic() >= deadline:
                raise RuntimeError(
                    'Timed out waiting for the foreground ClipBridge monitor to finish uploads '
                    'and release its lock. It was not forcibly stopped; inspect its terminal '
                    'before trying clipbridge start again.')
            time.sleep(POLL_INTERVAL)
    finally:
        _remove_matching(request_path, token, request_id)
