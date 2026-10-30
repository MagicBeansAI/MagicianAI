#!/usr/bin/env python3
"""Optional installer configuration using private env files and existing provisioners."""
import argparse
import getpass
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile

REPO = Path(__file__).resolve().parents[1]
CLOUD_FIELDS = (
    ('MAGICIAN_TUNNEL_ZONE', 'Your Cloudflare zone (example.com)', False),
    ('MAGICIAN_CF_ACCOUNT_ID', 'Your Cloudflare account ID', False),
    ('MAGICIAN_MACHINE_NAME', 'This machine’s unique label (laptop)', False),
    ('MAGICIAN_UI_ACCESS_EMAILS', 'Browser Access emails (comma-separated)', False),
    ('CLOUDFLARE_API_TOKEN', 'Scoped Cloudflare API token', True),
)
# Saved hostnames win over the zone. A domain switch has to drop them so both
# tunnel provisioners derive names on the new zone.
HOST_KEYS = (
    'MAGICIAN_WEBHOOK_HOST',
    'MAGICIAN_UI_HOST',
    'MAGICIAN_CONNECT_HOST',
    'MAGICIAN_MOBILE_PUBLIC_ORIGIN',
)
CLARITY_KEY = 'MAGICIAN_CLARITY_PROJECT_ID'
WEBSITE_CLARITY_KEY = 'PUBLIC_CLARITY_PROJECT_ID'


def file_values(path, keys=None):
    result = {}
    if path.is_file():
        for line in path.read_text().splitlines():
            match = re.match(r'^\s*(?:export\s+)?([A-Z][A-Z0-9_]*)\s*=\s*(.*)$', line)
            if match and (keys is None or match[1] in keys):
                result.setdefault(match[1], ' '.join(shlex.split(match[2], comments=True)))
    return result


def values_at(root, keys=None):
    if keys is None:
        keys = {field[0] for field in CLOUD_FIELDS} | {CLARITY_KEY, WEBSITE_CLARITY_KEY}
    values = {}
    for name in ('.env.development', '.env'):
        for key, value in file_values(root / name, keys).items():
            if key in values and values[key] != value and not os.environ.get(key):
                raise ValueError('Private env files disagree for ' + key + '; supply that setting explicitly for this setup')
            values.setdefault(key, value)
    for key in keys:
        if os.environ.get(key):
            values[key] = os.environ[key]
    return values


def write_values(root, updates):
    """Atomic per file; preserve unrelated settings and comments, remove duplicates."""
    for value in updates.values():
        if any(char in value for char in '\r\n\0'):
            raise ValueError('Environment values must be single-line')
    root.mkdir(parents=True, exist_ok=True)
    for name in ('.env', '.env.development'):
        path = root / name
        lines = path.read_text().splitlines() if path.is_file() else []
        output = []
        for line in lines:
            match = re.match(r'^\s*(?:export\s+)?([A-Z][A-Z0-9_]*)\s*=', line)
            if not match or match[1] not in updates:
                output.append(line)
        for key, value in updates.items():
            escaped = value.replace('\\', '\\\\').replace('"', '\\"').replace('$', '\\$')
            output.append(f'{key}="{escaped}"')
        fd, temp = tempfile.mkstemp(prefix=name + '.', dir=root)
        try:
            with os.fdopen(fd, 'w') as stream:
                stream.write('\n'.join(output) + '\n')
                stream.flush()
                os.fsync(stream.fileno())
            os.chmod(temp, 0o600)
            os.replace(temp, path)
        finally:
            if os.path.exists(temp):
                os.unlink(temp)


def hostname(value):
    text = value.strip()
    if '://' in text:
        text = text.split('://', 1)[1]
    return text.split('/', 1)[0].split(':', 1)[0].rstrip('.').lower()


def on_zone(host, zone):
    zone = zone.lower().rstrip('.')
    return bool(host) and (host == zone or host.endswith('.' + zone))


def stale_hostnames(root, zone):
    """Hostnames still pinned to a domain other than `zone`, in first-seen order."""
    found = []
    for name in ('.env.development', '.env'):
        for value in file_values(root / name, set(HOST_KEYS)).values():
            value = value.strip()
            if value and not on_zone(hostname(value), zone) and value not in found:
                found.append(value)
    return found


def remove_keys(root, keys):
    """Drop settings from both private env files. Absent files stay absent."""
    keys = set(keys)
    for name in ('.env', '.env.development'):
        path = root / name
        if not path.is_file():
            continue
        kept = []
        for line in path.read_text().splitlines():
            match = re.match(r'^\s*(?:export\s+)?([A-Z][A-Z0-9_]*)\s*=', line)
            if not match or match[1] not in keys:
                kept.append(line)
        fd, temp = tempfile.mkstemp(prefix=name + '.', dir=root)
        try:
            with os.fdopen(fd, 'w') as stream:
                stream.write('\n'.join(kept) + ('\n' if kept else ''))
                stream.flush()
                os.fsync(stream.fileno())
            os.chmod(temp, 0o600)
            os.replace(temp, path)
        finally:
            if os.path.exists(temp):
                os.unlink(temp)


def validate_cloud(values):
    missing = [key for key, _, _ in CLOUD_FIELDS if not values.get(key, '').strip()]
    if missing:
        raise ValueError('Set these private runtime values first: ' + ', '.join(missing))
    zone = values['MAGICIAN_TUNNEL_ZONE']
    if '.' not in zone or len(zone) > 253 or any(not re.fullmatch(r'[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?', label) for label in zone.split('.')):
        raise ValueError('Cloudflare zone must be a DNS name, without a URL or path')
    if not re.fullmatch(r'[a-fA-F0-9]{32}', values['MAGICIAN_CF_ACCOUNT_ID']):
        raise ValueError('Use the 32-character Cloudflare account ID from your dashboard')
    if not re.fullmatch(r'[a-z0-9](?:[a-z0-9-]{0,53}[a-z0-9])?', values['MAGICIAN_MACHINE_NAME']):
        raise ValueError('Machine label must use lowercase DNS characters, at most 55 characters')
    emails = [email.strip() for email in values['MAGICIAN_UI_ACCESS_EMAILS'].split(',')]
    if any(not re.fullmatch(r'[^\s@,]+@[^\s@,]+\.[^\s@,]+', email) for email in emails):
        raise ValueError('Browser Access needs explicit comma-separated email addresses')


def validate_clarity(value):
    if not re.fullmatch(r'[A-Za-z0-9]+', value):
        raise ValueError('Use a Clarity tracking project ID, never a Data Export API token')


def configure(root, *, cloud=False, clarity=False, interactive=False, dry=False,
              disable=False, website=False, configure_only=False, runner=subprocess.run):
    if cloud and not configure_only and not dry and sys.platform != 'darwin':
        raise ValueError('Automatic connector installation currently requires macOS; use the manual Cloudflare setup guide on other hosts')
    keys = {field[0] for field in CLOUD_FIELDS} if cloud else set()
    if clarity and not disable:
        keys.add(WEBSITE_CLARITY_KEY if website else CLARITY_KEY)
    values = values_at(root, keys)
    updates = {}
    moving = []
    if cloud:
        print('Cloudflare uses your domain and scoped token. See docs/setup/cloudflare.md.')
        current = values.get('MAGICIAN_TUNNEL_ZONE', '').strip()
        if interactive and current:
            print('Cloudflare zone is ' + current + '.')
            switch = input('Switch this computer to a different domain? [y/N] ').strip().lower()
            if switch in ('y', 'yes'):
                entered = input('New Cloudflare zone (example.com): ').strip()
                if not entered:
                    raise ValueError('A domain switch needs the new zone')
                values['MAGICIAN_TUNNEL_ZONE'] = entered
        for key, label, secret in CLOUD_FIELDS:
            if not values.get(key) and interactive:
                values[key] = (getpass.getpass if secret else input)(label + ': ').strip()
        validate_cloud(values)
        updates.update({key: values[key] for key, _, _ in CLOUD_FIELDS})
        moving = stale_hostnames(root, values['MAGICIAN_TUNNEL_ZONE'])
    if clarity:
        project_key = WEBSITE_CLARITY_KEY if website else CLARITY_KEY
        print('Clarity is optional website session recording.' if website else 'Clarity is optional session recording, including app screens. It sends usage to Microsoft.')
        if disable:
            updates[project_key] = ''
        else:
            if not values.get(project_key) and interactive:
                values[project_key] = input('Opt in: your Clarity tracking project ID: ').strip()
            validate_clarity(values.get(project_key, ''))
            updates[project_key] = values[project_key]
    if dry:
        print('Would save selected settings in both private runtime env files; no values are printed.')
        if cloud and not configure_only:
            print('Would provision this machine’s Cloudflare Tunnel and Access through the existing script.')
        if moving:
            print('Would move every tunnel onto ' + values['MAGICIAN_TUNNEL_ZONE'] + ' and clear saved hostnames: ' + ', '.join(moving) + '.')
            print('Would run the original shared tunnel script as well. Old DNS records stay until each hostname is parked.')
        return
    if updates:
        write_values(root, updates)
        print('Saved selected settings in private .env and .env.development files.')
    if moving:
        remove_keys(root, HOST_KEYS)
        print('Cleared saved hostnames from the previous domain: ' + ', '.join(moving) + '.')
    if cloud and not configure_only:
        bash = shutil.which('bash')
        if not bash:
            raise ValueError('bash is required for Cloudflare provisioning')
        child_env = dict(os.environ, MAGICIAN_ROOT_DIR=str(root))
        child_env.update({key: values[key] for key, _, _ in CLOUD_FIELDS})
        if moving:
            for key in HOST_KEYS:
                child_env.pop(key, None)
        runner([bash, str(REPO / 'scripts/ensure-machine-tunnel.sh'), values['MAGICIAN_MACHINE_NAME'], '--install'], env=child_env, check=True)
        if moving:
            runner([bash, str(REPO / 'scripts/ensure-magician-tunnel.sh')], env=child_env, check=True)
            print('Tunnels now follow ' + values['MAGICIAN_TUNNEL_ZONE'] + '. Restart Magician and pair phones again. Kapso and AgentMail register the new webhook when Magician starts.')
            print('Old DNS records and Access apps stay. Park one leftover hostname with: make machine-tunnel PARK=<hostname>')
    if clarity:
        if website:
            print('Website recording configuration saved for the next website build; app recording is unchanged.')
        else:
            print('Clarity disabled; reload open app pages.' if disable else 'Clarity configured; reload the remote app page. Loopback sessions do not record.')


def ui_build_environment(root, inherited, *, website):
    env = dict(inherited)
    # Personal-app artifacts never inherit website recording consent.
    project = inherited.get(WEBSITE_CLARITY_KEY, values_at(root, {WEBSITE_CLARITY_KEY}).get(WEBSITE_CLARITY_KEY)) if website else ''
    if project is None:
        # With no runtime override, let Vite honor an explicitly configured UI env file.
        env.pop(WEBSITE_CLARITY_KEY, None)
        return env
    if project:
        validate_clarity(project)
    env[WEBSITE_CLARITY_KEY] = project
    return env


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cloudflare', action='store_true')
    parser.add_argument('--clarity', action='store_true')
    parser.add_argument('--website', action='store_true', help='Configure standalone website recording separately from app recording')
    builds = parser.add_mutually_exclusive_group()
    builds.add_argument('--ui-build', nargs=argparse.REMAINDER)
    builds.add_argument('--website-build', nargs=argparse.REMAINDER)
    parser.add_argument('--disable', action='store_true', help='Disable Clarity; preserve other settings')
    parser.add_argument('--non-interactive', action='store_true')
    parser.add_argument('--from-environment', action='store_true', help='Persist explicitly selected env inputs without provisioning')
    parser.add_argument('--dry-run', action='store_true')
    args = parser.parse_args()
    root = Path(os.environ.get('MAGICIAN_ROOT_DIR', str(Path.home() / 'MagicianNotes'))).expanduser()
    build_command = args.ui_build if args.ui_build is not None else args.website_build
    if build_command is not None:
        if not build_command:
            parser.error('A UI build command is required')
        program = shutil.which(build_command[0])
        if not program:
            raise ValueError('UI build command is not installed')
        return subprocess.run([program, *build_command[1:]], env=ui_build_environment(root, os.environ, website=args.website_build is not None)).returncode
    if args.website and not args.clarity:
        parser.error('--website requires --clarity')
    if args.disable and not args.clarity:
        parser.error('--disable requires --clarity')
    interactive = sys.stdin.isatty() and not args.non_interactive and not args.from_environment and not args.dry_run and os.environ.get('MAGICIAN_INSTALL_DRYRUN') != '1'
    explicit = args.cloudflare or args.clarity
    cloud = args.cloudflare or (not explicit and os.environ.get('MAGICIAN_ENABLE_FUNNEL') == '1')
    clarity = args.clarity or (not explicit and os.environ.get('MAGICIAN_ENABLE_CLARITY') == '1')
    website = not explicit and bool(os.environ.get(WEBSITE_CLARITY_KEY))
    if args.from_environment:
        # Core preparation seeds only inputs explicitly supplied by its caller.
        # Optional account prerequisites belong to the later capability plan.
        keys = {field[0] for field in CLOUD_FIELDS} if cloud else set()
        if clarity:
            keys.add(WEBSITE_CLARITY_KEY if args.website else CLARITY_KEY)
        if website:
            keys.add(WEBSITE_CLARITY_KEY)
        updates = {key: os.environ[key] for key in keys if os.environ.get(key)}
        for key in (CLARITY_KEY, WEBSITE_CLARITY_KEY):
            if key in updates:
                validate_clarity(updates[key])
        if updates and not (args.dry_run or os.environ.get('MAGICIAN_INSTALL_DRYRUN') == '1'):
            write_values(root, updates)
        print('Prepared explicitly supplied optional settings; missing account setup remains in the capability plan.')
        return 0
    if interactive and not (args.cloudflare or args.clarity):
        cloud = cloud or input('Configure your own Cloudflare remote access? [y/N] ').strip().lower() in ('y', 'yes')
        clarity = clarity or input('Opt in to Microsoft Clarity app session recording? [y/N] ').strip().lower() in ('y', 'yes')
        website = website or input('Configure Clarity for standalone website builds? [y/N] ').strip().lower() in ('y', 'yes')
    if not (cloud or clarity or website):
        print('Optional Cloudflare/Clarity settings remain unchanged. Use make setup-cloudflare or make setup-clarity to configure them.')
        return 0
    configure(root, cloud=cloud, clarity=clarity, interactive=interactive,
              dry=args.dry_run or os.environ.get('MAGICIAN_INSTALL_DRYRUN') == '1',
              disable=args.disable, website=args.website, configure_only=args.from_environment)
    if website:
        configure(root, clarity=True, website=True, interactive=interactive,
                  dry=args.dry_run or os.environ.get('MAGICIAN_INSTALL_DRYRUN') == '1')
    return 0


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (ValueError, OSError, EOFError, subprocess.CalledProcessError) as error:
        # Never include values, env maps or subprocess argv in the diagnostic.
        print('Cloud service setup did not complete: ' + (str(error) if isinstance(error, ValueError) else type(error).__name__), file=sys.stderr)
        sys.exit(1)
