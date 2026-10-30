"""No real credentials, network provisioning, runtime data or services."""
import contextlib
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('cloud_setup', Path(__file__).with_name('configure-cloud-services.py'))
setup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(setup)


class CloudServicesSetupTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name) / 'notes'
        self.values = {
            'MAGICIAN_TUNNEL_ZONE': 'example.com',
            'MAGICIAN_CF_ACCOUNT_ID': 'a' * 32,
            'MAGICIAN_MACHINE_NAME': 'fixture-laptop',
            'MAGICIAN_UI_ACCESS_EMAILS': 'owner@example.com',
            'CLOUDFLARE_API_TOKEN': 'synthetic-token',
        }
        self.env = patch.dict(os.environ, {}, clear=True)
        self.env.start()
        self.addCleanup(self.env.stop)
        platform = patch.object(setup.sys, 'platform', 'darwin')
        platform.start()
        self.addCleanup(platform.stop)

    def test_optional_defaults_have_no_effects(self):
        setup.configure(self.root)
        self.assertFalse(self.root.exists())

    def test_unsupported_connector_host_is_refused_before_writes_or_dispatch(self):
        with patch.object(setup.sys, 'platform', 'linux'), patch.dict(os.environ, self.values):
            with self.assertRaisesRegex(ValueError, 'requires macOS'):
                setup.configure(self.root, cloud=True, runner=lambda *a, **kw: self.fail('unexpected dispatch'))
        self.assertFalse(self.root.exists())

    def test_cloud_provisioner_receives_private_values_and_selected_machine(self):
        setup.write_values(self.root, dict(self.values, KEEP='owner-value'))
        calls = []
        output = io.StringIO()
        with patch.object(setup.shutil, 'which', return_value='/bin/bash'), contextlib.redirect_stdout(output):
            setup.configure(self.root, cloud=True, runner=lambda args, **kw: calls.append((args, kw)))
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][0][-2:], ['fixture-laptop', '--install'])
        self.assertEqual(calls[0][1]['env']['CLOUDFLARE_API_TOKEN'], self.values['CLOUDFLARE_API_TOKEN'])
        self.assertNotIn(self.values['CLOUDFLARE_API_TOKEN'], output.getvalue())
        for name in ('.env', '.env.development'):
            self.assertEqual(setup.file_values(self.root / name)['KEEP'], 'owner-value')
            self.assertEqual((self.root / name).stat().st_mode & 0o777, 0o600)

    def test_missing_configuration_does_not_create_files_or_dispatch(self):
        with self.assertRaisesRegex(ValueError, 'Set these private runtime values'):
            setup.configure(self.root, cloud=True, runner=lambda *a, **kw: self.fail('unexpected dispatch'))
        self.assertFalse(self.root.exists())

    def test_invalid_cloud_configuration_is_rejected_before_writing(self):
        for key, value in [('MAGICIAN_TUNNEL_ZONE', 'https://example.com'), ('MAGICIAN_MACHINE_NAME', '../other'), ('MAGICIAN_UI_ACCESS_EMAILS', '*')]:
            with self.subTest(key=key), patch.dict(os.environ, dict(self.values, **{key: value})):
                with self.assertRaises(ValueError):
                    setup.configure(self.root, cloud=True)
                self.assertFalse(self.root.exists())

    def test_dry_run_has_no_files_or_provisioning(self):
        with patch.dict(os.environ, self.values):
            setup.configure(self.root, cloud=True, dry=True, runner=lambda *a, **kw: self.fail('unexpected dispatch'))
        self.assertFalse(self.root.exists())

    def test_clarity_is_explicit_and_disable_preserves_other_owner_settings(self):
        setup.write_values(self.root, {'KEEP': 'owner-value'})
        with patch.dict(os.environ, {setup.CLARITY_KEY: 'synthetic123'}):
            setup.configure(self.root, clarity=True)
        for name in ('.env', '.env.development'):
            self.assertEqual(setup.file_values(self.root / name)[setup.CLARITY_KEY], 'synthetic123')
        setup.configure(self.root, clarity=True, disable=True)
        for name in ('.env', '.env.development'):
            values = setup.file_values(self.root / name)
            self.assertEqual(values[setup.CLARITY_KEY], '')
            self.assertEqual(values['KEEP'], 'owner-value')

    def test_export_token_is_never_accepted_as_a_tracking_project(self):
        with patch.dict(os.environ, {setup.CLARITY_KEY: 'header.payload.signature'}):
            with self.assertRaises(ValueError):
                setup.configure(self.root, clarity=True)
        self.assertFalse(self.root.exists())

    def test_repeated_writes_preserve_owner_values_and_remove_duplicate_assignments(self):
        self.root.mkdir()
        (self.root / '.env').write_text('# owner comment\nKEEP=unchanged\nKEY=first\nKEY=second\n')
        setup.write_values(self.root, {'KEY': 'new'})
        self.assertEqual((self.root / '.env').read_text().count('KEY='), 1)
        self.assertIn('# owner comment\nKEEP=unchanged', (self.root / '.env').read_text())

    def test_website_opt_in_never_enables_the_personal_app(self):
        with patch.dict(os.environ, {setup.WEBSITE_CLARITY_KEY: 'website123'}):
            setup.configure(self.root, clarity=True, website=True)
        for name in ('.env', '.env.development'):
            values = setup.file_values(self.root / name)
            self.assertEqual(values[setup.WEBSITE_CLARITY_KEY], 'website123')
            self.assertNotIn(setup.CLARITY_KEY, values)
        inherited = {'KEEP': 'owner-value', setup.WEBSITE_CLARITY_KEY: 'inherited123'}
        self.assertEqual(setup.ui_build_environment(self.root, inherited, website=False)[setup.WEBSITE_CLARITY_KEY], '')
        self.assertEqual(setup.ui_build_environment(self.root, inherited, website=True)[setup.WEBSITE_CLARITY_KEY], 'inherited123')
        self.assertEqual(inherited[setup.WEBSITE_CLARITY_KEY], 'inherited123')

    def test_website_build_projects_only_the_public_tracking_id(self):
        setup.write_values(self.root, {**self.values, setup.WEBSITE_CLARITY_KEY: 'website123'})
        env = setup.ui_build_environment(self.root, {}, website=True)
        self.assertEqual(env, {setup.WEBSITE_CLARITY_KEY: 'website123'})
        self.assertNotIn('CLOUDFLARE_API_TOKEN', env)
        with (self.root / '.env.development').open('a') as stream:
            stream.write('UNRELATED_PRIVATE_INPUT="unfinished\n')
        self.assertEqual(setup.ui_build_environment(self.root, {}, website=True), env)

    def test_explicit_disable_does_not_dispatch_an_unrelated_cloud_opt_in(self):
        with patch.dict(os.environ, {'MAGICIAN_ENABLE_FUNNEL': '1', 'MAGICIAN_ROOT_DIR': str(self.root)}), patch.object(setup.sys, 'argv', ['setup', '--clarity', '--disable', '--non-interactive']):
            self.assertEqual(setup.main(), 0)
        self.assertEqual(setup.file_values(self.root / '.env'), {setup.CLARITY_KEY: ''})

    def test_conflicting_owner_values_are_not_silently_replaced(self):
        setup.write_values(self.root, {setup.CLARITY_KEY: 'original123'})
        (self.root / '.env.development').write_text(setup.CLARITY_KEY + '=different123\n')
        before = {name: (self.root / name).read_bytes() for name in ('.env', '.env.development')}
        with self.assertRaisesRegex(ValueError, 'disagree'):
            setup.configure(self.root, clarity=True)
        self.assertEqual(before, {name: (self.root / name).read_bytes() for name in before})
        with patch.dict(os.environ, {setup.CLARITY_KEY: 'chosen123'}):
            setup.configure(self.root, clarity=True)
        self.assertEqual(setup.file_values(self.root / '.env.development')[setup.CLARITY_KEY], 'chosen123')

    def test_core_preparation_does_not_require_or_provision_optional_accounts(self):
        with patch.dict(os.environ, {'MAGICIAN_ENABLE_FUNNEL': '1', 'MAGICIAN_ROOT_DIR': str(self.root)}), patch.object(setup.sys, 'argv', ['setup', '--from-environment']):
            self.assertEqual(setup.main(), 0)
        self.assertFalse(self.root.exists())

    def saved(self, name='.env'):
        return setup.file_values(self.root / name)

    def test_same_zone_keeps_saved_hostnames_and_dispatches_once(self):
        hosts = {
            'MAGICIAN_WEBHOOK_HOST': 'webhook.example.com',
            'MAGICIAN_UI_HOST': 'https://ui.example.com:443/app',
            'MAGICIAN_CONNECT_HOST': 'connect.example.com',
            'MAGICIAN_MOBILE_PUBLIC_ORIGIN': 'https://laptop.example.com',
        }
        setup.write_values(self.root, dict(self.values, KEEP='owner-value', **hosts))
        calls = []
        with patch.object(setup.shutil, 'which', return_value='/bin/bash'), patch.dict(os.environ, dict(self.values, **hosts)):
            setup.configure(self.root, cloud=True, runner=lambda args, **kw: calls.append((args, kw)))
        self.assertEqual(len(calls), 1)
        self.assertTrue(calls[0][0][1].endswith('ensure-machine-tunnel.sh'))
        self.assertEqual(calls[0][1]['env']['MAGICIAN_UI_HOST'], hosts['MAGICIAN_UI_HOST'])
        for name in ('.env', '.env.development'):
            saved = self.saved(name)
            self.assertEqual(saved['KEEP'], 'owner-value')
            self.assertEqual(saved['MAGICIAN_UI_HOST'], hosts['MAGICIAN_UI_HOST'])

    def test_zone_switch_clears_stale_hostnames_and_rebuilds_both_tunnels(self):
        old_hosts = {
            'MAGICIAN_WEBHOOK_HOST': 'webhook.old.example',
            'MAGICIAN_UI_HOST': 'ui.old.example',
            'MAGICIAN_CONNECT_HOST': 'connect.old.example',
            'MAGICIAN_MOBILE_PUBLIC_ORIGIN': 'https://laptop.old.example',
        }
        setup.write_values(self.root, dict(self.values, MAGICIAN_TUNNEL_ZONE='old.example', KEEP='owner-value', **old_hosts))
        calls = []
        output = io.StringIO()
        with patch.object(setup.shutil, 'which', return_value='/bin/bash'), patch.dict(os.environ, dict(self.values, **old_hosts)), contextlib.redirect_stdout(output):
            setup.configure(self.root, cloud=True, runner=lambda args, **kw: calls.append((args, kw)))
        self.assertEqual([Path(call[0][1]).name for call in calls], ['ensure-machine-tunnel.sh', 'ensure-magician-tunnel.sh'])
        self.assertEqual(calls[0][0][-2:], ['fixture-laptop', '--install'])
        text = output.getvalue()
        self.assertNotIn(self.values['CLOUDFLARE_API_TOKEN'], text)
        self.assertIn('webhook.old.example', text)
        self.assertIn('make machine-tunnel PARK=<hostname>', text)
        self.assertIn('Restart Magician', text)
        for call in calls:
            for key in setup.HOST_KEYS:
                self.assertNotIn(key, call[1]['env'])
            self.assertEqual(call[1]['env']['MAGICIAN_TUNNEL_ZONE'], 'example.com')
        for name in ('.env', '.env.development'):
            saved = self.saved(name)
            self.assertEqual(saved['KEEP'], 'owner-value')
            self.assertEqual(saved['MAGICIAN_TUNNEL_ZONE'], 'example.com')
            self.assertEqual((self.root / name).stat().st_mode & 0o777, 0o600)
            for key in setup.HOST_KEYS:
                self.assertNotIn(key, saved)

    def test_dry_run_reports_a_zone_switch_without_writing_or_dispatching(self):
        setup.write_values(self.root, dict(self.values, MAGICIAN_TUNNEL_ZONE='old.example', MAGICIAN_MOBILE_PUBLIC_ORIGIN='https://laptop.old.example/health'))
        before = {name: (self.root / name).read_bytes() for name in ('.env', '.env.development')}
        output = io.StringIO()
        with patch.dict(os.environ, self.values), contextlib.redirect_stdout(output):
            setup.configure(self.root, cloud=True, dry=True, runner=lambda *args, **kw: self.fail('unexpected dispatch'))
        self.assertEqual(before, {name: (self.root / name).read_bytes() for name in before})
        text = output.getvalue()
        self.assertIn('Would move every tunnel onto example.com', text)
        self.assertIn('Would run the original shared tunnel script', text)
        self.assertIn('https://laptop.old.example/health', text)

    def test_interactive_switch_asks_for_the_new_zone(self):
        setup.write_values(self.root, dict(self.values, MAGICIAN_TUNNEL_ZONE='old.example', MAGICIAN_WEBHOOK_HOST='webhook.old.example'))
        calls = []
        with patch.object(setup.shutil, 'which', return_value='/bin/bash'), patch('builtins.input', side_effect=['y', 'other.example']), contextlib.redirect_stdout(io.StringIO()):
            setup.configure(self.root, cloud=True, interactive=True, runner=lambda args, **kw: calls.append(args))
        self.assertEqual([Path(call[1]).name for call in calls], ['ensure-machine-tunnel.sh', 'ensure-magician-tunnel.sh'])
        self.assertEqual(self.saved()['MAGICIAN_TUNNEL_ZONE'], 'other.example')
        self.assertNotIn('MAGICIAN_WEBHOOK_HOST', self.saved())

    def test_interactive_switch_rejects_an_empty_zone_before_writing(self):
        setup.write_values(self.root, dict(self.values, MAGICIAN_TUNNEL_ZONE='old.example', MAGICIAN_WEBHOOK_HOST='webhook.old.example'))
        before = {name: (self.root / name).read_bytes() for name in ('.env', '.env.development')}
        with patch('builtins.input', side_effect=['yes', '']), self.assertRaisesRegex(ValueError, 'needs the new zone'):
            setup.configure(self.root, cloud=True, interactive=True, runner=lambda *args, **kw: self.fail('unexpected dispatch'))
        self.assertEqual(before, {name: (self.root / name).read_bytes() for name in before})

    def test_declining_a_switch_keeps_hostnames_that_already_match(self):
        setup.write_values(self.root, dict(self.values, MAGICIAN_WEBHOOK_HOST='webhook.example.com'))
        calls = []
        with patch.object(setup.shutil, 'which', return_value='/bin/bash'), patch('builtins.input', return_value='n'):
            setup.configure(self.root, cloud=True, interactive=True, runner=lambda args, **kw: calls.append(args))
        self.assertEqual(len(calls), 1)
        self.assertTrue(calls[0][1].endswith('ensure-machine-tunnel.sh'))
        self.assertEqual(self.saved()['MAGICIAN_WEBHOOK_HOST'], 'webhook.example.com')

    def test_declining_a_switch_finishes_a_half_moved_zone(self):
        setup.write_values(self.root, dict(self.values, MAGICIAN_WEBHOOK_HOST='webhook.old.example', KEEP='owner-value'))
        calls = []
        with patch.object(setup.shutil, 'which', return_value='/bin/bash'), patch('builtins.input', return_value='n'), contextlib.redirect_stdout(io.StringIO()):
            setup.configure(self.root, cloud=True, interactive=True, runner=lambda args, **kw: calls.append(args))
        self.assertEqual([Path(call[1]).name for call in calls], ['ensure-machine-tunnel.sh', 'ensure-magician-tunnel.sh'])
        self.assertEqual(self.saved()['KEEP'], 'owner-value')
        self.assertEqual(self.saved()['MAGICIAN_TUNNEL_ZONE'], 'example.com')
        self.assertNotIn('MAGICIAN_WEBHOOK_HOST', self.saved())

    def test_core_preparation_saves_only_explicit_inputs(self):
        with patch.dict(os.environ, {'MAGICIAN_ENABLE_FUNNEL': '1', 'MAGICIAN_TUNNEL_ZONE': 'example.com', 'MAGICIAN_ROOT_DIR': str(self.root)}), patch.object(setup.sys, 'argv', ['setup', '--from-environment']):
            self.assertEqual(setup.main(), 0)
        self.assertEqual(setup.file_values(self.root / '.env'), {'MAGICIAN_TUNNEL_ZONE': 'example.com'})


if __name__ == '__main__':
    unittest.main()
