# Architecture Model

`architecture.yaml` is the human-owned source of truth for Magician's
runtime architecture *intent*: which processes exist, who talks to whom,
over which transport, and why. It powers the C4 Architecture Canvas in the
codegraph explorer and is enforced against the generated code graph so it
cannot silently drift from the code.

## Where it is used

- `make graph-index` runs `scripts/generate_c4_index.py`, which loads this
  model, validates it against `docs/codegraph/graph.json`, and writes the
  merged `docs/codegraph/c4.json` consumed by `docs/codegraph/c4.html`.
- `make graph-check` re-runs validation in strict mode: a `code_refs` entry
  that no longer exists in the graph (renamed module, removed crate) fails
  the check.

## Schema

| Section | Purpose |
|---|---|
| `system` | The whole system: label, summary, canonical doc. |
| `nodes` | `runtime:*` processes/systems (C4 containers) and `area:*` feature areas inside a system (C4 components). |
| `edges` | Connections between nodes: `kind`, `transport`, optional `trigger`, `summary`. |
| `externals` | `external:*` systems outside the repo (LLM providers, channels, infra). |
| `actors` | `actor:*` humans/roles interacting with the system. |

Node fields:

- `id` — namespaced slug (`runtime:supervisor`, `area:agentic-loop`).
- `label`, `summary` — required; `summary` is one sentence shown on the canvas.
- `detail` — optional longer inspector text.
- `doc` — required path (repo-relative) to the canonical doc for the node.
- `parent` — required for `area:*` nodes (the runtime node containing them).
- `runs` — optional display metadata (`port`).
- `code_refs` — graph node ids grounding the node in code
  (`crate::magician`, `module::magician::magician_v2::execution`).
- `endpoints` — optional route evidence (`"POST /executions"`); resolved
  by method + route prefix, reported when missing but not fatal.
- `ungrounded: true` — escape hatch for real processes with no indexed
  code (e.g. the Android app before it joins the graph).

## Authoring rules

1. Intent lives here, code lives in the graph. Never invent a code ref to
   satisfy the validator — either find the real id (inspect
   `docs/codegraph/graph.json` or the 2D explorer) or mark the node
   `ungrounded: true`.
2. Every edge should say *how* (`transport`) and, when it is not always
   on, *when* (`trigger`).
3. `summary` is the one-liner a new reader sees on the canvas; keep it
   plain-language and jargon-light. Put depth in `detail`.
4. `doc` must point at the doc a reader should open next.

Magician feature areas that must stay current: the **plane / replaceable
runtime** (`execution.harness_engine` + `/plane/mcp`), the **chat mouth**
(`chat.harness_engine`, ChatScoped grants), **apps** (`magician-apps` +
public contract), and **background ops** (scheduler, monitors, harness
company loop).

## Drift contract

- Dangling `code_refs` / `endpoints` are surfaced by `make graph-check`
  (refs are fatal, endpoint routes are warnings).
- Missing `doc` paths, duplicate ids, edges to unknown nodes, and area
  nodes without parents are validation errors.
