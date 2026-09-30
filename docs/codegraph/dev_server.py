#!/usr/bin/env python3
"""Code graph static server with a dev-only allowlisted make runner API.

Serves docs/codegraph assets and exposes:
- GET  /api/health
- GET  /api/make/targets
- POST /api/make/run   {"target": "graph-index"}
- GET  /api/flow/sources
- GET  /api/flow/mock?source_id=...
- POST /api/flow/simulate {"source_id":"...","payload":{...},"max_hops":6,"max_traces":24}
- POST /api/flow/delta {"source_id":"...","base_ref":"origin/main","compare_mode":"workspace|head"}
- GET  /api/file/read?path=...
- GET  /api/file/stat?path=...
- POST /api/file/write {"path":"...","content":"..."}
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import time
from http import HTTPStatus
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from urllib.parse import parse_qs, unquote, urlparse

CODEGRAPH_DIR = Path(__file__).resolve().parent
REPO_ROOT = CODEGRAPH_DIR.parents[1]
SCRIPTS_DIR = REPO_ROOT / "scripts"
MAX_OUTPUT_CHARS = 200_000
DEFAULT_TIMEOUT_SECONDS = 60 * 30
MAX_REQUEST_BYTES = 64 * 1024
GIT_COMMAND_TIMEOUT_SECONDS = 12
MAX_FILE_READ_BYTES = 2 * 1024 * 1024
MAX_FILE_WRITE_BYTES = 2 * 1024 * 1024
GRAPH_JSON_PATH = CODEGRAPH_DIR / "graph.json"
PAYLOAD_PROFILES_PATH = CODEGRAPH_DIR / "payload_profiles.json"
FLOW_SNAPSHOT_ROOT = Path("/tmp/codegraph_flow_snapshots")

ALLOWED_TARGETS: dict[str, str] = {
    "graph-index": "Regenerate code graph artifacts.",
    "graph-check": "Validate generated graph artifact schema.",
}

ALLOWED_IMPACT_MODES = {"workspace", "head"}

try:
    if str(SCRIPTS_DIR) not in sys.path:
        sys.path.insert(0, str(SCRIPTS_DIR))
    import simulate_code_flow as flow_sim  # type: ignore[import-not-found]
except Exception:
    flow_sim = None

try:
    import query_code_graph as graph_query  # type: ignore[import-not-found]
except Exception:
    graph_query = None

try:
    import find_dead_code as dead_code  # type: ignore[import-not-found]
except Exception:
    dead_code = None

try:
    import test_coverage as test_cov  # type: ignore[import-not-found]
except Exception:
    test_cov = None

try:
    import audit_code_graph as audit_mod  # type: ignore[import-not-found]
except Exception:
    audit_mod = None

try:
    import find_flows as flows_mod  # type: ignore[import-not-found]
except Exception:
    flows_mod = None

# Codegraph extensions: each registers extra HTTP routes via `http_routes()`.
# We collect them once at module load and dispatch in `do_GET` below.
try:
    from codegraph_ext import load_extensions  # type: ignore
    _EXTENSIONS = load_extensions()
except Exception as _exc:
    print(f"[dev_server] extension loader failed: {_exc}", file=sys.stderr)
    _EXTENSIONS = []
_EXTENSION_ROUTES: dict = {}
for _ext in _EXTENSIONS:
    try:
        for _route, _handler in _ext.http_routes().items():
            _EXTENSION_ROUTES[_route] = _handler
    except Exception as _exc:
        print(f"[dev_server] extension {_ext.name} http_routes failed: {_exc}",
              file=sys.stderr)


def _trim_output(value: str) -> str:
    if len(value) <= MAX_OUTPUT_CHARS:
        return value
    clipped = value[-MAX_OUTPUT_CHARS:]
    return f"[output truncated to last {MAX_OUTPUT_CHARS} chars]\n{clipped}"


def _run_git_command(args: list[str], timeout_seconds: int = GIT_COMMAND_TIMEOUT_SECONDS) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args],
        cwd=str(REPO_ROOT),
        capture_output=True,
        text=True,
        timeout=max(1, timeout_seconds),
        env=os.environ.copy(),
        check=False,
    )


def _parse_changed_paths(raw_text: str) -> list[str]:
    changed: list[str] = []
    seen: set[str] = set()
    for line in raw_text.splitlines():
        path = line.strip()
        if not path or path in seen:
            continue
        seen.add(path)
        changed.append(path)
    return changed


def _choose_default_base_ref(available_refs: list[str], current_branch: str) -> str:
    preferred = ["origin/main", "origin/master", "main", "master"]
    for candidate in preferred:
        if candidate in available_refs:
            return candidate
    if current_branch and current_branch in available_refs:
        return current_branch
    if available_refs:
        return available_refs[0]
    return current_branch or "HEAD"


def _resolve_repo_relative_path(raw_path: str) -> tuple[Path | None, str | None]:
    candidate = str(raw_path or "").strip()
    if not candidate:
        return None, "path is required"

    # Keep file access constrained to repository-relative paths.
    if Path(candidate).is_absolute():
        return None, "path must be repository-relative"

    resolved = (REPO_ROOT / candidate).resolve()
    try:
        resolved.relative_to(REPO_ROOT)
    except ValueError:
        return None, "path escapes repository root"

    return resolved, None


def _repo_relative_display_path(path: Path) -> str:
    return str(path.resolve().relative_to(REPO_ROOT)).replace(os.sep, "/")


def _load_flow_artifacts() -> tuple[dict[str, Any], dict[str, Any]]:
    if not GRAPH_JSON_PATH.exists():
        raise FileNotFoundError(f"graph artifact missing: {GRAPH_JSON_PATH}")
    graph = json.loads(GRAPH_JSON_PATH.read_text(encoding="utf-8"))

    if PAYLOAD_PROFILES_PATH.exists():
        payload_profiles = json.loads(PAYLOAD_PROFILES_PATH.read_text(encoding="utf-8"))
    else:
        payload_profiles = {"profiles": []}

    return graph, payload_profiles


def _resolve_git_commit(ref_name: str) -> str:
    candidate = str(ref_name or "").strip()
    if not candidate:
        raise ValueError("base_ref is required")
    resolved = _run_git_command(["rev-parse", "--verify", "--quiet", f"{candidate}^{{commit}}"])
    if resolved.returncode != 0:
        raise ValueError(f"Could not resolve git ref '{candidate}'")
    commit = resolved.stdout.strip()
    if not commit:
        raise ValueError(f"Could not resolve git ref '{candidate}'")
    return commit


def _snapshot_dir_for_commit(commit: str) -> Path:
    digest = hashlib.sha1(commit.encode("utf-8")).hexdigest()[:12]
    return FLOW_SNAPSHOT_ROOT / digest


def _ensure_snapshot_checkout(commit: str) -> Path:
    snapshot_dir = _snapshot_dir_for_commit(commit)
    manifest = snapshot_dir / "Cargo.toml"
    if manifest.exists():
        return snapshot_dir

    snapshot_dir.mkdir(parents=True, exist_ok=True)
    archive = subprocess.Popen(
        ["git", "archive", commit],
        cwd=str(REPO_ROOT),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=os.environ.copy(),
    )
    assert archive.stdout is not None
    tar_run = subprocess.run(
        ["tar", "-x", "-C", str(snapshot_dir)],
        stdin=archive.stdout,
        capture_output=True,
        text=True,
        env=os.environ.copy(),
        check=False,
    )
    archive.stdout.close()
    archive_stderr = (archive.stderr.read().decode("utf-8", "replace") if archive.stderr else "").strip()
    archive.wait(timeout=20)
    if archive.returncode != 0 or tar_run.returncode != 0:
        raise RuntimeError(
            "Could not materialize git snapshot: "
            + " ".join(part for part in [archive_stderr, tar_run.stderr.strip()] if part)
        )
    return snapshot_dir


def _ensure_snapshot_artifacts(snapshot_root: Path) -> tuple[dict[str, Any], dict[str, Any], str]:
    graph_path = snapshot_root / "docs" / "codegraph" / "graph.json"
    payload_path = snapshot_root / "docs" / "codegraph" / "payload_profiles.json"
    stats_path = snapshot_root / "docs" / "codegraph" / "stats.json"
    if not graph_path.exists() or not payload_path.exists():
        command = [
            sys.executable,
            str(REPO_ROOT / "scripts" / "generate_code_graph.py"),
            "--root",
            str(snapshot_root),
            "--output",
            "docs/codegraph/graph.json",
            "--stats",
            "docs/codegraph/stats.json",
            "--payload",
            "docs/codegraph/payload_profiles.json",
        ]
        completed = subprocess.run(
            command,
            cwd=str(REPO_ROOT),
            capture_output=True,
            text=True,
            timeout=60 * 8,
            env=os.environ.copy(),
            check=False,
        )
        if completed.returncode != 0:
            message = completed.stderr.strip() or completed.stdout.strip() or "unknown error"
            raise RuntimeError(f"Snapshot graph generation failed: {message}")
    graph = json.loads(graph_path.read_text(encoding="utf-8"))
    payload_profiles = (
        json.loads(payload_path.read_text(encoding="utf-8"))
        if payload_path.exists()
        else {"profiles": []}
    )
    return graph, payload_profiles, str(stats_path)


def _find_endpoint_source_id(
    source_id: str,
    source_graph: dict[str, Any],
    target_graph: dict[str, Any],
) -> str | None:
    source_id = str(source_id or "").strip()
    if not source_id:
        return None
    target_nodes = target_graph.get("nodes", [])
    for node in target_nodes:
        if str(node.get("id", "")) == source_id:
            return source_id

    source_node = next(
        (node for node in source_graph.get("nodes", []) if str(node.get("id", "")) == source_id),
        None,
    )
    if not source_node:
        return None

    method = str(source_node.get("method", "")).upper()
    route = str(source_node.get("route", ""))
    path = str(source_node.get("path", ""))
    handler = str(source_node.get("handler", ""))

    matches = []
    for node in target_nodes:
        if str(node.get("kind", "")) != "endpoint":
            continue
        if str(node.get("method", "")).upper() != method:
            continue
        if str(node.get("route", "")) != route:
            continue
        matches.append(node)

    if len(matches) == 1:
        return str(matches[0].get("id", ""))
    if len(matches) > 1:
        for node in matches:
            if str(node.get("path", "")) == path and str(node.get("handler", "")) == handler:
                return str(node.get("id", ""))
        for node in matches:
            if str(node.get("path", "")) == path:
                return str(node.get("id", ""))
        return str(matches[0].get("id", ""))

    return None


def _trace_signature(trace: dict[str, Any]) -> str:
    nodes = trace.get("nodes", [])
    if not isinstance(nodes, list):
        return ""
    parts = []
    for node in nodes:
        kind = str(node.get("kind", "unknown"))
        label = str(node.get("label") or node.get("id") or "")
        parts.append(f"{kind}:{label}")
    return " -> ".join(parts)


def _severity_counts(findings: list[dict[str, Any]]) -> dict[str, int]:
    counts = {"high": 0, "medium": 0, "low": 0, "info": 0}
    for finding in findings:
        key = str(finding.get("severity", "info"))
        if key not in counts:
            key = "info"
        counts[key] += 1
    return counts


def _compare_trace_sets(
    current_traces: list[dict[str, Any]],
    base_traces: list[dict[str, Any]],
    limit: int = 20,
) -> dict[str, Any]:
    current_by_sig = {sig: trace for trace in current_traces if (sig := _trace_signature(trace))}
    base_by_sig = {sig: trace for trace in base_traces if (sig := _trace_signature(trace))}

    current_keys = set(current_by_sig.keys())
    base_keys = set(base_by_sig.keys())
    added_keys = sorted(current_keys - base_keys)
    removed_keys = sorted(base_keys - current_keys)
    unchanged_keys = sorted(current_keys & base_keys)

    return {
        "added_count": len(added_keys),
        "removed_count": len(removed_keys),
        "unchanged_count": len(unchanged_keys),
        "added_traces": [current_by_sig[key] for key in added_keys[:limit]],
        "removed_traces": [base_by_sig[key] for key in removed_keys[:limit]],
    }


class CodeGraphDevHandler(SimpleHTTPRequestHandler):
    def __init__(self, *args: Any, **kwargs: Any) -> None:
        super().__init__(*args, directory=str(CODEGRAPH_DIR), **kwargs)

    def end_headers(self) -> None:
        # Disable caching for dev server so JS/CSS changes take effect
        # immediately. Belt-and-suspenders across all proxy / older
        # browser variants — some still honour Pragma / Expires even
        # in incognito.
        self.send_header("Cache-Control", "no-cache, no-store, must-revalidate, max-age=0")
        self.send_header("Pragma", "no-cache")
        self.send_header("Expires", "0")
        super().end_headers()

    def _cors_headers(self) -> dict[str, str]:
        return {
            "Access-Control-Allow-Origin": "*",
            "Access-Control-Allow-Methods": "GET, POST, OPTIONS",
            "Access-Control-Allow-Headers": "Content-Type",
            "Access-Control-Max-Age": "300",
        }

    def _send_json(self, status: HTTPStatus, payload: dict[str, Any]) -> None:
        raw = json.dumps(payload, ensure_ascii=True).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(raw)))
        for key, value in self._cors_headers().items():
            self.send_header(key, value)
        self.end_headers()
        self.wfile.write(raw)

    def do_OPTIONS(self) -> None:  # noqa: N802
        self.send_response(HTTPStatus.NO_CONTENT)
        for key, value in self._cors_headers().items():
            self.send_header(key, value)
        self.end_headers()

    def do_GET(self) -> None:  # noqa: N802
        parsed = urlparse(self.path)
        route = parsed.path

        if route == "/api/health":
            self._send_json(
                HTTPStatus.OK,
                {
                    "status": "ok",
                    "service": "codegraph-dev-server",
                    "repo_root": str(REPO_ROOT),
                },
            )
            return

        # Repo-docs viewer route for the explorer views: the static
        # docroot is docs/codegraph/, so curated doc links (e.g. the C4
        # canvas inspector's "Open doc") would otherwise 404.
        if route.startswith("/docs/"):
            self._handle_repo_doc(parsed)
            return

        if route == "/api/make/targets":
            targets = [
                {"name": name, "description": description}
                for name, description in ALLOWED_TARGETS.items()
            ]
            self._send_json(HTTPStatus.OK, {"targets": targets})
            return

        if route == "/api/git/refs":
            self._handle_git_refs()
            return

        if route == "/api/flow/sources":
            self._handle_flow_sources()
            return

        if route == "/api/flow/mock":
            self._handle_flow_mock(parsed)
            return

        if route == "/api/file/read":
            self._handle_file_read(parsed)
            return

        if route == "/api/file/stat":
            self._handle_file_stat(parsed)
            return

        if route == "/api/query":
            self._handle_graph_query(parsed)
            return

        if route == "/api/dead-code":
            self._handle_dead_code(parsed)
            return

        if route == "/api/test-coverage":
            self._handle_test_coverage(parsed)
            return

        if route == "/api/contracts":
            self._handle_contracts(parsed)
            return

        if route == "/api/audit":
            self._handle_audit(parsed)
            return

        if route == "/api/flows":
            self._handle_flows(parsed)
            return

        # Discovery endpoint: every loaded extension's slash-command
        # specs. The viewers fetch this once at startup and build
        # their command palette + dispatcher from the response —
        # no core-file branches required per extension.
        if route == "/api/extensions":
            specs = []
            for ext in _EXTENSIONS:
                try:
                    cmds = ext.slash_commands()
                except Exception as exc:
                    specs.append({
                        "name": getattr(ext, "name", "?"),
                        "error": str(exc),
                        "slash_commands": [],
                    })
                    continue
                specs.append({
                    "name": getattr(ext, "name", "?"),
                    "slash_commands": list(cmds or []),
                })
            self._send_json(HTTPStatus.OK, {"extensions": specs})
            return

        # Extension-registered routes — each handler returns a dict
        # which we wrap into a JSON response. Exceptions are surfaced
        # as HTTP 500 so a flaky extension can't crash the server.
        ext_handler = _EXTENSION_ROUTES.get(route)
        if ext_handler is not None:
            try:
                payload = ext_handler(parsed)
            except Exception as exc:
                self._send_json(
                    HTTPStatus.INTERNAL_SERVER_ERROR,
                    {"error": "extension_failed", "message": str(exc)},
                )
                return
            if not isinstance(payload, dict):
                payload = {"result": payload}
            self._send_json(HTTPStatus.OK, payload)
            return

        super().do_GET()

    def do_POST(self) -> None:  # noqa: N802
        parsed = urlparse(self.path)
        route = parsed.path

        if route == "/api/make/run":
            self._handle_make_run()
            return

        if route == "/api/git/impact":
            self._handle_git_impact()
            return

        if route == "/api/flow/simulate":
            self._handle_flow_simulate()
            return

        if route == "/api/flow/delta":
            self._handle_flow_delta()
            return

        if route == "/api/file/write":
            self._handle_file_write()
            return

        self._send_json(
            HTTPStatus.NOT_FOUND,
            {
                "error": "unknown_endpoint",
                "message": f"Unknown endpoint: {self.path}",
            },
        )

    def _read_json_request(self) -> tuple[dict[str, Any] | None, HTTPStatus | None, dict[str, Any] | None]:
        content_length_header = self.headers.get("Content-Length")
        if content_length_header is None:
            return None, HTTPStatus.BAD_REQUEST, {
                "error": "missing_content_length",
                "message": "Content-Length header is required",
            }

        try:
            content_length = int(content_length_header)
        except ValueError:
            return None, HTTPStatus.BAD_REQUEST, {
                "error": "invalid_content_length",
                "message": "Content-Length must be an integer",
            }

        if content_length < 0 or content_length > MAX_REQUEST_BYTES:
            return None, HTTPStatus.BAD_REQUEST, {
                "error": "payload_too_large",
                "message": f"Payload must be <= {MAX_REQUEST_BYTES} bytes",
            }

        raw = self.rfile.read(content_length)
        try:
            return json.loads(raw.decode("utf-8")), None, None
        except json.JSONDecodeError:
            return None, HTTPStatus.BAD_REQUEST, {
                "error": "invalid_json",
                "message": "Request body must be valid JSON",
            }

    def _handle_make_run(self) -> None:
        body, error_status, error_payload = self._read_json_request()
        if error_status is not None and error_payload is not None:
            self._send_json(error_status, error_payload)
            return
        if body is None:
            self._send_json(
                HTTPStatus.INTERNAL_SERVER_ERROR,
                {"error": "request_read_failed", "message": "Could not read request body"},
            )
            return

        target = str(body.get("target", "")).strip()
        if target not in ALLOWED_TARGETS:
            self._send_json(
                HTTPStatus.BAD_REQUEST,
                {
                    "error": "invalid_target",
                    "message": f"Target '{target}' is not allowlisted",
                    "allowed_targets": sorted(ALLOWED_TARGETS.keys()),
                },
            )
            return

        timeout_seconds = body.get("timeout_seconds", DEFAULT_TIMEOUT_SECONDS)
        try:
            timeout = max(1, min(int(timeout_seconds), DEFAULT_TIMEOUT_SECONDS))
        except (ValueError, TypeError):
            timeout = DEFAULT_TIMEOUT_SECONDS

        command = ["make", target]
        started_at = time.perf_counter()
        try:
            completed = subprocess.run(
                command,
                cwd=str(REPO_ROOT),
                capture_output=True,
                text=True,
                timeout=timeout,
                env=os.environ.copy(),
                check=False,
            )
            elapsed_ms = int((time.perf_counter() - started_at) * 1000)
            self._send_json(
                HTTPStatus.OK,
                {
                    "target": target,
                    "command": command,
                    "exit_code": completed.returncode,
                    "duration_ms": elapsed_ms,
                    "timed_out": False,
                    "stdout": _trim_output(completed.stdout or ""),
                    "stderr": _trim_output(completed.stderr or ""),
                },
            )
        except subprocess.TimeoutExpired as exc:
            elapsed_ms = int((time.perf_counter() - started_at) * 1000)
            stdout = exc.stdout if isinstance(exc.stdout, str) else (exc.stdout or b"").decode("utf-8", "replace")
            stderr = exc.stderr if isinstance(exc.stderr, str) else (exc.stderr or b"").decode("utf-8", "replace")
            self._send_json(
                HTTPStatus.REQUEST_TIMEOUT,
                {
                    "target": target,
                    "command": command,
                    "exit_code": None,
                    "duration_ms": elapsed_ms,
                    "timed_out": True,
                    "stdout": _trim_output(stdout),
                    "stderr": _trim_output(stderr),
                    "message": f"Command timed out after {timeout} seconds",
                },
            )

    def _handle_flow_sources(self) -> None:
        if flow_sim is None:
            self._send_json(
                HTTPStatus.SERVICE_UNAVAILABLE,
                {
                    "error": "flow_sim_unavailable",
                    "message": "simulate_code_flow module is not available.",
                },
            )
            return

        try:
            graph, payload_profiles = _load_flow_artifacts()
            sources = flow_sim.list_sources(graph, payload_profiles)
            self._send_json(
                HTTPStatus.OK,
                {
                    "sources": sources,
                    "source_count": len(sources),
                    "graph_path": str(GRAPH_JSON_PATH),
                    "payload_profiles_path": str(PAYLOAD_PROFILES_PATH),
                },
            )
        except FileNotFoundError as exc:
            self._send_json(
                HTTPStatus.NOT_FOUND,
                {
                    "error": "flow_artifact_missing",
                    "message": str(exc),
                },
            )
        except Exception as exc:
            self._send_json(
                HTTPStatus.INTERNAL_SERVER_ERROR,
                {
                    "error": "flow_sources_failed",
                    "message": str(exc),
                },
            )

    def _handle_flow_mock(self, parsed_url: Any) -> None:
        if flow_sim is None:
            self._send_json(
                HTTPStatus.SERVICE_UNAVAILABLE,
                {
                    "error": "flow_sim_unavailable",
                    "message": "simulate_code_flow module is not available.",
                },
            )
            return

        query = parse_qs(parsed_url.query, keep_blank_values=False)
        source_id = str((query.get("source_id") or [""])[0]).strip()
        if not source_id:
            self._send_json(
                HTTPStatus.BAD_REQUEST,
                {
                    "error": "invalid_source_id",
                    "message": "source_id query parameter is required",
                },
            )
            return

        try:
            _graph, payload_profiles = _load_flow_artifacts()
            payload = flow_sim.mock_payload_for_source(payload_profiles, source_id)
            self._send_json(
                HTTPStatus.OK,
                {
                    "source_id": source_id,
                    "payload": payload,
                },
            )
        except FileNotFoundError as exc:
            self._send_json(
                HTTPStatus.NOT_FOUND,
                {
                    "error": "flow_artifact_missing",
                    "message": str(exc),
                },
            )
        except Exception as exc:
            self._send_json(
                HTTPStatus.INTERNAL_SERVER_ERROR,
                {
                    "error": "flow_mock_failed",
                    "message": str(exc),
                },
            )

    def _handle_flow_simulate(self) -> None:
        if flow_sim is None:
            self._send_json(
                HTTPStatus.SERVICE_UNAVAILABLE,
                {
                    "error": "flow_sim_unavailable",
                    "message": "simulate_code_flow module is not available.",
                },
            )
            return

        body, error_status, error_payload = self._read_json_request()
        if error_status is not None and error_payload is not None:
            self._send_json(error_status, error_payload)
            return
        if body is None:
            self._send_json(
                HTTPStatus.INTERNAL_SERVER_ERROR,
                {"error": "request_read_failed", "message": "Could not read request body"},
            )
            return

        source_id = str(body.get("source_id", "")).strip()
        if not source_id:
            self._send_json(
                HTTPStatus.BAD_REQUEST,
                {
                    "error": "invalid_source_id",
                    "message": "source_id is required",
                },
            )
            return

        payload = body.get("payload")
        max_hops_raw = body.get("max_hops", 6)
        max_traces_raw = body.get("max_traces", 12)
        try:
            max_hops = max(1, min(int(max_hops_raw), 12))
        except (TypeError, ValueError):
            max_hops = 6
        try:
            max_traces = max(1, min(int(max_traces_raw), 40))
        except (TypeError, ValueError):
            max_traces = 12

        try:
            graph, payload_profiles = _load_flow_artifacts()
            if payload is None:
                payload = flow_sim.mock_payload_for_source(payload_profiles, source_id)
            result = flow_sim.simulate_flow(
                graph=graph,
                payload_index=payload_profiles,
                source_id=source_id,
                payload=payload,
                max_hops=max_hops,
                max_traces=max_traces,
            )
            self._send_json(
                HTTPStatus.OK,
                {
                    "result": result,
                },
            )
        except FileNotFoundError as exc:
            self._send_json(
                HTTPStatus.NOT_FOUND,
                {
                    "error": "flow_artifact_missing",
                    "message": str(exc),
                },
            )
        except ValueError as exc:
            self._send_json(
                HTTPStatus.BAD_REQUEST,
                {
                    "error": "flow_invalid_request",
                    "message": str(exc),
                },
            )
        except Exception as exc:
            self._send_json(
                HTTPStatus.INTERNAL_SERVER_ERROR,
                {
                    "error": "flow_simulation_failed",
                    "message": str(exc),
                },
            )

    def _handle_flow_delta(self) -> None:
        if flow_sim is None:
            self._send_json(
                HTTPStatus.SERVICE_UNAVAILABLE,
                {
                    "error": "flow_sim_unavailable",
                    "message": "simulate_code_flow module is not available.",
                },
            )
            return

        body, error_status, error_payload = self._read_json_request()
        if error_status is not None and error_payload is not None:
            self._send_json(error_status, error_payload)
            return
        if body is None:
            self._send_json(
                HTTPStatus.INTERNAL_SERVER_ERROR,
                {"error": "request_read_failed", "message": "Could not read request body"},
            )
            return

        source_id = str(body.get("source_id", "")).strip()
        base_ref = str(body.get("base_ref", "")).strip()
        compare_mode = str(body.get("compare_mode", "workspace")).strip().lower()
        payload = body.get("payload")
        max_hops_raw = body.get("max_hops", 6)
        max_traces_raw = body.get("max_traces", 12)

        if not source_id:
            self._send_json(
                HTTPStatus.BAD_REQUEST,
                {"error": "invalid_source_id", "message": "source_id is required"},
            )
            return
        if not base_ref:
            self._send_json(
                HTTPStatus.BAD_REQUEST,
                {"error": "invalid_base_ref", "message": "base_ref is required"},
            )
            return
        if compare_mode not in ALLOWED_IMPACT_MODES:
            self._send_json(
                HTTPStatus.BAD_REQUEST,
                {
                    "error": "invalid_compare_mode",
                    "message": f"compare_mode must be one of: {sorted(ALLOWED_IMPACT_MODES)}",
                },
            )
            return

        try:
            max_hops = max(1, min(int(max_hops_raw), 12))
        except (TypeError, ValueError):
            max_hops = 6
        try:
            max_traces = max(1, min(int(max_traces_raw), 40))
        except (TypeError, ValueError):
            max_traces = 12

        try:
            current_graph, current_payload = _load_flow_artifacts()
            source_id_current = _find_endpoint_source_id(source_id, current_graph, current_graph)
            if not source_id_current:
                raise ValueError(f"Could not resolve source endpoint '{source_id}' in current graph artifact.")

            if payload is None:
                payload = flow_sim.mock_payload_for_source(current_payload, source_id_current)

            if compare_mode == "head":
                head_commit = _resolve_git_commit("HEAD")
                head_snapshot = _ensure_snapshot_checkout(head_commit)
                compare_graph, compare_payload, compare_stats_path = _ensure_snapshot_artifacts(head_snapshot)
                compare_source_id = _find_endpoint_source_id(source_id_current, current_graph, compare_graph)
                if not compare_source_id:
                    raise ValueError("Source endpoint does not exist in HEAD snapshot.")
                current_result = flow_sim.simulate_flow(
                    graph=compare_graph,
                    payload_index=compare_payload,
                    source_id=compare_source_id,
                    payload=payload,
                    max_hops=max_hops,
                    max_traces=max_traces,
                )
                current_snapshot_commit = head_commit
                current_snapshot_stats = compare_stats_path
            else:
                current_result = flow_sim.simulate_flow(
                    graph=current_graph,
                    payload_index=current_payload,
                    source_id=source_id_current,
                    payload=payload,
                    max_hops=max_hops,
                    max_traces=max_traces,
                )
                current_snapshot_commit = _resolve_git_commit("HEAD")
                current_snapshot_stats = str(CODEGRAPH_DIR / "stats.json")

            base_commit = _resolve_git_commit(base_ref)
            base_snapshot = _ensure_snapshot_checkout(base_commit)
            base_graph, base_payload, base_stats_path = _ensure_snapshot_artifacts(base_snapshot)
            base_source_id = _find_endpoint_source_id(source_id_current, current_graph, base_graph)
            if not base_source_id:
                raise ValueError("Source endpoint does not exist in base snapshot.")

            base_result = flow_sim.simulate_flow(
                graph=base_graph,
                payload_index=base_payload,
                source_id=base_source_id,
                payload=payload,
                max_hops=max_hops,
                max_traces=max_traces,
            )

            downstream_delta = _compare_trace_sets(
                list(current_result.get("traces", [])),
                list(base_result.get("traces", [])),
            )
            upstream_delta = _compare_trace_sets(
                list(current_result.get("upstream_traces", [])),
                list(base_result.get("upstream_traces", [])),
            )

            current_findings = list(current_result.get("findings", []))
            base_findings = list(base_result.get("findings", []))
            current_counts = _severity_counts(current_findings)
            base_counts = _severity_counts(base_findings)
            finding_delta = {
                severity: current_counts[severity] - base_counts[severity]
                for severity in ("high", "medium", "low", "info")
            }

            self._send_json(
                HTTPStatus.OK,
                {
                    "compare_mode": compare_mode,
                    "base_ref": base_ref,
                    "base_commit": base_commit,
                    "current_commit": current_snapshot_commit,
                    "current_result": current_result,
                    "base_result": base_result,
                    "delta": {
                        "downstream": downstream_delta,
                        "upstream": upstream_delta,
                        "findings": {
                            "current": current_counts,
                            "base": base_counts,
                            "delta": finding_delta,
                        },
                    },
                    "snapshot_stats": {
                        "current": current_snapshot_stats,
                        "base": base_stats_path,
                    },
                },
            )
        except FileNotFoundError as exc:
            self._send_json(
                HTTPStatus.NOT_FOUND,
                {"error": "flow_artifact_missing", "message": str(exc)},
            )
        except ValueError as exc:
            self._send_json(
                HTTPStatus.BAD_REQUEST,
                {"error": "flow_delta_invalid_request", "message": str(exc)},
            )
        except Exception as exc:
            self._send_json(
                HTTPStatus.INTERNAL_SERVER_ERROR,
                {"error": "flow_delta_failed", "message": str(exc)},
            )

    def _extract_query_path(self, parsed_url: Any) -> str:
        query = parse_qs(parsed_url.query, keep_blank_values=False)
        values = query.get("path", [])
        if not values:
            return ""
        return str(values[0] or "").strip()

    def _handle_repo_doc(self, parsed_url: Any) -> None:
        """Serve a repo file for the /docs/<path> viewer route.

        Read-only browsing surface for documentation links from the
        explorer views. Path safety reuses the repo-relative resolver
        (no absolute paths, no escaping the repository root) and is
        further restricted to documentation-safe file types.
        """
        requested_path = unquote(parsed_url.path[len("/docs/"):]).lstrip("/")
        resolved_path, error = _resolve_repo_relative_path(requested_path)
        if error:
            self._send_json(
                HTTPStatus.BAD_REQUEST,
                {"error": "invalid_path", "message": error},
            )
            return
        assert resolved_path is not None
        if not resolved_path.exists() or not resolved_path.is_file():
            self._send_json(
                HTTPStatus.NOT_FOUND,
                {"error": "file_not_found", "message": f"File not found: {requested_path}"},
            )
            return

        allowed_suffixes = {
            ".md", ".markdown", ".txt", ".html", ".htm", ".json",
            ".yaml", ".yml", ".toml", ".svg", ".png", ".jpg", ".jpeg",
            ".gif", ".webp", ".csv",
        }
        if resolved_path.suffix.lower() not in allowed_suffixes:
            self._send_json(
                HTTPStatus.FORBIDDEN,
                {
                    "error": "unsupported_doc_type",
                    "message": "Only documentation file types are served on /docs/.",
                },
            )
            return

        content_type = self.guess_type(str(resolved_path))
        # The system mime map often lacks markdown/yaml/toml; unknown
        # types default to octet-stream, which makes browsers download
        # instead of displaying the doc.
        text_types = {
            ".md": "text/markdown; charset=utf-8",
            ".markdown": "text/markdown; charset=utf-8",
            ".txt": "text/plain; charset=utf-8",
            ".yaml": "text/yaml; charset=utf-8",
            ".yml": "text/yaml; charset=utf-8",
            ".toml": "text/plain; charset=utf-8",
            ".csv": "text/csv; charset=utf-8",
            ".json": "application/json; charset=utf-8",
        }
        suffix = resolved_path.suffix.lower()
        if suffix in text_types:
            content_type = text_types[suffix]
        elif content_type == "application/octet-stream":
            content_type = "text/plain; charset=utf-8"
        try:
            payload = resolved_path.read_bytes()
        except OSError as exc:
            self._send_json(
                HTTPStatus.INTERNAL_SERVER_ERROR,
                {"error": "read_failed", "message": str(exc)},
            )
            return
        self.send_response(HTTPStatus.OK)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(payload)))
        for key, value in self._cors_headers().items():
            self.send_header(key, value)
        self.end_headers()
        self.wfile.write(payload)

    def _handle_file_read(self, parsed_url: Any) -> None:
        requested_path = self._extract_query_path(parsed_url)
        resolved_path, error = _resolve_repo_relative_path(requested_path)
        if error:
            self._send_json(
                HTTPStatus.BAD_REQUEST,
                {"error": "invalid_path", "message": error},
            )
            return
        assert resolved_path is not None
        if not resolved_path.exists():
            self._send_json(
                HTTPStatus.NOT_FOUND,
                {"error": "file_not_found", "message": f"File not found: {requested_path}"},
            )
            return
        if not resolved_path.is_file():
            self._send_json(
                HTTPStatus.BAD_REQUEST,
                {"error": "invalid_file", "message": "path must point to a file"},
            )
            return

        stat_result = resolved_path.stat()
        if stat_result.st_size > MAX_FILE_READ_BYTES:
            self._send_json(
                HTTPStatus.REQUEST_ENTITY_TOO_LARGE,
                {
                    "error": "file_too_large",
                    "message": f"File exceeds {MAX_FILE_READ_BYTES} bytes",
                    "size_bytes": stat_result.st_size,
                },
            )
            return

        try:
            content = resolved_path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            self._send_json(
                HTTPStatus.UNSUPPORTED_MEDIA_TYPE,
                {
                    "error": "unsupported_encoding",
                    "message": "Only UTF-8 text files are supported",
                },
            )
            return

        self._send_json(
            HTTPStatus.OK,
            {
                "path": _repo_relative_display_path(resolved_path),
                "mtime_ms": int(stat_result.st_mtime * 1000),
                "size_bytes": stat_result.st_size,
                "content": content,
            },
        )

    def _handle_file_stat(self, parsed_url: Any) -> None:
        requested_path = self._extract_query_path(parsed_url)
        resolved_path, error = _resolve_repo_relative_path(requested_path)
        if error:
            self._send_json(
                HTTPStatus.BAD_REQUEST,
                {"error": "invalid_path", "message": error},
            )
            return
        assert resolved_path is not None
        if not resolved_path.exists():
            self._send_json(
                HTTPStatus.NOT_FOUND,
                {"error": "file_not_found", "message": f"File not found: {requested_path}"},
            )
            return
        if not resolved_path.is_file():
            self._send_json(
                HTTPStatus.BAD_REQUEST,
                {"error": "invalid_file", "message": "path must point to a file"},
            )
            return

        stat_result = resolved_path.stat()
        self._send_json(
            HTTPStatus.OK,
            {
                "path": _repo_relative_display_path(resolved_path),
                "mtime_ms": int(stat_result.st_mtime * 1000),
                "size_bytes": stat_result.st_size,
            },
        )

    def _handle_graph_query(self, parsed_url: Any) -> None:
        if graph_query is None:
            self._send_json(
                HTTPStatus.SERVICE_UNAVAILABLE,
                {"error": "query_module_unavailable", "message": "query_code_graph module not loaded"},
            )
            return

        params = parse_qs(parsed_url.query)
        mode = (params.get("mode", ["search"])[0]).strip().lower()
        q = params.get("q", [""])[0].strip()
        crate_filter = params.get("crate", [""])[0].strip() or None

        graph_path = str(GRAPH_JSON_PATH)
        graph_data = graph_query.load_graph(GRAPH_JSON_PATH)

        # Build a namespace matching what cmd_* functions expect
        ns = argparse.Namespace(
            graph=graph_path,
            query=q or None,
            pattern=q or None,
            endpoints=True,
            how=q or None,
            kind=None,
            limit=30,
            depth=2,
            crate=crate_filter,
        )

        try:
            if mode == "how" and q:
                result = graph_query.cmd_how(ns, graph_data)
            elif mode == "endpoints":
                result = graph_query.cmd_endpoints(ns, graph_data)
            elif mode in ("pattern", "detail") and q:
                result = graph_query.cmd_pattern(ns, graph_data)
            elif mode == "search" and q:
                result = graph_query.cmd_query(ns, graph_data)
            else:
                self._send_json(
                    HTTPStatus.BAD_REQUEST,
                    {"error": "invalid_mode", "message": f"Unknown mode '{mode}' or missing q param"},
                )
                return
        except Exception as exc:
            self._send_json(
                HTTPStatus.INTERNAL_SERVER_ERROR,
                {"error": "query_failed", "message": str(exc)},
            )
            return

        self._send_json(HTTPStatus.OK, result)

    # ── Analyzer endpoints (dead-code / test-coverage / contracts / audit) ──
    # Each handler is a thin shell around the corresponding script's
    # `analyze()` function — same logic as the CLI, same args via query
    # string (`?crate=…&tier=…&limit=…` etc.). Designed to stay in sync
    # automatically: when the analyzer script changes, the HTTP endpoint
    # picks up the new behaviour on next request.

    def _handle_dead_code(self, parsed_url: Any) -> None:
        if dead_code is None:
            self._send_json(
                HTTPStatus.SERVICE_UNAVAILABLE,
                {"error": "module_unavailable", "message": "find_dead_code module not loaded"},
            )
            return
        params = parse_qs(parsed_url.query)
        crate_filter = (params.get("crate", [""])[0]).strip() or None
        tier_filter = (params.get("tier", [""])[0]).strip().upper() or None
        if tier_filter and tier_filter not in {"A", "C"}:
            self._send_json(
                HTTPStatus.BAD_REQUEST,
                {"error": "invalid_tier", "message": "tier must be A or C"},
            )
            return
        try:
            limit = int(params.get("limit", ["200"])[0])
        except ValueError:
            limit = 200
        try:
            word_counts = dead_code.build_word_occurrence_counts(REPO_ROOT)
            buckets = dead_code.analyze(GRAPH_JSON_PATH, crate_filter, word_counts)
        except Exception as exc:
            self._send_json(
                HTTPStatus.INTERNAL_SERVER_ERROR,
                {"error": "analysis_failed", "message": str(exc)},
            )
            return

        def _shape(fns: list[dict]) -> list[dict]:
            return [
                {
                    "id": f.get("id"),
                    "name": f.get("label"),
                    "path": f.get("path"),
                    "line": f.get("line"),
                    "crate": f.get("crate"),
                    "module": f.get("module"),
                    "params": f.get("params"),
                    "occurrences": int(f.get("_occurrences") or 0),
                    "test_callers": int(f.get("_test_callers") or 0),
                    "public": bool(f.get("public")),
                }
                for f in fns[:limit]
            ]

        result = {
            "totals": {tier: len(fns) for tier, fns in buckets.items()},
            "near_certain": sum(
                1 for fns in buckets.values() for f in fns
                if int(f.get("_occurrences") or 0) <= 1
            ),
            "filters": {"crate": crate_filter, "tier": tier_filter, "limit": limit},
        }
        if tier_filter:
            result["candidates"] = _shape(buckets.get(tier_filter, []))
        else:
            result["candidates"] = {tier: _shape(fns) for tier, fns in buckets.items()}
        self._send_json(HTTPStatus.OK, result)

    def _handle_test_coverage(self, parsed_url: Any) -> None:
        if test_cov is None:
            self._send_json(
                HTTPStatus.SERVICE_UNAVAILABLE,
                {"error": "module_unavailable", "message": "test_coverage module not loaded"},
            )
            return
        params = parse_qs(parsed_url.query)
        crate_filter = (params.get("crate", [""])[0]).strip() or None
        bucket_filter = (params.get("bucket", [""])[0]).strip() or None
        valid_buckets = {"untested", "light", "moderate", "well", "only_tested"}
        if bucket_filter and bucket_filter not in valid_buckets:
            self._send_json(
                HTTPStatus.BAD_REQUEST,
                {"error": "invalid_bucket",
                 "message": f"bucket must be one of {sorted(valid_buckets)}"},
            )
            return
        try:
            limit = int(params.get("limit", ["200"])[0])
        except ValueError:
            limit = 200
        try:
            buckets = test_cov.analyze(GRAPH_JSON_PATH, crate_filter)
        except Exception as exc:
            self._send_json(
                HTTPStatus.INTERNAL_SERVER_ERROR,
                {"error": "analysis_failed", "message": str(exc)},
            )
            return

        def _shape(fns: list[dict]) -> list[dict]:
            return [
                {
                    "id": f.get("id"),
                    "name": f.get("label"),
                    "path": f.get("path"),
                    "line": f.get("line"),
                    "crate": f.get("crate"),
                    "module": f.get("module"),
                    "params": f.get("params"),
                    "public": bool(f.get("public")),
                    "prod_callers": int(f.get("_prod_callers") or 0),
                    "test_callers": int(f.get("_test_callers") or 0),
                }
                for f in fns[:limit]
            ]

        result = {
            "totals": {b: len(fns) for b, fns in buckets.items()},
            "filters": {"crate": crate_filter, "bucket": bucket_filter, "limit": limit},
        }
        if bucket_filter:
            result["functions"] = _shape(buckets.get(bucket_filter, []))
        else:
            result["functions"] = {b: _shape(fns) for b, fns in buckets.items()}
        self._send_json(HTTPStatus.OK, result)

    def _handle_contracts(self, parsed_url: Any) -> None:
        path = CODEGRAPH_DIR / "contracts.json"
        if not path.exists():
            self._send_json(
                HTTPStatus.NOT_FOUND,
                {"error": "contracts_missing",
                 "message": "contracts.json not found — run `make graph-contracts`"},
            )
            return
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except Exception as exc:
            self._send_json(
                HTTPStatus.INTERNAL_SERVER_ERROR,
                {"error": "contracts_parse_failed", "message": str(exc)},
            )
            return
        self._send_json(HTTPStatus.OK, data)

    def _handle_audit(self, parsed_url: Any) -> None:
        if audit_mod is None:
            self._send_json(
                HTTPStatus.SERVICE_UNAVAILABLE,
                {"error": "module_unavailable", "message": "audit_code_graph module not loaded"},
            )
            return
        try:
            result = audit_mod.compute_audit(REPO_ROOT, GRAPH_JSON_PATH)
        except FileNotFoundError as exc:
            self._send_json(
                HTTPStatus.INTERNAL_SERVER_ERROR,
                {"error": "graph_load_failed", "message": str(exc)},
            )
            return
        except Exception as exc:
            self._send_json(
                HTTPStatus.INTERNAL_SERVER_ERROR,
                {"error": "audit_failed", "message": str(exc)},
            )
            return
        self._send_json(HTTPStatus.OK, result)

    def _handle_flows(self, parsed_url: Any) -> None:
        """Trace incoming or outgoing flows for a target node.
        Query: ?target=<name-or-id>&hops=<n>&direction=in|out
        Returns the induced subgraph with per-hop layers — the 2D/3D
        viewers feed this into their `/flows` slash command handler
        to render animated arrows toward the target."""
        if flows_mod is None:
            self._send_json(
                HTTPStatus.SERVICE_UNAVAILABLE,
                {"error": "module_unavailable", "message": "find_flows module not loaded"},
            )
            return
        params = parse_qs(parsed_url.query)
        target = (params.get("target", [""])[0]).strip()
        if not target:
            self._send_json(
                HTTPStatus.BAD_REQUEST,
                {"error": "missing_target", "message": "Pass ?target=<name|id>"},
            )
            return
        try:
            hops = int(params.get("hops", ["3"])[0])
        except ValueError:
            hops = 3
        direction = (params.get("direction", ["in"])[0]).strip()
        if direction not in {"in", "out", "both"}:
            direction = "in"
        try:
            limit = int(params.get("limit", [str(flows_mod.DEFAULT_LIMIT)])[0])
        except ValueError:
            limit = flows_mod.DEFAULT_LIMIT
        fmt = (params.get("format", ["graph"])[0]).strip()
        if fmt not in {"graph", "compact", "mermaid"}:
            fmt = "graph"

        try:
            graph = json.loads(GRAPH_JSON_PATH.read_text(encoding="utf-8"))
        except Exception as exc:
            self._send_json(
                HTTPStatus.INTERNAL_SERVER_ERROR,
                {"error": "graph_load_failed", "message": str(exc)},
            )
            return
        node = flows_mod.resolve_target(graph, target)
        if not node:
            self._send_json(
                HTTPStatus.NOT_FOUND,
                {"error": "no_match", "message": f"No node matched '{target}'"},
            )
            return
        result = flows_mod.trace_flows(
            graph, node["id"],
            hops=hops, direction=direction,
            limit=limit, fmt=fmt,
        )
        self._send_json(HTTPStatus.OK, result)

    def _handle_file_write(self) -> None:
        body, error_status, error_payload = self._read_json_request()
        if error_status is not None and error_payload is not None:
            self._send_json(error_status, error_payload)
            return
        if body is None:
            self._send_json(
                HTTPStatus.INTERNAL_SERVER_ERROR,
                {"error": "request_read_failed", "message": "Could not read request body"},
            )
            return

        requested_path = str(body.get("path", "")).strip()
        content = body.get("content", "")
        if not isinstance(content, str):
            self._send_json(
                HTTPStatus.BAD_REQUEST,
                {"error": "invalid_content", "message": "content must be a string"},
            )
            return
        if len(content.encode("utf-8")) > MAX_FILE_WRITE_BYTES:
            self._send_json(
                HTTPStatus.REQUEST_ENTITY_TOO_LARGE,
                {
                    "error": "content_too_large",
                    "message": f"content exceeds {MAX_FILE_WRITE_BYTES} bytes",
                },
            )
            return

        resolved_path, error = _resolve_repo_relative_path(requested_path)
        if error:
            self._send_json(
                HTTPStatus.BAD_REQUEST,
                {"error": "invalid_path", "message": error},
            )
            return
        assert resolved_path is not None
        if resolved_path.exists() and not resolved_path.is_file():
            self._send_json(
                HTTPStatus.BAD_REQUEST,
                {"error": "invalid_file", "message": "path must point to a file"},
            )
            return
        resolved_path.parent.mkdir(parents=True, exist_ok=True)
        resolved_path.write_text(content, encoding="utf-8")
        stat_result = resolved_path.stat()
        self._send_json(
            HTTPStatus.OK,
            {
                "path": _repo_relative_display_path(resolved_path),
                "mtime_ms": int(stat_result.st_mtime * 1000),
                "size_bytes": stat_result.st_size,
                "saved": True,
            },
        )

    def _handle_git_refs(self) -> None:
        local_refs_run = _run_git_command(
            ["for-each-ref", "--format=%(refname:short)", "refs/heads"],
        )
        remote_refs_run = _run_git_command(
            ["for-each-ref", "--format=%(refname:short)", "refs/remotes/origin"],
        )
        current_branch_run = _run_git_command(["rev-parse", "--abbrev-ref", "HEAD"])

        if local_refs_run.returncode != 0 or remote_refs_run.returncode != 0:
            stderr = "\n".join(
                part
                for part in [
                    local_refs_run.stderr.strip(),
                    remote_refs_run.stderr.strip(),
                ]
                if part
            )
            self._send_json(
                HTTPStatus.INTERNAL_SERVER_ERROR,
                {
                    "error": "git_refs_failed",
                    "message": "Failed to query git refs.",
                    "stderr": _trim_output(stderr),
                },
            )
            return

        local_refs = _parse_changed_paths(local_refs_run.stdout or "")
        remote_refs = [
            ref
            for ref in _parse_changed_paths(remote_refs_run.stdout or "")
            if ref != "origin/HEAD"
        ]
        merged = sorted(set([*local_refs, *remote_refs]), key=lambda value: value.lower())
        current_branch = (
            current_branch_run.stdout.strip()
            if current_branch_run.returncode == 0
            else ""
        )

        self._send_json(
            HTTPStatus.OK,
            {
                "refs": merged,
                "current_branch": current_branch,
                "default_base_ref": _choose_default_base_ref(merged, current_branch),
            },
        )

    def _handle_git_impact(self) -> None:
        body, error_status, error_payload = self._read_json_request()
        if error_status is not None and error_payload is not None:
            self._send_json(error_status, error_payload)
            return
        if body is None:
            self._send_json(
                HTTPStatus.INTERNAL_SERVER_ERROR,
                {"error": "request_read_failed", "message": "Could not read request body"},
            )
            return

        base_ref = str(body.get("base_ref", "")).strip()
        if not base_ref:
            self._send_json(
                HTTPStatus.BAD_REQUEST,
                {
                    "error": "invalid_base_ref",
                    "message": "base_ref is required",
                },
            )
            return

        compare_mode = str(body.get("compare_mode", "workspace")).strip().lower()
        if compare_mode not in ALLOWED_IMPACT_MODES:
            self._send_json(
                HTTPStatus.BAD_REQUEST,
                {
                    "error": "invalid_compare_mode",
                    "message": f"compare_mode must be one of: {sorted(ALLOWED_IMPACT_MODES)}",
                },
            )
            return

        verify_base = _run_git_command(["rev-parse", "--verify", "--quiet", base_ref])
        if verify_base.returncode != 0:
            self._send_json(
                HTTPStatus.BAD_REQUEST,
                {
                    "error": "unknown_base_ref",
                    "message": f"Could not resolve base ref '{base_ref}'",
                },
            )
            return

        if compare_mode == "head":
            diff_args = [
                "diff",
                "--name-only",
                "--diff-filter=ACMRTD",
                f"{base_ref}...HEAD",
                "--",
            ]
        else:
            diff_args = [
                "diff",
                "--name-only",
                "--diff-filter=ACMRTD",
                base_ref,
                "--",
            ]

        started_at = time.perf_counter()
        diff_run = _run_git_command(diff_args)
        elapsed_ms = int((time.perf_counter() - started_at) * 1000)
        if diff_run.returncode != 0:
            self._send_json(
                HTTPStatus.INTERNAL_SERVER_ERROR,
                {
                    "error": "git_diff_failed",
                    "message": "Could not compute git diff paths.",
                    "stderr": _trim_output(diff_run.stderr or ""),
                },
            )
            return

        changed_files = _parse_changed_paths(diff_run.stdout or "")

        if compare_mode == "workspace":
            untracked_run = _run_git_command(["ls-files", "--others", "--exclude-standard"])
            if untracked_run.returncode == 0:
                untracked = _parse_changed_paths(untracked_run.stdout or "")
                if untracked:
                    merged = set(changed_files)
                    for path in untracked:
                        if path not in merged:
                            changed_files.append(path)
                            merged.add(path)

        self._send_json(
            HTTPStatus.OK,
            {
                "base_ref": base_ref,
                "compare_mode": compare_mode,
                "changed_files": changed_files,
                "changed_file_count": len(changed_files),
                "duration_ms": elapsed_ms,
                "command": ["git", *diff_args],
            },
        )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Serve docs/codegraph with make-runner API")
    parser.add_argument("--host", default="127.0.0.1", help="Bind host (default: 127.0.0.1)")
    parser.add_argument("--port", type=int, default=8077, help="Bind port (default: 8077)")
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    server = ThreadingHTTPServer((args.host, args.port), CodeGraphDevHandler)
    print(f"Serving codegraph at http://{args.host}:{args.port}/")
    print(f"Repository root: {REPO_ROOT}")
    print("Make API:")
    print(f"  GET  http://{args.host}:{args.port}/api/make/targets")
    print(f"  POST http://{args.host}:{args.port}/api/make/run")
    print("Git API:")
    print(f"  GET  http://{args.host}:{args.port}/api/git/refs")
    print(f"  POST http://{args.host}:{args.port}/api/git/impact")
    print("Flow API:")
    print(f"  GET  http://{args.host}:{args.port}/api/flow/sources")
    print(f"  GET  http://{args.host}:{args.port}/api/flow/mock?source_id=<endpoint-id>")
    print(f"  POST http://{args.host}:{args.port}/api/flow/simulate")
    print(f"  POST http://{args.host}:{args.port}/api/flow/delta")
    print("File API:")
    print(f"  GET  http://{args.host}:{args.port}/api/file/read?path=<repo-relative-path>")
    print(f"  GET  http://{args.host}:{args.port}/api/file/stat?path=<repo-relative-path>")
    print(f"  POST http://{args.host}:{args.port}/api/file/write")
    server.serve_forever()


if __name__ == "__main__":
    main()
