#!/usr/bin/env python3
"""Check Apple Container's localhost resolver, without changing system state.

This checks configuration only. The container integration harness must still
prove packet forwarding and the desktop gateway's peer/permission checks.
"""
import argparse
import ipaddress
import json
from pathlib import Path

DEFAULT_RESOLVER = Path('/etc/resolver/containerization.host.container.internal')


def check_resolver(path):
    try:
        with Path(path).open('rb') as source:
            raw = source.read(4097)
        if len(raw) > 4096:
            raise ValueError('resolver exceeds 4096 bytes')
        fields = {}
        for line in raw.decode('utf-8').splitlines():
            parts = line.split('#', 1)[0].split()
            if parts:
                fields.setdefault(parts[0], []).append(parts[1:])
        if fields.get('domain') != [['host.container.internal']]:
            raise ValueError('expected the host.container.internal domain')
        if fields.get('nameserver') != [['127.0.0.1']]:
            raise ValueError('expected the local Apple DNS server')
        if fields.get('port') != [['1053']]:
            raise ValueError('ordinary DNS is not localhost forwarding; expected port 1053')
        options = [part for row in fields.get('options', []) for part in row]
        addresses = [part.removeprefix('localhost:') for part in options if part.startswith('localhost:')]
        if len(addresses) != 1:
            raise ValueError('expected exactly one options localhost:<IPv4> binding')
        address = ipaddress.IPv4Address(addresses[0])
        if address.is_loopback or address.is_unspecified or address.is_multicast or address == ipaddress.IPv4Address('255.255.255.255'):
            raise ValueError('localhost alias must use a distinct unicast IPv4 address')
        return {'configured': True, 'alias_address': str(address), 'transport_verified': False}
    except (OSError, UnicodeError, ValueError) as error:
        return {'configured': False, 'reason': str(error), 'transport_verified': False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--resolver-file', type=Path, default=DEFAULT_RESOLVER)
    parser.add_argument('--quiet', action='store_true')
    args = parser.parse_args()
    result = check_resolver(args.resolver_file)
    if not args.quiet:
        print(json.dumps(result))
    return 0 if result['configured'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
