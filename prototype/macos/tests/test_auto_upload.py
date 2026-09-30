from concurrent.futures import Future
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parents[1]))
import auto_upload as m


class ConfigTests(unittest.TestCase):
    def load(self, value):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / 'config.json'
            path.write_text(json.dumps(value))
            return m.load_config(path)

    def test_explicit_destination(self):
        self.assertEqual(self.load({
            'ssh_host': 'dev-server', 'remote_directory': '/home/example/images/'
        }), ('dev-server', '/home/example/images'))

    def test_rejects_unsafe_or_missing_configuration(self):
        for value in [[], {}, {'ssh_host': '-oProxyCommand=bad'},
                      {'ssh_host': 'host:22', 'remote_directory': '/tmp/images'},
                      {'ssh_host': 'host', 'remote_directory': '/tmp/../images'},
                      {'ssh_host': 'host', 'remote_directory': '/tmp/$(id)'},
                      {'ssh_host': 'host', 'remote_directory': "'/tmp/images"},
                      {'ssh_host': 'host', 'remote_directory': '~/images'},
                      {'ssh_host': 'host', 'remote_directory': 42}]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.load(value)


class SessionTests(unittest.TestCase):
    def test_connected_requires_same_user_process_and_socket(self):
        listing = subprocess.CompletedProcess([], 0, '42 1000 ssh dev-server\n43 1001 ssh dev-server\n', '')
        sockets = subprocess.CompletedProcess([], 0, 'p42\nn127.0.0.1:50000->127.0.0.1:22\n', '')
        with patch.object(m.os, 'getuid', return_value=1000), \
                patch.object(m, 'run', side_effect=[listing, sockets]) as run:
            self.assertTrue(m.connected('dev-server'))
            self.assertIn('42', run.call_args.args[0])
            self.assertNotIn('43', run.call_args.args[0])

    def test_missing_session_never_checks_sockets(self):
        listing = subprocess.CompletedProcess([], 0, '42 1000 ssh elsewhere\n', '')
        with patch.object(m.os, 'getuid', return_value=1000), \
                patch.object(m, 'run', return_value=listing) as run:
            self.assertFalse(m.connected('dev-server'))
            self.assertEqual(run.call_count, 1)

    def test_process_inspection_failure_is_offline(self):
        with patch.object(m, 'run', side_effect=OSError('unavailable')):
            self.assertFalse(m.connected('dev-server'))

    def test_session_only(self):
        self.assertTrue(m.is_session(['ssh', 'dev-server'], 'dev-server'))
        self.assertTrue(m.is_session(['/usr/bin/ssh', '-t', '-p', '22', 'dev-server'], 'dev-server'))
        for args in [[], ['ssh', 'elsewhere'], ['scp', 'x', 'dev-server:x'],
                     ['ssh', 'dev-server', 'true'], ['ssh', '-G', 'dev-server'],
                     ['ssh', '-O', 'check', 'dev-server']]:
            self.assertFalse(m.is_session(args, 'dev-server'))


class UploadTests(unittest.TestCase):
    def test_unexpected_worker_failure_is_logged(self):
        future = Future()
        future.set_exception(OSError('cache unavailable'))
        with self.assertLogs(level='ERROR') as logs:
            m.report_upload_result(future)
        self.assertIn('cache unavailable', '\n'.join(logs.output))

    def exercise_upload(self, fail=False, clipboard_result='copied'):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / 'capture.png'
            path.write_bytes(b'test image')
            calls = []

            def fake_run(args, timeout=10):
                calls.append(args)
                if fail and args[0] == '/usr/bin/scp':
                    raise subprocess.CalledProcessError(1, args)
                output = clipboard_result if 'set-if' in args else '10\n'
                return subprocess.CompletedProcess(args, 0, output, '')

            with patch.object(m, 'CACHE', Path(root)), \
                    patch.object(m, 'REMOTE', '/home/example/images'), \
                    patch.object(m, 'connected', return_value=True), \
                    patch.object(m, 'run', side_effect=fake_run), \
                    patch.object(m, 'notify') as notify:
                if fail:
                    with self.assertLogs(level='ERROR'):
                        m.upload(path, 'dev-server', '10')
                else:
                    m.upload(path, 'dev-server', '10')
            retained = list(Path(root).glob('upload-*/*'))
            self.assertEqual(bool(retained), fail)
            if fail:
                self.assertEqual(retained[0].read_bytes(), b'test image')
                self.assertFalse(any('set-if' in args for args in calls))
            else:
                setting = next(args for args in calls if 'set-if' in args)
                self.assertEqual(setting[2], '10')
                self.assertTrue(setting[3].startswith('/home/example/images/shot-'))
                self.assertIn('newer clipboard preserved' if clipboard_result == 'preserved'
                              else 'remote path copied', notify.call_args.args[0])

    def test_upload_failure_keeps_image_and_does_not_copy_path(self):
        self.exercise_upload(fail=True)

    def test_success_copies_path_and_removes_local_copy(self):
        self.exercise_upload()

    def test_newer_clipboard_is_preserved_after_upload(self):
        self.exercise_upload(clipboard_result='preserved')

    def test_disconnect_skips_queued_upload_and_discards_capture(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / 'capture.png'
            path.write_bytes(b'image')
            with patch.object(m, 'connected', return_value=False), patch.object(m, 'run') as run:
                m.upload(path, 'dev-server', '10')
            run.assert_not_called()
            self.assertFalse(path.exists())


if __name__ == '__main__':
    unittest.main()
