#!/usr/bin/env python3
"""
Fail when code changes are made without matching docs updates.
"""

from __future__ import annotations

import argparse
import fnmatch
import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Any, Iterable

DEFAULT_RULES_PATH = "docs/docs_guard_rules.json"
DOC_EXTENSIONS = (".md", ".mdx")
BYPASS_ENV = "DOCS_GUARD_BYPASS"


def run_git(args: list[str], check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(["git", *args], text=True, capture_output=True, check=check)


def git_stdout(args: list[str], check: bool = True) -> str:
    result = run_git(args, check=check)
    return result.stdout.strip()


def git_ref_exists(ref: str) -> bool:
    result = run_git(["rev-parse", "--verify", "--quiet", ref], check=False)
    return result.returncode == 0


def find_base_ref(explicit_ref: str | None) -> str:
    if explicit_ref:
        return explicit_ref

    github_base = os.getenv("GITHUB_BASE_REF", "").strip()
    if github_base:
        remote_ref = f"origin/{github_base}"
        if git_ref_exists(remote_ref):
            merge_base = git_stdout(["merge-base", "HEAD", remote_ref], check=False)
            if merge_base:
                return merge_base

    for candidate in ("origin/main", "origin/master", "main", "master", "HEAD~1"):
        if git_ref_exists(candidate):
            return candidate

    return "HEAD~1"


def changed_files(staged: bool, base_ref: str) -> list[str]:
    if staged:
        result = run_git(
            ["diff", "--cached", "--name-only", "--diff-filter=ACMR"], check=False
        )
        if result.returncode != 0:
            raise RuntimeError(result.stderr.strip() or "git diff --cached failed")
        return [line.strip() for line in result.stdout.splitlines() if line.strip()]

    primary = run_git(
        ["diff", "--name-only", "--diff-filter=ACMR", f"{base_ref}...HEAD"], check=False
    )
    if primary.returncode == 0:
        return [line.strip() for line in primary.stdout.splitlines() if line.strip()]

    fallback = run_git(
        ["diff", "--name-only", "--diff-filter=ACMR", base_ref, "HEAD"], check=False
    )
    if fallback.returncode != 0:
        msg = fallback.stderr.strip() or primary.stderr.strip() or "git diff failed"
        raise RuntimeError(msg)
    return [line.strip() for line in fallback.stdout.splitlines() if line.strip()]


def changed_working_tree_files() -> list[str]:
    tracked = run_git(
        ["diff", "--name-only", "--diff-filter=ACMR", "HEAD"], check=False
    )
    if tracked.returncode != 0:
        raise RuntimeError(tracked.stderr.strip() or "git diff HEAD failed")

    untracked = run_git(["ls-files", "--others", "--exclude-standard"], check=False)
    if untracked.returncode != 0:
        raise RuntimeError(untracked.stderr.strip() or "git ls-files failed")

    combined = [
        line.strip()
        for line in (tracked.stdout.splitlines() + untracked.stdout.splitlines())
        if line.strip()
    ]
    return list(dict.fromkeys(combined))


def normalize(path: str) -> str:
    return path.replace("\\", "/").lstrip("./")


def match_any(path: str, patterns: Iterable[str]) -> bool:
    for raw_pattern in patterns:
        pattern = normalize(raw_pattern)
        if not pattern:
            continue
        if fnmatch.fnmatch(path, pattern):
            return True
        if pattern.endswith("/") and path.startswith(pattern):
            return True
    return False


def is_doc(path: str) -> bool:
    lowered = path.lower()
    return lowered.endswith(DOC_EXTENSIONS)


def is_code(path: str, config: dict[str, Any]) -> bool:
    if match_any(path, config.get("ignore_code_patterns", [])):
        return False
    if match_any(path, config.get("always_check_patterns", [])):
        return True

    extension = Path(path).suffix.lower()
    code_extensions = {ext.lower() for ext in config.get("code_extensions", [])}
    return extension in code_extensions


def load_rules(path: Path) -> dict[str, Any]:
    try:
        with path.open("r", encoding="utf-8") as handle:
            data = json.load(handle)
    except FileNotFoundError as exc:
        raise RuntimeError(f"Rules file not found: {path}") from exc
    except json.JSONDecodeError as exc:
        raise RuntimeError(f"Invalid JSON in rules file: {path}: {exc}") from exc

    if "rules" not in data or not isinstance(data["rules"], list):
        raise RuntimeError(f"Rules file must contain a 'rules' list: {path}")
    return data


def summarize(items: list[str], limit: int = 6) -> str:
    if not items:
        return "    - none"
    head = items[:limit]
    tail = len(items) - len(head)
    if tail > 0:
        head.append(f"... (+{tail} more)")
    return "\n".join(f"    - {item}" for item in head)


def evaluate_changes(
    files: list[str], config: dict[str, Any]
) -> tuple[
    list[str],
    list[str],
    dict[str, list[str]],
    list[str],
    list[tuple[str, list[str], list[str]]],
]:
    doc_files = [path for path in files if is_doc(path)]
    code_files = [path for path in files if is_code(path, config)]

    impacted: dict[str, list[str]] = {}
    unmatched: list[str] = []

    for code_path in code_files:
        matched = False
        for rule in config["rules"]:
            if match_any(code_path, rule.get("code_patterns", [])):
                matched = True
                impacted.setdefault(rule["name"], []).append(code_path)
        if not matched:
            unmatched.append(code_path)

    missing_docs: list[tuple[str, list[str], list[str]]] = []
    for rule in config["rules"]:
        rule_name = rule["name"]
        if rule_name not in impacted:
            continue

        doc_patterns = rule.get("doc_patterns", [])
        has_doc_update = any(match_any(doc_path, doc_patterns) for doc_path in doc_files)
        if not has_doc_update:
            missing_docs.append((rule_name, impacted[rule_name], doc_patterns))

    return doc_files, code_files, impacted, unmatched, missing_docs


def print_reminder(
    source: str,
    missing_docs: list[tuple[str, list[str], list[str]]],
    unmatched: list[str],
) -> None:
    print(f"docs-reminder ({source}): documentation updates are likely needed.")
    print("")

    if missing_docs:
        print("Rules needing doc updates:")
        for rule_name, files_for_rule, doc_patterns in missing_docs:
            print(f"- {rule_name}")
            print("  changed code:")
            print(summarize(files_for_rule))
            print("  expected docs (one of):")
            print(summarize([normalize(pattern) for pattern in doc_patterns]))
        print("")

    if unmatched:
        print("Code changes with no docs ownership rule:")
        print(summarize(unmatched))
        print("")

    print("Suggested prompt for AI agents:")
    print(
        "  'Update the owned docs for the changed code paths in this repo and summarize "
        "what changed.'"
    )
    print("For Magician API/event changes in Claude Code, you can run `/sync-all-docs`.")


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Fail when code changes are not accompanied by docs changes."
    )
    parser.add_argument("--staged", action="store_true", help="Check staged files only.")
    parser.add_argument(
        "--working-tree",
        action="store_true",
        help="Check current working tree changes against HEAD (tracked + untracked).",
    )
    parser.add_argument(
        "--base-ref",
        default=None,
        help="Base ref to diff from when not using --staged.",
    )
    parser.add_argument(
        "--rules",
        default=DEFAULT_RULES_PATH,
        help=f"Path to rules file (default: {DEFAULT_RULES_PATH}).",
    )
    parser.add_argument(
        "--remind-only",
        action="store_true",
        help="Print a docs reminder instead of failing with non-zero exit.",
    )
    parser.add_argument(
        "--source",
        default="docs-guard",
        help="Label used in reminder output (default: docs-guard).",
    )
    args = parser.parse_args()

    if args.staged and args.working_tree:
        parser.error("--staged and --working-tree are mutually exclusive.")

    if os.getenv(BYPASS_ENV, "").lower() in {"1", "true", "yes"}:
        print(f"docs-guard bypassed via {BYPASS_ENV}.")
        return 0

    try:
        repo_root = Path(git_stdout(["rev-parse", "--show-toplevel"]))
    except subprocess.CalledProcessError as exc:
        sys.stderr.write(exc.stderr or "Not inside a git repository.\n")
        return 2

    if not args.remind_only:
        linters = (
            repo_root / "scripts" / "skillshub_runtime_root_linter.py",
            repo_root / "scripts" / "check_typed_storage_boundaries.py",
        )
        for linter in linters:
            if not linter.is_file():
                continue
            linter_result = subprocess.run(
                [sys.executable, str(linter), "--repo-root", str(repo_root)],
                cwd=repo_root,
            )
            if linter_result.returncode != 0:
                return linter_result.returncode

    rules_path = Path(args.rules)
    if not rules_path.is_absolute():
        rules_path = repo_root / rules_path

    try:
        config = load_rules(rules_path)
        if args.working_tree:
            files = [normalize(path) for path in changed_working_tree_files()]
        else:
            base_ref = find_base_ref(args.base_ref)
            files = [normalize(path) for path in changed_files(args.staged, base_ref)]
    except RuntimeError as exc:
        sys.stderr.write(f"docs-guard error: {exc}\n")
        return 2

    doc_files, code_files, impacted, unmatched, missing_docs = evaluate_changes(files, config)

    if not code_files:
        print("docs-guard: no relevant code changes detected.")
        return 0

    if unmatched or missing_docs:
        if args.remind_only:
            print_reminder(args.source, missing_docs, unmatched)
            return 0

        print("docs-guard failed.")
        print("")

        if missing_docs:
            print("Rules missing documentation updates:")
            for rule_name, files_for_rule, doc_patterns in missing_docs:
                print(f"- {rule_name}")
                print("  changed code:")
                print(summarize(files_for_rule))
                print("  expected docs (one of):")
                print(summarize([normalize(pattern) for pattern in doc_patterns]))
            print("")

        if unmatched:
            print("Code changes with no ownership rule:")
            print(summarize(unmatched))
            print("")

        print(f"Fix by updating docs, or bypass once with {BYPASS_ENV}=1.")
        return 1

    satisfied_rules = sorted(impacted.keys())
    print(
        "docs-guard passed: "
        f"{len(code_files)} code file(s), {len(doc_files)} doc file(s), "
        f"rules={', '.join(satisfied_rules)}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
