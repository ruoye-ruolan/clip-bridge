"""Exercise monitor handoff polling with harmless pipe writers, not clipboard helpers."""
import subprocess
import sys
import unittest
from unittest.mock import Mock, patch
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parents[1]))
import auto_upload


class MonitorRuntimeTests(unittest.TestCase):
    def writer(self, code):
        process = subprocess.Popen([sys.executable, '-u', '-c', code], stdout=subprocess.PIPE, bufsize=0)

        def cleanup():
            if process.poll() is None:
                process.terminate()
            process.wait(timeout=5)
            process.stdout.close()

        self.addCleanup(cleanup)
        return process

    def test_idle_monitor_observes_handoff_without_a_clipboard_event(self):
        watcher = self.writer('import time; time.sleep(30)')
        control = Mock()
        control.handoff_requested.side_effect = [False, True]
        with patch.object(auto_upload, 'process_event') as event:
            auto_upload.monitor_events(watcher, 'test-host', Mock(), control)
        event.assert_not_called()

    def test_split_event_is_delivered_once_before_handoff(self):
        watcher = self.writer(
            'import sys, time; sys.stdout.write(\'{"image":true,\'); sys.stdout.flush(); '
            'time.sleep(0.05); sys.stdout.write(\'"count":12}\\n\'); sys.stdout.flush(); time.sleep(30)')
        control = Mock()
        control.handoff_requested.return_value = False
        executor = Mock()

        def delivered(*args):
            control.handoff_requested.return_value = True

        with patch.object(auto_upload, 'process_event', side_effect=delivered) as event:
            auto_upload.monitor_events(watcher, 'test-host', executor, control)
        event.assert_called_once_with({'image': True, 'count': 12}, 'test-host', executor)

    def test_unexpected_watcher_exit_is_not_silent(self):
        watcher = self.writer('pass')
        control = Mock()
        control.handoff_requested.return_value = False
        with self.assertRaisesRegex(RuntimeError, 'watcher exited'):
            auto_upload.monitor_events(watcher, 'test-host', Mock(), control)


if __name__ == '__main__':
    unittest.main()
