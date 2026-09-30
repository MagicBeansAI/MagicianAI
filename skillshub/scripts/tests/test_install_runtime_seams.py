"""Expose installer/build-profile seam tests to the standard skill test lane."""

from __future__ import annotations

import importlib.util
from pathlib import Path


SCRIPTS_ROOT = Path(__file__).resolve().parent.parent


def load(name: str, filename: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS_ROOT / filename)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


_installer = load("installer_seam_tests", "test_install_skill_layer.py")
_profile = load(
    "document_to_markdown_profile_tests",
    "test_document_to_markdown_build_profile.py",
)

# unittest discovery collects imported TestCase subclasses under these names.
InstallSkillLayerTests = _installer.InstallSkillLayerTests
DocumentToMarkdownBuildProfileTests = _profile.DocumentToMarkdownBuildProfileTests
