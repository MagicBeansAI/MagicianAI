from pathlib import Path

from skill_layer import governed_auth_kind, needs_scope


def write_skill(root: Path, auth: str | None) -> Path:
    skill = root / "fixture"
    skill.mkdir()
    auth_block = "" if auth is None else f"      auth: {{kind: {auth}, requirement: required}}\n"
    (skill / "SKILL.md").write_text(
        "---\n"
        "name: fixture\n"
        "metadata:\n"
        "  magician:\n"
        "    runtime_contract:\n"
        "      schema_version: tool-runtime.skill-runtime.v1\n"
        f"{auth_block}"
        "---\n"
        "# Fixture\n",
        encoding="utf-8",
    )
    return skill


def test_governed_authenticated_skill_is_scope_owned_without_legacy_schema(tmp_path: Path) -> None:
    skill = write_skill(tmp_path, "oauth_session")

    assert governed_auth_kind(skill) == "oauth_session"
    assert needs_scope(skill)


def test_governed_unauthenticated_skill_remains_system_eligible(tmp_path: Path) -> None:
    skill = write_skill(tmp_path, "none")

    assert governed_auth_kind(skill) == "none"
    assert not needs_scope(skill)


def test_missing_or_malformed_frontmatter_does_not_invent_scope_auth(tmp_path: Path) -> None:
    skill = tmp_path / "fixture"
    skill.mkdir()
    (skill / "SKILL.md").write_text("# no frontmatter\n", encoding="utf-8")

    assert governed_auth_kind(skill) is None
    assert not needs_scope(skill)
