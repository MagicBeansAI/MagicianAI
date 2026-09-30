#!/usr/bin/env python3
"""Fail image qualification on missing or wrong-platform tool executables.

Inventory only: this does not invoke providers or qualify governed dispatch.
Requires the image's PyYAML; run as the final non-root image user.
"""
import argparse
import json
import os
from pathlib import Path
import platform
import shlex
import shutil
import sys

import yaml

HOST_SKILLS = {'macos-ui-automation', 'screen-draw'}
ARCHES = {'arm64': 183, 'aarch64': 183, 'amd64': 62, 'x86_64': 62}


def executable_problem(path, search_path, machine):
    if not path or not Path(path).is_file() or not os.access(path, os.X_OK):
        return 'missing or not executable'
    try:
        with Path(path).open('rb') as source:
            header = source.read(256)
    except OSError as error:
        return 'cannot inspect executable: ' + str(error)
    if header.startswith(b'\x7fELF'):
        if len(header) < 20 or header[4:6] != b'\x02\x01' or int.from_bytes(header[18:20], 'little') != ARCHES[machine]:
            return 'wrong Linux architecture'
    elif header.startswith(b'#!'):
        try:
            command = shlex.split(header.splitlines()[0][2:].decode())
        except (ValueError, UnicodeError):
            return 'invalid interpreter declaration'
        if command and command[0] == '/usr/bin/env':
            command = command[1:]
            if command and command[0] == '-S':
                command = command[1:]
        if not command or not shutil.which(command[0], path=search_path):
            return 'script interpreter missing'
    else:
        return 'neither a Linux ELF binary nor an executable script'
    return None


def inventory(root, machine, system_path):
    if machine not in ARCHES:
        raise ValueError('unsupported Linux architecture: ' + machine)
    rows = []
    for manifest in sorted(root.glob('*/SKILL.md')):
        text = manifest.read_text()
        if not text.startswith('---\n'):
            continue
        document = yaml.safe_load(text.split('---', 2)[1]) or {}
        metadata = (document.get('metadata') or {}).get('magician') or {}
        contract = metadata.get('runtime_contract') or {}
        if not contract and not metadata.get('runtime_actions'):
            continue
        name = manifest.parent.name
        row = {'skill': name, 'binaries': {}}
        rows.append(row)
        if name in HOST_SKILLS:
            row['status'] = 'host_only'
            continue
        search = os.pathsep.join(map(str, [manifest.parent / 'bin', root / '.node/bin',
                                         root / '.venv/bin', root / 'node_modules/.bin', system_path]))
        bins = sorted(set((contract.get('requires') or {}).get('bins') or []) |
                      set((metadata.get('requires') or {}).get('bins') or []))
        for binary in bins:
            resolved = shutil.which(binary, path=search)
            problem = executable_problem(resolved, search, machine)
            row['binaries'][binary] = {'path': resolved, 'problem': problem}
        row['status'] = ('fail' if any(v['problem'] for v in row['binaries'].values()) else
                         'executables_present' if bins else 'no_local_binary')
    if not rows:
        raise ValueError('no runtime skill manifests found')
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path('/app/skillshub'))
    args = parser.parse_args()
    if platform.system() != 'Linux':
        parser.error('run inside the target Linux image')
    rows = inventory(args.root, platform.machine(), os.environ.get('PATH', '/usr/bin:/bin'))
    failed = [r['skill'] for r in rows if r['status'] == 'fail']
    print(json.dumps({'schema': 'magician.container-skill-bins.v1', 'uid': os.getuid(),
                      'functional_execution_tested': False, 'failed_skills': failed, 'skills': rows}, indent=2))
    return 1 if failed else 0


if __name__ == '__main__':
    raise SystemExit(main())
