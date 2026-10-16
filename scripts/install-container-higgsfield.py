#!/usr/bin/env python3
"""Install the pinned Linux Higgsfield executable, without login or credentials."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parent.parent


def unpack(archive, destination, checksum, arch):
    digest = hashlib.sha256()
    with archive.open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(chunk)
    if digest.hexdigest() != checksum:
        raise ValueError('Higgsfield release checksum mismatch')
    with tarfile.open(archive, 'r:gz') as source:
        matches = [m for m in source if m.name in ('hf', './hf')]
        if len(matches) != 1 or not matches[0].isfile() or not 20 <= matches[0].size <= 128 * 1024 * 1024:
            raise ValueError('expected one bounded regular hf executable')
        with source.extractfile(matches[0]) as binary:
            header = binary.read(20)
            if header[:6] != b'\x7fELF\x02\x01' or int.from_bytes(header[18:20], 'little') != {'arm64': 183, 'amd64': 62}[arch]:
                raise ValueError('Higgsfield executable has the wrong Linux architecture')
            with destination.open('wb') as out:
                out.write(header)
                shutil.copyfileobj(binary, out)
    destination.chmod(0o755)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=ROOT / 'skillshub')
    parser.add_argument('--release', type=Path, default=ROOT / 'containers/tools/higgsfield-release.json')
    parser.add_argument('--archive', type=Path, help='use a predownloaded archive; the pinned checksum still applies')
    args = parser.parse_args()
    machine = platform.machine()
    if platform.system() != 'Linux' or machine not in ('aarch64', 'arm64', 'x86_64', 'amd64'):
        parser.error('requires Linux ARM64 or AMD64')
    arch = 'arm64' if machine in ('aarch64', 'arm64') else 'amd64'
    release = json.loads(args.release.read_text())
    asset = release['assets'][arch]
    url = 'https://github.com/{}/releases/download/v{}/{}'.format(release['repository'], release['version'], asset['name'])
    target = args.root / 'higgsfield/bin/higgsfield'
    target.parent.mkdir(parents=True, exist_ok=True)
    curl = shutil.which('curl')
    if not curl and not args.archive:
        raise ValueError('curl is required')
    with tempfile.TemporaryDirectory(prefix='.higgsfield-install-', dir=target.parent) as scratch:
        scratch = Path(scratch)
        archive, binary = args.archive or scratch / 'release.tar.gz', scratch / 'higgsfield'
        if not args.archive:
            subprocess.run([curl, '--fail', '--location', '--silent', '--show-error', '--max-time', '120',
                            '--max-filesize', str(128 * 1024 * 1024), '--output', str(archive), url], check=True, timeout=130)
        unpack(archive, binary, asset['sha256'], arch)
        # The version probe gets a temporary, empty HOME; it cannot discover a
        # developer login, update existing credentials, or bill a provider.
        result = subprocess.run([str(binary), 'version'], check=True, capture_output=True, text=True,
                                timeout=15, env={'HOME': str(scratch), 'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8'})
        if release['version'] not in result.stdout + result.stderr:
            raise ValueError('Higgsfield version does not match the pinned release')
        os.replace(binary, target)
    print('Installed Higgsfield {} for Linux {}; authentication is separate'.format(release['version'], arch))


if __name__ == '__main__':
    main()
