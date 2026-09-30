# Engagement-Scoped Retrieval — Containment

**Modules:** `magician-vector-index/src/retrieval_scope.rs` (the rule),
`magician/src/magician_v2/engagement_retrieval.rs` (the seam to the runtime)
**Design:** `docs/archive/plans/2026-08-07-opc-engagements-contextual-authority.md` §5A.2
**Companion:** [`engagements.md`](./engagements.md) — the authority carrier

## The problem this closes

[`engagements.md`](./engagements.md) bounds what an execution may **do**. It
bounds nothing about what an execution may **read**, and agent-scoped memory
does not either: `memory_tiers: scope: agent` separates one agent from another,
never one of an agent's own engagements from the next.

One outward actor serves investors, candidates, vendors and press. Without
containment it can retrieve one counterparty's material while answering
another — a leak between outsiders even though no owner data moved.

## The rule, in one sentence

An execution is `Unbound` (no engagement — unchanged behaviour) or confined
(`Bound { engagement_id }`, or `Meeting { meeting_id }` for a single occasion).
A confined execution retrieves items labelled to its own binding, plus — for an
engagement — items **explicitly** labelled neutral. Everything else is refused.

## Unlabelled is NOT neutral

This is the load-bearing decision and the one most likely to be
well-meaningly reverted.

The tempting rule is "filter out items labelled with a *different*
engagement". That rule is **vacuously true on an unlabelled corpus**: every
item passes, the filter reports success, and the leak is exactly as wide as
before. Today's memory corpus is almost entirely unlabelled, so that version of
the filter would change essentially nothing while every "different engagement"
test still passed.

So: **unlabelled context defaults to NOT retrievable under a confined
execution.** Neutrality is an assertion an author makes, never an inference
from the absence of one.

The practical consequence is deliberate and should not be treated as a bug
report: **a newly created engagement retrieves almost nothing** until its
material is labelled. That is the boundary working.

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

Every loaded candidate carries the resolved label in `metadata_json`, written
as an explicit `null` when there was none — a reader must be able to tell "this
pipeline looked and found no label" from "this pipeline never looked".

## Where it is enforced

| surface | how |
|---|---|
| **Persisted memory** (tiers, collection items, user memory) | `load_memory_candidate_documents` filters its whole candidate set. `MemoryCandidateRequest.retrieval_scope` is a **required field with no default**, so every caller of the one canonical extraction path is made to state the containment it loads under. |
| **Prompt-injected memory** | `MemoryRenderRequest::bound_to_engagement`, applied per render in `memory_prompt_blocks`. The most important surface: prompt memory needs **no tool call**, so no ceiling and no dispatch gate stands between it and another engagement's material. |
| **Prompt-snapshot cache** | The shared snapshot is loaded **unbound** and filtered per render. Its cache key has no engagement field, so building it confined would write one engagement's filtered set into a cache the next engagement reads. |
| **`search_memory` / `forget_memory`** | `rank_memory_candidates_hybrid` takes the scope; the handlers read it from the runtime-owned `__engagement_id` dispatch argument. `forget_memory` is included because its deletion preview is the same retrieval. |
| **Episodic memory** | Episodes live on their own disk surface and never pass through the canonical extraction path, so `rank_memory_candidates_hybrid` re-applies the filter over the assembled universe. An episode carries the **occasion** it was produced in, not an engagement: `episode_candidate_metadata` stamps `V3EpisodeRecord::origin_meeting_label()`, so a room reads back the episodes it produced itself and no other room's. An owner-surface episode names no occasion, stays unlabelled, and is refused under any bound retrieval. |
| **Unlabelled corpora** (notes, filesystem, tasks, artifacts, app data, acquired content) | **Refused**, not filtered — `cross_engagement_corpus_refusal`, called from `execute_action_inner`, the one function every action passes through. |
| **Staged context reuse** | `engagement_scope` is part of `source_revisions`, so it is part of `ContextReuseKey` and of `matches_request`. A contribution gathered for one engagement cannot be replayed into another's turn. |
| **Browser** | Session id is namespaced by binding, and CDP mode is refused outright. See below. |
| **Derived memory index** | Built **unbound**, on purpose. It returns scores keyed by candidate identity, never text; a score whose candidate was filtered out has nothing to attach to. Per-engagement indexes would be partial and stale and buy nothing. |

### Why unlabelled corpora are refused rather than filtered

Filtering needs a label to filter on. Notes, files, tasks, artifacts, app data
and acquired content have none. A "filtered" read of them returns the whole
store and reports success — the same vacuous truth as above, one level up. A
refusal is the only answer that is true.

The gate reads the capability name and is deliberately **not** an allowlist:
the complement of the refused list is not "safe", it is "not a read of an
owner-wide corpus", and that includes the outward acts an engagement exists to
perform. Refusing everything unlisted would refuse the ambassador's own job.
There are **two** lists, kept apart because they fail in opposite directions.
`unlabelled_corpus_capabilities()` names corpora with no engagement labels at
all — the owner's notes, filesystem, task history, artifacts. Those already fail
closed: with nothing to filter on, the read is refused.

`ENGAGEMENT_LABELLED_ONLY_CAPABILITIES` names corpora that ARE filterable, just
not by a programme: `search_memory` and `forget_memory` read records carrying an
`engagement_scope` label and no programme label. Those fail **open**, which is
why they need their own list and are checked first — under a programme no scope
argument is stamped, an absent key reads as unbound, and the tool call returns
everything. Refusing to render the corpus into the prompt and then handing the
same corpus over through a tool call is the same leak arriving by a second door.

### Why the inward gate exists at all

The engagement's tool ceiling is the intended long-term shape — an outward
actor's grant simply would not contain these tools. But the ceiling is enforced
today **only on acts `classify_outward_dispatch` recognises as outward**.
Inward reads reach their corpus with nothing in the way, which is why this gate
is a separate check placed ahead of the outward gate rather than folded into
it.

## Browser containment

`agent-browser` keys a launched browser's state — cookies, logged-in accounts,
storage — by `--session <id>`. Two executions resolving the same id share one
browser. So:

- **Session partition.** `PrimitiveExecCtx::effective_browser_session_id`
  namespaces the id by binding: `scope-engagement-<id>-…`. Applied to an
  inherited `browser_session_id_override` as well as to the derived id — an
  override is a request to share a window, and sharing across engagements is
  exactly what must not happen. Within one engagement, parent and child still
  namespace the same override identically and still share.
- **CDP mode refused.** `ConnectionMode::Cdp` attaches to the owner's real
  Chrome through the magicutor proxy; that mode exists to preserve the owner's
  signed-in profile. Under a confined execution that is the leak in its most
  direct form, so it is refused rather than silently downgraded — an act that
  quietly ran somewhere other than where it was asked to run produces a result
  nobody can account for.
- **A second, independent refusal: the agent's own ceiling.** The check above
  keys on the WORK CARRIER, so it says nothing about an execution that carries
  none. `AgentDefinition::browser_transports` declares which transports an agent
  may use at all (empty = all three, an opt-in restriction), and
  `BrowserTransportCeiling` applies it in the same dispatch function, after the
  call's `connection_mode` and the `MAGICIAN_AGENT_BROWSER_MODE` override and
  above both. A typed retrieval handoff is a grant to the *scope*, not to the
  agent holding it (`handoff_session_allows` never asks which agent is
  calling), so the ceiling is applied to that resolved mode as well —
  otherwise an agent that declares `browser_transports: [headless, headed]`
  could continue an `authenticated_interact` handoff into the owner's
  signed-in Chrome. Complementary, not redundant: carrier confinement catches
  every agent under an engagement, the ceiling catches the unbound cycle
  confinement skips. See
  [`agents/agent-definition-reference.md`](agents/agent-definition-reference.md#browser_transports).

**No separate profile directory.** Sessions are separated by session id — a
session boundary, not a filesystem-level profile boundary. A per-engagement
profile would need a `--user-data-dir`-shaped flag on the vendored
`agent-browser` invocation in `primitive_dispatch/browser/session.rs`.

## Where the scope comes from

One source: the `EngagementAuthorityRef` the execution carries, inherited
verbatim from the parent's durable `ExecutionRun`. Never a goal label (free
text a delegating agent wrote), never a tool argument. The autonomous dispatch
boundary strips every model-supplied `__*` key before stamping the runtime's
own `__engagement_id`, which is what makes that argument safe for a compiled
handler to read.

Fail-closed shapes:

- **No carrier** → `Unbound`. Not a permissive default: an execution with no
  engagement was never narrowed by one, and narrowing it would delete the
  owner's own memory from the owner's own chat.
- **Unreadable carrier at a dispatch gate** → refuse the act.
- **Unreadable carrier at a render site** → `contained_retrieval_scope` returns
  `None` and the site renders **no memory at all**. It deliberately does not
  manufacture a placeholder binding: a `Bound` scope with a nonsense id still
  admits every `neutral` item, so the "safe fallback" would quietly retrieve
  the one class of material an author marked shareable, for an engagement
  nobody could identify.

## Residual — what this does NOT contain

Named precisely, because a boundary whose gaps are unlisted is a boundary
nobody can audit:

1. **`shell` / `bash`.** Arbitrary command execution can read anything a
   confined execution's process can. Not gated here: shell is not a retrieval
   capability, and refusing it belongs to the engagement's tool ceiling. The
   outward actor's grant does not include it.
2. **A retrieval capability added after this list.** A new tool reading an
   owner-wide corpus is not contained until it is added to
   `UNLABELLED_CORPUS_CAPABILITIES` (or filtered like memory). The gate cannot
   fail closed on unknown capabilities without refusing the outward acts an
   engagement exists to perform.
3. **Internal, non-agent-invocable episodic reads** —
   `AgentMemoryService::recall_native_episodes` /
   `load_recent_native_episodes`, used by the feedback loop and the social
   worker. Not reachable as a tool from a confined execution; not filtered.
4. **`load_fresh_index_documents`** returns whole indexed documents. Reachable
   from operator/analytics surfaces and evals, not wired to any agent tool.
5. **Chat.** Chat turns carry no engagement ref at all, so chat is unbound by construction rather than contained.
   Its reuse key states `engagement_scope: unbound` explicitly so it can never
   share a cached contribution with a confined turn.
6. **The corpus is labelled on one axis only.** One production writer stamps
   `engagement_scope`: `agents::memory_consolidator::stamp_origin_meeting` labels
   consolidation output with `ContextLabel::Meeting` before *any* target sees it,
   and only when every source episode agrees on one `origin_meeting`. Owner,
   tier-sourced and step-sourced runs resolve to `Unlabelled` and their payload
   is returned byte for byte. So **meeting** labels are being written; nothing in
   production writes an `engagement:` label or the `neutral` token. An
   engagement-confined execution therefore still retrieves only what an author
   explicitly marked neutral — which nothing does.

## Tests

`magician/tests/engagement_scoped_retrieval.rs`, plus unit tests in both
modules. Every containment test asserts the other engagement's entry **is**
retrievable unbound before asserting it is absent when confined. That ordering
is the point: an absence-only assertion passes just as well on an empty corpus,
a misspelled tier, a query that matched nothing, or a deleted filter with
nothing left to find.
