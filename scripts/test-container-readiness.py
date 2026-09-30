#!/usr/bin/env python3
"""Offline regressions for false-ready DNS, missing tools and release integrity.

No containers, compilers, providers or system configuration changes. Requires
PyYAML, like the Linux skill inventory; fixtures never execute synthetic ELF.
"""
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tarfile
import tempfile
import unittest

HERE = Path(__file__).resolve().parent


def module(name):
    spec = importlib.util.spec_from_file_location(name, HERE / (name + '.py'))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


dns = module('check-apple-container-host')
bins = module('verify-container-skill-bins')
hf = module('install-container-higgsfield')


def elf(arch=183):
    header = bytearray(64)
    header[:6] = b'\x7fELF\x02\x01'
    header[18:20] = arch.to_bytes(2, 'little')
    return bytes(header)


def executable(path, body):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(body)
    path.chmod(0o755)
    return path


class Scratch(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name)


class HostForwardingTests(Scratch):
    def resolver(self, suffix):
        path = self.root / 'resolver'
        path.write_text('domain host.container.internal\nnameserver 127.0.0.1\n' + suffix)
        return path

    def test_domain_without_forwarding_is_not_ready(self):
        result = dns.check_resolver(self.resolver('port 2053\n'))
        self.assertFalse(result['configured'])
        self.assertFalse(result['transport_verified'])

    def test_configured_is_not_transport_qualification(self):
        result = dns.check_resolver(self.resolver('port 1053\noptions localhost:203.0.113.113\n'))
        self.assertTrue(result['configured'])
        self.assertEqual(result['alias_address'], '203.0.113.113')
        self.assertFalse(result['transport_verified'])

    def test_missing_ambiguous_and_invalid_bindings_fail(self):
        for suffix in ('port 1053\n', 'port 1053\noptions localhost:127.0.0.1\n',
                       'port 1053\noptions localhost:203.0.113.113 localhost:203.0.113.114\n',
                       'port 1053\noptions localhost:300.0.0.1\n', '#' * 4097):
            with self.subTest(suffix=suffix[:100]):
                self.assertFalse(dns.check_resolver(self.resolver(suffix))['configured'])
        self.assertFalse(dns.check_resolver(self.root / 'missing')['configured'])

    def test_setup_check_preserves_failure_and_never_calls_sudo(self):
        resolver = self.resolver('port 2053\n')
        fake = self.root / 'bin'
        executable(fake / 'container', b'#!/bin/sh\ncase "$*" in\n"system dns list --quiet") echo host.container.internal;;\nesac\nexit 0\n')
        executable(fake / 'python3', ('#!/bin/sh\nexec ' + shlex.quote(sys.executable) +
                   ' "$@" --resolver-file ' + shlex.quote(str(resolver)) + '\n').encode())
        marker = self.root / 'sudo-called'
        executable(fake / 'sudo', ('#!/bin/sh\ntouch ' + shlex.quote(str(marker)) + '\nexit 99\n').encode())
        env = dict(os.environ, PATH=str(fake) + os.pathsep + os.environ['PATH'],
                   MAGICIAN_CONTAINER_RUNTIME='apple-container')
        command = ['/bin/bash', str(HERE / 'setup-container-runtime.sh'), '--check']
        failed = subprocess.run(command, env=env, capture_output=True, timeout=10)
        self.assertEqual(failed.returncode, 1, failed.stdout)
        self.resolver('port 1053\noptions localhost:203.0.113.113\n')
        passed = subprocess.run(command, env=env, capture_output=True, timeout=10)
        self.assertEqual(passed.returncode, 0, passed.stdout)
        self.assertFalse(marker.exists())


class ExecutableInventoryTests(Scratch):
    def manifest(self, name, required, top_required=None):
        directory = self.root / name
        directory.mkdir(parents=True)
        metadata = {'runtime_contract': {'requires': {'bins': required}}}
        if top_required:
            metadata['requires'] = {'bins': top_required}
        # JSON is valid YAML and keeps these fixture manifests self-contained.
        (directory / 'SKILL.md').write_text('---\n' + json.dumps({'metadata': {'magician': metadata}}) + '\n---\nFixture')
        return directory

    def test_missing_and_wrong_platform_fail_instead_of_false_green(self):
        for name, body in [('linux', elf()), ('intel', elf(62)), ('mac', b'\xcf\xfa\xed\xfe')]:
            directory = self.manifest(name, ['tool'])
            executable(directory / 'bin/tool', body)
        self.manifest('missing', ['tool'])
        rows = {r['skill']: r for r in bins.inventory(self.root, 'aarch64', '/usr/bin:/bin')}
        self.assertEqual(rows['linux']['status'], 'executables_present')
        self.assertEqual([rows[n]['status'] for n in ('intel', 'mac', 'missing')], ['fail'] * 3)

    def test_dangling_link_and_absent_interpreter_fail(self):
        directory = self.manifest('dangling', ['tool'])
        (directory / 'bin').mkdir()
        (directory / 'bin/tool').symlink_to('/nonexistent/magician-test-executable')
        directory = self.manifest('script', ['tool'])
        executable(directory / 'bin/tool', b'#!/nonexistent/magician-test-interpreter\n')
        self.assertTrue(all(r['status'] == 'fail' for r in bins.inventory(self.root, 'arm64', '/usr/bin:/bin')))

    def test_top_level_requirements_are_not_masked_by_contract(self):
        directory = self.manifest('tool', ['present'], ['missing'])
        executable(directory / 'bin/present', b'#!/usr/bin/env sh\n')
        row = bins.inventory(self.root, 'arm64', '/usr/bin:/bin')[0]
        self.assertEqual(row['status'], 'fail')
        self.assertIsNone(row['binaries']['present']['problem'])
        self.assertIsNotNone(row['binaries']['missing']['problem'])

    def test_host_and_mcp_definitions_do_not_claim_linux_execution(self):
        self.manifest('macos-ui-automation', ['cua-driver', 'open'])
        self.manifest('screen-draw', ['native-draw'])
        self.manifest('mcp-definition', [])
        rows = {r['skill']: r['status'] for r in bins.inventory(self.root, 'arm64', '/usr/bin:/bin')}
        self.assertEqual(rows, {'macos-ui-automation': 'host_only', 'screen-draw': 'host_only',
                                'mcp-definition': 'no_local_binary'})

    def test_empty_skill_tree_is_not_a_pass(self):
        with self.assertRaisesRegex(ValueError, 'no runtime skill'):
            bins.inventory(self.root, 'arm64', '/usr/bin:/bin')


class HiggsfieldReleaseTests(Scratch):
    def archive(self, entries):
        path = self.root / 'release.tar.gz'
        with tarfile.open(path, 'w:gz') as archive:
            for name, body, kind in entries:
                member = tarfile.TarInfo(name)
                member.type = kind
                if kind == tarfile.REGTYPE:
                    member.size = len(body)
                else:
                    member.linkname = '../elsewhere'
                archive.addfile(member, io.BytesIO(body))
        return path, hashlib.sha256(path.read_bytes()).hexdigest()

    def test_only_pinned_executable_is_written(self):
        archive, digest = self.archive([('../escape', b'bad', tarfile.REGTYPE),
                                        ('hf', elf(), tarfile.REGTYPE)])
        destination = self.root / 'higgsfield'
        hf.unpack(archive, destination, digest, 'arm64')
        self.assertEqual(destination.read_bytes(), elf())
        self.assertTrue(os.access(destination, os.X_OK))
        self.assertEqual(sorted(p.name for p in self.root.iterdir()), ['higgsfield', 'release.tar.gz'])

    def test_changed_release_cannot_replace_existing_binary(self):
        archive, _ = self.archive([('hf', elf(), tarfile.REGTYPE)])
        destination = self.root / 'higgsfield'
        destination.write_bytes(b'previous version')
        with self.assertRaisesRegex(ValueError, 'checksum'):
            hf.unpack(archive, destination, '0' * 64, 'arm64')
        self.assertEqual(destination.read_bytes(), b'previous version')

    def test_wrong_arch_links_and_duplicate_members_fail(self):
        cases = [[('hf', elf(62), tarfile.REGTYPE)], [('hf', b'', tarfile.SYMTYPE)],
                 [('hf', elf(), tarfile.REGTYPE), ('./hf', elf(), tarfile.REGTYPE)]]
        for entries in cases:
            with self.subTest(entries=[e[0] for e in entries]):
                archive, digest = self.archive(entries)
                destination = self.root / 'higgsfield'
                with self.assertRaises(ValueError):
                    hf.unpack(archive, destination, digest, 'arm64')
                self.assertFalse(destination.exists())


if __name__ == '__main__':
    unittest.main()
