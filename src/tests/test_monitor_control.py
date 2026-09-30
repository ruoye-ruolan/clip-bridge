from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parents[1]))
import monitor_control as control


@contextmanager
def locked_cache(cache):
    with (cache / 'watcher.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield


class RegistrationTests(unittest.TestCase):
    def test_registration_is_private_and_cleans_only_own_metadata(self):
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            with locked_cache(cache), control.monitor_registration(
                    cache, cache / 'config.json', foreground=True) as registration:
                state = json.loads((cache / 'monitor.json').read_text())
                self.assertEqual(state['version'], 1)
                self.assertEqual(state['pid'], os.getpid())
                self.assertEqual(state['mode'], 'foreground')
                self.assertEqual(state['config'], str((cache / 'config.json').resolve()))
                self.assertEqual(stat.S_IMODE((cache / 'monitor.json').stat().st_mode), 0o600)
                self.assertFalse(registration.handoff_requested())
            self.assertFalse((cache / 'monitor.json').exists())
            self.assertTrue((cache / 'watcher.lock').exists())

    def test_stale_request_does_not_stop_new_instance(self):
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            request = {'version': 1, 'token': 'old-instance', 'request_id': 'old-request'}
            (cache / 'handoff.json').write_text(json.dumps(request))
            with locked_cache(cache), control.monitor_registration(
                    cache, cache / 'config.json', foreground=True) as registration:
                self.assertFalse(registration.handoff_requested())

    def test_background_monitor_ignores_handoff_even_for_matching_token(self):
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            with locked_cache(cache), control.monitor_registration(
                    cache, cache / 'config.json', foreground=False) as registration:
                state = json.loads((cache / 'monitor.json').read_text())
                (cache / 'handoff.json').write_text(json.dumps({
                    'version': 1, 'token': state['token'], 'request_id': 'request'}))
                self.assertFalse(registration.handoff_requested())

    def test_exit_does_not_remove_replacement_state_or_request(self):
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            with locked_cache(cache), control.monitor_registration(
                    cache, cache / 'config.json', foreground=True):
                other = {'version': 1, 'token': 'replacement', 'mode': 'foreground'}
                (cache / 'monitor.json').write_text(json.dumps(other))
                (cache / 'handoff.json').write_text(json.dumps(other))
            self.assertEqual(json.loads((cache / 'monitor.json').read_text()), other)
            self.assertEqual(json.loads((cache / 'handoff.json').read_text()), other)

    def test_matching_request_is_detected_and_removed_on_exit(self):
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            with locked_cache(cache), control.monitor_registration(
                    cache, cache / 'config.json', foreground=True) as registration:
                state = json.loads((cache / 'monitor.json').read_text())
                (cache / 'handoff.json').write_text(json.dumps({
                    'version': 1, 'token': state['token'], 'request_id': 'request'}))
                self.assertTrue(registration.handoff_requested())
            self.assertFalse((cache / 'handoff.json').exists())


class HandoffTests(unittest.TestCase):
    def test_absent_unsupported_and_background_states_are_not_requested(self):
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            for state in (None, {}, {'version': 99, 'mode': 'foreground', 'token': 'token'},
                          {'version': 1, 'mode': 'background', 'token': 'token'}):
                if state is not None:
                    (cache / 'monitor.json').write_text(json.dumps(state))
                with self.subTest(state=state):
                    self.assertFalse(control.request_handoff(cache))
                    self.assertFalse((cache / 'handoff.json').exists())
            self.assertFalse((cache / 'watcher.lock').exists())

    def test_stale_foreground_metadata_without_lock_is_not_requested(self):
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            with control.monitor_registration(cache, cache / 'config.json', foreground=True):
                self.assertFalse(control.request_handoff(cache))
                self.assertFalse((cache / 'handoff.json').exists())

    def test_timeout_removes_request_so_monitor_cannot_stop_later(self):
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            with locked_cache(cache), control.monitor_registration(
                    cache, cache / 'config.json', foreground=True) as registration:
                with patch.object(control.time, 'monotonic', side_effect=[0, 181]), \
                        patch.object(control.time, 'sleep'):
                    with self.assertRaisesRegex(RuntimeError, 'timed out|Timed out'):
                        control.request_handoff(cache, timeout=180)
                self.assertFalse((cache / 'handoff.json').exists())
                self.assertFalse(registration.handoff_requested())

    def test_cancellation_removes_only_request_written_by_this_caller(self):
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            with locked_cache(cache), control.monitor_registration(
                    cache, cache / 'config.json', foreground=True) as registration:
                with patch.object(control.time, 'sleep', side_effect=KeyboardInterrupt):
                    with self.assertRaises(KeyboardInterrupt):
                        control.request_handoff(cache)
                self.assertFalse((cache / 'handoff.json').exists())
                self.assertFalse(registration.handoff_requested())

    def test_replacement_instance_is_not_stopped(self):
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            with locked_cache(cache), control.monitor_registration(
                    cache, cache / 'config.json', foreground=True):
                def replace_instance(_delay):
                    state = json.loads((cache / 'monitor.json').read_text())
                    state['token'] = 'unrelated-instance'
                    (cache / 'monitor.json').write_text(json.dumps(state))

                with patch.object(control.time, 'sleep', side_effect=replace_instance):
                    self.assertFalse(control.request_handoff(cache))
                self.assertFalse((cache / 'handoff.json').exists())

    def test_cancellation_preserves_another_callers_request(self):
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            with locked_cache(cache), control.monitor_registration(
                    cache, cache / 'config.json', foreground=True):
                def replace_request(_delay):
                    path = cache / 'handoff.json'
                    self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
                    request = json.loads(path.read_text())
                    request['request_id'] = 'another-caller'
                    path.write_text(json.dumps(request))
                    raise KeyboardInterrupt

                with patch.object(control.time, 'sleep', side_effect=replace_request):
                    with self.assertRaises(KeyboardInterrupt):
                        control.request_handoff(cache)
                remaining = json.loads((cache / 'handoff.json').read_text())
                self.assertEqual(remaining['request_id'], 'another-caller')

    def test_success_waits_until_child_releases_monitor_lock(self):
        script = '''
import fcntl
from pathlib import Path
import sys
import time
from monitor_control import monitor_registration
cache = Path(sys.argv[1])
with (cache / 'watcher.lock').open('a') as lock:
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    with monitor_registration(cache, cache / 'config.json', foreground=True) as registration:
        print('ready', flush=True)
        deadline = time.monotonic() + 10
        while not registration.handoff_requested():
            if time.monotonic() > deadline:
                raise RuntimeError('No handoff request received')
            time.sleep(0.01)
        time.sleep(0.1)
        (cache / 'upload-finished').write_text('drained')
'''
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            process = subprocess.Popen([sys.executable, '-u', '-c', script, str(cache)],
                                       cwd=Path(__file__).parents[1],
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                self.assertEqual(process.stdout.readline().strip(), 'ready')
                self.assertTrue(control.request_handoff(cache, timeout=5))
                stdout, stderr = process.communicate(timeout=5)
                self.assertEqual(process.returncode, 0, stderr)
                self.assertEqual((cache / 'upload-finished').read_text(), 'drained')
                self.assertFalse((cache / 'monitor.json').exists())
                self.assertFalse((cache / 'handoff.json').exists())
                self.assertTrue((cache / 'watcher.lock').exists())
            finally:
                if process.poll() is None:
                    process.terminate()
                process.communicate(timeout=5)


if __name__ == '__main__':
    unittest.main()
