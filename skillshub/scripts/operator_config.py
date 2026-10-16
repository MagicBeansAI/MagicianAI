"""Load operator-supplied secrets and Google Workspace profile metadata from the
consolidated `operator-config.yaml` file.

Replaces the legacy split where secrets lived in `<repo>/.env copy.development`
and accounts in `<repo>/accounts.txt`. The legacy paths are still
honored as a fallback when `operator-config.yaml` is absent — emits a
one-time deprecation warning at module load time so operators see the
migration prompt.

Usage:
    from operator_config import load_secrets, load_gws_accounts, load_gws_fixed_profiles

    secrets: dict[str, str] = load_secrets()
    # → {"OPENAI_API_KEY": "sk-...", ...}

    accounts: list[tuple[str, str | None]] = load_gws_accounts()
    # → [("business", "alice@example.com"), ("personal", None), ...]
"""
from __future__ import annotations

import os
import re
import sys
from pathlib import Path
from typing import Optional

import yaml

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runtime_root_shim import legacy_runtime_root  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
SKILLSHUB = REPO / "skillshub"


def runtime_root() -> Path:
    """Effective runtime root via the Task 16A compatibility shim."""
    return legacy_runtime_root(owner="operator_config")


def _resolve_config_file() -> Path:
    """operator-config.yaml location: the runtime root first (so a mounted
    container root is self-contained and matches magician's reader), then the
    in-tree `skillshub/operator-config.yaml` for dev checkouts."""
    candidate = runtime_root() / "operator-config.yaml"
    if candidate.is_file():
        return candidate
    return SKILLSHUB / "operator-config.yaml"


CONFIG_FILE = _resolve_config_file()


def _display_path(path: Path) -> str:
    """Repo-relative for in-tree paths; absolute for runtime-root paths
    (outside the repo, where `relative_to(REPO)` would raise)."""
    try:
        return str(path.relative_to(REPO))
    except ValueError:
        return str(path)

# Legacy fallback locations. First existing file wins (matches the old
# ROOT_SECRET_CANDIDATES order).
LEGACY_SECRETS_CANDIDATES = (
    ".env.copy.development",
    ".env copy.development",
    ".env.development",
)
LEGACY_ACCOUNTS_FILE = "accounts.txt"

_warned_legacy = False

ENV_REFERENCE = re.compile(r"^\$(?:\{([A-Za-z_][A-Za-z0-9_]*)\}|([A-Za-z_][A-Za-z0-9_]*))$")


def _warn_legacy_once(reason: str) -> None:
    global _warned_legacy
    if _warned_legacy:
        return
    _warned_legacy = True
    print(
        f"  [operator-config] {reason}\n"
        f"  [operator-config] migrate to {_display_path(CONFIG_FILE)} "
        f"(template: magician_data_v3/operator-config.template.yaml)",
        file=sys.stderr,
    )


def _parse_legacy_secrets(path: Path) -> dict[str, str]:
    out: dict[str, str] = {}
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        out[key.strip()] = value.strip()
    return out


def _runtime_environment() -> dict[str, str]:
    """Resolve setup-time values using the same production-first env order.

    The service loads runtime-root dotenv files at boot. Setup runs in a
    separate process, so it must read those files itself before materializing
    per-skill credential bridges. Explicit process variables win, followed by
    `.env` and then `.env.development`; this preserves a production value when
    production and development intentionally differ.
    """
    values: dict[str, str] = {}
    root = runtime_root()
    for path in (root / ".env.development", root / ".env"):
        if path.is_file():
            values.update(_parse_legacy_secrets(path))
    values.update(os.environ)
    return values


def _resolve_secret_value(value: object, environment: dict[str, str]) -> str:
    rendered = str(value)
    match = ENV_REFERENCE.fullmatch(rendered.strip())
    if match is None:
        return rendered
    name = match.group(1) or match.group(2)
    return environment.get(name, "")


def _parse_legacy_accounts(path: Path) -> list[tuple[str, Optional[str]]]:
    out: list[tuple[str, Optional[str]]] = []
    for line in path.read_text().splitlines():
        s = line.strip()
        if not s or s.startswith("#"):
            continue
        if "=" in s:
            name, email = s.split("=", 1)
            name = name.strip()
            email_v: Optional[str] = email.strip() or None
        else:
            name, email_v = s, None
        if name:
            out.append((name, email_v))
    return out


def _load_yaml() -> Optional[dict]:
    if not CONFIG_FILE.is_file():
        return None
    text = CONFIG_FILE.read_text()
    if not text.strip():
        return None
    parsed = yaml.safe_load(text)
    if parsed is None:
        return {}
    if not isinstance(parsed, dict):
        raise ValueError(
            f"{_display_path(CONFIG_FILE)}: expected a mapping at top level, "
            f"got {type(parsed).__name__}"
        )
    return parsed


def load_secrets() -> dict[str, str]:
    """Return resolved `secrets:` values without retaining `${VAR}` literals.

    Exact `$VAR` and `${VAR}` references resolve from the process environment,
    then the runtime-root production/development dotenv files. Literal values
    remain supported for backward compatibility. Returns an empty dict if
    neither the YAML file nor any legacy source exists.
    """
    yaml_data = _load_yaml()
    if yaml_data is not None:
        secrets = yaml_data.get("secrets") or {}
        if not isinstance(secrets, dict):
            raise ValueError(
                f"{_display_path(CONFIG_FILE)}: `secrets` must be a mapping, "
                f"got {type(secrets).__name__}"
            )
        # YAML auto-types small integers / booleans; downstream env writers
        # always serialize as strings. Exact env references are resolved here,
        # once, so bots and skill env materialization use the same authority.
        environment = _runtime_environment()
        return {
            str(key): _resolve_secret_value(value, environment)
            for key, value in secrets.items()
            if value is not None
        }

    for name in LEGACY_SECRETS_CANDIDATES:
        candidate = REPO / name
        if candidate.is_file():
            _warn_legacy_once(
                f"reading secrets from legacy {name} (deprecated)"
            )
            return _parse_legacy_secrets(candidate)

    return {}


def load_gws_accounts() -> list[tuple[str, Optional[str]]]:
    """Return `[(name, expected_email | None), ...]`. Empty list if
    neither the YAML `gws_accounts:` section nor a legacy
    `accounts.txt` exists."""
    yaml_data = _load_yaml()
    if yaml_data is not None:
        return _load_gws_yaml_entries(yaml_data, "gws_accounts")

    legacy = REPO / LEGACY_ACCOUNTS_FILE
    if legacy.is_file():
        _warn_legacy_once(
            f"reading gws accounts from legacy {LEGACY_ACCOUNTS_FILE} (deprecated)"
        )
        return _parse_legacy_accounts(legacy)

    return []


def _load_gws_yaml_entries(
    yaml_data: dict, key: str
) -> list[tuple[str, Optional[str]]]:
    entries = yaml_data.get(key) or []
    if not isinstance(entries, list):
        raise ValueError(
            f"{_display_path(CONFIG_FILE)}: `{key}` must be a list, "
            f"got {type(entries).__name__}"
        )
    out: list[tuple[str, Optional[str]]] = []
    for idx, entry in enumerate(entries):
        if not isinstance(entry, dict):
            raise ValueError(
                f"{_display_path(CONFIG_FILE)}: `{key}[{idx}]` must be a mapping "
                f"with `name:`, got {type(entry).__name__}"
            )
        name = entry.get("name")
        if not name or not isinstance(name, str) or not name.strip():
            raise ValueError(
                f"{_display_path(CONFIG_FILE)}: `{key}[{idx}].name` is required "
                "and must be a non-empty string"
            )
        email = entry.get("expected_email")
        email_v: Optional[str] = (
            str(email).strip() if email is not None and str(email).strip() else None
        )
        out.append((name.strip(), email_v))
    names = [name for name, _ in out]
    if len(names) != len(set(names)):
        raise ValueError(
            f"{_display_path(CONFIG_FILE)}: `{key}` profile names must be unique"
        )
    return out


def load_gws_fixed_profiles() -> list[tuple[str, Optional[str]]]:
    """Return service-owned profiles excluded from user-facing account selectors."""
    yaml_data = _load_yaml()
    if yaml_data is None:
        return []
    return _load_gws_yaml_entries(yaml_data, "gws_fixed_profiles")


def gws_account_names() -> list[str]:
    """Convenience: just the names from `gws_accounts:`."""
    return [name for name, _ in load_gws_accounts()]


def gws_profile_names() -> list[str]:
    """All auth directories required by selectable and fixed GWS profiles.

    Duplicate names fail closed so setup cannot silently make one physical auth
    directory authoritative for two different profile classes.
    """
    yaml_data = _load_yaml()
    profiles = (
        _load_gws_yaml_entries(yaml_data, "gws_accounts")
        + _load_gws_yaml_entries(yaml_data, "gws_fixed_profiles")
        if yaml_data is not None
        else load_gws_accounts()
    )
    names = [name for name, _ in profiles]
    if len(names) != len(set(names)):
        raise ValueError(
            f"{_display_path(CONFIG_FILE)}: Google Workspace profile names must be "
            "unique across `gws_accounts` and `gws_fixed_profiles`"
        )
    return names
