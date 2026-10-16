"""Resolve Ollama operation settings from magician-config.yaml without defaults."""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess


def _config_path(repo_root: Path) -> Path:
    explicit = os.environ.get("MAGICIAN_CONFIG_PATH")
    if explicit:
        return Path(explicit).expanduser()
    data_root = Path(
        os.environ.get("MAGICIAN_ROOT_DIR")
        or os.environ.get("MAGICIAN_STORAGE_PATH")
        or Path.home() / "MagicianNotes"
    ).expanduser()
    for candidate in (
        data_root / "magician-config.yaml",
        repo_root / "magician-config.yaml",
    ):
        if candidate.is_file():
            return candidate
    raise RuntimeError("magician-config.yaml was not found")


def resolve_operation(repo_root: Path, operation: str) -> tuple[str, int, str]:
    ruby = shutil.which("ruby")
    if not ruby:
        raise RuntimeError("ruby is required to resolve magician-config.yaml")
    resolver = repo_root / "scripts" / "resolve-ollama-config.rb"
    result = subprocess.run(
        [ruby, str(resolver), str(_config_path(repo_root))],
        check=True,
        capture_output=True,
        text=True,
    )
    values = dict(
        line.split("=", 1)
        for line in result.stdout.splitlines()
        if "=" in line
    )
    prefix = f"operation_{operation}"
    try:
        return (
            values[f"{prefix}_model"],
            int(values[f"{prefix}_context_tokens"]),
            values[f"{prefix}_endpoint"],
        )
    except (KeyError, ValueError) as error:
        raise RuntimeError(
            f"Ollama operation {operation!r} is not fully configured"
        ) from error
