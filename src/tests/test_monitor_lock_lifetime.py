"""Exercise monitor lock ownership across interrupted executor shutdown."""

import fcntl
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest


CHILD_SCRIPT = r'''
import concurrent.futures
import json
import os
from pathlib import Path
import sys
import threading
import time
from unittest.mock import patch

import auto_upload

cache = Path(sys.argv[1])
scenario = sys.argv[2]
config_path = cache / 'config.json'
config_path.write_text(json.dumps({
    'ssh_host': 'example', 'remote_directory': '/tmp/clipbridge-test'}))
worker_started = threading.Event()


class PipeWatcher:
    """A private pipe only; never invoke the native clipboard watcher."""
    def __init__(self, *args, **kwargs):
        read_fd, write_fd = os.pipe()
        self.stdout = os.fdopen(read_fd, 'rb', buffering=0)
        self.writer = os.fdopen(write_fd, 'wb', buffering=0)
        self.returncode = None

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.stdout.close()
        self.writer.close()

    def poll(self):
        return self.returncode

    def terminate(self):
        self.returncode = 0
        self.writer.close()

    def wait(self, timeout=None):
        return self.returncode


class ObservedExecutor(concurrent.futures.ThreadPoolExecutor):
    def shutdown(self, wait=True, *, cancel_futures=False):
        (cache / 'drain-started').write_text('draining')
        if scenario == 'interrupted' and wait and not getattr(self, 'interrupted', False):
            # Model Ctrl+C interrupting __exit__ while its worker is still busy.
            self.interrupted = True
            super().shutdown(wait=False, cancel_futures=cancel_futures)
            raise KeyboardInterrupt
        super().shutdown(wait=wait, cancel_futures=cancel_futures)


def fake_upload():
    worker_started.set()
    deadline = time.monotonic() + 5
    while not (cache / 'release-worker').exists():
        if time.monotonic() >= deadline:
            (cache / 'worker-timed-out').write_text('timed out')
            return
        time.sleep(0.01)
    (cache / 'worker-finished').write_text('finished')


def fake_monitor_events(watcher, host, executor, control):
    executor.submit(fake_upload)
    if not worker_started.wait(timeout=2):
        raise RuntimeError('Fake upload worker did not start')


with patch.object(auto_upload, 'CACHE', cache), \
        patch.object(auto_upload.os, 'access', return_value=True), \
        patch.object(auto_upload.subprocess, 'Popen', PipeWatcher), \
        patch.object(auto_upload.concurrent.futures, 'ThreadPoolExecutor', ObservedExecutor), \
        patch.object(auto_upload, 'monitor_events', fake_monitor_events):
    auto_upload.main(['--config', str(config_path), '--foreground'])
    (cache / 'main-returned').write_text('returned')
# Python now joins the real executor worker before running ordinary atexit hooks.
'''


class MonitorLockLifetimeTests(unittest.TestCase):
    def test_interrupted_drain_holds_lock_until_worker_and_interpreter_exit(self):
        self.exercise_shutdown('interrupted')

    def test_normal_drain_holds_lock_and_delays_main_return_until_worker_finishes(self):
        self.exercise_shutdown('normal')

    def exercise_shutdown(self, scenario):
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            process = subprocess.Popen(
                [sys.executable, '-u', '-c', CHILD_SCRIPT, str(cache), scenario],
                cwd=Path(__file__).resolve().parents[1],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                ready = 'main-returned' if scenario == 'interrupted' else 'drain-started'
                deadline = time.monotonic() + 3
                while not (cache / ready).exists():
                    if process.poll() is not None:
                        stdout, stderr = process.communicate(timeout=1)
                        self.fail('Child exited before ' + ready + ': ' + stdout + stderr)
                    if time.monotonic() >= deadline:
                        self.fail('Child did not reach ' + ready)
                    time.sleep(0.01)

                self.assertIsNone(process.poll(), 'Worker should still keep the child alive')
                self.assertFalse((cache / 'worker-finished').exists())
                if scenario == 'normal':
                    self.assertFalse((cache / 'main-returned').exists(),
                                     'Normal main must wait for the upload worker')
                with (cache / 'watcher.lock').open('r') as lock:
                    with self.assertRaises(BlockingIOError):
                        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)

                (cache / 'release-worker').write_text('release')
                stdout, stderr = process.communicate(timeout=5)
                self.assertEqual(process.returncode, 0, stdout + stderr)
                self.assertTrue((cache / 'main-returned').exists())
                self.assertTrue((cache / 'worker-finished').exists())
                self.assertFalse((cache / 'worker-timed-out').exists())
                with (cache / 'watcher.lock').open('r') as lock:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            finally:
                (cache / 'release-worker').touch()
                try:
                    process.communicate(timeout=5)
                except subprocess.TimeoutExpired:
                    process.terminate()
                    try:
                        process.communicate(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.communicate(timeout=5)


if __name__ == '__main__':
    unittest.main()
