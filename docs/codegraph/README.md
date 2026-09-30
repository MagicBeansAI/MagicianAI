# Code Graph Explorer

The codegraph toolchain turns the current workspace into a generated structural
index for people and coding agents. It provides four linked visual surfaces,
local analysis APIs, command-line queries, and an MCP server over the same graph.

| Surface | Best for | Path |
| --- | --- | --- |
| Plane | Browsing or searching the whole indexed workspace | `plane.html` (the landing view) |
| Architecture | Moving from the supervised runtime and product areas into crates, modules and symbols | `c4.html` |
| Explorer | Bounded structural analysis, impact, flows, guides, Mermaid and live file impact | `explorer.html` |
| 3D | Spatial exploration of the complete graph and animated flows | `graph3d.html` |
| MCP | Giving coding agents structural code intelligence | `mcp_server.py` |

The graph is generated evidence, not a live database. Regenerate it after source
changes before relying on search, impact, coverage or architecture results.

## Quick Start

```bash
make graph-index
make graph-check
make graph-serve
```

Open `http://localhost:8077/`. The root redirects to the Plane view and preserves
deep-link query parameters. Use `make graph-serve-bg` for a background server,
`make graph-serve-restart` after server changes, and `make graph-serve-stop` to
stop it.

The server binds to localhost by default. Keep it local: its developer APIs can
run allowlisted Make targets and read or write bounded repository-relative files.

## Generated Graph And Artifacts

`make graph-index` generates the structural graph and every derived analysis
artifact in one pass:

| Artifact | Purpose |
| --- | --- |
| `graph.json` | Full nodes and edges, including structurally identified test-call edges. |
| `graph_ui.json` | Browser-oriented graph with `test_calls` removed to reduce transfer and parse cost. |
| `stats.json` | Counts and generation metadata. |
| `payload_profiles.json` | Endpoint/API payload profiles used for mock payloads and flow simulation. |
| `contracts.json` | Extracted validation rules, kind gates, constants and type shapes. |
| `c4.json` | Merged architecture index used by the C4 canvas. |
| `dead_code_candidates.md` | Functions with no production callers, with test-only reachability kept separate. |
| `test_coverage.md` | Structural production-function coverage buckets derived from `test_calls`. |

The core graph models crates, modules, files, symbols, types, API endpoints,
outbound API calls and their structural relationships. Codegraph extensions add
domain-specific nodes and tools without branching the core generator. The
shipped extensions index:

- AgentSkills as tool, procedure, personality and bot-agent packages.
- Tauri applications, capabilities and icon/config metadata.

Generated build and coverage output is pruned through
`scripts/codegraph_exclude.txt`. A repo-local `coverage/` directory or symlink is
therefore never indexed as source or allowed to escape the repository-relative
path contract.

## Freshness And Validation

Use the checks according to the question being asked:

```bash
make graph-check       # graph schema plus strict curated C4 code references
make graph-audit       # filesystem coverage versus graph file nodes
make graph-dead-code   # refresh the full dead-code candidate report
make graph-coverage    # refresh the structural test-coverage report
```

`graph-check` validates the generated graph and rejects curated architecture
`code_refs` that no longer resolve. `graph-audit` re-walks the repository with
the generator's exclusion rules and reports missing, unexpected and uncovered
manifest paths.

The C4 canvas displays a staleness badge when `c4.json` predates the regenerated
graph. The Explorer also watches `graph.json`, `payload_profiles.json` and
`stats.json`; a Guide can auto-rebuild from changed artifacts or report that its
current runbook is stale. Neither the visual server nor the MCP server silently
regenerates the graph.

## Explorer Surfaces

### Plane View

The Plane view is the default whole-workspace surface. It renders
`graph_ui.json` through GPU-batched node and edge buffers, so expanding or
collapsing changes visibility without remounting the graph.

- **Explore:** click containers to expand or collapse their subtrees. `Load
  Full` reveals the complete indexed graph, and `Collapse` clears expansion,
  search, blast and impact state.
- **Search:** lights the top matches and their ancestry, dims the rest, and lets
  the result list fly the camera to a node.
- **Blast:** traces a bounded call/reference neighbourhood around the selected
  node with configurable hop depth.
- **Git impact:** maps files changed from a selected base ref into graph nodes
  and their structural neighbourhood.
- **Inspect:** hover labels, select nodes, filter by kind, follow Explorer deep
  links, fit the graph with `0`, and switch theme.

Camera work is demand-driven, and offscreen/low-detail geometry is suppressed at
overview zoom. Current graph size belongs in `stats.json`, not in this document.

### Architecture Canvas

The Architecture view is an infinite zoomable C4 canvas backed by the small
merged `c4.json` artifact rather than the complete graph. It starts with the
human-owned model at `docs/architecture/architecture.yaml` and progressively
connects that model to generated code:

1. **Runtime:** supervisor, Magican, Magicutor, local sidecars, client surfaces,
   actors, external systems, transports and triggers.
2. **Feature areas:** agentic execution, Plane and replaceable runtime, chat,
   background operations, App Platform, skills, resource authority and other
   documented product areas.
3. **Implementation:** the crate ladder from foundations through orchestrators,
   apps, bots, skills and standalone clients; expand a crate to see its modules.
4. **Lazy detail:** load files and symbols for a module from
   `GET /api/c4/slice`, or deep-link to the 2D Explorer when served statically.

Drag to pan, scroll to zoom at the cursor, click to inspect, double-click to
expand, press `/` to search, or use the minimap to jump. Camera, selection and
expansion state are encoded in the URL. See
[`docs/architecture/README.md`](../architecture/README.md) to edit the curated
model.

### 2D Analysis Explorer

The Cytoscape Explorer is optimized for bounded analysis rather than showing the
entire workspace at once. It fetches `graph_ui.json` with a `graph.json`
fallback, initially renders crates, and progressively expands through modules,
files, symbols and one-hop relations. Large slices use deterministic
crate-to-symbol placement, frame-budgeted mounting and viewport culling. Full
structural edges are available without mounting the complete call graph; call
detail is pulled into bounded search, impact, blast and flow slices.

For a large workspace, **Load Full Graph** opens the GPU Plane view with the
current query parameters. Small fixture graphs can still render fully inside the
Explorer.

The resizable right panel has four current tabs:

- **Details:** node metadata, source location and the kind legend.
- **Guide:** endpoint-driven guided explanations and runbook generation.
- **Mermaid:** diagrams derived from the active search, impact or flow.
- **Editor:** a Monaco editor with structural impact highlighting.

Panel visibility, tab, width, theme, reveal speed, fit tightness and several
analysis controls persist in browser storage. Search, selection, expansion and
Mermaid state persist in URL parameters for refreshable, shareable deep links.

#### Search And Slash Commands

Ordinary search renders matches plus the ancestry required to understand them,
without expanding unrelated branches. Type `/` in the search box for structural
commands:

| Command | Result |
| --- | --- |
| `/how <topic>` | Architecture explanation or extension blueprint. |
| `/detail <crate>` | Crate files, symbols and internal/cross-crate edges. |
| `/endpoints [crate]` | API endpoints, handlers, payload types and callers. |
| `/flows <target>` | Animated bidirectional flow subgraph for a symbol, file, module, crate, endpoint or API path. |
| `/dead-code [crate]` | Functions with no production callers, separated into review tiers. |
| `/coverage [crate]` | Structural test-coverage buckets. |
| `/skills [type]` | AgentSkill inventory supplied by the Magican extension. |
| `/tauri [app\|capability\|icon]` | Tauri metadata supplied by the Tauri extension. |

`/flows` is the interactive flow entry point; there is no separate Flow tab. It
opens a Mermaid overlay and paints the returned trace slice into the graph. The
extension registry supplies additional slash commands through
`GET /api/extensions`, so adding an extension does not require another core UI
branch.

#### Impact And Blast Analysis

Git Impact compares either the working tree or branch `HEAD` with a selected
base ref, then expands changed file nodes by one to three graph hops. Blast Lens
centres a selected node between its upstream and downstream neighbourhood,
supports bounded hop/node caps, and can treat container edges as zero-cost so
crate/module/file hierarchy does not consume the useful relation budget.

Both modes are intentionally bounded. Container-kind toggles let an analysis
focus on symbols while retaining the underlying ancestry.

#### Guides, Runbooks And Mermaid

The Guide tab selects an endpoint source and runs static flow simulation before
building an upstream, downstream or combined walkthrough. Its step cards centre
the corresponding graph node and can replay a trace. The generated runbook
includes a Mermaid diagram, payload evolution, dependencies, inferred owners,
failure modes, findings and rollback hints.

Runbooks can be copied, exported, or saved under
`docs/codegraph/runbooks/*.md`. Auto-rebuild refreshes the guide after graph
artifacts change; when disabled, the UI marks the guide stale.

The Mermaid tab can choose `Auto`, `Flow` or `Search` as its source. The same
diagram can open in a resizable split view or popout, with persistent semantic
colors for flow sources, paths and sinks.

#### Live Impact Editor

The Editor tab loads a repository-relative file through the local dev server,
saves changes, and projects seed/upstream/downstream structural impact into the
graph. It polls for external changes and supports manual or automatic refresh.
Because this is a real file writer, use it only against a local checkout you
intend to edit.

### 3D View

The 3D view renders the complete graph through `graph3d_fast.js`: nodes are one
GPU point cloud and edges are a merged line buffer rather than one scene object
per element. Rendering is on demand when idle, and selection or flow styling is
recomputed in frame-sized chunks.

It supports search, selection, `/flows` animation, kind filters, themes, an FPS
and draw-call display, and pointer-lock flight using WASD. The compatibility
layer retains the interaction API used by the original ForceGraph3D view while
the renderer stays batch-oriented.

## Local Developer Server

`make graph-serve` runs `docs/codegraph/dev_server.py`, which serves the static
surfaces and a localhost-only analysis API:

| Capability | Routes |
| --- | --- |
| Health and graph queries | `GET /api/health`, `/api/query`, `/api/contracts`, `/api/audit`, `/api/dead-code`, `/api/test-coverage`, `/api/flows` |
| Extension discovery | `GET /api/extensions` plus extension routes such as `/api/skills`, `/api/tauri` and `/api/c4/slice` |
| Make runner | `GET /api/make/targets`, `POST /api/make/run` |
| Git impact | `GET /api/git/refs`, `POST /api/git/impact` |
| Flow simulation | `GET /api/flow/sources`, `GET /api/flow/mock`, `POST /api/flow/simulate`, `POST /api/flow/delta` |
| File editor | `GET /api/file/read`, `GET /api/file/stat`, `POST /api/file/write` |

The Make runner permits only `graph-index` and `graph-check`. File operations
accept bounded repository-relative paths and supported source/document types.
Those restrictions reduce accidental scope, but they are not a reason to expose
the developer server to another network.

## MCP Server

The same indexed structure is available to coding agents through a local stdio
MCP server. Generate or refresh the graph first, then start the server:

```bash
make graph-index
python3 docs/codegraph/mcp_server.py
```

| MCP tool | What it gives a coding agent |
| --- | --- |
| `cgraph_search` | Keyword and kind-filtered structural search across labels, paths, modules and crates, with bounded neighbour expansion. |
| `cgraph_how` | Architecture explanations and extension blueprints for concepts such as agents, approvals, memory tiers and channel adapters. |
| `cgraph_detail` | A crate's files, symbols, internal edges, cross-crate connections and relevant contracts. |
| `cgraph_endpoints` | API routes with handlers, payload types and callers, optionally narrowed to one crate. |
| `cgraph_flows` | Bounded incoming, outgoing or end-to-end call/handler/API flows as compact text, graph JSON or Mermaid. |
| `cgraph_skills` | Tool, procedure, personality and bot-agent package inventory from the Magican extension. |
| `cgraph_tauri` | Tauri applications, capabilities and icon/config inventory from the Tauri extension. |
| `cgraph_dead_code` | Review candidates with no production callers, separated from test-only reachability. |
| `cgraph_test_coverage` | Structural coverage buckets derived from production and test call edges. |
| `cgraph_audit` | Filesystem-versus-graph coverage, including missing, unexpected and uncovered manifest paths. |

The tools read `docs/codegraph/graph.json`; they do not regenerate it. Repository
MCP configuration is already present for Claude Code/Grok, Codex, Gemini CLI,
Antigravity (`agy`) and ZCode.

Keep checked-in stdio arguments repo-relative and launch the server from the
repository root. If an MCP client cannot set the working directory, use an
absolute path to `docs/codegraph/mcp_server.py` as a local machine override only.

## Command-Line Queries

Search the graph without starting the visual server:

```bash
python3 scripts/query_code_graph.py --query v2_orchestrator --depth 2
```

Trace a source through the static flow model:

```bash
python3 scripts/simulate_code_flow.py \
  --source-id endpoint::POST::/v2/threads
```

Useful flow options include:

- `--mock-only` to print the generated mock payload.
- `--payload-json '{"json":{"email":"test@example.com"}}'` to supply input.
- `--max-hops 8` to extend traversal depth.

Query extension-owned inventories directly:

```bash
python3 scripts/codegraph_ext_cli.py --list
python3 scripts/codegraph_ext_cli.py skills --type tool
python3 scripts/codegraph_ext_cli.py tauri --kind capability
```
