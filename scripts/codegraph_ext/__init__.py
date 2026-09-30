"""Codegraph extension system.

The core codegraph engine (`generate_code_graph.py`, `query_code_graph.py`,
`find_dead_code.py`, `test_coverage.py`, `find_flows.py`, `audit_code_graph.py`)
stays project-agnostic. Anything project-specific — magician's
tools/agents/skills/personalities, a different repo's plugin system,
some team's API-versioning convention — lives in a separate file
under this directory and plugs in through a small contract.

### Authoring a new extension

Create `scripts/codegraph_ext/<name>.py` with a subclass of
`CodegraphExtension`. Override the hooks you want to participate in:

    from scripts.codegraph_ext import CodegraphExtension

    class MyExtension(CodegraphExtension):
        name = "myext"

        def discover_nodes(self, root, graph_data): ...
        def discover_edges(self, root, graph_data): ...
        def mcp_tools(self): ...
        def http_routes(self): ...
        def cli_commands(self): ...

### How each hook is consumed

| Hook | Consumer | When |
|---|---|---|
| `discover_nodes` | `generate_code_graph.py` | once per graph build — yielded nodes are merged into `graph.json` |
| `discover_edges` | `generate_code_graph.py` | same — yielded edges merged into `graph.json` |
| `mcp_tools` | `docs/codegraph/mcp_server.py` | server startup — registers each tool with FastMCP |
| `http_routes` | `docs/codegraph/dev_server.py` | server startup — wires `/api/<route>` to the handler |
| `cli_commands` | `scripts/codegraph_ext_cli.py` | invoked via `python3 scripts/codegraph_ext_cli.py <subcmd> …` |

Extensions are loaded automatically from every `.py` file in this
directory (except `__init__.py`). No registration required.
"""

from __future__ import annotations

import importlib.util
import inspect
import sys
from abc import ABC
from pathlib import Path
from typing import Any, Callable, Iterator


class CodegraphExtension(ABC):
    """Base class for a codegraph extension. Subclass + override what
    you need. Override nothing → no-op extension (still loads, just
    contributes nothing). See the module-level docstring for details
    and `magician_skills.py` for a working reference."""

    name: str = "unnamed"

    def discover_nodes(self, root: Path, graph_data: dict[str, Any]) -> Iterator[dict]:
        """Yield extra graph nodes to merge into `graph.json`. `graph_data`
        is the in-progress graph dict (`{"nodes": [...], "edges": [...]}`
        — the existing nodes from the core walker are already present
        so you can cross-reference them)."""
        return iter([])

    def discover_edges(self, root: Path, graph_data: dict[str, Any]) -> Iterator[dict]:
        """Yield extra edges (dicts with `from`, `to`, `kind`)."""
        return iter([])

    def mcp_tools(self) -> dict[str, dict[str, Any]]:
        """Return `{tool_name: {"fn": callable, "description": str,
        "args_schema": {...optional...}}}`. Tool names should be
        prefixed `cgraph_` for consistency with the core tools. The
        callable's signature determines the FastMCP arg schema."""
        return {}

    def http_routes(self) -> dict[str, Callable]:
        """Return `{route: handler}` where route is the path (e.g.
        `/api/skills`) and handler is a callable `(parsed_url) -> dict`.
        Handlers return a JSON-serialisable dict; the server adds the
        envelope and serialises it."""
        return {}

    def cli_commands(self) -> dict[str, Callable]:
        """Return `{subcommand: main_fn(argv: list[str]) -> int}` —
        each subcommand becomes invocable via
        `python3 scripts/codegraph_ext_cli.py <subcommand> [args…]`."""
        return {}

    def slash_commands(self) -> list[dict[str, Any]]:
        """Return slash-command specs the 2D / 3D viewers will pick up
        via `/api/extensions`. Each spec is a dict shaped like:

            {
                "cmd": "/skills",          # leading slash required
                "arg": "[type]",            # placeholder shown in palette
                "description": "List …",
                "kinds": null | [...],      # node kinds offered as arg autocomplete;
                                            # null → no autocomplete (free-form / no arg)
                "http_route": "/api/skills",
                "arg_param": "type",        # which query param the typed arg becomes
                "summary": {                # how to render the response in the side panel
                    "header_fields": [
                        {"label": "Total",  "from": "total"},
                        {"label": "Counts", "from": "counts", "format": "kv"},
                    ],
                    "list_field": "skills", # array on the response to render row-by-row
                    "row_template": "{skill_type}  {label}  v{version}  @ {path}",
                },
            }

        The core viewers consume these declaratively — they never need
        to know `skills` (or any extension name) specifically."""
        return []


def _ext_dir() -> Path:
    return Path(__file__).resolve().parent


def load_extensions() -> list[CodegraphExtension]:
    """Discover, import, and instantiate every `CodegraphExtension`
    subclass defined in a sibling .py file. Quiet on import errors —
    a broken extension shouldn't break the whole pipeline; it's logged
    to stderr but skipped."""
    out: list[CodegraphExtension] = []
    # Use the actual package name (`codegraph_ext`) so the `from .` /
    # `from codegraph_ext import …` relative imports inside each
    # extension file resolve against the already-registered package.
    pkg_name = __name__  # "codegraph_ext"
    for path in sorted(_ext_dir().glob("*.py")):
        if path.name == "__init__.py":
            continue
        module_name = f"{pkg_name}.{path.stem}"
        try:
            spec = importlib.util.spec_from_file_location(module_name, path)
            if spec is None or spec.loader is None:
                continue
            module = importlib.util.module_from_spec(spec)
            sys.modules[module_name] = module
            spec.loader.exec_module(module)
        except Exception as exc:
            print(
                f"[codegraph_ext] failed to load {path.name}: {exc}",
                file=sys.stderr,
            )
            continue
        for _, obj in inspect.getmembers(module, inspect.isclass):
            if obj is CodegraphExtension:
                continue
            if not issubclass(obj, CodegraphExtension):
                continue
            try:
                out.append(obj())
            except Exception as exc:
                print(
                    f"[codegraph_ext] failed to instantiate {obj.__name__}: {exc}",
                    file=sys.stderr,
                )
    return out
