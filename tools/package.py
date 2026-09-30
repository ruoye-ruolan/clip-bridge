#!/usr/bin/env python3
"""Build an allowlisted source archive without personal config or generated files."""
import hashlib
import os
from pathlib import Path
import sys
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / 'src'))
from installer import payload_files
from runtime import version


def build_package(root=ROOT):
    root = Path(root).resolve()
    name = 'clipbridge-' + version(root)
    files = set(payload_files(root))
    files.update(Path(name) for name in ('AGENTS.md', 'Makefile', 'tools/package.py'))
    for pattern in ('docs/**/*.md', 'src/tests/*.py', 'src/tests/*.swift'):
        files.update(path.relative_to(root) for path in root.glob(pattern))
    for relative in files:
        if (root / relative).is_symlink() or not (root / relative).is_file():
            raise RuntimeError('Missing or symlinked package file: ' + str(relative))
    destination = root / 'dist'
    destination.mkdir(exist_ok=True)
    archive = destination / (name + '.tar.gz')
    descriptor, temporary = tempfile.mkstemp(prefix='.package-', dir=destination)
    os.close(descriptor)
    try:
        with tarfile.open(temporary, 'w:gz') as stream:
            for relative in sorted(files):
                info = stream.gettarinfo(str(root / relative), arcname=str(Path(name) / relative))
                info.uid = info.gid = 0
                info.uname = info.gname = ''
                info.mode = 0o755 if str(relative) in ('clipbridge', 'install.sh') else 0o644
                with (root / relative).open('rb') as contents:
                    stream.addfile(info, contents)
        Path(temporary).replace(archive)
    finally:
        Path(temporary).unlink(missing_ok=True)
    checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
    archive.with_name(archive.name + '.sha256').write_text(checksum + '  ' + archive.name + '\n')
    return archive


if __name__ == '__main__':
    print(build_package())
