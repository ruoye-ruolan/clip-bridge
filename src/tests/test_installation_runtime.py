"""Real compilation and installed command execution, with service operations isolated."""
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parents[1]))
import installer
import runtime
import service


class SourcePackageTests(unittest.TestCase):
    def test_archive_excludes_personal_config_and_generated_files(self):
        actual = Path(__file__).resolve().parents[2]
        spec = importlib.util.spec_from_file_location('clipbridge_package', actual / 'tools/package.py')
        package = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(package)
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory)
            for relative in installer.payload_files(actual) + [Path('AGENTS.md'), Path('Makefile'), Path('tools/package.py')]:
                target = source / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes((actual / relative).read_bytes())
            (source / 'src/config.json').write_text('PRIVATE CONFIG')
            (source / '.env').write_text('PRIVATE ENV')
            (source / 'src/build').mkdir()
            (source / 'src/build/clipboard-watch').write_text('OLD BINARY')
            archive = package.build_package(source)
            with tarfile.open(archive) as stream:
                names = stream.getnames()
                prefix = 'clipbridge-' + runtime.version(source) + '/'
                self.assertIn(prefix + 'install.sh', names)
                self.assertIn(prefix + 'src/installer.py', names)
                self.assertNotIn(prefix + 'src/config.json', names)
                self.assertNotIn(prefix + '.env', names)
                self.assertFalse(any('/build/' in name for name in names))
            self.assertTrue(archive.with_name(archive.name + '.sha256').is_file())


@unittest.skipUnless(sys.platform == 'darwin' and shutil.which('swiftc') and shutil.which('make'),
                     'Requires macOS Swift build tools')
class InstalledRuntimeTests(unittest.TestCase):
    def test_real_build_upgrade_run_without_checkout_and_uninstall(self):
        with tempfile.TemporaryDirectory(prefix='clipbridge runtime & ') as directory:
            home = Path(directory).resolve()
            source = home / 'source'
            actual = Path(__file__).resolve().parents[2]
            for relative in installer.payload_files(actual):
                target = source / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes((actual / relative).read_bytes())
            prefix = home / '.local'
            root = prefix / 'share/clipbridge'
            command = prefix / 'bin/clipbridge'
            config = home / '.config/clipbridge/config.json'
            env = dict(os.environ, HOME=str(home))
            with patch.object(service, 'HOME', home), patch.object(service, 'CACHE', home / 'cache'), \
                    patch.object(service, '_service_info', return_value=None), \
                    patch.object(service, '_monitor_running', return_value=False), \
                    patch.object(service, '_check_legacy_services'), \
                    patch.object(service, '_install_service') as activate, \
                    patch.object(installer.configuration, 'DEFAULT_CONFIG', config):
                installer.install(source, prefix=prefix)
                first = (root / 'current').resolve()
                config.write_text('{"ssh_host":"mine","remote_directory":"/tmp/mine"}')
                saved = config.read_bytes()
                # Upgrade from a changed local source, without restarting a stopped service.
                (source / 'VERSION').write_text('0.1.1\n')
                installer.install(source, prefix=prefix, upgrade=True)
                self.assertEqual((root / 'previous').resolve(), first)
                self.assertEqual(config.read_bytes(), saved)
                shutil.rmtree(source)
                result = subprocess.run([str(command), '--version'], cwd=home, env=env,
                                        capture_output=True, text=True, check=True, timeout=15)
                self.assertEqual(result.stdout.strip(), 'ClipBridge 0.1.1')
                result = subprocess.run([str(command), '--help'], cwd=home, env=env,
                                        capture_output=True, text=True, check=True, timeout=15)
                self.assertIn('uninstall', result.stdout)
                self.assertIn('upgrade', result.stdout)
                # Installed start/run should never require make: verify the shared builder.
                with patch.object(service, 'ROOT', (root / 'current/src').resolve()), \
                        patch.object(service, '_run') as build:
                    service._build_helpers()
                    build.assert_not_called()
                activate.assert_not_called()
                # Execute uninstall through installed modules after deleting the source tree.
                script = ('import sys; sys.path.insert(0, sys.argv[1]); import service, cli; '
                          'service._service_info=lambda: None; service._monitor_running=lambda: False; '
                          'raise SystemExit(cli.main(["uninstall"]))')
                result = subprocess.run([sys.executable, '-c', script, str((root / 'current/src').resolve())],
                                        cwd=home, env=env, capture_output=True, text=True, check=True, timeout=15)
                self.assertIn('uninstalled', result.stdout)
                self.assertFalse(root.exists())
                self.assertFalse(command.exists())
                self.assertEqual(config.read_bytes(), saved)


if __name__ == '__main__':
    unittest.main()
