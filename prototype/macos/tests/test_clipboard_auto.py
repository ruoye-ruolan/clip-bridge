import sys
from pathlib import Path
import unittest
from unittest.mock import patch, Mock
sys.path.insert(0, str(Path(__file__).parents[1]))
import auto_upload as m

class ClipboardTests(unittest.TestCase):
    def test_text_skipped(self):
        with patch.object(m, 'connected') as connected:
            m.process_event({'count': 10, 'image': False}, 'wsl', Mock())
            connected.assert_not_called()

    def test_offline_image_skipped(self):
        pool = Mock()
        with patch.object(m, 'connected', return_value=False), patch.object(m, 'run') as run:
            m.process_event({'count': 10, 'image': True}, 'wsl', pool)
            run.assert_not_called()
            pool.submit.assert_not_called()

    def test_online_image_captures_original_count(self):
        import tempfile
        pool = Mock()
        with tempfile.TemporaryDirectory() as d, patch.object(m, 'CACHE', Path(d)), patch.object(m, 'connected', return_value=True), patch.object(m, 'run') as run:
            m.process_event({'count': 10, 'image': True}, 'wsl', pool)
            self.assertEqual(run.call_args.args[0][1:3], ['capture', '10'])
            self.assertEqual(pool.submit.call_args.args[3], '10')

    def test_changed_clipboard_not_uploaded(self):
        import tempfile, subprocess
        pool = Mock()
        with tempfile.TemporaryDirectory() as d, patch.object(m, 'CACHE', Path(d)), patch.object(m, 'connected', return_value=True), patch.object(m, 'run', side_effect=subprocess.CalledProcessError(3, ['capture'])):
            m.process_event({'count': 10, 'image': True}, 'wsl', pool)
            pool.submit.assert_not_called()
            self.assertEqual(list(Path(d).iterdir()), [])

if __name__ == '__main__': unittest.main()
