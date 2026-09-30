#!/usr/bin/env python3
"""Generate per-bot runtime env files at the scope from the repo-root
secrets source.

Each bot under `skillshub/bots/` reads a `--env-file=<path>` declared
in `bot_configs.yaml`. Files land at:

  <scope>/bots/<bot>/.env.development          (single-instance bots)
  <scope>/bots/gmail/.env.<account>            (gmail multi-account)

This script populates those files from the operator config file (same
source as `aggregate_env_examples.py`):

  <repo>/skillshub/operator-config.yaml   (canonical; gitignored)

Falls back to the legacy `<repo>/.env copy.development` if the YAML
file is absent — emits a deprecation warning when it does.

Per-bot value resolution (only writes a file if at least one required
key resolves to a non-empty value, unless the file is purely
auto-derived like gmail-per-account):

  kapso/.env.development      <- KAPSO_API_KEY, KAPSO_PHONE_NUMBER_ID,
                                 KAPSO_WEBHOOK_SECRET
                                 (also accepts the typo'd APSO_PHONE_NUMBER_ID
                                  with a warning), MAGICIAN_URL,
  agentmail/.env.development  <- AGENT_MAIL_KEY (required), plus optional
                                 AGENTMAIL_WEBHOOK_URL / AGENTMAIL_WEBHOOK_SECRET
                                 and Magician transport settings
  telegram/.env.development   <- TELEGRAM_TOKEN, Magician transport settings
  telegram-self/.env.development
                              <- TGCLI_API_ID, TGCLI_API_HASH, Magician transport settings
  whatsapp/.env.development   <- WHATSAPP_PHONE, Magician transport settings
  gmail/.env.<account>        <- one per account in `accounts.txt`. Writes
                                 GWS_CONFIG_DIR=../../auth/gws-<account>,
                                 GWS_PROFILE_LABEL=<Account>, GWS_BINARY,
                                 GWS_EXPECTED_EMAIL (optional, see below),
                                 Magician transport settings.

The expected-email pin is read from `accounts.txt`. Each line accepts
`<name>=<email>`, e.g. `business=alice@example.com`. When set,
`GWS_EXPECTED_EMAIL` is written into the bot env and the gmail adapter
refuses to bind any other Google identity at OAuth time (guards against
accidentally signing all bots into one account). Bare `<name>` lines
(no `=email`) fall back to a legacy `GWS_EXPECTED_EMAIL_<NAME>` key in
the secrets file; if neither is set, the bot runs without the
account-mismatch guard and the SIGN IN dialog reads
"Expected account: unset".

Idempotent: existing files are NOT overwritten unless `--force`. Re-run
after editing the root source by passing `--force` (or rm the file).

Usage:
  python3 skillshub/scripts/setup_bot_envs.py
  python3 skillshub/scripts/setup_bot_envs.py --scope alice/staging
  python3 skillshub/scripts/setup_bot_envs.py --force   # regenerate all
"""
from __future__ import annotations

import argparse
import sys
from pathlib import Path

# Make the operator_config helper importable when this script runs as
# `python3 skillshub/scripts/setup_bot_envs.py` from the repo root.
sys.path.insert(0, str(Path(__file__).resolve().parent))
from operator_config import load_gws_accounts, load_secrets  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
DEFAULT_SCOPE = "anonymous/default"


def kapso_phone_id(secrets: dict[str, str], log: list[str]) -> str:
    canonical = secrets.get("KAPSO_PHONE_NUMBER_ID", "").strip()
    if canonical:
        return canonical
    typo = secrets.get("APSO_PHONE_NUMBER_ID", "").strip()
    if typo:
        log.append(
            "    ⚠  source has APSO_PHONE_NUMBER_ID (typo). Treating as "
            "KAPSO_PHONE_NUMBER_ID — please rename in the source file."
        )
        return typo
    return ""


def write_if_missing(path: Path, content: str, force: bool, log: list[str]) -> bool:
    if path.exists() and not force:
        log.append(f"    — {path.name}: exists, leaving untouched")
        return False
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content)
    path.chmod(0o600)
    log.append(f"    ✓ {path.name}: wrote {len(content.splitlines())} line(s)")
    return True


def render_block(header: str, kv_pairs: list[tuple[str, str]]) -> str:
    lines = [f"# {header}"]
    for key, value in kv_pairs:
        lines.append(f"{key}={value}")
    return "\n".join(lines) + "\n"


def append_magician_transport(
    pairs: list[tuple[str, str]], secrets: dict[str, str]
) -> None:
    # MAGICIAN_BEARER_TOKEN is deliberately absent: the runtime mints a scoped,
    # in-memory token and injects it at spawn (see the bot launcher), so seeding
    # one here would write a credential to disk that the injected value then
    # overrides anyway.
    for key in ("MAGICIAN_URL",):
        value = secrets.get(key, "").strip()
        if value:
            pairs.append((key, value))


def write_kapso(scope_bots: Path, secrets: dict[str, str], force: bool, log: list[str]) -> int:
    api_key = secrets.get("KAPSO_API_KEY", "").strip()
    phone_id = kapso_phone_id(secrets, log)
    webhook_secret = secrets.get("KAPSO_WEBHOOK_SECRET", "").strip()
    if not api_key and not phone_id and not webhook_secret:
        log.append("    — kapso: no KAPSO_* keys in source; skipped")
        return 0
    if not webhook_secret:
        log.append(
            "    ⚠  kapso: KAPSO_WEBHOOK_SECRET is empty; the bot will refuse "
            "to start until its number-webhook signing secret is configured"
        )
    target = scope_bots / "kapso" / ".env.development"
    pairs: list[tuple[str, str]] = [
        ("KAPSO_API_KEY", api_key),
        ("KAPSO_PHONE_NUMBER_ID", phone_id),
        ("KAPSO_WEBHOOK_SECRET", webhook_secret),
    ]
    append_magician_transport(pairs, secrets)
    content = render_block(
        "Kapso WhatsApp bot — generated by skillshub/scripts/setup_bot_envs.py", pairs
    )
    return 1 if write_if_missing(target, content, force, log) else 0


def write_agentmail(scope_bots: Path, secrets: dict[str, str], force: bool, log: list[str]) -> int:
    api_key = secrets.get("AGENT_MAIL_KEY", "").strip()
    if not api_key:
        log.append("    — agentmail: no AGENT_MAIL_KEY in source; skipped")
        return 0
    target = scope_bots / "agentmail" / ".env.development"
    pairs: list[tuple[str, str]] = [("AGENT_MAIL_KEY", api_key)]
    # Optional: public base URL for webhook self-register + the svix
    # signing secret for signature verification. Only written when present.
    if secrets.get("AGENTMAIL_WEBHOOK_URL"):
        pairs.append(("AGENTMAIL_WEBHOOK_URL", secrets["AGENTMAIL_WEBHOOK_URL"]))
    if secrets.get("AGENTMAIL_WEBHOOK_SECRET"):
        pairs.append(("AGENTMAIL_WEBHOOK_SECRET", secrets["AGENTMAIL_WEBHOOK_SECRET"]))
    # MAGICIAN_URL is unused in phase 1 (receive + log only) but seeded now
    # so phase-2 magician forwarding works without re-running setup.
    append_magician_transport(pairs, secrets)
    content = render_block(
        "AgentMail inbound bot — generated by skillshub/scripts/setup_bot_envs.py", pairs
    )
    return 1 if write_if_missing(target, content, force, log) else 0


def write_telegram(scope_bots: Path, secrets: dict[str, str], force: bool, log: list[str]) -> int:
    token = secrets.get("TELEGRAM_TOKEN", "").strip()
    if not token:
        log.append("    — telegram: no TELEGRAM_TOKEN in source; skipped")
        return 0
    target = scope_bots / "telegram" / ".env.development"
    pairs: list[tuple[str, str]] = [("TELEGRAM_TOKEN", token)]
    append_magician_transport(pairs, secrets)
    content = render_block(
        "Telegram bot — generated by skillshub/scripts/setup_bot_envs.py", pairs
    )
    return 1 if write_if_missing(target, content, force, log) else 0


def write_telegram_self(scope_bots: Path, secrets: dict[str, str], force: bool, log: list[str]) -> int:
    api_id = secrets.get("TGCLI_API_ID", "").strip()
    api_hash = secrets.get("TGCLI_API_HASH", "").strip()
    if not api_id or not api_hash:
        log.append("    — telegram-self: TGCLI_API_ID / TGCLI_API_HASH not in source; skipped")
        return 0
    target = scope_bots / "telegram-self" / ".env.development"
    # No TGCLI_BINARY env var written. install_bot_bundles.py installs
    # @dapi/tgcli into <scope>/bots/telegram-self/node_modules/, and the
    # bot's resolveTgcliBinary() finds it at packageRoot/node_modules/
    # .bin/tgcli (its first lookup, before the env override or the
    # legacy ../../node_modules fallback). Setting TGCLI_BINARY to an
    # absolute path here would re-introduce the same fragility we
    # fought against in whatsapp.
    pairs: list[tuple[str, str]] = [
        ("TGCLI_API_ID", api_id),
        ("TGCLI_API_HASH", api_hash),
    ]
    append_magician_transport(pairs, secrets)
    content = render_block(
        "Telegram self-bot — generated by skillshub/scripts/setup_bot_envs.py", pairs
    )
    return 1 if write_if_missing(target, content, force, log) else 0


def write_whatsapp(scope_bots: Path, secrets: dict[str, str], force: bool, log: list[str]) -> int:
    phone = secrets.get("WHATSAPP_PHONE", "").strip()
    if not phone and not secrets.get("MAGICIAN_URL") and not secrets.get("MAGICIAN_BEARER_TOKEN"):
        log.append(
            "    — whatsapp: no WHATSAPP_PHONE / Magician transport settings in source; skipped"
        )
        return 0
    target = scope_bots / "whatsapp" / ".env.development"
    pairs: list[tuple[str, str]] = []
    if phone:
        pairs.append(("WHATSAPP_PHONE", phone))
    append_magician_transport(pairs, secrets)
    if not pairs:
        return 0
    content = render_block(
        "WhatsApp bot — generated by skillshub/scripts/setup_bot_envs.py", pairs
    )
    return 1 if write_if_missing(target, content, force, log) else 0


def write_gmail(
    scope_bots: Path,
    accounts: list[tuple[str, str | None]],
    secrets: dict[str, str],
    force: bool,
    log: list[str],
) -> int:
    if not accounts:
        log.append("    — gmail: accounts.txt empty; skipped")
        return 0
    # No GWS_BINARY env var written. install_bot_bundles.py installs
    # @googleworkspace/cli into <scope>/bots/gmail/node_modules/, and
    # both lookup paths (node-side resolveGwsBinary + Rust-side
    # resolve_google_workspace_binary) find `gws` there at
    # `<bot_cwd>/node_modules/.bin/gws` BEFORE any env override or the
    # legacy ../../node_modules fallback. Hardcoding an absolute path
    # here would make scope copies non-portable across machines.
    written = 0
    for account, account_email in accounts:
        target = scope_bots / "gmail" / f".env.{account}"
        pairs: list[tuple[str, str]] = [
            ("GWS_CONFIG_DIR", f"../../auth/gws-{account}"),
            ("GWS_PROFILE_LABEL", account.capitalize()),
        ]
        # Per-account email pin sources, in priority order:
        #   1. `<name>=<email>` form in accounts.txt (canonical, lives
        #      with the account list itself)
        #   2. legacy `GWS_EXPECTED_EMAIL_<NAME>` key in the repo-root
        #      secrets file (kept as a fallback for older setups)
        # When set, the gmail bot's `assertExpectedAccount` refuses to
        # bind a different Google identity at re-auth time — guards
        # against accidentally signing all bots into one account.
        legacy_key = f"GWS_EXPECTED_EMAIL_{account.upper()}"
        expected_email = account_email or secrets.get(legacy_key)
        if expected_email:
            pairs.append(("GWS_EXPECTED_EMAIL", expected_email))
        else:
            log.append(
                f"    — gmail/{account}: no expected email (neither `{account}=<email>` "
                f"in accounts.txt nor `{legacy_key}` in source) — "
                f"GWS_EXPECTED_EMAIL unset; account-mismatch validation disabled"
            )
        append_magician_transport(pairs, secrets)
        content = render_block(
            f"Gmail bot ({account}) — generated by skillshub/scripts/setup_bot_envs.py",
            pairs,
        )
        if write_if_missing(target, content, force, log):
            written += 1
    return written


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--scope", default=DEFAULT_SCOPE,
                        help="<principal>/<workspace> to seed (default: %(default)s)")
    parser.add_argument("--data-root", default=str(REPO / "magician_data_v3"),
                        help="path to magician_data_v3 (default: %(default)s)")
    parser.add_argument("--force", action="store_true",
                        help="overwrite existing per-bot env files")
    args = parser.parse_args()

    secrets = load_secrets()
    if not secrets:
        print(
            "  no operator-supplied secrets found "
            "(skillshub/operator-config.yaml absent and no legacy fallback); "
            "nothing to write."
        )
        return 0
    print(f"  loaded {len(secrets)} key(s) from operator config")

    scope_root = Path(args.data_root) / "scopes" / args.scope
    scope_bots = scope_root / "bots"
    accounts = load_gws_accounts()
    if accounts:
        names = ", ".join(name for name, _ in accounts)
        with_email = sum(1 for _, email in accounts if email)
        print(
            f"  loaded {len(accounts)} gmail account(s): {names}"
            f"  ({with_email} with expected_email pin)"
        )

    log: list[str] = []
    written = 0
    written += write_kapso(scope_bots, secrets, args.force, log)
    written += write_agentmail(scope_bots, secrets, args.force, log)
    written += write_telegram(scope_bots, secrets, args.force, log)
    written += write_telegram_self(scope_bots, secrets, args.force, log)
    written += write_whatsapp(scope_bots, secrets, args.force, log)
    written += write_gmail(scope_bots, accounts, secrets, args.force, log)
    for line in log:
        print(line)

    print(f"  done: {written} bot env file(s) written into {scope_bots}/")
    return 0


if __name__ == "__main__":
    sys.exit(main())
