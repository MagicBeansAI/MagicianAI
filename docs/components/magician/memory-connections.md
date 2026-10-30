# Memory connections and owner attention

Magician can relate an existing attention item to relevant memories and offer
context or ask the owner for a clarification. The model reasons across goals,
tensions, opportunities and complementary facts; domains such as food, health,
work and travel do not have separate rules.

This is a background advisory path, not a synchronous interceptor of every chat
message, purchase or external action. Activity must first enter the existing
scoped attention corpus and memory recall. Source ingestion and the hourly
scorer can add delay. Citation validation establishes supporting text; it cannot
prove that every model-inferred relationship is true or useful.

## Retrieval and judgement

The resurfacing worker reconciles connections every minute. It reserves at most
**three reviews per principal/workspace per rolling hour**, durably before recall
or model I/O; empty inputs, cached decisions, rejected replies and crashes still
spend a reservation, and check and debit share one SQLite transaction across
workers. A review that ends before any decision exists (the review timed out, no
route or engine answered, or its sources could not be read) is recorded
`deferred` and released from that budget. Deferrals have their own cap of three
per scope per rolling hour, enforced in the same transaction, and a pass stops at
its first deferral. A failing engine therefore costs at most one review timeout
per tick, and the candidate keeps its one-hour hold. Each deferral logs its root
cause (`memory connection review deferred`, debug; warn when the scope already
deferred within the hour).
Active decisions are capped at 100 per scope and expire after seven days.
Reconciliation spends no generation budget.

The existing interaction registry verifies the source. Deleted, stale,
suppressed, unsupported and unavailable sources are ineligible. Canonical user
memory activities also use the shared lifecycle predicate: marking an entry
superseded or replaced withdraws it even when its text has not changed. User
memory recall uses hybrid retrieval when available, falling back to direct
retrieval. Recall gets five seconds, at most 18 memories and a 12,000-character
render budget. The model sees at most 1,000 characters per memory and 2,500 for the
activity. Scope, supersession and app processing gates still apply; local-only
app material cannot enter this remote background operation. Recall runs no
extra preference judge and records no prompt usage. The hybrid recall path
heap-allocates its large async sub-operations, so it runs on a normal 2 MiB
worker stack.

The `memory_connection_review` operation is mapped to
`gpt6luna-responses-toolsany` in the repo-root `llm-router.yaml` (also compiled
in as the built-in router fallback, `magician/src/config.rs`) and in the live
router file, and appears in Settings' operation mappings. It has its own route,
independent of any chat/execution harness. Removing the binding stops
generation; reconciliation still handles existing answers and withdrawal.
`RESURFACING_ENABLED` stops the whole resurfacing worker.

A review runs under one 60-second budget: the `memory_connection_gate` engine
decides within half of it (capped by the engine's own decision and queue
budgets), and accepted text gets the rest. The caller's outer timeout
(`REVIEW_TIMEOUT`) is that budget plus a five-second margin, derived from the
same constant, so it never cuts a decision the engine was granted. The judge's
prompt says to usually return
`{"connection":null}` and that similar topics alone are insufficient. It must cite
the activity and one to three distinct supplied memories with exact quotes of
12–300 characters. Unknown IDs, invented quotes, duplicate evidence, invalid
output or insufficient confidence produce no alert. Information requires 0.8
confidence; questions require 0.9 and a bounded nonempty question. These are
admission thresholds, not guarantees of model correctness.

## Existing surfaces and responses

| Result | Delivery | Owner response |
| --- | --- | --- |
| Optional context | **Worth a look**, attached to the original candidate | Existing Open, Dismiss and contextual actions keep their real source targets. The explanation includes quotes and references. |
| Useful personal insight | **For you**, through the existing Changed/learning feed projection | An individual card opens full feed details and has its own dismissal identity. |
| Clarification needed | Existing **HITL / Needs you** user request | **Got it** acknowledges; **Remember my clarification** accepts free text; **Dismiss** closes it. Timeout dismisses. |

Questions cannot approve or execute actions. Inferred advice never authorizes
tools, changes a purchase, cancels work or overwrites a goal. Remembering saves
only the owner's supplied words as an explicit user-memory revision linked to
the cited context. The inferred summary is not promoted. The
[shared memory lifecycle](memory-lifecycle.md) reconciles that revision against existing claims rather than treating its unique
clarification key as proof that it is an unrelated fact.

Connection detail links always identify their own feed item, including when a
card also carries an originating task, thread or other source identifier.

HITL uses `submit_nonblocking_durable` with a deterministic scope-qualified ID.
The worker consumes durable response history, so handling does not depend on an
in-memory callback. Clarification writes use an idempotent merge key; feedback uses
a stable response event ID so a replay cannot increase dismissal counts. Sources
are revalidated after judgement and when consuming an answer; stale answers are
refused. These are validation points, not one atomic transaction across every
independent source store.

An insight or question holds its original candidate out of Worth a look while
it occupies another lane. Changed or ineligible evidence withdraws active
connection text, cards and questions on reconciliation. Owner feedback and
durable decisions prevent recreation on restart. A changed source can be
reconsidered after withdrawal; unchanged successful-empty judgements are cached.
Completed historical responses remain in their existing owner audit stores.

## Persistence and diagnostics

Derived decisions live in `resurfacing_memory_connections` in the existing scoped
resurfacing database, retaining source identities/revisions and generation state.
They are not a memory tier or a second evidence store. Delivery reuses the
attention router, feed store and user-request service. Review attempts use the
`memory_connections` resurfacing run kind; idle zero-review ticks are omitted.
The [memory lifecycle](memory-lifecycle.md) pass that runs first on the same
tick is timed and recorded separately as `memory_lifecycle`, so
`memory_connections` durations cover the connection pass only. For both
kinds a repeating failure writes one row per 15 minutes per scope unless its
message changes, so a persistent fault cannot crowd the bounded run history.
Memory-effect Shadow/Canary/Enforced is a separate ranking mechanism.

Applicability reorders precisely the 12 offered preferences even when recall
contains more, preserving later preferences and other lanes.
[Taste capture](taste-profile.md) distinguishes failures from successful empty
extractions and exposes durable retry health.

Actual model decisions have a separate
[behavioural evaluation lane](memory-connections-evaluation.md); its scenario
results, journey checks and owner usefulness review are distinct from
implementation-test pass counts.

### Approved profile handoff

Connection review also reads the existing scope's owner-managed taste profile
through `TasteProfileLoader`. This includes approved capture proposals and direct
owner edits, and excludes pending proposals. It does not duplicate the profile
into a structured memory tier. A profile is a separately identified, quoted source
whose full content revision is checked again before delivery and owner-answer
processing. Changing, disabling, or losing access to that profile withdraws a
connection citing it. Reads have a five-second timeout; notes over 4,000 characters
are skipped as a whole, so truncation cannot hide an exception or qualification.
Structured recall continues when the profile is absent.

The review prompt requires a concrete unmet condition, useful opportunity, or
relevant unresolved ambiguity. It preserves the timing, context and exceptions
in conditional preferences, and returns null when a preference's trigger is
absent or already satisfied (no routine reassurance). Behavioral evaluation measures this separately from quote
validation and the model's own confidence.
