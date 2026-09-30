import fcntl
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parents[1]))
import service


class ServiceTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='clipbridge service & ')
        self.addCleanup(temporary.cleanup)
        self.home = Path(temporary.name)
        self.root = self.home / 'source files'
        self.root.mkdir()
        self.config = self.root / 'config.json'
        self.config.write_text('{"ssh_host":"dev","remote_directory":"/tmp/images"}')
        self.jobs = {}
        self.overrides = {}
        self.bootstrap_info = 'state = running\n pid = 123\n'
        for name, value in [('HOME', self.home), ('ROOT', self.root),
                            ('CACHE', self.home / 'cache')]:
            patcher = patch.object(service, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)
        patcher = patch.object(service.subprocess, 'run', side_effect=self.run_command)
        self.command = patcher.start()
        self.addCleanup(patcher.stop)

    @staticmethod
    def result(code=0, stdout='', stderr=''):
        return subprocess.CompletedProcess([], code, stdout, stderr)

    def run_command(self, args, **kwargs):
        self.assertNotIn('shell', kwargs)
        if args[0] == 'make':
            return self.overrides.get('make', self.result())
        self.assertEqual(args[0], '/bin/launchctl')
        action = args[1]
        if action in self.overrides:
            return self.overrides[action]
        if action == 'print':
            label = args[2].rsplit('/', 1)[-1]
            if label in self.jobs:
                return self.result(stdout=self.jobs[label])
            return self.result(113, stderr='Could not find service "' + label + '" in domain for user gui: 501')
        if action == 'bootstrap':
            if self.bootstrap_info is not None:
                self.jobs[service.LABEL] = self.bootstrap_info
            return self.result()
        if action == 'bootout':
            self.jobs.pop(service.LABEL, None)
            return self.result()
        self.fail('Unexpected command: ' + repr(args))

    def test_start_uses_selected_config_and_safe_plist_paths(self):
        result = service.start(self.config)
        self.assertIn('running (PID 123)', result)
        with service._plist_path().open('rb') as stream:
            plist = plistlib.load(stream)
        self.assertEqual(plist['Label'], service.LABEL)
        self.assertEqual(plist['ProgramArguments'], [str(Path(sys.executable).resolve()),
                         '-u', str(self.root / 'auto_upload.py'), '--config', str(self.config.resolve())])
        self.assertEqual(plist['StandardErrorPath'], str(service.log_path()))
        self.assertTrue(plist['RunAtLoad'])
        self.assertTrue(plist['KeepAlive'])
        self.assertEqual(plist['Umask'], 0o077)
        self.assertEqual(service._plist_path().stat().st_mode & 0o777, 0o600)
        commands = [call.args[0] for call in self.command.call_args_list]
        self.assertIn(['make', '-C', str(self.root), 'build'], commands)
        self.assertIn([service.LAUNCHCTL, 'bootstrap', service._domain(),
                       str(service._plist_path())], commands)
        with (service.CACHE / 'watcher.lock').open('a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)

    def test_invalid_config_never_builds_or_installs(self):
        self.config.write_text('{}')
        with self.assertRaises(ValueError):
            service.start(self.config)
        self.command.assert_not_called()
        self.assertFalse(service._plist_path().exists())

    def test_already_loaded_requires_restart(self):
        self.jobs[service.LABEL] = 'state = spawn scheduled\n'
        with self.assertRaisesRegex(RuntimeError, 'stop first'):
            service.start(self.config)
        self.assertFalse(service._plist_path().exists())
        self.assertEqual(self.command.call_count, 1)

    def test_loaded_legacy_uploader_is_not_changed(self):
        for label in service.LEGACY_LABELS:
            with self.subTest(label=label):
                self.jobs = {label: 'state = waiting\n'}
                self.command.reset_mock()
                with self.assertRaisesRegex(RuntimeError, 'Stop it manually'):
                    service.start(self.config)
                self.assertEqual(set(self.jobs), {label})
                self.assertTrue(all(call.args[0][1] == 'print'
                                    for call in self.command.call_args_list))

    def test_existing_foreground_monitor_prevents_start(self):
        service.CACHE.mkdir()
        with (service.CACHE / 'watcher.lock').open('a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with self.assertRaisesRegex(RuntimeError, 'Another ClipBridge monitor'):
                service.start(self.config)
        self.assertFalse(service._plist_path().exists())

    def test_build_failure_does_not_install_service(self):
        self.overrides['make'] = self.result(2, stderr='compiler unavailable')
        with self.assertRaisesRegex(RuntimeError, 'compiler unavailable'):
            service.start(self.config)
        self.assertFalse(service._plist_path().exists())

    def test_bootstrap_failure_removes_new_plist(self):
        self.overrides['bootstrap'] = self.result(5, stderr='bootstrap failed')
        with self.assertRaisesRegex(RuntimeError, 'bootstrap failed'):
            service.start(self.config)
        self.assertFalse(service._plist_path().exists())

    def test_bootstrap_failure_restores_previous_plist(self):
        path = service._plist_path()
        path.parent.mkdir(parents=True)
        path.write_bytes(b'previous service configuration')
        self.overrides['bootstrap'] = self.result(5, stderr='bootstrap failed')
        with self.assertRaisesRegex(RuntimeError, 'bootstrap failed'):
            service.start(self.config)
        self.assertEqual(path.read_bytes(), b'previous service configuration')

    def test_success_without_loaded_service_is_failure(self):
        self.bootstrap_info = None
        with self.assertRaisesRegex(RuntimeError, 'not loaded'):
            service.start(self.config)
        self.assertFalse(service._plist_path().exists())

    def test_waiting_service_does_not_claim_running(self):
        self.bootstrap_info = 'state = spawn scheduled\n last exit code = 2\n'
        result = service.start(self.config)
        self.assertIn('loaded but not running', result)
        self.assertIn('Last exit code: 2', result)
        self.assertNotIn('is running', result)

    def test_status_reports_running_waiting_stopped_and_installed(self):
        self.assertEqual(service.status(), 'ClipBridge is stopped.')
        service._plist_path().parent.mkdir(parents=True)
        service._plist_path().touch()
        self.assertIn('login plist remains', service.status())
        self.jobs[service.LABEL] = 'pid = 52\n'
        self.assertIn('PID 52', service.status())
        self.jobs[service.LABEL] = 'state = waiting\n'
        self.assertIn('loaded but not running', service.status())

    def test_status_does_not_hide_inspection_errors(self):
        for code, error in [(113, 'Could not find domain'), (1, 'Permission denied')]:
            with self.subTest(error=error):
                self.overrides['print'] = self.result(code, stderr=error)
                with self.assertRaisesRegex(RuntimeError, error):
                    service.status()

    def test_status_detects_foreground_monitor_without_changing_its_lock(self):
        service.CACHE.mkdir()
        with (service.CACHE / 'watcher.lock').open('a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            self.assertIn('foreground or unmanaged ClipBridge monitor is running', service.status())
            with self.assertRaisesRegex(RuntimeError, 'Another ClipBridge monitor'):
                service._check_monitor_lock()
        self.assertEqual(service.status(), 'ClipBridge is stopped.')

    def test_status_does_not_create_cache_directory(self):
        self.assertEqual(service.status(), 'ClipBridge is stopped.')
        self.assertFalse(service.CACHE.exists())

    def test_missing_launchctl_is_actionable_error(self):
        self.command.side_effect = FileNotFoundError('launchctl missing')
        with self.assertRaisesRegex(RuntimeError, 'launchctl missing'):
            service.status()

    def test_launchctl_timeout_is_actionable_error(self):
        self.command.side_effect = subprocess.TimeoutExpired('launchctl', 15)
        with self.assertRaisesRegex(RuntimeError, 'Could not run /bin/launchctl'):
            service.status()

    def test_stop_unloads_only_own_service_removes_login_plist_and_repeats(self):
        self.jobs[service.LABEL] = 'pid = 52\n'
        self.jobs[service.LEGACY_LABELS[0]] = 'pid = 53\n'
        path = service._plist_path()
        path.parent.mkdir(parents=True)
        path.touch()
        self.assertIn('background service is stopped', service.stop())
        self.assertFalse(path.exists())
        self.assertNotIn(service.LABEL, self.jobs)
        self.assertIn(service.LEGACY_LABELS[0], self.jobs)
        self.assertIn('stopped', service.stop())

    def test_stop_failure_does_not_claim_stopped_or_remove_plist(self):
        self.jobs[service.LABEL] = 'pid = 52\n'
        self.overrides['bootout'] = self.result(1, stderr='permission denied')
        path = service._plist_path()
        path.parent.mkdir(parents=True)
        path.touch()
        with self.assertRaisesRegex(RuntimeError, 'permission denied'):
            service.stop()
        self.assertTrue(path.exists())

    def test_foreground_check_rejects_loaded_service(self):
        self.jobs[service.LABEL] = 'pid = 52\n'
        with self.assertRaisesRegex(RuntimeError, 'already loaded'):
            service.check_foreground_available()

    def test_failed_verification_unloads_partial_service_and_restores_plist(self):
        original = self.run_command
        bootstrapped = False
        failed_once = False

        def run(args, **kwargs):
            nonlocal bootstrapped, failed_once
            if args[0] == service.LAUNCHCTL and args[1] == 'bootstrap':
                bootstrapped = True
            elif bootstrapped and args[1] == 'print' and not failed_once:
                failed_once = True
                return self.result(1, stderr='temporary inspection error')
            return original(args, **kwargs)

        self.command.side_effect = run
        with self.assertRaisesRegex(RuntimeError, 'temporary inspection error'):
            service.start(self.config)
        self.assertNotIn(service.LABEL, self.jobs)
        self.assertFalse(service._plist_path().exists())

    def test_cancelled_bootstrap_unloads_partial_service_and_removes_login_plist(self):
        original = self.run_command

        def run(args, **kwargs):
            result = original(args, **kwargs)
            if args[0] == service.LAUNCHCTL and args[1] == 'bootstrap':
                raise KeyboardInterrupt
            return result

        self.command.side_effect = run
        with self.assertRaises(KeyboardInterrupt):
            service.start(self.config)
        self.assertNotIn(service.LABEL, self.jobs)
        self.assertFalse(service._plist_path().exists())

    def test_cancelled_verification_unloads_partial_service_and_restores_old_plist(self):
        path = service._plist_path()
        path.parent.mkdir(parents=True)
        previous = b'previous login service'
        path.write_bytes(previous)
        original = self.run_command
        bootstrapped = False
        cancelled = False

        def run(args, **kwargs):
            nonlocal bootstrapped, cancelled
            if args[0] == service.LAUNCHCTL and args[1] == 'bootstrap':
                bootstrapped = True
            elif bootstrapped and args[1] == 'print' and not cancelled:
                cancelled = True
                raise KeyboardInterrupt
            return original(args, **kwargs)

        self.command.side_effect = run
        with self.assertRaises(KeyboardInterrupt):
            service.start(self.config)
        self.assertNotIn(service.LABEL, self.jobs)
        self.assertEqual(path.read_bytes(), previous)

    def test_cancelled_start_reports_cleanup_failure_instead_of_only_cancellation(self):
        original = self.run_command

        def run(args, **kwargs):
            result = original(args, **kwargs)
            if args[0] == service.LAUNCHCTL and args[1] == 'bootstrap':
                raise KeyboardInterrupt
            return result

        self.command.side_effect = run
        self.overrides['bootout'] = self.result(1, stderr='permission denied')
        with self.assertRaisesRegex(RuntimeError, 'Startup cancelled.*permission denied'):
            service.start(self.config)
        self.assertIn(service.LABEL, self.jobs)
        self.assertFalse(service._plist_path().exists())


if __name__ == '__main__':
    unittest.main()
