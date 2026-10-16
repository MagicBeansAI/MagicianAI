#!/usr/bin/env python3
"""Bootstrap per-account auth dirs for the Google Workspace skills
(gmail, calendar, sheets) — driven by the gws CLI.

Layout produced for each selectable account or fixed service profile `<name>` listed in
`operator-config.yaml` (`gws_accounts:` or `gws_fixed_profiles:`):

  <scope>/auth/gws-<name>/
    └── client_secret.json   (real copy of skillshub/client_secret.json)

The OAuth client_secret.json is NOT shared at scope — each auth dir
gets its own copy. The skillshub/client_secret.json file is the single
source of truth; re-running this script after replacing it propagates
the new identity to every account's auth dir.

OAuth tokens (the actual login state) are written by the gws CLI into
each account's auth dir during `gws auth login`. This script does NOT
run the OAuth flow — operators run it once per account after this
bootstrap:

  GOOGLE_WORKSPACE_CLI_CONFIG_DIR=<scope>/auth/gws-<name> \\
    gws auth login -s gmail,sheets,drive,docs,calendar

Idempotent — running again only creates missing dirs; existing OAuth
tokens are preserved.

Usage:
  python3 skillshub/scripts/setup_gws_accounts.py
  python3 skillshub/scripts/setup_gws_accounts.py --scope alice/staging
"""
from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path

# Make the operator_config helper importable when this script runs as
# `python3 skillshub/scripts/setup_gws_accounts.py` from the repo root.
sys.path.insert(0, str(Path(__file__).resolve().parent))
from operator_config import gws_profile_names, runtime_root  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
SKILLSHUB = REPO / "skillshub"
DEFAULT_SCOPE = "anonymous/default"


# OAuth Desktop client_secret.json source. Canonical location is the runtime
# root (env `MAGICIAN_ROOT_DIR`, else `$HOME/MagicianNotes`) alongside
# operator-config.yaml so a mounted container root is self-contained; falls back
# to the in-tree skillshub/ copy, then legacy <repo>/client_secret.json.
def find_client_secret() -> Path | None:
    runtime = runtime_root() / "client_secret.json"
    if runtime.is_file():
        return runtime
    canonical = SKILLSHUB / "client_secret.json"
    if canonical.is_file():
        return canonical
    legacy = REPO / "client_secret.json"
    if legacy.is_file():
        print(
            "  [operator-config] reading client_secret.json from legacy "
            "<repo>/client_secret.json (deprecated); move it to "
            "MAGICIAN_ROOT_DIR/client_secret.json to silence this warning.",
            file=sys.stderr,
        )
        return legacy
    return None


def install_client_secret(
    src: Path,
    dst: Path,
) -> str:
    """Copy `src` to `dst` as a real file. Replaces any pre-existing
    symlink (legacy install pointed at `config/gws/client_secret.json`)
    or older-content file. Refreshes only when src is newer."""
    if dst.is_symlink():
        # Legacy symlink → replace with a real copy.
        dst.unlink()
        shutil.copy2(src, dst)
        dst.chmod(0o600)
        return "replaced legacy symlink with copy"
    if dst.exists():
        if dst.stat().st_mtime >= src.stat().st_mtime:
            return "already up to date"
        shutil.copy2(src, dst)
        dst.chmod(0o600)
        return "refreshed (root copy newer)"
    shutil.copy2(src, dst)
    dst.chmod(0o600)
    return "installed"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--scope", default=DEFAULT_SCOPE,
                        help="<principal>/<workspace> (default: %(default)s)")
    parser.add_argument("--data-root", default=str(REPO / "magician_data_v3"),
                        help="path to magician_data_v3 (default: %(default)s)")
    args = parser.parse_args()

    root_secret = find_client_secret()

    if root_secret is None:
        print("  ✗ client_secret.json missing.")
        print("    Place your OAuth Desktop client at skillshub/client_secret.json, then re-run.")
        print("    Get it from https://console.cloud.google.com/apis/credentials")
        print("    (Create OAuth Client → Desktop app → Download JSON).")
        return 1

    accounts = gws_profile_names()
    if not accounts:
        print("  ✗ no gws accounts configured.")
        print("    Add `gws_accounts:` or `gws_fixed_profiles:` to operator-config.yaml")
        print("    (or fall back to a legacy <repo>/accounts.txt) and re-run.")
        return 1

    auth_root = Path(args.data_root) / "scopes" / args.scope / "auth"
    auth_root.mkdir(parents=True, exist_ok=True)

    print(f"  loaded {len(accounts)} profile(s): {', '.join(accounts)}")
    for account in accounts:
        account_dir = auth_root / f"gws-{account}"
        account_dir.mkdir(parents=True, exist_ok=True)
        dst = account_dir / "client_secret.json"
        status = install_client_secret(root_secret, dst)
        print(f"  ✓ gws-{account}: client_secret.json {status}")

    print()
    print("OAuth login per-profile (run once each):")
    for account in accounts:
        cmd = (
            f"  GOOGLE_WORKSPACE_CLI_CONFIG_DIR={auth_root}/gws-{account} "
            f"gws auth login -s gmail,sheets,drive,docs,calendar"
        )
        print(cmd)
    return 0


if __name__ == "__main__":
    sys.exit(main())
