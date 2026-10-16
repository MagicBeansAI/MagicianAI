# Engagement-Scoped Retrieval — Containment

**Modules:** `magician-vector-index/src/retrieval_scope.rs` (the rule),
`magician/src/magician_v2/engagement_retrieval.rs` (the seam to the runtime)
**Design:** `docs/archive/plans/2026-08-07-opc-engagements-contextual-authority.md` §5A.2
**Companion:** [`engagements.md`](./engagements.md) — the authority carrier

## The problem this closes

[`engagements.md`](./engagements.md) bounds what an execution may **do**, not
what it may **read**. Agent-scoped memory (`memory_tiers: scope: agent`)
separates one agent from another, never one of an agent's engagements from the
next. One outward actor serves investors, candidates, vendors and press; without
containment it can retrieve one counterparty's material while answering another,
a leak between outsiders even though no owner data moved.

## The rule, in one sentence

An execution is `Unbound` (no engagement, unchanged behaviour) or confined
(`Bound { engagement_id }`, or `Meeting { meeting_id }` for a single occasion).
A confined execution retrieves items labelled to its own binding, plus, for an
engagement, items **explicitly** labelled neutral. Everything else is refused.

## Unlabelled is NOT neutral

This is the load-bearing decision; do not revert it. "Filter out items labelled
with a *different* engagement" is **vacuously true on an unlabelled corpus**:
every item passes and the leak is as wide as before. The memory corpus is almost
entirely unlabelled, so that filter would change nothing while its tests passed.

So **unlabelled context is NOT retrievable under a confined execution.**
Neutrality is an assertion an author makes, never an inference from absence. A
new engagement therefore retrieves almost nothing until its material is
labelled. That is the boundary working, not a bug.

## The label

One key, one string value, on the item or on the record that contains it:

| value | meaning |
|---|---|
| `"neutral"` | any engagement may retrieve this |
| `"engagement:<id>"` | only that engagement |
| `"meeting:<id>"` | only that occasion — `neutral` does **not** cross into a room |
| absent / null / non-string / unparseable | unlabelled → refused when confined |

Key: `engagement_scope`. A record-level label is **inherited** by every row
inside it that declares none; a row's own label always wins, so inheritance can
narrow a row and never widen a record.

Every loaded candidate carries the resolved label in `metadata_json`, as an
explicit `null` when there was none, so a reader can tell "looked and found no
label" from "never looked".

## Where it is enforced

| surface | how |
|---|---|
| **Persisted memory** (tiers, collection items, user memory) | `load_memory_candidate_documents` filters its whole candidate set. `MemoryCandidateRequest.retrieval_scope` is a **required field with no default**, so every caller of the canonical extraction path states its containment. |
| **Prompt-injected memory** | `MemoryRenderRequest::bound_to_engagement`, applied per render in `memory_prompt_blocks`. The most important surface: prompt memory needs **no tool call**, so no ceiling or dispatch gate stands in the way. |
| **Prompt-snapshot cache** | Loaded **unbound** and filtered per render. Its cache key has no engagement field, so a confined build would leak one engagement's set to the next reader. |
| **`search_memory` / `forget_memory`** | `rank_memory_candidates_hybrid` takes the scope; the handlers read it from the runtime-owned `__engagement_id` dispatch argument. `forget_memory`'s deletion preview is the same retrieval. |
| **Episodic memory** | Episodes never pass through the canonical extraction path, so `rank_memory_candidates_hybrid` re-applies the filter. An episode carries the **occasion** it was produced in: `episode_candidate_metadata` stamps `V3EpisodeRecord::origin_meeting_label()`, so a room reads back only its own episodes. An owner-surface episode is unlabelled and refused under any bound retrieval. |
| **Unlabelled corpora** (notes, filesystem, tasks, artifacts, app data, acquired content) | **Refused**, not filtered: `cross_engagement_corpus_refusal`, called through `work_retrieval_refusal` from `execute_action_inner` (`execution/agentic/executor.rs`), which every action passes through. |
| **Staged context reuse** | `engagement_scope` is part of `source_revisions`, hence of `ContextReuseKey` and `matches_request`; a contribution gathered for one engagement cannot replay into another's turn. |
| **Browser** | Session id is namespaced by binding, and CDP mode is refused outright. See below. |
| **Derived memory index** | Built **unbound**, on purpose. It returns scores keyed by candidate identity, never text; a score whose candidate was filtered out has nothing to attach to. |

### Why unlabelled corpora are refused rather than filtered

Notes, files, tasks, artifacts, app data and acquired content have no label to
filter on, so a "filtered" read returns the whole store. Refusal is the only
true answer.

The gate reads the capability name and is deliberately **not** an allowlist: the
complement of the refused list is "not a read of an owner-wide corpus", which
includes the outward acts an engagement exists to perform. There are **two**
lists, kept apart because they fail in opposite directions:

- `UNLABELLED_CORPUS_CAPABILITIES` (17 tools) names corpora with no engagement
  labels (notes, filesystem, task and execution history, artifacts, connected
  app data, acquired content). With nothing to filter on, the
  read is refused: these fail closed.
- `ENGAGEMENT_LABELLED_ONLY_CAPABILITIES` names corpora filterable by engagement
  but not by programme: `search_memory` and `forget_memory`. Under a programme
  no scope argument is stamped, an absent key reads as unbound, and the call
  would return everything, so these fail **open** and are checked first.
  Otherwise the corpus withheld from the prompt would arrive through a tool call.

### Why the inward gate exists at all

The long-term shape is the engagement's tool ceiling: an outward actor's grant
would not contain these tools. But the ceiling is enforced **only on acts
`classify_outward_dispatch` recognises as outward**; inward reads reach their
corpus unchecked. So this gate is a separate check ahead of the outward gate.

## Browser containment

`agent-browser` keys a browser's state (cookies, logged-in accounts, storage) by
`--session <id>`; two executions with the same id share one browser.

- **Session partition.** `PrimitiveExecCtx::effective_browser_session_id`
  namespaces the id by binding (`scope-engagement-<id>-…`), including an
  inherited `browser_session_id_override`, since sharing across engagements is
  what must not happen. Within one engagement, parent and child namespace the
  same override identically and still share.
- **CDP mode refused.** `ConnectionMode::Cdp` attaches to the owner's real,
  signed-in Chrome through the magicutor proxy. Under a confined execution it is
  refused, not silently downgraded, since an act run somewhere other than asked
  produces an unaccountable result.
- **The agent's own ceiling, independently.** The check above keys on the work
  carrier, so it misses executions with none. `AgentDefinition::browser_transports`
  declares which transports an agent may use (empty = all three), and
  `BrowserTransportCeiling` applies it in the same dispatch function, after and
  above the call's `connection_mode` and the `MAGICIAN_AGENT_BROWSER_MODE` override. A
  typed retrieval handoff is a grant to the *scope*, not the agent
  (`handoff_session_allows` never asks which agent is calling), so the ceiling
  also applies to the handoff's mode; otherwise an agent declaring
  `browser_transports: [headless, headed]` could continue an
  `authenticated_interact` handoff into the owner's Chrome. Carrier confinement
  catches every agent under an engagement; the ceiling catches unbound cycles.
  See [`agents/agent-definition-reference.md`](agents/agent-definition-reference.md#browser_transports).

**No separate profile directory.** Sessions are separated by id, not by a
filesystem profile. A per-engagement profile would need a
`--user-data-dir`-shaped flag on the vendored `agent-browser` invocation in
`primitive_dispatch/browser/session.rs`.

## Where the scope comes from

One source: the `EngagementAuthorityRef` the execution carries, inherited
verbatim from the parent's durable `ExecutionRun`. Never a goal label or a tool
argument. The autonomous dispatch boundary strips every model-supplied `__*` key
before stamping the runtime's own `__engagement_id`, so a compiled handler can
trust that argument.

Fail-closed shapes:

- **No carrier** → `Unbound`. An execution with no engagement was never
  narrowed; narrowing it would remove the owner's own memory from the owner's
  own chat.
- **Unreadable carrier at a dispatch gate** → refuse the act.
- **Unreadable carrier at a render site** → `contained_retrieval_scope` returns
  `None` and the site renders **no memory at all**. It does not invent a
  placeholder binding: a `Bound` scope with a nonsense id would still admit
  every `neutral` item.

## Residual — what this does NOT contain

1. **`shell` / `bash`.** Arbitrary commands can read anything the process can.
   Shell is not a retrieval capability; refusing it belongs to the engagement's
   tool ceiling, and the outward actor's grant does not include it.
2. **A retrieval capability added after this list.** A new tool reading an
   owner-wide corpus is not contained until it is added to
   `UNLABELLED_CORPUS_CAPABILITIES` (or filtered like memory). The gate cannot
   fail closed on unknown capabilities without refusing outward acts.
3. **Internal, non-agent-invocable episodic reads**:
   `AgentMemoryService::recall_native_episodes` /
   `load_recent_native_episodes`, used by the feedback loop and the social
   worker. Not reachable as a tool from a confined execution; not filtered.
4. **`load_fresh_index_documents`** returns whole indexed documents. Reachable
   from operator/analytics surfaces and evals, not from any agent tool.
5. **Chat.** Chat turns carry no engagement ref, so chat is unbound by
   construction. Its reuse key states `engagement_scope: unbound` so it never
   shares a cached contribution with a confined turn.
6. **The corpus is labelled on one axis only.** The one production writer of
   `engagement_scope` is `agents::memory_consolidator::stamp_origin_meeting`: it
   labels consolidation output with `ContextLabel::Meeting` before any target
   sees it, only when every source episode agrees on one `origin_meeting`.
   Owner, tier-sourced and step-sourced runs resolve to `Unlabelled` and are
   returned byte for byte. Nothing in production writes an `engagement:` label
   or the `neutral` token, so an engagement-confined execution retrieves only
   what an author explicitly marked neutral, which nothing does.

## Tests

`magician/tests/engagement_scoped_retrieval.rs`, plus unit tests in both
modules. Every containment test first asserts the other engagement's entry
**is** retrievable unbound, then that it is absent when confined; an
absence-only assertion would also pass on an empty corpus, a misspelled tier, a
query that matched nothing, or a deleted filter.
