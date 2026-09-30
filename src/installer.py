"""User-level, transactional installation from a trusted local source tree."""
from hashlib import sha256
import json
import os
from pathlib import Path
import plistlib
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import uuid

import configuration
import runtime
import service

RUNTIME_MODULES = ('auto_upload.py', 'cli.py', 'configuration.py', 'installer.py',
                   'monitor_control.py', 'runtime.py', 'service.py')


def payload_files(source):
    """Explicit allowlist: never copy local config, logs, caches or old binaries."""
    return [Path(name) for name in ('clipbridge', 'install.sh', 'VERSION', 'LICENSE', 'README.md')] + [
        Path('src') / name for name in RUNTIME_MODULES
    ] + [Path(name) for name in ('src/Makefile', 'src/README.md', 'src/config.example.json',
                                 'src/swift/clipboard-watch.swift', 'src/swift/clipboard-path.swift')]


def _locations(prefix=None):
    if prefix is None:
        installed = runtime.installation_root()
        prefix = installed.parent.parent if installed else Path.home() / '.local'
    prefix = Path(prefix).expanduser().resolve()
    if prefix == Path('/'):
        raise ValueError('Choose a user installation prefix, not the filesystem root')
    return prefix, prefix / 'share/clipbridge', prefix / 'bin/clipbridge'


def _read_marker(root):
    if root.is_symlink():
        raise RuntimeError('Refusing a symlink installation directory')
    if not root.exists():
        return None
    path = root / runtime.INSTALL_MARKER
    try:
        if path.is_symlink():
            raise ValueError('symlink marker')
        marker = json.loads(path.read_text())
        if marker.get('product') != 'clipbridge' or marker.get('schema') != 1:
            raise ValueError('unknown product/schema')
    except (OSError, ValueError, AttributeError) as error:
        raise RuntimeError('Installation directory is unmanaged, not owned by ClipBridge: ' + str(root)) from error
    releases = root / 'releases'
    if releases.is_symlink() or not releases.is_dir():
        raise RuntimeError('Invalid managed releases directory')
    for name in ('current', 'previous'):
        _link_target(root / name)
    return marker


def _link_target(path):
    if not path.exists() and not path.is_symlink():
        return None
    if not path.is_symlink():
        raise RuntimeError('Expected a managed release link: ' + str(path))
    target = Path(os.readlink(path))
    if (target.is_absolute() or len(target.parts) != 2 or target.parts[0] != 'releases'
            or target.parts[1] in ('.', '..')):
        raise RuntimeError('Release link escapes the installation: ' + str(path))
    release = path.parent / target
    if release.is_symlink() or not release.is_dir():
        raise RuntimeError('Invalid release directory: ' + str(release))
    try:
        marker = json.loads((release / runtime.RELEASE_MARKER).read_text())
        if marker.get('product') != 'clipbridge' or marker.get('schema') != 1:
            raise ValueError('unknown release')
    except (OSError, ValueError, AttributeError) as error:
        raise RuntimeError('Release is not owned by ClipBridge: ' + str(release)) from error
    return str(target)


def _check_launcher(launcher, marker):
    if launcher.is_symlink():
        raise RuntimeError('Existing command is not owned by this installer: ' + str(launcher))
    if launcher.exists():
        if not launcher.is_file() or not marker or sha256(launcher.read_bytes()).hexdigest() != marker.get('launcher_sha256'):
            raise RuntimeError('Existing command is unmanaged or modified; it will not be overwritten: ' + str(launcher))
    if launcher.parent.is_symlink():
        raise RuntimeError('Command directory must not be a symlink: ' + str(launcher.parent))


def _write(path, content, mode=0o600):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as stream:
            temporary = Path(stream.name)
            os.fchmod(stream.fileno(), mode)
            stream.write(content)
        temporary.replace(path)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def _link(path, target):
    if target is None:
        path.unlink(missing_ok=True)
        return
    temporary = path.with_name('.' + path.name + '-' + uuid.uuid4().hex)
    try:
        temporary.symlink_to(target)
        temporary.replace(path)
    finally:
        temporary.unlink(missing_ok=True)


def _prepare_release(source, stage):
    for relative in payload_files(source):
        original = source / relative
        if original.is_symlink() or not original.is_file():
            raise RuntimeError('Missing or symlinked source file: ' + str(relative))
        target = stage / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(original, target)
    for name in ('clipbridge', 'install.sh'):
        (stage / name).chmod(0o755)
    # Documentation ships with the installed program as a local reference.
    for relative in (Path('AGENTS.md'), *sorted(source.glob('docs/**/*.md'))):
        relative = relative.relative_to(source) if relative.is_absolute() else relative
        original = source / relative
        if original.is_file() and not original.is_symlink():
            target = stage / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(original, target)
    print('Building native clipboard helpers...', flush=True)
    subprocess.run(['make', '-C', str(stage / 'src'), 'build'],
                   capture_output=True, text=True, check=True, timeout=180)
    runtime.require_helpers(stage / 'src')
    subprocess.run([sys.executable, str(stage / 'clipbridge'), '--version'],
                   capture_output=True, text=True, check=True, timeout=20)
    subprocess.run([sys.executable, str(stage / 'clipbridge'), '--help'],
                   capture_output=True, text=True, check=True, timeout=20)


def _service_snapshot(source, root):
    info = service._service_info()
    path = service._plist_path()
    data = path.read_bytes() if path.exists() else None
    if info is not None and data is None:
        raise RuntimeError('Loaded service has no readable plist; stop it before installing')
    config = None
    if data is not None:
        try:
            plist = plistlib.loads(data)
            args = plist['ProgramArguments']
            script = Path(args[args.index('-u') + 1]).resolve()
            config = Path(args[args.index('--config') + 1]).expanduser().resolve()
            owned = script == (source / 'src/auto_upload.py').resolve() if source else False
            if root.exists() and script.is_relative_to(root.resolve()):
                owned = True
            if not owned:
                raise ValueError('another checkout is in use')
            if info is not None:
                configuration.load_config(config)
        except (KeyError, ValueError, IndexError, TypeError, OSError, plistlib.InvalidFileException) as error:
            raise RuntimeError('Service is not owned by this installation/source, or its configuration is invalid; '
                               'stop it before continuing: ' + str(error)) from error
    if info is None and service._monitor_running():
        raise RuntimeError('Stop foreground monitoring with Ctrl+C before installing, upgrading or uninstalling')
    return data, info is not None, config


def _restore_service(data, loaded):
    if service._service_info() is not None:
        service._stop()
    path = service._plist_path()
    if data is None:
        path.unlink(missing_ok=True)
        return
    service._atomic_write(path, data)
    if loaded:
        result = service._run([service.LAUNCHCTL, 'bootstrap', service._domain(), str(path)])
        if result.returncode or service._service_info() is None:
            raise RuntimeError('Could not restore the previous background service: ' + service._details(result))


def _wait_healthy(config_path, timeout=10):
    """Require the new process to register itself and hold the upload lock."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        info = service._service_info() or ''
        pid = re.search(r'^\s*pid = (\d+)\s*$', info, re.M)
        try:
            state = json.loads((service.CACHE / 'monitor.json').read_text())
        except (OSError, ValueError):
            state = {}
        if (pid and state.get('pid') == int(pid.group(1))
                and state.get('config') == str(config_path.resolve())
                and service._monitor_running()):
            return
        time.sleep(0.1)
    raise RuntimeError('Installed monitor did not become ready; see ' + str(service.log_path()))


def _persistent_config(source, root, selected):
    default = configuration.DEFAULT_CONFIG
    if selected is not None:
        # A source-local config must survive deletion of the source/install payload.
        if selected.is_relative_to(source) or selected.is_relative_to(root):
            target = default if not default.exists() else default.with_name('migrated-' + uuid.uuid4().hex[:8] + '.json')
            configuration.save_config(target, *configuration.load_config(selected))
            return target.resolve()
        return selected
    if not default.exists():
        legacy = source / 'src/config.json'
        origin = legacy if legacy.is_file() else source / 'src/config.example.json'
        configuration.save_config(default, *configuration.load_config(origin))
    return default


def install(source, *, prefix=None, upgrade=False):
    if sys.platform != 'darwin':
        raise RuntimeError('Installation currently requires macOS')
    if os.geteuid() == 0:
        raise RuntimeError('Run the installer as your normal user, without sudo')
    source = Path(source).expanduser().resolve(strict=True)
    version = runtime.version(source)
    prefix, root, launcher = _locations(prefix)
    with service._operation_lock():
        marker = _read_marker(root)
        if upgrade and marker is None:
            raise RuntimeError('ClipBridge is not installed at ' + str(root) + '; run ./install.sh first')
        _check_launcher(launcher, marker)
        service._check_legacy_services()
        root.parent.mkdir(parents=True, exist_ok=True)
        stage = Path(tempfile.mkdtemp(prefix='.clipbridge-stage-', dir=root.parent))
        release = None
        committed = False
        rollback_ok = True
        try:
            _prepare_release(source, stage)
            release_id = version + '-' + uuid.uuid4().hex[:12]
            _write(stage / runtime.RELEASE_MARKER, json.dumps({
                'product': 'clipbridge', 'schema': 1, 'version': version,
                'python': str(Path(sys.executable).resolve()),
            }).encode())
            old_plist, was_loaded, selected = _service_snapshot(source, root)
            selected = _persistent_config(source, root, selected)
            old_launcher = launcher.read_bytes() if launcher.exists() else None
            marker_path = root / runtime.INSTALL_MARKER
            old_marker = marker_path.read_bytes() if marker else None
            old_current = _link_target(root / 'current') if marker else None
            old_previous = _link_target(root / 'previous') if marker else None
            interpreter = str(Path(sys.executable).resolve())
            launcher_bytes = ('#!/bin/sh\n# Managed by ClipBridge installer v1\nexec '
                              + shlex.quote(interpreter) + ' '
                              + shlex.quote(str(root / 'current/clipbridge')) + ' "$@"\n').encode()
            service_changed = False
            try:
                (root / 'releases').mkdir(parents=True, exist_ok=True, mode=0o700)
                release = root / 'releases' / release_id
                stage.rename(release)
                if was_loaded:
                    service_changed = True
                    service._stop()
                    service._check_monitor_lock()
                _link(root / 'current', 'releases/' + release_id)
                _link(root / 'previous', old_current)
                _write(launcher, launcher_bytes, mode=0o755)
                _write(marker_path, json.dumps({'product': 'clipbridge', 'schema': 1,
                       'launcher_sha256': sha256(launcher_bytes).hexdigest()}).encode())
                if was_loaded:
                    service._install_service(selected, source_root=root / 'current/src',
                                             python_executable=interpreter)
                    _wait_healthy(selected)
                elif old_plist is not None:
                    service_changed = True
                    plist = plistlib.loads(old_plist)
                    plist['ProgramArguments'] = [interpreter, '-u', str(root / 'current/src/auto_upload.py'),
                                                 '--config', str(selected)]
                    plist['WorkingDirectory'] = str(root / 'current/src')
                    service._atomic_write(service._plist_path(), plistlib.dumps(plist))
                committed = True
            except BaseException as error:
                errors = []
                try:
                    if service_changed and service._service_info() is not None:
                        service._stop()
                    _link(root / 'current', old_current)
                    _link(root / 'previous', old_previous)
                    if old_launcher is None:
                        launcher.unlink(missing_ok=True)
                    else:
                        _write(launcher, old_launcher, mode=0o755)
                    if old_marker is None:
                        marker_path.unlink(missing_ok=True)
                    else:
                        _write(marker_path, old_marker)
                    if service_changed:
                        _restore_service(old_plist, was_loaded)
                except BaseException as cleanup:
                    rollback_ok = False
                    errors.append(str(cleanup) or type(cleanup).__name__)
                if errors:
                    raise RuntimeError('Installation failed and recovery needs attention: '
                                       + '; '.join(errors) + '. Preserved files at ' + str(root)) from error
                raise
            # Keep the current and preceding release; remove only marked older payloads.
            keep = {release.name}
            if old_current:
                keep.add(Path(old_current).name)
            for entry in (root / 'releases').iterdir():
                if entry.name not in keep and _owned_release(entry):
                    try:
                        shutil.rmtree(entry)
                    except OSError as error:
                        print(f'Installed successfully; old release cleanup needs attention: {entry}: {error}')
            command = str(launcher)
            path_hint = '' if str(launcher.parent) in os.environ.get('PATH', '').split(os.pathsep) else (
                '\nAdd this directory to your shell PATH: ' + str(launcher.parent))
            state = 'Background service restored with the new version.' if was_loaded else 'Monitoring was not started.'
            return (f'Installed ClipBridge {version} at {root}\nCommand: {command}\nConfiguration: {selected}\n'
                    + state + '\nEdit configuration before first use; run clipbridge start when ready.' + path_hint)
        finally:
            if stage.exists():
                shutil.rmtree(stage)
            if not committed and rollback_ok and release is not None:
                if release.exists():
                    shutil.rmtree(release)
                if marker is None and root.exists():
                    # Only our just-created directory is removed, never an old installation.
                    shutil.rmtree(root)


def _owned_release(path):
    if path.is_symlink() or not path.is_dir():
        return False
    try:
        marker = json.loads((path / runtime.RELEASE_MARKER).read_text())
        return marker.get('product') == 'clipbridge' and marker.get('schema') == 1
    except (OSError, ValueError, AttributeError):
        return False


def uninstall(*, prefix=None):
    _, root, launcher = _locations(prefix)
    with service._operation_lock():
        marker = _read_marker(root)
        if marker is None:
            return 'ClipBridge is not installed at ' + str(root)
        _check_launcher(launcher, marker)
        allowed = {'releases', 'current', 'previous', runtime.INSTALL_MARKER}
        if any(entry.name not in allowed for entry in root.iterdir()):
            raise RuntimeError('Unrecognized files in installation directory; preserved ' + str(root))
        for entry in (root / 'releases').iterdir():
            if not _owned_release(entry):
                raise RuntimeError('Unmanaged release data will not be deleted: ' + str(entry))
        data, loaded, selected = _service_snapshot(None, root)
        preserved = None
        if selected is not None and selected.is_relative_to(root):
            preserved = _persistent_config(root, root, selected)
        if loaded:
            service._stop()
            service._check_monitor_lock()
        elif data is not None:
            service._plist_path().unlink(missing_ok=True)
        launcher.unlink(missing_ok=True)
        shutil.rmtree(root)
    result = 'ClipBridge uninstalled. Configuration, logs, retained images and remote uploads were preserved.'
    return result + ('\nMoved active configuration to: ' + str(preserved) if preserved else '')
