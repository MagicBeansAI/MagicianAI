# Task-Result Summary (chat card + `/tasks` listing)

How a completed task's result shows as readable text in the chat result card
and the `/tasks` listing, and how a long or noisy HTML deliverable is condensed
by an auxiliary Luna call after the primary answer is ready, cached once per
output revision, and shown without ever calling the model on a read path.

Agent-decision evidence, output-synthesis caps and the grounding gate that runs
before task-user publication are in
[Unified Task Panel](../unified-ui/unified-task-panel.md). Auxiliary
`task_summary` condensation starts only after that primary output is ready.

## File-backed execution artifacts

The result summary and the file are separate records. Terminal artifacts expose
task/execution-relative paths so web and native clients can offer
Open/Download. The execution artifact index writer records these addresses for
files inside the owning execution's output directory; reads recover missing
relative paths from older absolute-only records without rewriting history.
Canonical path checks reject files in another execution and symlinks escaping
the owning output folder. Tests: `make test-terminal-artifact-links`.

Media captured by the current execution has no copied-artifact source fields;
its capture payload must bind that execution and task. Copied records keep their
explicit source execution. Foreign producers and orphaned copy lineage are
rejected before media registration.

## Deliverables vs internal outputs

A task has one primary user deliverable, `out_task_user_*`, built directly from
the execution output. There is no separate agent-audience output
(`out_task_agent_*` is retired); agents read the user output.

`GET /api/magician/v3/tasks/{id}/outputs` marks every output with
`deliverable` (user-audience `primary_task_user` / `user_media`). Internal
entries, such as the continuation context (the state used to resume the task)
and the `task_summary.json` / `voice_speech.json` sidecars in `internal_files`,
belong under technical details. Each entry also carries `absolute_path` when the
file exists inside the task's `outputs/` directory. See
[internal tasks API](internal-tasks-api.md#get-apimagicianv3taskstask_idoutputs).

## The problem it solves

The final user deliverable defaults to **HTML** (`out_task_user_<id>.html`).
The chat result card (`ChatMessageContent::TaskStatusUpdate.summary`) and the
`/tasks` listing derive their summary from it, so loaders must read HTML; an
`.md`/`.txt`-only loader falls back to a generic "Execution completed." and
hides the result.

## How it works

### 1. HTML-aware loaders (zero LLM cost)

`load_task_user_summary_inner` (chat card) and `read_output_summary` (`/tasks`
listing) accept `out_task_user_*.html` and strip it to text via
`strip_html_to_text` (shared with the web-fetch handler: removes script/style,
drops tags, decodes entities, collapses whitespace). MD/TXT are used verbatim.
The result prose shows at once, with no model call.

### 2. Optional Luna condensation, after answer readiness

`FilesystemTaskUserFinalizer::finalize` persists and returns the primary user
output without summarizing. The terminal reducer commits that `OutputRef` and
clears `synthesis_pending`; that durable write is the answer readiness
boundary. Feed/progress refresh and summarization happen afterward and cannot
delay or roll back the answer. The summary worker is scheduled after the initial
feed/progress refresh, so task cards show the answer before the condensed
metadata arrives.

Continuation-context output, media `OutputRef` registration, terminal schedule
refresh and root episode recording use a separate post-answer worker keyed by
scope, execution and primary-output revision. It reloads fresh task/execution
state, merges auxiliary refs through the reducer instead of writing a stale
snapshot, and writes `terminal_auxiliary.json` only after required work
succeeds. The startup reconciler repairs a missing marker or episode; an
in-process single-flight claim coalesces duplicate terminal observers.

The background job strips the committed deliverable to text and gates it:

- **long** (> 250 chars) **OR noisy** (characters neither alphanumeric nor
  whitespace > 0.35 of the text, `task_summary_needs_llm`) →
  summarized once by the `task_summary` LLM op;
- otherwise → cached **verbatim**.

The result is written atomically to a **`task_summary.json` sidecar** beside the
output (like the `voice_speech.json` sidecar):

```json
{
  "text": "…",
  "generated_by": "llm" | "verbatim",
  "source_output_id": "out_task_user_…",
  "source_output_revision": "sha256…"
}
```

Both readers **prefer a current sidecar**, validating its stored revision
fingerprint against the authoritative primary `OutputRef`. File modification
times are not revision authority (an obsolete job can finish after the current
output), so a reused accumulate/rerun output id cannot briefly show its previous
summary. The LLM is **never** called on a read path; the `/tasks` listing loop
stays LLM-free. When `generated_by == "llm"`, a small **"— LLM generated"** note
is appended. The call is **best-effort**: a model or router outage cannot change
answer readiness, and the job caches verbatim text when the call fails. The
cache read admits at most 64 KiB with explicit encoded JSON depth/node ceilings,
then decodes from the same open file handle. Missing, malformed, oversized, deep
or concurrently changed sidecars are cache misses and never hide the output.

Sidecars without a revision fingerprint are not authoritative: readers fall back
to the primary output, and startup reconciliation treats them as stale and
schedules a revision-pinned replacement.

The background call stays part of the execution tree for routing, cancellation,
cost and telemetry. Before dispatch the worker reloads the source execution's
durable routing overlay, applies it to the summary router, and attaches a full
`TaskRef` (scope, task, root execution, source execution, the source
execution's actual owner agent including delegated children, chat session).
Eval and diagnostic executions therefore keep their selected profile instead of
falling back to a process-global production route.

The Luna call emits a paired `LLMRequestSent` and terminal
`LLMResponseReceived` for success, validation failure or router failure, with
task, execution, chat-session and explicit root-execution lineage, so tree-wide
analytics include this cost even though readiness does not wait for it.

> Tutor tasks keep their raw action-result path (the chat reader runs the tutor
> side effects first and prefers the sidecar only for non-tutor tasks).

### 3. Revision-idempotent background materialization

The logical key is scope, task id and an `OutputRef` revision fingerprint
(output identity, path/media type, creation instant, owning execution/plan,
source output ids). An in-memory claim coalesces concurrent observers within one
boot. After the model call, the worker takes the task's cross-process write lock
and revalidates the exact primary-output fingerprint just before atomically
replacing the sidecar, so a stale rerun can spend a provider call but cannot
overwrite the current revision. The sidecar is the durable materialization
marker.

Summary arrival refreshes only the keyed task feed item used by Tasks and Today.
It does **not** republish task progress or another terminal
`task.status_changed`, so chat subscriptions, Request Activity, output-ready
handling and completion speech see one completion. The frontend also
deduplicates off-call completion speech by `(task_id, execution_id)`.

Concurrent parent-conversation copies use a stable request-derived message ID.
When a canonical task result arrives after a generic completion, its text and
original-answer link replace that same copy without resetting read or playback
receipts. The storage update and receipt acknowledgement share a coordinator
lock, and web uses the canonical result timestamp to reject stale transport
updates. The current-turn delegation guard reuses an already-running child
instead of relaunching it with reworded instructions
(`make test-chat-completion-repeat`).

Output-synthesis provider failures keep the bounded retry and HITL recovery
path. A dispatch cancellation owned by a task/execution stops on the first
attempt (the cancelled authority cannot succeed on retry) and creates no
retry-synthesis HITL.

### Config (no routing code)

`llm-router.yaml` `operation_mapping.task_summary.default: op-task-summary-luna`
binds the op to a dedicated GPT-6 Luna profile with `tool_choice: none`,
reasoning effort `none`, `max_output_tokens: 2048` and low verbosity. Meeting
summaries keep their own `meeting_summary` mapping (`op-meeting-summary-local`,
`op-meeting-summary-remote` in cloud mode); the task-summary binding does not
change it. Routing is **config-only**
(`LLMOperation::Other("task_summary")` is map-keyed, no Rust enum change). The system prompt lives in the store:
`data/magician_v2/prompts/task_summary_system_v1.0.0.json` (edit there, never in
Rust).

## Startup recovery

Startup reconciliation scans completed tasks and fills missing or stale
revision-aware sidecars through the same runtime `task_summary` operation used
for new tasks. Tasks whose primary synthesis is still pending are skipped; their
normal completion path schedules the job. This is the canonical backfill path
and needs no read-time LLM work.

## Key files

| Concern | Path |
|---|---|
| Chat card summary loader (HTML-aware + sidecar-prefer) | `magician/src/magician_v2/chat/service.rs` (`load_task_user_summary_inner`) |
| `/tasks` listing summary (HTML-aware + sidecar-prefer) | `magician/src/magician_v2/artifact_v2/service.rs` (`read_output_summary`) |
| Answer-ready boundary + background revision claim | `magician/src/magician_v2/artifact_v2/service.rs` (`spawn_task_summary_for_output`, `reconcile_stale_task_summaries_at_startup`) |
| Sidecar + gate + LLM call | `magician/src/magician_v2/artifact_v2/service.rs` (`TaskSummarySidecar`, `build_task_summary_sidecar`, `commit_task_summary_sidecar_if_current`) |
| HTML→text helper | `magician/src/magician_v2/execution/compiled_handlers/web_fetch.rs` (`strip_html_to_text`) |
| Store prompt | `data/magician_v2/prompts/task_summary_system_v1.0.0.json` |
| Op → profile binding | `llm-router.yaml` (`operation_mapping.task_summary`) |
