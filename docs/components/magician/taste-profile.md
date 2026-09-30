# Taste Profile

One owner-edited markdown note — by default `profile.md` under the notes root —
loaded and snapshotted so prompt assembly can inject the owner's standing taste
(voice, process, boundaries) into every run. Module:
`magician/src/magician_v2/taste_profile.rs`. Capture (distilling proposals from
sessions) is `taste_capture.rs`; applicability judging is
`memory_applicability.rs`.

Capture is **off by default** and inert until a distiller model is bound under
`llm.router.operation_mapping`, so without opt-in the owner is the note's only
writer. Design records:
`docs/archive/plans/2026-08-13-cross-session-taste-implementation.md`,
`docs/archive/plans/2026-08-13-cross-session-taste-slice2-capture.md`.

Naming: unrelated to `OwnerExecutionProfile` (what an agent may do). This is
about how outputs should read.

## The contract

**The profile note is injected as it is read, because nothing needing
exclusion is ever written to it — unapproved proposals live in a sibling note
(`proposals_note_path`) that this loader never opens.**

Filtering a section (e.g. `## Inbox`) out at read time puts a markdown scanner
between untrusted text and a system prompt, and CommonMark has more ways to make
`##` not-a-heading (straddling code fences, HTML blocks, list continuations,
lone-CR line endings) than a hand-rolled scanner enumerates. Proposals are
machine-generated from transcripts that may quote web content, so a parser bug
there is an injection path. **Do not reintroduce filtering here: if something
must not be injected, it must not be in this file.**

## Boundary

- The note is read **through the Notes provider layer** (`NotesSettingsStore`),
  never by raw path, so provider selection, scope boundaries, symlink rules and
  read bounds apply. The owner's default provider is tried first.
- Absence is silent: disabled feature, missing/blank note or unreadable
  provider loads as `None`. Prompt assembly never fails for a missing note.
- Silent is not undiagnosable: a declined path or disabled provider logs at
  debug; a provider fault warns on the **first occurrence per stage**
  (settings, catalog, note read latch separately), debug after. The latch is per
  loader, so injection must hold **one long-lived loader**. A note path the read
  path could never resolve is refused at config load.

## Snapshot semantics

`TasteProfileLoader::load(principal, workspace)` → `Option<TasteProfileSnapshot>`:

- `injectable` — note content, surrounding whitespace aside; nothing filtered,
  line endings preserved, trailing blank lines dropped.
- `version` — BLAKE3 hex of the note as read.
- `over_ceiling` — content exceeds `max_chars`. Advisory only: content is
  **never cut**. The provider's read bound (32K chars) still applies, and a note
  past its byte ceiling does not load.
- `loaded_at` — materialization time.

Every load is a provider read; no content cache (debounce belongs to the
caller).

## Injection

The profile rides the **system prompt**: the cache-stable prefix (breaks the
prompt cache only on edit) and immune to compaction.

It renders **after** the agent persona (`{identity_section}` then
`{taste_profile_section}` in `agentic_decision_system` v1.0.7; after
`{agent_persona}` in `chat_outer_loop_system` v0.0.6). Operator-authored persona
or policy always outranks owner taste.

`render_taste_profile_section` wraps the note in `<owner_taste_profile>` and runs
`neutralize_boundary_tags` (the tag is registered), so the note cannot close the
wrapper. It **never truncates** (a directive cut mid-sentence can invert its
meaning). An absent profile renders the empty string, so prompts are
byte-identical to a deployment without the feature.

The block is **not** in the templates' `SAFETY:` tag list: that list marks
regions the model must not take instructions from, and the profile is owner
direction it should obey.

No dedicated cache-key field: `static_prompt_context_revision` digests the render
variables, so a new version is a new `StaticPromptKey`. A test asserts the key
moves on an edit **and only then** (churn would disable the prefix cache).

### Freeze semantics, and where they differ per surface

- **Executions** freeze the snapshot on `AgenticContext.taste_profile` at the
  same boundary retrieval freezes, so a mid-run edit cannot shift a live run.
  Like the `prior_*` memory fields, it is not persisted in `AgenticPauseState`;
  a resumed run re-reads (one freeze rule in the executor).
- **Chat resolves per turn.** Chat sessions are durable (the default thread
  persists indefinitely), so a per-session freeze would never pick up edits. A
  turn is atomic.
- **Delegated children inherit it.** An owner swap clears agent-scoped `prior_*`
  memory but keeps the profile, which is owner-level. A test pins the split.

## Read-only mirror

`GET /api/magician/v2/taste-profile` (scope via workspace-bound bearer headers
or query) returns what would be injected now: `injectable`, `version`,
`over_ceiling`, `chars`, `note_path`, `max_chars`. No write path — the note is
the single writable copy. No profile or no loader answers `injectable: null`,
`enabled: false`, not a 500.

### The over-ceiling nudge

A profile past `max_chars` injects in full, and the loader raises one feed item
per distinct over-ceiling **version** (version-keyed id plus a process-local
latch), so the owner is nudged once per oversized version. The feed store is
attached after construction (`set_over_ceiling_feed`) because it opens later in
boot; a process without a feed never nudges. Best-effort throughout: **a
profile that cannot be announced still injects**, and a feed failure warns.

## Capture (Slice 2)

Distils finished sessions into candidate directives the owner approves.

**The proposals note is write-only. Nothing parses it.** Machine state (id,
directive, evidence, destination, confidence, status) lives in JSONL —
`taste-proposals-<scope digest>.jsonl` under `<base_root>/system/`. The note is
a rendered *mirror*, so untrusted distiller output never round-trips through a
markdown parser. Reading that note later is a design change owing adversarial
review.

**Capture defaults to off** (it reads transcripts; injection reads one note the
owner wrote).

**The distiller is fail-closed on binding, not on locality.** It binds under
`taste_profile_distill` in `llm.router.operation_mapping`; unbound means off
(`default_profile` is not consulted). It is **not** restricted to a local
provider: locality protects content not yet sent to a model, and a chat
transcript already went to the model that conducted the session. Binding it to
a *different vendor* is a new disclosure the owner should choose knowingly.
`RouterPinnedDispatch` is parameterized by operation and `require_local`
(channel ingest `true`, capture `false`).

**Admission is strict.** A candidate is refused — never truncated or shown
bare — when it exceeds the length ceiling, has no verbatim evidence, or falls
below `min_confidence` (gate written `!(x >= min)` so NaN fails). A wrong
directive shapes every future session; a missed one costs a second chance.

### Identity, and why it is a content hash

A proposal id is BLAKE3 over the *normalized* directive (trim, collapse
whitespace, casefold), so a rejected directive re-derived later gets the same id
and is not re-proposed. Punctuation and negation are **not** stripped ("never
use em-dashes" ≠ "use em-dashes"). Pinned by test.

### Approving, and the ordering that matters

The mediated write **only inserts lines — never edits, reorders or deletes
one** (a test checks every original line byte-for-byte). An unknown destination
heading creates a new section rather than guessing.

`notes.rs` locks per call, so the profile write and status write are not
atomic. Order: **note first, then status** — a crash leaves the proposal pending
and re-approving is idempotent. The reverse could mark approved with the
directive lost. **Duplicate-visible beats silently-lost.** Pinned by test.

### Endpoints

```
GET  /api/magician/v2/taste-proposals
POST /api/magician/v2/taste-proposals/{id}/approve
POST /api/magician/v2/taste-proposals/{id}/reject
```

- Approve returns `placed: false` when the directive was already present.
- Deciding with no service installed is **503, not 404**.
- `GET` returns the pending queue **and** health from one store read, including
  `capture_health` / `capture_health_unavailable`. Accept-rate is
  `approved / (approved + rejected)`, absent until the first decision.

The review panel (`ui/unified-ui/src/lib/taste/TasteProposalsPanel.svelte`, on
`/memory`) shows evidence quotes by default and displays retry/unavailable
status.

### The distiller prompt

`taste_profile_distill` v1.2.0. The transcript is **not** a prompt variable: it
is the user turn, because untrusted input must not sit inside the instructions
judging it. The template names where the transcript arrives. The JSON example's
braces are safe — the renderer replaces only *declared* `{name}` variables; an
undeclared slot renders literally.

### The sweep

`TasteCaptureWorker` runs every 15 min. **One worker enumerates every scope per
sweep** (a boot-time per-scope snapshot would miss later workspaces). An
interval sweep, not a session-close hook, because there is no single close path.

- **Both sources**: chat sessions idle for 30 min (judged from the last
  message, not a status field) and executions in a terminal `WaitingState`.
  Running executions are never read. Both newest-first by explicit sort.
- **Batch split**: chat takes the odd slot and hands back what it does not use.
  When one daily slot remains, chat and execution alternate across sweeps and
  restarts; an empty execution queue returns the slot to chat.
- **Bounded**: refuses before reading any transcript once the day's proposal cap
  is met, and processes a handful of sessions per pass.
- Only plain owner text reaches the distiller; tool calls and structured
  payloads are excluded. Transcript rides the **user** turn.
- Locality guard is re-resolved against live config each pass.

**Watermark.** `taste-capture-<scope digest>.json` records processed **ids**
(bounded 500 per kind, not a timestamp, which would skip silently), plus
completion, no-new-proposal, filed and failed-attempt counters and pending
retries. One capped list per kind: executions prefixed `exec:`, chat ids bare; a
legacy single list is split by kind on read. Empty sessions are recorded too.
Idle chat sessions with new revisions can be captured again; legacy completed
ids get a baseline without replaying history.

**Reliability.** Provider errors, timeouts, invalid JSON and storage failures
leave the item unprocessed and retry with persisted backoff (15 min up to one
day). A valid empty extraction completes; missing `candidates` is invalid. Due
execution retries keep reserved capacity after leaving the newest discovery
page. Corrupt watermarks and failed durable writes report failure rather than
resetting progress.

### Capture lifecycle

Retraction ("still true?") proposals cite the standing directive and contrary
session evidence; the owner removes or retains it. Addition and retraction have
distinct decision identities: an approved addition cannot suppress a later
retraction with matching text, and an opposite approved transition opens a new
decision generation (so a retracted directive can be restored). Replays and
in-batch duplicates keep the existing decision; legacy decisions remain
recognized. See [shared memory lifecycle](memory-lifecycle.md).

Not implemented: a separate provenance sidecar, and automatic conversion of
profile directives into preference-store entries. Evidence and decision history
stay in the capture store.

## Slice 3 — applicability, not similarity

Module: `magician/src/magician_v2/memory_applicability.rs`, wired into
`agents/memory_prompt_blocks.rs` between dedupe and lane selection.

Cosine similarity to the goal is not applicability: a delivery-style preference
can govern a research task while sitting far away in embedding space. One
bounded judge call decides which candidate preferences govern the task.

**No scope-tag pre-filter.** There is no write path to persist tags where
selection reads them: a candidate's `metadata_json` is either a synthesized
literal (`{"candidate_kind": "tier"}`) or the stored value wrapped as
`{"semantic_memory_type": …, "value": <scalar>}`. Adding tags would change every
preference's stored shape and rendered prompt text; the judge needs none. Do not
re-attempt this from the design doc alone.

### Failing safely is the design

**Every failure returns the incoming order unchanged**: unbound operation, no
router, model error, timeout, unparseable reply, or a verdict set that does not
match what was asked. The incoming order is the lane's relevance ranking, so the
fallback is never worse than the baseline.

- `narrow` does **not** re-rank; it bounds the set and breaks ties stably.
- Unjudged candidates keep their position (partial answers are common).
- Rejected candidates are demoted, not removed.

### Cost, and why the cache is load-bearing

Bound under `memory_applicability_judge`, 3-second timeout enforced locally.
The render path runs on every prompt assembly, many times per run, so verdicts
are cached on `(goal digest, exact candidate set)` with a 5-minute TTL and a
256-entry bound, consulted *before* resolving bindings or rendering. Empty
verdicts are cached too. The TTL is short because the key covers *which*
preferences were judged, not their contents.

### Blast radius

`apply_applicability_judge` touches the **user-preference lane only**,
rewriting entries into the same slots: no other lane is displaced and the
admitted count cannot change. A mismatched verdict set writes nothing.

### The frozen eval

`data/magician_v2/memory_evals/taste-applicability-regression.json`, on the
`memory_eval_runner` chassis. The chassis asserts presence, so precision comes
from tight `max_entries` per case. **Raising `max_entries` to make a case pass
means the selector regressed.** Every case sets `skip_if_absent`.

### Budgets

`user.user_preference` 6/2400, `agent_goal.user_preference` 2/800, `agent`
0/0 — an agent's own scope is not where owner taste belongs.

## Configuration

`TasteProfileSettings` lives on the `memory` block as `taste_profile`
(`enabled` / `note_path` / `proposals_note_path` / `max_chars`), defaults in
code: enabled, `profile.md`, 4000 chars. The key is **absent from the shipped
YAML** (comment only): `memory` rejects unknown fields, so an active key would
break an older binary sharing the config.

`proposals_note_path` is where proposals awaiting approval are filed; the
profile note's safety rests on it being elsewhere. Unset, it derives from
`note_path` by inserting `.inbox` before the extension (`profile.md` →
`profile.inbox.md`), so repointing the profile carries its proposals along.

`TasteProfileSettings::validate` runs from `enforce_memory_config_invariant`.
While enabled: `note_path` must be non-empty and resolvable by the notes read
path (relative, no parent/root components, Markdown extension); the effective
`proposals_note_path` must satisfy the same rule **and differ from
`note_path`**; `max_chars` ≥ 1. A disabled profile is not validated.
