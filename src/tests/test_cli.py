import contextlib
import io
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parents[1]))
import cli
import configuration
import installer


class CliTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / 'config.json'
        self.output = io.StringIO()
        self.errors = io.StringIO()

    def command(self, *args):
        with contextlib.redirect_stdout(self.output), contextlib.redirect_stderr(self.errors):
            return cli.main(['--config', str(self.path), *args])

    def test_scripted_setup_checks_then_saves_without_starting(self):
        with patch.object(cli, 'check_destination', return_value='/home/test/images') as check, \
                patch.object(cli.service, 'start') as start:
            self.assertEqual(self.command('configure', '--host', 'server'), 0)
        check.assert_called_once_with('server', None)
        start.assert_not_called()
        self.assertEqual(configuration.load_config(self.path), ('server', '/home/test/images'))
        self.assertIn('start', self.output.getvalue())

    def test_failed_check_preserves_existing_configuration(self):
        configuration.save_config(self.path, 'old-server', '/home/test/old')
        before = self.path.read_bytes()
        with patch.object(cli, 'check_destination', side_effect=subprocess.CalledProcessError(
                255, ['ssh'], stderr='Host key verification failed.')):
            self.assertEqual(self.command('configure', '--host', 'new-server'), 1)
        self.assertEqual(self.path.read_bytes(), before)
        self.assertIn('Host key verification failed', self.errors.getvalue())

    def test_corrupt_configuration_can_be_repaired_with_private_backup(self):
        self.path.write_bytes(b'{invalid json')
        self.assertEqual(self.command('configure', '--host', 'server', '--remote-dir',
                                      '/home/test/images', '--no-check'), 0)
        self.assertEqual(configuration.load_config(self.path), ('server', '/home/test/images'))
        backups = list(self.path.parent.glob('config.json.backup-*'))
        self.assertEqual(len(backups), 1)
        self.assertEqual(backups[0].read_bytes(), b'{invalid json')
        self.assertEqual(backups[0].stat().st_mode & 0o777, 0o600)

    def test_failed_repair_keeps_corrupt_file_without_creating_backup(self):
        self.path.write_bytes(b'{invalid json')
        with patch.object(cli, 'check_destination', side_effect=RuntimeError('offline')):
            self.assertEqual(self.command('configure', '--host', 'server'), 1)
        self.assertEqual(self.path.read_bytes(), b'{invalid json')
        self.assertEqual(list(self.path.parent.glob('config.json.backup-*')), [])

    def test_offline_setup_needs_absolute_directory_and_never_connects(self):
        with patch.object(cli, 'check_destination') as check:
            self.assertEqual(self.command('configure', '--host', 'server', '--no-check'), 1)
            self.assertFalse(self.path.exists())
            self.assertEqual(self.command('configure', '--host', 'server', '--remote-dir',
                                          '/home/test/images', '--no-check'), 0)
            check.assert_not_called()
        self.assertIn('not checked', self.output.getvalue())

    def test_wizard_selects_alias_and_remote_home_default(self):
        with patch.object(cli.sys.stdin, 'isatty', return_value=True), \
                patch.object(cli, 'discover_ssh_aliases', return_value=['server', 'work']), \
                patch('builtins.input', side_effect=['2', '']), \
                patch.object(cli, 'check_destination', return_value='/home/test/images') as check:
            self.assertEqual(self.command('configure'), 0)
        check.assert_called_once_with('work', None)
        self.assertIn('2. work', self.output.getvalue())

    def test_interrupted_wizard_does_not_save(self):
        with patch.object(cli.sys.stdin, 'isatty', return_value=True), \
                patch.object(cli, 'discover_ssh_aliases', return_value=[]), \
                patch('builtins.input', side_effect=KeyboardInterrupt):
            self.assertEqual(self.command('configure'), 130)
        self.assertFalse(self.path.exists())

    def test_noninteractive_without_host_has_actionable_error(self):
        with patch.object(cli.sys.stdin, 'isatty', return_value=False):
            self.assertEqual(self.command('configure'), 1)
        self.assertIn('--host', self.errors.getvalue())

    def test_invalid_host_does_not_connect(self):
        with patch.object(cli, 'check_destination') as check:
            self.assertEqual(self.command('configure', '--host', 'bad;host'), 1)
            check.assert_not_called()

    def test_legacy_configuration_is_migrated_without_modifying_original(self):
        legacy = Path(self.temp.name) / 'legacy.json'
        configuration.save_config(legacy, 'server', '/home/test/images')
        original = legacy.read_bytes()
        with patch.object(cli, 'DEFAULT_CONFIG', self.path), \
                patch.object(cli, 'LEGACY_CONFIG', legacy), \
                patch.object(cli, 'check_destination', return_value='/home/test/images'):
            self.assertEqual(self.command('configure', '--host', 'server'), 0)
        self.assertEqual(legacy.read_bytes(), original)
        self.assertEqual(configuration.load_config(self.path), ('server', '/home/test/images'))

    def test_custom_config_is_in_next_step_command(self):
        self.assertEqual(self.command('configure', '--host', 'server', '--remote-dir',
                                      '/home/test/images', '--no-check'), 0)
        self.assertIn(f'--config {self.path} start', self.output.getvalue())

    def test_doctor_checks_remote_without_creating_files(self):
        configuration.save_config(self.path, 'server', '/home/test/images')
        with patch.object(cli, 'check_destination') as check, \
                patch.object(cli.service, 'legacy_services', return_value=[]), \
                patch.object(cli, 'connected', return_value=False), \
                patch.object(cli, 'check_local_tools', return_value=True):
            self.assertEqual(self.command('doctor'), 0)
        check.assert_called_once_with('server', '/home/test/images', create=False)
        self.assertIn('waiting', self.output.getvalue().lower())

    def test_doctor_reports_legacy_service_conflict(self):
        configuration.save_config(self.path, 'server', '/home/test/images')
        with patch.object(cli.service, 'legacy_services', return_value=['local.codex.xnip-wsl']), \
                patch.object(cli, 'check_local_tools', return_value=True), \
                patch.object(cli, 'check_destination') as check:
            self.assertEqual(self.command('doctor'), 1)
        check.assert_not_called()
        self.assertIn('local.codex.xnip-wsl', self.errors.getvalue())

    def test_run_refuses_legacy_before_build_or_exec(self):
        configuration.save_config(self.path, 'server', '/home/test/images')
        with patch.object(cli.service, 'check_foreground_available', side_effect=RuntimeError('legacy running')), \
                patch.object(cli.subprocess, 'run') as run, patch.object(cli.os, 'execv') as execute:
            self.assertEqual(self.command('run'), 1)
        run.assert_not_called()
        execute.assert_not_called()

    def test_start_passes_config_to_service(self):
        with patch.object(cli.service, 'start', return_value='Started') as start:
            self.assertEqual(self.command('start'), 0)
        start.assert_called_once_with(self.path)

    def test_restart_passes_config_to_service(self):
        with patch.object(cli.service, 'restart', return_value='Restarted', create=True) as restart:
            self.assertEqual(self.command('restart'), 0)
        restart.assert_called_once_with(self.path)

    def test_install_upgrade_and_uninstall_dispatch_without_monitoring(self):
        prefix = Path(self.temp.name) / 'prefix'
        source = Path(self.temp.name) / 'new-source'
        with patch.object(installer, 'install', return_value='Installed') as install, \
                patch.object(installer, 'uninstall', return_value='Uninstalled') as uninstall, \
                patch.object(cli.service, 'start') as start:
            self.assertEqual(self.command('install', '--prefix', str(prefix)), 0)
            install.assert_called_with(cli.ROOT.parent, prefix=prefix, upgrade=False)
            self.assertEqual(self.command('upgrade', '--from', str(source), '--prefix', str(prefix)), 0)
            install.assert_called_with(source, prefix=prefix, upgrade=True)
            self.assertEqual(self.command('uninstall', '--prefix', str(prefix)), 0)
            uninstall.assert_called_once_with(prefix=prefix)
            start.assert_not_called()

    def test_foreground_run_enables_cooperative_handoff(self):
        configuration.save_config(self.path, 'server', '/home/test/images')
        with patch.object(cli.service, 'check_foreground_available'), \
                patch.object(cli, 'check_local_tools'), patch.object(cli.service, '_build_helpers'), \
                patch.object(cli.os, 'execv') as execute:
            self.assertEqual(self.command('run'), 0)
        self.assertIn('--foreground', execute.call_args.args[1])
        self.assertIn(str(self.path.resolve()), execute.call_args.args[1])

    def test_status_does_not_misidentify_running_config(self):
        with patch.object(cli.service, 'status', return_value='ClipBridge is running (PID 42).'):
            self.assertEqual(self.command('status'), 0)
        self.assertEqual(self.output.getvalue(), 'ClipBridge is running (PID 42).\n')

    def test_logs_are_read_only_and_show_last_lines(self):
        log = Path(self.temp.name) / 'app.log'
        log.write_text('first\nsecond\nthird\n')
        with patch.object(cli.service, 'log_path', return_value=log):
            self.assertEqual(self.command('logs', '--lines', '2'), 0)
        self.assertEqual(self.output.getvalue(), 'second\nthird\n')

    def test_config_option_can_follow_subcommand(self):
        with contextlib.redirect_stdout(self.output), patch.object(cli.service, 'start', return_value='Started') as start:
            self.assertEqual(cli.main(['start', '--config', str(self.path)]), 0)
        start.assert_called_once_with(self.path)


if __name__ == '__main__':
    unittest.main()
