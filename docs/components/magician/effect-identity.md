# Effect identity — which attempt did this, and did it fire

Design: `docs/archive/plans/2026-08-26-effect-identity-threading.md`.

## The problem it solves

A dispatch that leaves the process can fail in a way that proves nothing: a POST
that times out may have been received, and a CDP connection that drops mid-click
may have landed the click. The runtime's record is empty either way, so "it
failed" and "we do not know" must be told apart, and the second must never be
silently retried.

That needs a name for the attempt: not for the operation (several subsystems
deliberately identify operations by content) but for the particular try, stable
across a crash and re-run of the same decision and distinct across a deliberate
retry.

## The key

```
effect_id = "{llm_call_id}:tool:{model_tool_call_id}"
```

`LlmToolLineageIdentity::tool_execution_id_for` is the only composer; nothing
formats this string by hand.

- **Stable across a crash-replay.** Re-running the same decision reuses its
  `llm_call_id`, so the effect is recognised as already attempted.
- **Distinct across a deliberate retry.** A retry re-decides and mints a new
  `llm_call_id`, so a legitimate second attempt is never suppressed.
- **Runtime-minted only.** Every writer derives it from a runtime-issued trace
  receipt; a model-supplied value never reaches it.

`None` means **not attributable**, never "safe to repeat", wherever it appears.

### Two ways it is refused

Both deliberately yield `None`:

- The turn has no trace receipt. Telemetry may be absent; the dispatch still
  happens, and no key is invented.
- The model gave the call no id. A blank id would compose to `{llm_call}:tool:`
  for every unidentified call in the turn, so two requests would share a key and
  a remote honouring it would answer the second from the first's cached
  response, losing a real effect silently.

The id is read off the candidate being dispatched, not looked up by position in
the turn, which could drift across dispatch sites and key the effect to another
call.

## Where it flows

`PrimitiveExecCtx::effect_id` carries it through dispatch to:

| Surface | What it does there |
|---|---|
| Native HTTP | derives the `Idempotency-Key` header |
| API-mining replay | same, per replayed step |
| Governed runtime | derives `CredentialCallId` |
| Governed MCP | names the attempt in the audit record |
| Protected app dispatch | held on the prepared value across the attestation fences |
| Outward assertions | stamped on each act transition |

### Sub-ids where one dispatch fans out

A workflow replay turns one tool call into several HTTP requests; each step gets
`{effect_id}:step:{step_id}`. One key for the whole replay would let a remote
collapse the fan-out into one request.

## Derived keys

The raw id never leaves the process. Two domain-separated digests come from it:

```rust
far_side_idempotency_key(effect_id) -> "mag-{32 hex}"   // sent to third parties
local_attempt_key(effect_id)        -> "{32 hex}"       // indexes our own records
```

The far-side value travels to arbitrary remotes, so it must not also index our
audit trail. Domains are hashed with a NUL separator, so no effect id can make
two domains agree.

### When a far-side key is sent

Only on `POST | PUT | PATCH | DELETE`. A lost GET response costs nothing to
re-issue, and a per-attempt header on every read is noise that defeats
intermediary caching. On the replay path, which reproduces a recorded browser
request, a header no browser sent would also change the fingerprint bot
detection reads.

**The runtime's key outranks one already on the action.** An `http_post` step
takes headers from step parameters, which for an agent tool call are the
model's, and nothing at that layer can tell a model's header from a pack
author's. So the runtime value wins wherever there is one. A dispatch with no
`effect_id` forwards whatever it was given: there is nothing to substitute, and
stripping the header would break a pack managing its own.

The bound-HTTP app owner sends no key at all; it admits GET only, by
construction.

## Retry safety at the transport

A CDP disconnect is not proof a command did nothing. `agent-browser` commands
are re-run after a disconnect only when they read state, move the viewport or
re-issue a GET:

- **Retried:** `snapshot`, `screenshot`, `find`, `get`, `wait`, `scroll`,
  `open`/`goto`, and `tab list`.
- **Not retried:** anything that can commit input (`click`, `fill`, `type`,
  `press`, `select`, `upload`, `drag`, `eval`) and any verb not on the list.
  Default-deny.

Navigation is retried on purpose: reconnecting resets the tab to `about:blank`
or the initial URL, which destroys the evidence an agent would need, and it
would re-navigate anyway.

A command that is not retried returns its result marked **INDETERMINATE**, with
an instruction the next decision can act on. A plain failure would invite the
model to re-issue the click, moving the duplicate rather than preventing it. If
the reconnect itself fails, the marked result is still returned.

## Did it fire — one answer from three rails

| Rail | How an attempt is found |
|---|---|
| Credential audit | `CredentialCallId` is `{prefix}-{local_attempt_key}`; compute it from the effect id |
| Outward acts | the effect id is stamped verbatim on each transition |
| App resource journal | via the loop, which logs the attempt beside the binding digest |

Two of these are keyed by **what the effect is**, not who attempted it, on
purpose:

- An **outward act** is keyed by capability, action and exact payload, so a
  replay of the same send resolves to the same disclosure. An attempt id in the
  key would make two attempts two disclosures.
- An **app resource journal** row proves a dispatch started by re-issuing
  byte-identical material and reading `AlreadyPresent`. An attempt id in the
  material would make a deliberate retry conflict or double-start a reservation.

So the attempt goes on the outward act's append-only *transition* history and,
for the journal, stays with the loop, which holds both the attempt id and the
binding digest at every recovery point.

### Reading it back — the outward reconciler

`reconcile_outward_effect` (`execution/agentic/outward_settle.rs`) reads the
`effect_id` on `OutwardActTransition` and answers one of three ways:

| Act status | Verdict |
|---|---|
| `ProviderAccepted`, `Delivered`, `Corrected`, `Retracted` | `Fired` — the effect exists |
| `Failed`, `Prepared` never dispatched (`dispatched_at` unset), or no record at all | `DidNotFire` |
| `Failed`, `Prepared` after dispatch, with a provider message id | `Fired` — a hard bounce is a send a provider took |
| `Failed`, `Prepared` after dispatch, no provider message id | `StillUnknown` |
| `Dispatching`, `DispatchUnknown` | `StillUnknown` |

`StillUnknown` is kept apart from `DidNotFire` because reading "we don't know" as
"nothing left" re-sends a live message. An act resting at `dispatching` proves
neither a send nor a non-send.

`DidNotFire` on an **absent** record is sound only because the gate writes the
disclosure before anything leaves and fails the send closed if that write fails
(`record_outward_disclosure(...)?`). Every case where the gate skips writing (not
a pack dispatch, not an outward class, no scoped store) maps to `StillUnknown`,
so the reconciler never answers where the gate stayed silent. `Prepared` is
likewise positive evidence: production writes `mark_dispatching` and
`mark_dispatch_unknown` before the send, so a process death reads as "we do not
know", and an act still at `Prepared` never reached the send.

`by_this_attempt` separates "the effect exists" from "this attempt made it" (a
byte-identical send from another attempt resolves to the same act). Re-send
decisions use only the first.

It runs only where the loop cannot answer itself: a dispatch that ran a
transport and did not report success. On the happy path it would add a
filesystem read to every tool call. **No new ledger**: it reads the existing
outward record.

## Side-effect state

`LlmToolSideEffectState` on a lineage record says whether a dispatch's effects
stand:

| Transport | Tool reported | State |
|---|---|---|
| did not run | — | `None` |
| ran | success | `Remained` |
| ran | not success | `Unknown` |

Only an authoritative rollback marker, written by the runtime that attempted
restoration, yields `Reversed` or `RollbackFailed`, and only on the
rollback-finished record.

**What this axis does not say.** It says whether effects, if any, stand, not
whether there were any: a successful read is `Remained`. Separating read-only
dispatches would need a signal the emitters lack (`classify_tool_dispatch` sees a
success flag and failure code, never the action). Query `Unknown` to find
dispatches needing reconciliation; `Remained` nearly restates
`tool_reported_success`.

`Remained` is guarded: a record may claim it only if it also says a transport
ran. It is the one state a reader acts on without further checks.

## Determination does not depend on telemetry

`finish_agentic_tool_lineage` (`execution/agentic/executor.rs`) returns early on
`lineage?` and `event_broadcaster.as_ref()?`, so everything after is skipped when
observability is off. Its first call, `classify_agentic_tool_dispatch`, runs
*before* those returns and takes neither: it decides what happened, then the
function emits. Anything durable belongs on that side; a
runtime must not forget whether it charged a customer because nobody subscribed
to its event bus.

`agentic_tool_repeat_counts` and `agentic_failed_tool_fingerprints` are
observability continuity, not limiters; nothing withholds a tool on their
strength. They carry across a pause so a resumed run continues the series.

## App invocation and terminal recovery

Governed App dispatch uses the validated per-call effect ID, scoped by execution
session, for its resource reservation and retained-result lookup; a turn may
hold several tool calls, so the iteration number is not enough. Calls without
valid effect attribution fail before App dispatch.

Terminal outbox recovery can retire a physically deleted task without recreating
its execution. It validates the exact scope, segment, terminal sequence and
receipt seal, then, holding the task transaction lock, checks for a valid durable
deletion marker and the absence of both task directories. It then suppresses
only that terminal batch, keeps the immutable journal and saves the cursor under
the current loop lease. Missing executions, partial deletion, malformed markers
and failed cursor saves keep the recovery debt.

## Known gaps

- **The elicitation discovery rail** runs LLM-authored shell, `curl` included,
  outside the loop's dispatch: no attempt id, far-side key or lineage. It is not
  wired (constructed only in its own tests); wiring it needs a dispatch path.
- **Unattributable dispatches** still forward a caller-supplied
  `Idempotency-Key`. Model control of request headers in general is a broader
  question.
- **No durable effect-id → binding-digest index** exists; the join to the app
  resource journal is a log line.
- **A ceiling-only pause is not tamper-checked.** `authorization_hash` covers the
  four ceilings but is verified only for a pause that `is_elevation()`, and a
  ceiling does not make one. Closing it needs an authority entry for
  non-elevation pauses; see `agentic-loop-termination.md`.
