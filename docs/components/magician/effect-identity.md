# Effect identity — which attempt did this, and did it fire

Design: `docs/archive/plans/2026-08-26-effect-identity-threading.md`.

## The problem it solves

A dispatch that leaves the process can fail in a way that proves nothing. A POST
that times out may have been received; a CDP connection that drops mid-click may
have landed the click. The runtime's own record is empty either way, so *"it
failed"* and *"we do not know"* must be told apart — the second must never be
silently retried.

Answering that needs a name for the attempt. Not for the *operation* — several
subsystems already identify operations by content, deliberately — but for the
particular try, stable across a crash and re-run of the same decision, distinct
across a deliberate retry.

## The key

```
effect_id = "{llm_call_id}:tool:{model_tool_call_id}"
```

`LlmToolLineageIdentity::tool_execution_id_for` is the single composer; nothing
formats this string by hand.

The properties that make it usable:

- **Stable across a crash-replay.** Re-running the same decision reuses the same
  `llm_call_id`, so the effect is recognised as already-attempted.
- **Distinct across a deliberate retry.** A retry re-decides, minting a new
  `llm_call_id`, so a legitimate second attempt is never suppressed as a
  duplicate.
- **Runtime-minted only.** Every writer derives it from a trace receipt the
  runtime issued. A model-supplied value must never reach it.

`None` means **not attributable** — never *"safe to repeat"*. Every field and
parameter carrying this reads the same way.

### Two ways it is refused

Both are deliberate, and both mean `None`:

- The turn carries no trace receipt. Telemetry can be absent; the dispatch still
  happens, and no key is invented to paper over it.
- The model gave the call no id. A blank id would compose to `{llm_call}:tool:`
  for *every* unidentified call in the turn, so two distinct requests would
  present one key — and a remote honouring it answers the second from the first's
  cached response, losing a real effect with no error raised.

The id is read off the candidate about to be dispatched, not looked up by
position in the turn: a positional derivation repeated at three dispatch sites
could drift silently and key the effect to a *different* tool call.

## Where it flows

`PrimitiveExecCtx::effect_id` carries it through dispatch. It reaches:

| Surface | What it does there |
|---|---|
| Native HTTP | derives the `Idempotency-Key` header |
| API-mining replay | same, per replayed step |
| Governed runtime | derives `CredentialCallId` |
| Governed MCP | names the attempt in the audit record |
| Protected app dispatch | held on the prepared value across the attestation fences |
| Outward assertions | stamped on each act transition |

### Sub-ids where one dispatch fans out

A workflow replay turns one model tool call into several HTTP requests. Each step
gets `{effect_id}:step:{step_id}`. One key for the whole replay would identify
nothing, and a remote honouring it would collapse the fan-out into a single
request.

## Derived keys

The raw id never leaves the process. Two domain-separated digests derive from it:

```rust
far_side_idempotency_key(effect_id) -> "mag-{32 hex}"   // sent to third parties
local_attempt_key(effect_id)        -> "{32 hex}"       // indexes our own records
```

They are separated because the far-side value travels to arbitrary remotes. If it
also indexed our audit trail, a remote holding it would hold our internal index.
The domain is hashed with a NUL separator, so no two domains can be made to agree
by choosing an effect id.

### When a far-side key is sent

Only on `POST | PUT | PATCH | DELETE`. A lost response to a GET costs nothing to
re-issue, so a key buys the caller nothing there, and a per-attempt header on
every read is noise to the remote and enough to defeat intermediary caching. On
the replay path the argument is stronger: that path reproduces a *recorded browser
request*, and a header no browser ever sent changes the fingerprint bot detection
reads.

**The runtime's key outranks one already on the action.** An `http_post` step
takes its headers from step parameters, and for an agent tool call those
parameters are the model's — so without this rule a model could author the one
header that must never be model-authored. Nothing at that layer can tell a
model's header from a pack author's, so the runtime value wins wherever there is
one. A dispatch with no `effect_id` forwards whatever it was given: there is no
runtime identity to substitute, and stripping the header would break a pack
managing its own without putting anything in its place.

The bound-HTTP app owner carries no key at all — it admits GET only, by
construction rather than omission.

## Retry safety at the transport

A CDP disconnect is not proof the command did nothing. `agent-browser` commands
are re-run after a disconnect only when they read state, move the viewport, or
re-issue a GET:

- **Retried:** `snapshot`, `screenshot`, `find`, `get`, `wait`, `scroll`,
  `open`/`goto`, and `tab list`.
- **Not retried:** anything that can commit input — `click`, `fill`, `type`,
  `press`, `select`, `upload`, `drag`, `eval` — plus every verb the list has not
  seen. Default-deny.

Navigation is included deliberately. Reconnecting puts the tab back on
`about:blank` or the session's initial URL, which destroys the evidence an agent
would need to judge an indeterminate result — so declining the retry means telling
it to re-observe a page the reconnect just navigated away from, after which it
re-issues the navigation anyway.

A command that is not retried returns its result annotated **INDETERMINATE**,
with an instruction the next decision can act on. An ordinary failure would invite
the model to re-issue the same click, moving the duplicate from the transport to
the model rather than preventing it. If the reconnect itself fails — the usual
reason the command disconnected — the annotated result is still returned, because
a session we could not restore makes that warning more important, not less.

## Did it fire — one answer from three rails

| Rail | How an attempt is found |
|---|---|
| Credential audit | `CredentialCallId` is `{prefix}-{local_attempt_key}`; compute it from the effect id |
| Outward acts | the effect id is stamped verbatim on each transition |
| App resource journal | via the loop, which logs the attempt beside the binding digest |

Two of these records are keyed by **what the effect is**, not who attempted it,
and that is deliberate in both cases:

- An **outward act** is keyed by capability, action and exact payload, so a replay
  of the same send resolves to the same disclosure. An attempt id in that key
  would turn two attempts at one disclosure into two disclosures.
- An **app resource journal** row proves a dispatch started by re-issuing
  byte-identical material and reading `AlreadyPresent`. That proof works only
  while the material is attempt-independent, so an attempt id there would make a
  deliberate retry stop matching and either conflict or double-start one
  reservation.

So the attempt goes on the outward act's *transition* — the append-only history,
where "when did we learn what" already lives — and, for the journal, stays with
the loop, which holds both the attempt id and the binding digest at every
recovery point.

### Reading it back — the outward reconciler

The table above says where an attempt is recorded. `reconcile_outward_effect`
(`execution/agentic/outward_settle.rs`) is what asks: the reader for the
`effect_id` on `OutwardActTransition`.

It answers one of three ways, and the third is the point:

| Act status | Verdict |
|---|---|
| `ProviderAccepted`, `Delivered`, `Corrected`, `Retracted` | `Fired` — the effect exists |
| `Failed`, `Prepared` never dispatched (`dispatched_at` unset), or no record at all | `DidNotFire` |
| `Failed`, `Prepared` after dispatch, with a provider message id | `Fired` — a hard bounce is a send a provider took |
| `Failed`, `Prepared` after dispatch, no provider message id | `StillUnknown` |
| `Dispatching`, `DispatchUnknown` | `StillUnknown` |

`StillUnknown` is separate from `DidNotFire` because reading "we don't know" as
"nothing left" re-sends a live message. An act resting at `dispatching` is
evidence of neither a send nor a non-send.

`DidNotFire` on an **absent** record is sound only because the gate writes the
disclosure before anything leaves and fails the send closed if that write fails
(`record_outward_disclosure(...)?`). Every condition under which the gate skips
writing — not a pack dispatch, not an outward class, no scoped store — is mirrored
by a condition under which the reconciler returns `StillUnknown`, so there is no
state where the gate stayed silent and the reconciler still answers. `Prepared` is
likewise positive evidence: the production path writes both `mark_dispatching` and
`mark_dispatch_unknown` *before* the send, precisely so a process death reads as
"we do not know", which means an act still at `Prepared` never reached the send.

`by_this_attempt` distinguishes "the effect exists" from "this attempt made it" —
a byte-identical send from another attempt resolves to the same act. For deciding
whether to re-send only the first matters; for explaining what happened, both do.

It runs only where the loop cannot answer itself: a dispatch that ran a transport
and did not report success. Asking on the happy path would put a filesystem read
in front of every tool call to answer a question nobody had. **No new ledger** —
the contract's own non-goal — it reads the outward record that already exists.

## Side-effect state

`LlmToolSideEffectState` on a lineage record answers whether a dispatch's effects
stand:

| Transport | Tool reported | State |
|---|---|---|
| did not run | — | `None` |
| ran | success | `Remained` |
| ran | not success | `Unknown` |

Reversal is separate: only an authoritative rollback marker, written by the
runtime that attempted restoration, yields `Reversed` or `RollbackFailed`, and
only on the rollback-finished record.

**What this axis does not say.** It answers whether effects *if any* stand, not
whether there were any — a successful read resolves to `Remained`, vacuously.
Separating read-only dispatches needs a signal the emitters are not given:
`classify_tool_dispatch` is built from a success flag and a failure code, and the
action never reaches it. So query `Unknown` to find the dispatches that need
reconciling; `Remained` alone is close to a restatement of
`tool_reported_success`.

`Remained` is guarded rather than trusted: a record may claim it only if the same
record says a transport ran. It is the one state a reader acts on without checking
anything else.

## Determination does not depend on telemetry

`finish_agentic_tool_lineage` opens with `lineage?` and `event_broadcaster?`, so
everything after those lines is skipped when observability is unconfigured.
`classify_agentic_tool_dispatch` therefore sits *before* them and takes neither: it
decides what happened, and only then does the file emit anything about it.

Anything durable belongs on that side of the line. A runtime that forgets whether
it charged a customer because nobody was subscribed to its event bus has the
failure exactly backwards.

Note that `agentic_tool_repeat_counts` and `agentic_failed_tool_fingerprints` are
observability continuity, not limiters — nothing withholds a tool on their
strength. They are carried across a pause so a resumed run's counts continue the
original series.

## App invocation and terminal recovery

Governed App dispatch uses the validated per-call effect ID, scoped by execution
session, for its resource reservation and retained-result lookup. A model turn
can contain several tool calls, so its iteration number is not a sufficient
invocation key. Calls without valid effect attribution fail before App dispatch.

Terminal outbox recovery can retire a physically deleted task without recreating
its execution. It validates the exact scope, segment, terminal sequence and
receipt seal, then holds the task transaction lock while checking a valid durable
deletion marker and the absence of both task directories. That disposition
suppresses only the exact terminal batch, retains the immutable journal, and
saves the cursor under the current loop lease. Missing executions, partial
deletion, malformed markers and failed cursor saves retain recovery debt.

## Known gaps

- **The elicitation discovery rail** runs LLM-authored shell, `curl` included,
  entirely outside the loop's dispatch — so no attempt id, no far-side key, no
  lineage. It is not wired (constructed only in its own tests); wiring it needs a
  dispatch path, not a parameter.
- **Unattributable dispatches** still forward a caller-supplied `Idempotency-Key`.
  Model control of request headers in general is a broader question than effect
  identity.
- **A durable effect-id → binding-digest index** does not exist; the join to the
  app resource journal is a log line.
- **A ceiling-only pause is not tamper-checked.** `authorization_hash` covers the
  four ceilings, but it is verified only for a pause that `is_elevation()`, and a
  ceiling deliberately does not make one. Closing this needs an authority entry
  written for non-elevation pauses, not a wider predicate — see
  `agentic-loop-termination.md`.
