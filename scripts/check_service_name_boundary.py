#!/usr/bin/env python3
"""Guard the boundary between the Magician backend and user/agent identity.

This intentionally does not ban the compatibility identifier repository-wide.
It scans model- and user-facing provenance and allows only bounded technical or
explicit operational uses such as `/api/magician` and `Magician backend`.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path
from typing import Iterable, Iterator


ROOT = Path(__file__).resolve().parents[1]
NAME = re.compile(r"\bmagician\b", re.IGNORECASE)
STRING_LITERAL = re.compile(r'(["\'`])((?:\\.|(?!\1).)*)\1')
YAML_LIST_KEY = re.compile(r"^([A-Za-z_][A-Za-z0-9_-]*):\s*(?:#.*)?$")

ACTIVE_PROMPTS = (
    "atomic_composition_system_v1.0.1.json",
    "unified_analysis_system_v1.0.1.json",
    "question_rewriting_v1.0.1.json",
    "progress_review_system_v1.0.1.json",
    "learning_reflection_system_v1.10.1.json",
    "execution_output_synthesize_system_v1.0.1.json",
    "task_agent_output_synthesize_system_v1.0.1.json",
    "task_user_output_synthesize_system_v1.4.1.json",
    "public_envoy_chat_only_instruction_v1.1.0.json",
    "vibedev_execution_policy_v1.0.1.json",
    "code_distill_system_v1.0.1.json",
)

COMPILED_FALLBACKS = (
    "magician/src/magician_v2/chat/service.rs",
    "magician/src/magician_v2/agents/memory_utility_reviewer.rs",
    "magician/src/magician_v2/execution/agentic/decision.rs",
    "magician/src/magician_v2/artifact_v2/service.rs",
    "magician/src/magician_v2/execution/compiled_handlers/run_coding_task.rs",
    "ui/unified-ui/src/lib/shell/vibe/conversation/submit.ts",
)

PRESENTATION_FILES = (
    "desktop/src/orb/OrbSurface.svelte",
    "desktop/src/lib/PermissionChecklist.svelte",
    "desktop/src/lib/Settings.svelte",
    "desktop/src/lib/Setup.svelte",
    "desktop/src-tauri/src/commands.rs",
    "desktop/src-tauri/src/orb_window.rs",
    "desktop/src-tauri/src/overlay.rs",
    "desktop/src-tauri/src/permissions.rs",
    "desktop/src-tauri/src/setup.rs",
    "desktop/src-tauri/src/tray.rs",
    "desktop/src-tauri/src/voice_note.rs",
    "magios/Magios/ActivityTimelineView.swift",
    "magios/Magios/AudioSettings.swift",
    "magios/Magios/PrivacyInfo.xcprivacy",
    "magios/Magios/SettingsView.swift",
    "magios/Magios/ThinkingMapPrototypeView.swift",
    "magios/Magios/TutorOverlayViewModel.swift",
    "magios/Magios/VoiceCallPanel.swift",
    "magios/Magios/ThinkingMapCanonical/LTMAPIClient.swift",
    "magios/MagiosAction/ContextualAssistView.swift",
    "skillshub/bots/kapso/src/index.ts",
    "skillshub/bots/telegram/src/adapter.ts",
    "skillshub/bots/telegram/src/index.ts",
    "skillshub/bots/sdk/src/magician-client.ts",
    "ui/unified-ui/src/lib/media/MediaPermissionToasts.svelte",
    "ui/unified-ui/src/lib/media/voice/VoiceCallOverlay.svelte",
    "ui/unified-ui/src/lib/shared/components/BackendHealthIndicator.svelte",
    "ui/unified-ui/src/lib/magician/presto/surfaces/DoublesListSurface.ts",
    "ui/unified-ui/src/lib/storage/StorageGovernancePanel.svelte",
    "ui/unified-ui/src/lib/shell/NetworkStatusBanner.svelte",
    "ui/unified-ui/src/lib/shell/VibeComposer.svelte",
    "ui/unified-ui/src/routes/(app)/attention/+page.svelte",
    "ui/unified-ui/src/routes/(app)/api-mining/+page.svelte",
    "ui/unified-ui/src/routes/(app)/observe/+page.svelte",
    "ui/unified-ui/src/routes/(app)/settings/+page.svelte",
    "ui/unified-ui/src/routes/(app)/skills/+page.svelte",
    "ui/unified-ui/src/routes/contextual-assist/+page.svelte",
    "ui/unified-ui/src/routes/draw-overlay/+page.svelte",
)

TECHNICAL_MARKERS = (
    "/api/magician",
    "@magician/",
    "magician-config",
    "magiciannotes",
    "magician_data",
    ".cache/magician",
    ".magician/",
    "data-magician-",
    "x-magician-",
    "magician.local",
    "magician.test",
    "github.com/magicbeansai/magician",
    "https://magician.dev/",
    "docs/components/magician/",
    "magician/src/",
    "magician-vector-",
    "magician-wu.",
    ".magician",
    "magician:",
    "magician_",
    "magician.task_",
    # Hash domain separators: `hasher.update(b"magician.<area>.<thing>.v1\0")`.
    # Never user- or model-facing, and NOT renameable — the literal is an input
    # to blake3, so changing it silently invalidates every id derived from it.
    "magician.chat.",
    "magician.context-retrieval.",
    "magician.result-display.",
    # Persisted stateless-execution HMAC/domain separators. Keep these exact:
    # changing one would invalidate already-written recovery authority.
    "magician.execution-pipeline-roster.",
    "magician.delegated-child-recovery.",
    "magician.execution-stateless-children-handoff.",
    # A literal CLI invocation an agent is told to run, like `restart-magician`
    # below. The agent needs the real command name to run the real command.
    "magician app",
    "magician storage",
    "magician-",
    "$lib/magician",
    "api/magician",
    "restart-magician",
    "stop-magician",
)

OPERATIONAL_MARKERS = (
    "magician backend",
    "magician binary",
    "magician service",
)


def allowed(text: str, *, operational: bool = True) -> bool:
    lowered = text.casefold()
    if not NAME.search(text):
        return True
    markers = TECHNICAL_MARKERS + (OPERATIONAL_MARKERS if operational else ())
    return any(marker in lowered for marker in markers)


def json_strings(value: object, path: str = "$") -> Iterator[tuple[str, str]]:
    if isinstance(value, str):
        yield path, value
    elif isinstance(value, list):
        for index, item in enumerate(value):
            yield from json_strings(item, f"{path}[{index}]")
    elif isinstance(value, dict):
        for key, item in value.items():
            if path == "$" and key == "metadata":
                continue
            yield from json_strings(item, f"{path}.{key}")


def line_strings(line: str) -> Iterator[str]:
    if line.lstrip().startswith(("//", "///", "#", "<!--")):
        return
    for match in STRING_LITERAL.finditer(line):
        yield match.group(2)
    if "<" in line and ">" in line:
        visible = re.sub(r"<[^>]+>", " ", line)
        if visible.strip():
            yield visible


def allowed_wake_spelling(line: str, *, yaml_list_key: str | None) -> bool:
    """Keep recognition spellings distinct from assistant/product identity."""
    return (
        yaml_list_key == "wake_spellings"
        and line.strip().casefold() == "- magician"
    )


def check_prompt(path: Path) -> list[str]:
    data = json.loads(path.read_text(encoding="utf-8"))
    return [f"{path.relative_to(ROOT)}:{key}: {text[:180]}" for key, text in json_strings(data) if not allowed(text, operational=False)]


def check_lines(path: Path, *, literals_only: bool, operational: bool = True) -> list[str]:
    failures: list[str] = []
    yaml_list_key: str | None = None
    for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        # Stop at the test module. Match the attribute's OPENING rather than
        # `#[cfg(test)]` exactly: the crate split rewrote these modules to
        # `#[cfg(any(test, feature = "test-fixtures"))]`, after which this
        # never fired, the scan ran on into test code, and assertions written to
        # prove the name does NOT leak were reported as leaks.
        if path.suffix == ".rs" and line.lstrip().startswith(
            ("#[cfg(test)]", "#[cfg(any(test")
        ):
            break
        if path.suffix in {".yaml", ".yml"} and line == line.lstrip():
            key_match = YAML_LIST_KEY.fullmatch(line)
            if key_match:
                yaml_list_key = key_match.group(1)
            elif line.strip() and not line.lstrip().startswith(("#", "-")):
                yaml_list_key = None
        if allowed_wake_spelling(line, yaml_list_key=yaml_list_key):
            continue
        candidates: Iterable[str] = line_strings(line) if literals_only else (line,)
        for candidate in candidates:
            if not allowed(candidate, operational=operational):
                failures.append(f"{path.relative_to(ROOT)}:{number}: {candidate.strip()[:180]}")
    return failures


def tracked_model_files() -> Iterator[Path]:
    yield from (ROOT / "magician_data_v3/system/agent_templates/agents").glob("*/definition.agent.yaml")
    yield from (ROOT / "magician_data_v3/scopes").glob("*/*/agent_runtime/agents/*/definition.agent.yaml")
    yield from (ROOT / "skillshub").glob("*/SKILL.md")
    yield from (ROOT / "skillshub").glob("*/tool_schema.yaml")
    yield from (ROOT / "magician/src/magician_v2/execution/embedded_pack_defs").glob("*.yaml")


def run() -> list[str]:
    failures: list[str] = []
    prompt_root = ROOT / "data/magician_v2/prompts"
    for name in ACTIVE_PROMPTS:
        failures.extend(check_prompt(prompt_root / name))
    for path in tracked_model_files():
        failures.extend(check_lines(path, literals_only=False, operational=False))
    for relative in COMPILED_FALLBACKS + PRESENTATION_FILES:
        failures.extend(check_lines(ROOT / relative, literals_only=True))
    return failures


def main() -> int:
    failures = run()
    if failures:
        print("service-name boundary violations:", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1
    print("service-name boundary passed: active prompts, agents, tools, and presentation surfaces are clean")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
