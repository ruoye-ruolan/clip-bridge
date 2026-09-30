"""Identify source and installed runtimes without depending on the checkout."""
import json
import os
from pathlib import Path
import re

PROJECT = Path(__file__).resolve().parent.parent
RELEASE_MARKER = '.clipbridge-release.json'
INSTALL_MARKER = '.clipbridge-install.json'
HELPERS = ('clipboard-watch', 'clipboard-path')


def version(project=None):
    value = ((Path(project) if project is not None else PROJECT) / 'VERSION').read_text().strip()
    if not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.-]+)?', value):
        raise ValueError('VERSION must contain a semantic version')
    return value


def is_installed(source_root=None):
    root = Path(source_root) if source_root is not None else PROJECT / 'src'
    return (root.parent / RELEASE_MARKER).is_file()


def installation_root():
    if is_installed() and PROJECT.parent.name == 'releases':
        root = PROJECT.parent.parent
        with (root / INSTALL_MARKER).open(encoding='utf-8') as stream:
            marker = json.load(stream)
        if marker.get('product') == 'clipbridge' and marker.get('schema') == 1:
            return root
    return None


def require_helpers(source_root=None):
    root = Path(source_root) if source_root is not None else PROJECT / 'src'
    missing = [name for name in HELPERS if not (root / 'build' / name).is_file()
               or not os.access(root / 'build' / name, os.X_OK)]
    if missing:
        raise RuntimeError('Installed helpers are missing: ' + ', '.join(missing)
                           + '. Reinstall with ./install.sh from a source checkout or extracted package.')
