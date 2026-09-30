"""Install transactions run in private prefixes with every service action mocked."""
import json
from pathlib import Path
import plistlib
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parents[1]))
import installer
import runtime
import service


class InstallerTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix='clipbridge install & ')
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()
        self.source = self.home / 'source'
        self.prefix = self.home / 'prefix'
        self.root = self.prefix / 'share/clipbridge'
        self.bin = self.prefix / 'bin/clipbridge'
        self.config = self.home / '.config/clipbridge/config.json'
        self.source.mkdir()
        actual = Path(__file__).resolve().parents[2]
        for relative in installer.payload_files(actual):
            target = self.source / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes((actual / relative).read_bytes())
        self.loaded = False
        self.fail_activation = False
        self.activations = []
        self.stops = 0
        patches = [
            patch.object(service, 'HOME', self.home),
            patch.object(service, 'CACHE', self.home / 'cache'),
            patch.object(service, '_service_info', side_effect=lambda: 'pid = 42' if self.loaded else None),
            patch.object(service, '_monitor_running', return_value=False),
            patch.object(service, '_check_legacy_services'),
            patch.object(service, '_stop', side_effect=self.stop),
            patch.object(service, '_install_service', side_effect=self.activate),
            patch.object(installer, '_restore_service', side_effect=self.restore),
            patch.object(installer, '_wait_healthy'),
            patch.object(installer, '_prepare_release', side_effect=self.prepare),
            patch.object(installer.configuration, 'DEFAULT_CONFIG', self.config),
            patch.object(installer.sys, 'platform', 'darwin'),
            patch.object(installer.os, 'geteuid', return_value=1000),
        ]
        for item in patches:
            item.start()
            self.addCleanup(item.stop)

    def prepare(self, source, stage):
        for relative in installer.payload_files(source):
            target = stage / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes((source / relative).read_bytes())
        for name in runtime.HELPERS:
            helper = stage / 'src/build' / name
            helper.parent.mkdir(parents=True, exist_ok=True)
            helper.write_text('test helper')
            helper.chmod(0o755)

    def stop(self):
        self.loaded = False
        self.stops += 1
        service._plist_path().unlink(missing_ok=True)

    def activate(self, config, *, source_root=None, python_executable=None):
        if self.fail_activation:
            raise RuntimeError('new monitor failed')
        self.loaded = True
        self.activations.append((config, source_root))
        service._atomic_write(service._plist_path(), plistlib.dumps(service._service_definition(
            config, source_root=source_root, python_executable=python_executable)))
        return 'running'

    def restore(self, data, loaded):
        if data is not None:
            service._atomic_write(service._plist_path(), data)
        else:
            service._plist_path().unlink(missing_ok=True)
        self.loaded = loaded

    def install(self):
        return installer.install(self.source, prefix=self.prefix)

    def test_first_install_is_independent_private_and_does_not_start(self):
        self.source.joinpath('src/config.json').write_text('{"ssh_host":"local","remote_directory":"/tmp/private","private":"not for installation"}')
        self.install()
        current = (self.root / 'current').resolve()
        self.assertEqual(current.parent, (self.root / 'releases').resolve())
        self.assertTrue((current / 'src/build/clipboard-watch').is_file())
        self.assertFalse((current / 'src/config.json').exists())
        self.assertNotIn(str(self.source), self.bin.read_text())
        self.assertIn(str(self.root / 'current/clipbridge'), self.bin.read_text())
        self.assertEqual(self.config.stat().st_mode & 0o777, 0o600)
        self.assertFalse(self.loaded)
        self.assertEqual(self.activations, [])

    def test_repeat_install_preserves_configuration_and_retains_previous_release(self):
        self.install()
        first = (self.root / 'current').resolve()
        self.config.write_text('{"ssh_host":"mine","remote_directory":"/tmp/mine"}')
        before = self.config.read_bytes()
        self.install()
        self.assertNotEqual(first, (self.root / 'current').resolve())
        self.assertEqual((self.root / 'previous').resolve(), first)
        self.assertEqual(self.config.read_bytes(), before)
        self.assertFalse(self.loaded)

    def test_build_failure_leaves_existing_install_and_service_untouched(self):
        self.install()
        first = (self.root / 'current').resolve()
        self.loaded = True
        with patch.object(installer, '_prepare_release', side_effect=RuntimeError('compiler failed')):
            with self.assertRaisesRegex(RuntimeError, 'compiler failed'):
                self.install()
        self.assertEqual(first, (self.root / 'current').resolve())
        self.assertEqual(self.stops, 0)
        self.assertTrue(self.loaded)

    def test_foreign_launcher_is_never_overwritten(self):
        self.bin.parent.mkdir(parents=True)
        self.bin.write_text('unrelated command')
        with self.assertRaisesRegex(RuntimeError, 'owned|unmanaged'):
            self.install()
        self.assertEqual(self.bin.read_text(), 'unrelated command')

    def test_foreign_install_directory_is_never_replaced(self):
        self.root.mkdir(parents=True)
        (self.root / 'important').write_text('keep')
        with self.assertRaisesRegex(RuntimeError, 'owned|unmanaged'):
            self.install()
        self.assertEqual((self.root / 'important').read_text(), 'keep')

    def test_upgrade_requires_existing_install(self):
        with self.assertRaisesRegex(RuntimeError, 'not installed'):
            installer.install(self.source, prefix=self.prefix, upgrade=True)

    def test_running_source_service_migrates_and_preserves_config(self):
        self.config.parent.mkdir(parents=True)
        self.config.write_text('{"ssh_host":"mine","remote_directory":"/tmp/mine"}')
        self.activate(self.config, source_root=self.source / 'src')
        original = self.config.read_bytes()
        self.install()
        self.assertTrue(self.loaded)
        self.assertEqual(self.stops, 1)
        self.assertEqual(self.config.read_bytes(), original)
        plist = plistlib.loads(service._plist_path().read_bytes())
        self.assertIn(str(self.root / 'current/src/auto_upload.py'), plist['ProgramArguments'])
        self.assertEqual(plist['WorkingDirectory'], str(self.root / 'current/src'))

    def test_activation_failure_restores_previous_install_and_service(self):
        self.install()
        first = (self.root / 'current').resolve()
        self.config.write_text('{"ssh_host":"mine","remote_directory":"/tmp/mine"}')
        self.activate(self.config, source_root=self.root / 'current/src')
        old_plist = service._plist_path().read_bytes()
        old_launcher = self.bin.read_bytes()
        self.fail_activation = True
        with self.assertRaisesRegex(RuntimeError, 'new monitor failed'):
            self.install()
        self.assertEqual((self.root / 'current').resolve(), first)
        self.assertEqual(service._plist_path().read_bytes(), old_plist)
        self.assertEqual(self.bin.read_bytes(), old_launcher)
        self.assertTrue(self.loaded)

    def test_cancelled_activation_rolls_back_source_service_and_new_install(self):
        self.config.parent.mkdir(parents=True)
        self.config.write_text('{"ssh_host":"mine","remote_directory":"/tmp/mine"}')
        self.activate(self.config, source_root=self.source / 'src')
        original = service._plist_path().read_bytes()
        with patch.object(service, '_install_service', side_effect=KeyboardInterrupt):
            with self.assertRaises(KeyboardInterrupt):
                self.install()
        self.assertEqual(service._plist_path().read_bytes(), original)
        self.assertTrue(self.loaded)
        self.assertFalse(self.root.exists())
        self.assertFalse(self.bin.exists())

    def test_readiness_failure_restores_old_version(self):
        self.install()
        first = (self.root / 'current').resolve()
        self.activate(self.config, source_root=self.root / 'current/src')
        with patch.object(installer, '_wait_healthy', side_effect=RuntimeError('not ready')):
            with self.assertRaisesRegex(RuntimeError, 'not ready'):
                self.install()
        self.assertEqual((self.root / 'current').resolve(), first)
        self.assertTrue(self.loaded)

    def test_source_local_active_config_is_copied_out_before_migration(self):
        local = self.source / 'src/config.json'
        local.write_text('{"ssh_host":"mine","remote_directory":"/tmp/mine"}')
        self.activate(local, source_root=self.source / 'src')
        self.install()
        self.assertEqual(self.activations[-1][0], self.config)
        self.assertEqual(json.loads(local.read_text()), json.loads(self.config.read_text()))
        self.assertTrue(local.exists())

    def test_foreground_monitor_blocks_installation_without_mutation(self):
        with patch.object(service, '_monitor_running', return_value=True):
            with self.assertRaisesRegex(RuntimeError, 'Stop foreground'):
                self.install()
        self.assertFalse(self.root.exists())
        self.assertFalse(self.bin.exists())

    def test_different_source_service_is_not_stopped(self):
        self.config.parent.mkdir(parents=True)
        self.config.write_text('{"ssh_host":"mine","remote_directory":"/tmp/mine"}')
        self.activate(self.config, source_root=self.home / 'another-checkout/src')
        with self.assertRaisesRegex(RuntimeError, 'not owned'):
            self.install()
        self.assertEqual(self.stops, 0)
        self.assertTrue(self.loaded)

    def test_unloaded_login_plist_is_migrated_without_starting_monitor(self):
        self.config.parent.mkdir(parents=True)
        self.config.write_text('{"ssh_host":"mine","remote_directory":"/tmp/mine"}')
        self.activate(self.config, source_root=self.source / 'src')
        self.loaded = False
        self.activations.clear()
        self.install()
        self.assertEqual(self.activations, [])
        self.assertFalse(self.loaded)
        self.assertIn(str(self.root / 'current/src/auto_upload.py'),
                      plistlib.loads(service._plist_path().read_bytes())['ProgramArguments'])

    def test_failed_recovery_keeps_files_and_reports_actionable_error(self):
        self.install()
        self.activate(self.config, source_root=self.root / 'current/src')
        self.fail_activation = True
        with patch.object(installer, '_restore_service', side_effect=RuntimeError('restore denied')):
            with self.assertRaisesRegex(RuntimeError, 'recovery needs attention.*restore denied'):
                self.install()
        self.assertTrue(self.root.exists())

    def test_upgrade_keeps_only_current_and_previous_release(self):
        for _ in range(3):
            self.install()
        self.assertEqual(len(list((self.root / 'releases').iterdir())), 2)

    def test_unknown_release_data_is_never_pruned_or_uninstalled(self):
        self.install()
        foreign = self.root / 'releases/unrelated'
        foreign.mkdir()
        (foreign / runtime.RELEASE_MARKER).write_text('{"product":"another-tool","schema":1}')
        (foreign / 'personal').write_text('preserve')
        self.install()
        self.assertEqual((foreign / 'personal').read_text(), 'preserve')
        with self.assertRaisesRegex(RuntimeError, 'Unmanaged release'):
            installer.uninstall(prefix=self.prefix)
        self.assertTrue(self.bin.exists())

    def test_link_outside_release_tree_is_rejected(self):
        self.install()
        (self.root / 'current').unlink()
        (self.root / 'current').symlink_to('/tmp')
        with self.assertRaisesRegex(RuntimeError, 'escapes'):
            installer.uninstall(prefix=self.prefix)
        self.assertTrue(self.bin.exists())

    def test_uninstall_preserves_active_configuration_inside_program_directory(self):
        self.install()
        internal = (self.root / 'current/src/config.json').resolve()
        internal.write_text('{"ssh_host":"mine","remote_directory":"/tmp/mine"}')
        self.activate(internal, source_root=self.root / 'current/src')
        result = installer.uninstall(prefix=self.prefix)
        migrated = list(self.config.parent.glob('migrated-*.json'))
        self.assertEqual(len(migrated), 1)
        self.assertEqual(json.loads(migrated[0].read_text())['ssh_host'], 'mine')
        self.assertIn('Moved active configuration', result)

    def test_uninstall_stops_only_owned_service_and_preserves_user_data(self):
        self.install()
        self.activate(self.config, source_root=self.root / 'current/src')
        log = service.log_path()
        log.parent.mkdir(parents=True)
        log.write_text('preserve log')
        before = self.config.read_bytes()
        installer.uninstall(prefix=self.prefix)
        self.assertFalse(self.root.exists())
        self.assertFalse(self.bin.exists())
        self.assertFalse(self.loaded)
        self.assertEqual(self.config.read_bytes(), before)
        self.assertEqual(log.read_text(), 'preserve log')
        self.assertIn('not installed', installer.uninstall(prefix=self.prefix))

    def test_uninstall_rejects_modified_launcher_without_deleting_program(self):
        self.install()
        self.bin.write_text('replacement command')
        with self.assertRaisesRegex(RuntimeError, 'owned|modified'):
            installer.uninstall(prefix=self.prefix)
        self.assertTrue(self.root.exists())
        self.assertEqual(self.bin.read_text(), 'replacement command')


if __name__ == '__main__':
    unittest.main()
