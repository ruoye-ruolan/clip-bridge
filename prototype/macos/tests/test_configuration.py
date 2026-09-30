import json
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parents[1]))
import configuration as config


class ConfigurationTests(unittest.TestCase):
    def test_validate_host_accepts_only_literal_aliases(self):
        self.assertEqual(config.validate_host('dev-server_2.example'), 'dev-server_2.example')
        for host in ('', '-option', 'user@host', 'host;id', 'host\n', 1):
            with self.subTest(host=host), self.assertRaises(ValueError):
                config.validate_host(host)

    def test_save_load_round_trip_and_private_permissions(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'clipbridge' / 'config.json'
            config.save_config(path, 'dev-server', '/home/example/images/')
            self.assertEqual(config.load_config(path), ('dev-server', '/home/example/images'))
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
            self.assertEqual(stat.S_IMODE(path.parent.stat().st_mode), 0o700)
            self.assertEqual(list(path.parent.iterdir()), [path])

    def test_failed_atomic_replace_preserves_existing_configuration(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'config.json'
            config.save_config(path, 'original', '/tmp/images')
            before = path.read_bytes()
            with patch.object(config.os, 'replace', side_effect=OSError('disk error')):
                with self.assertRaises(OSError):
                    config.save_config(path, 'replacement', '/tmp/new')
            self.assertEqual(path.read_bytes(), before)
            self.assertEqual(list(path.parent.iterdir()), [path])

    def test_save_preserves_existing_parent_mode_and_tightens_file_mode(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            root.chmod(0o755)
            path = root / 'config.json'
            path.write_text('{}')
            path.chmod(0o644)
            config.save_config(path, 'dev-server', '/tmp/images')
            self.assertEqual(stat.S_IMODE(root.stat().st_mode), 0o755)
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)

    def test_save_rejects_symlink_file_and_parent(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            original = root / 'original.json'
            original.write_text('keep me')
            linked = root / 'linked.json'
            linked.symlink_to(original)
            linked_parent = root / 'linked-parent'
            linked_parent.symlink_to(root, target_is_directory=True)
            for path in (linked, linked_parent / 'config.json'):
                with self.subTest(path=path), self.assertRaises(ValueError):
                    config.save_config(path, 'dev-server', '/tmp/images')
            self.assertEqual(original.read_text(), 'keep me')

    def test_invalid_values_never_create_configuration_or_run_ssh(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(config.subprocess, 'run') as run:
            path = Path(directory) / 'config.json'
            for host, remote in [('-option', '/tmp/images'), ('user@host', '/tmp/images'),
                                 ('host', '/tmp/../images'), ('host', '/tmp/$(id)'),
                                 ('host', '~/images'), ('host', '/tmp/a b'),
                                 ('host', '/tmp/images\n'), (None, '/tmp/images')]:
                with self.subTest(host=host, remote=remote):
                    with self.assertRaises(ValueError):
                        config.save_config(path, host, remote)
                    with self.assertRaises(ValueError):
                        config.check_destination(host, remote)
            self.assertFalse(path.exists())
            run.assert_not_called()

    def test_load_requires_json_object(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'config.json'
            for value in ([], {}, {'ssh_host': 'dev-server'}):
                path.write_text(json.dumps(value))
                with self.subTest(value=value), self.assertRaises(ValueError):
                    config.load_config(path)


class AliasDiscoveryTests(unittest.TestCase):
    def test_literal_aliases_include_globs_and_loop_detection(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'config'
            fragments = root / 'conf.d'
            fragments.mkdir()
            source.write_text('Host alpha beta *.example !hidden\nInclude conf.d/*\nHost=gamma\n')
            (fragments / 'one').write_text('Host "delta" alpha\nInclude config\n')
            (fragments / 'two').write_text('Match exec "never-execute-this"\nInclude conditional\n')
            (root / 'conditional').write_text('Host conditional-host\n')
            with patch.object(config.subprocess, 'run') as run:
                self.assertEqual(config.discover_ssh_aliases(source),
                                 ['alpha', 'beta', 'conditional-host', 'delta', 'gamma'])
                run.assert_not_called()

    def test_missing_config_and_invalid_lines_are_harmless(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'config'
            self.assertEqual(config.discover_ssh_aliases(path), [])
            path.write_text('Host "unterminated\nHost valid # comment\nHost bad:alias\n')
            self.assertEqual(config.discover_ssh_aliases(path), ['valid'])


class DestinationTests(unittest.TestCase):
    def completed(self, stdout=''):
        return subprocess.CompletedProcess([], 0, stdout, '')

    def test_default_directory_uses_remote_home_and_checks_write_access(self):
        with patch.object(config.subprocess, 'run', side_effect=[self.completed('/home/example\n'),
                                                               self.completed()]) as run:
            result = config.check_destination('dev-server')
        self.assertEqual(result, '/home/example/.local/share/clipbridge/images')
        self.assertEqual(run.call_count, 2)
        for call in run.call_args_list:
            argv = call.args[0]
            self.assertEqual(argv[0], '/usr/bin/ssh')
            self.assertIn('BatchMode=yes', argv)
            self.assertIn('StrictHostKeyChecking=yes', argv)
            self.assertEqual(argv[-2], 'dev-server')
            self.assertTrue(call.kwargs['check'])
            self.assertIn('timeout', call.kwargs)
            self.assertNotIn('shell', call.kwargs)
        command = run.call_args.args[0][-1]
        self.assertIn('umask 077', command)
        self.assertIn('mkdir -p', command)
        self.assertIn('mktemp', command)
        self.assertIn('trap', command)

    def test_explicit_directory_does_not_query_remote_home(self):
        with patch.object(config.subprocess, 'run', return_value=self.completed()) as run:
            self.assertEqual(config.check_destination('dev-server', '/srv/images/'), '/srv/images')
        self.assertEqual(run.call_count, 1)

    def test_read_only_check_never_creates_directory_or_probe(self):
        with patch.object(config.subprocess, 'run', return_value=self.completed()) as run:
            self.assertEqual(config.check_destination('dev-server', '/srv/images', create=False),
                             '/srv/images')
        command = run.call_args.args[0][-1]
        self.assertIn('test -d /srv/images', command)
        self.assertIn('test -w /srv/images', command)
        self.assertIn('test -x /srv/images', command)
        self.assertNotIn('mkdir', command)
        self.assertNotIn('mktemp', command)

    def test_unexpected_remote_home_is_rejected_before_directory_mutation(self):
        for home in ('', '/tmp/$(id)\n', '/home/example\nbanner\n', 'relative\n'):
            with self.subTest(home=home), patch.object(config.subprocess, 'run',
                                                      return_value=self.completed(home)) as run:
                with self.assertRaises(ValueError):
                    config.check_destination('dev-server')
                self.assertEqual(run.call_count, 1)

    def test_network_or_host_key_failures_propagate_without_saving(self):
        for error in (subprocess.CalledProcessError(255, ['ssh']),
                      subprocess.TimeoutExpired(['ssh'], 10)):
            with self.subTest(error=error), patch.object(config.subprocess, 'run', side_effect=error), \
                    patch.object(config, 'save_config') as save:
                with self.assertRaises(type(error)):
                    config.check_destination('dev-server', '/tmp/images')
                save.assert_not_called()


if __name__ == '__main__':
    unittest.main()
