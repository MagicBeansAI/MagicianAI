from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess

import pytest


ROOT = Path(__file__).resolve().parents[3]


def adapter(skill: str) -> Path:
    return ROOT / "skillshub" / skill / "bin" / "kapso-governed"


def environment(tmp_path: Path) -> dict[str, str]:
    fake = tmp_path / "kapso"
    fake.write_text("#!/usr/bin/env bash\nprintf '%s\\n' \"$@\"\n", encoding="utf-8")
    fake.chmod(0o755)
    value = os.environ.copy()
    value.update(
        {
            "PATH": f"{tmp_path}:/usr/bin:/bin",
            "KAPSO_API_KEY": "test-key",
            "KAPSO_PHONE_NUMBER_ID": "fixed-number",
        }
    )
    return value


def test_cli_is_exact_pinned_once_in_the_shared_skillshub_workspace() -> None:
    package = json.loads((ROOT / "skillshub" / "package.json").read_text(encoding="utf-8"))
    lock = json.loads(
        (ROOT / "skillshub" / "package-lock.json").read_text(encoding="utf-8")
    )
    declared = package["dependencies"]["@kapso/cli"]

    assert declared
    assert not declared.startswith(("^", "~", ">", "<", "*"))
    assert lock["packages"][""]["dependencies"]["@kapso/cli"] == declared
    assert lock["packages"]["node_modules/@kapso/cli"]["version"] == declared

    makefile = (ROOT / "skillshub" / "Makefile").read_text(encoding="utf-8")
    assert "setup-kapso-cli: setup-deps" in makefile
    assert "KAPSO_CLI_BIN := node_modules/.bin/kapso" in makefile
    assert "kapso-whatsapp-read/scripts/install.sh" not in makefile


@pytest.mark.parametrize("skill", ["kapso-whatsapp-read", "kapso-whatsapp-send"])
@pytest.mark.parametrize(
    "selector",
    ["--phone-number", "--phone-number=other", "--phone-number-id", "--phone-number-id=other"],
)
def test_fixed_identity_rejects_every_caller_number_selector(
    tmp_path: Path, skill: str, selector: str
) -> None:
    result = subprocess.run(
        [adapter(skill), "whatsapp", "messages", "list", selector, "other"],
        env=environment(tmp_path),
        text=True,
        capture_output=True,
        check=False,
    )
    assert result.returncode == 64
    assert result.stdout == ""
    assert "identity" in result.stderr


def test_read_injects_identity_only_for_the_exact_scoped_operations(tmp_path: Path) -> None:
    env = environment(tmp_path)
    scoped = subprocess.run(
        [adapter("kapso-whatsapp-read"), "whatsapp", "messages", "list"],
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )
    assert scoped.returncode == 0
    assert scoped.stdout.splitlines() == [
        "whatsapp",
        "messages",
        "list",
        "--phone-number-id",
        "fixed-number",
        "--output",
        "json",
    ]

    project_wide = subprocess.run(
        [adapter("kapso-whatsapp-read"), "whatsapp", "numbers", "list"],
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )
    assert project_wide.returncode == 0
    assert project_wide.stdout.splitlines() == [
        "whatsapp",
        "numbers",
        "list",
        "--output",
        "json",
    ]


def test_send_always_uses_the_fixed_identity(tmp_path: Path) -> None:
    result = subprocess.run(
        [adapter("kapso-whatsapp-send"), "whatsapp", "messages", "send", "--to", "+1"],
        env=environment(tmp_path),
        text=True,
        capture_output=True,
        check=False,
    )
    assert result.returncode == 0
    assert result.stdout.splitlines()[-4:] == [
        "--phone-number-id",
        "fixed-number",
        "--output",
        "json",
    ]
