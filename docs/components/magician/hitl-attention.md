# HITL / Attention contract

Every human-in-the-loop ask — approve a planned action, answer a planner clarification, decide an agentic
pause, sign off a plan-version draft, supply a one-off input — flows through one **canonical envelope**
and one **canonical endpoint**. The operator sees one badge, one inbox, one response modal.

## Sources (what kinds of HITL exist)

| `source` | When fired | Emit site |
|---|---|---|
| `agentic` | `need_user_input` / `cannot_proceed` from an agentic loop, plus `waiting_for_confirmation` / `max_iterations_reached` lifecycle | `native_catalog.rs` + executor escalation paths |
| `inner_loop` | Legacy value only: the nested inner loop (`InnerLoopUserInput`) is retired and nothing emits it; respond/feed paths still accept it alongside `agentic` / `primitive` / `escalation` | `magician-api/src/web_api.rs` (respond dispatch), `feed_api.rs` |
| `user_request` | `UserRequestService::ask` — channel-agnostic ask-the-human primitive | `user_requests/service.rs` |
| `approval` | Pre-execution `AgentConstraints.requires_approval` gate flags a step | `agents/approval_service.rs` |
| `plan_approval` | Task plan transitions to `Draft` (ready for review) | `artifact_v2/service.rs::emit_plan_approval_requested` |
| `clarification` | Planner needs slot value (V3 plan + V2 ask-loop) | `artifact_v2/service.rs::emit_v3_planning_clarification_needed` + `realtime_events.rs::clarification_queued` |
| `escalation` | Loop-detected, sandbox-override, tool-authorization, max-iterations | executor escalation paths |
| `diff_approval` | Approval-gated code changes. Resolves native `FileEditTransaction` ids or Pi-style `CodeChangeProposal` ids. | `execution/compiled_handlers/staged_file_edit.rs` and `execution/file_edit/proposal.rs` |
| `bot_auth` | Bot adapter (gmail/whatsapp/telegram/kapso) transitions into `NeedsAuth` / `AccountMismatch` | `magician-learning/src/bots/auth_hitl_broker.rs` — `AuthHitlBroker::record_snapshot` |

`bot_auth` is broker-mediated: the `GET /bots/auth` (`list_bots_auth`) poll runs every snapshot through
`AuthHitlBroker::record_snapshot`, which compares the persisted last-known cache and emits
`HitlRequested` / `HitlResolved` on transitions.

## Canonical wire shape

```rust
RuntimeTransportEvent::HitlRequested {
    correlation_id: String,      // dedup key — also the URL path id
    source: String,              // one of the sources above
    input_type: String,          // confirmation / text / password / choice /
                                 // multi_choice / external_action / file_path /
                                 // guidance / form
    prompt: String,
    hint: Option<String>,
    input_schema: Option<Value>, // {options?, multiline?, placeholder?,
                                 // confirm_label?, deny_label?, questions?,
                                 // chain_id?, chain_position?, chain_total?, …}
    task_id: Option<String>,
    execution_id: Option<String>,
    agent_id: Option<String>,
    principal: Option<String>,
    workspace: Option<String>,
    timestamp: i64,
}
```

Mirrored by `RuntimeTransportEvent::HitlResolved { correlation_id, source, outcome ∈ {responded, expired,
cancelled, dismissed}, decision: Option<String>, … }`.

**A correlation id is one ask.** The lifecycle journal is one-shot per correlation id: once a
correlation is resolved, a later `HitlRequested` under it is refused (a replayed request must not
resurrect an answered card). An agentic pause's storage key (`<execution>:<plan>:<step>`, or
`agent:<agent>:<goal>:<cycle>`) names the step's pause *slot* and is reused by the next question the
same step raises, so the canonical id of an agentic ask is `AgenticPauseState::hitl_correlation_id()`
= `<storage key>~<ask id>` (`~` never occurs in a key; the ask id is 12 hex chars minted when the pause
is built and persisted with it). A second question in one execution — an OTP re-ask after the service
rejected the first code, a confirmation after a password — is therefore a new request on every feed,
a new retrieval challenge for the verification-code resolver, and a new delivery for the coordinator.
Legacy `AgenticWaitingForUser` / `AgenticResumed` / `input.requested` markers keep the bare storage
key in `pause_state_id`; the attention snapshot resolves a marker by either spelling. Clients post the
correlation id back verbatim: the agentic resume path splits it (`split_hitl_correlation_id`), looks the
pause up by its key, and refuses with **409 `superseded_ask`** — restoring the pause — when the pause now
held under that key is a later ask than the one answered. A bare key (a pre-existing client, or a pause
written before asks had identities) still selects whatever the key holds.

**Persistence.** Every emit goes through `RuntimeTransportBroadcaster::emit` (not `emit_transport_only`),
so `map_v2_realtime_event` writes a `hitl.requested` / `hitl.resolved` row into the per-scope
`events.jsonl`. `/api/magician/v3/events?category=hitl` SSE backfill reads from there, so a `/attention`
page loaded after a pending HITL fired still sees it.

**Resolution scope.** The live `/v3/events` tail filters by **exact** `(principal, workspace)` equality,
so `HitlResolved` MUST carry the same scope as the matching `HitlRequested`.
`emit_approval_resolved_event` and `emit_approval_expired_event` (`magician-api/src/web_api.rs`) stamp
the approval's **originating** scope from `ApprovalRequest.{principal,workspace}` (captured from agentic
pause state at create time), falling back to the resolver's scope only for legacy approvals that predate
the stored field. Otherwise a web-session resolve of an `anonymous/default` desktop overlay HITL leaves
that overlay's card and tray badge stranded. The desktop notify-overlay also self-heals its tray count on
a timer — see `docs/components/unified-ui/notification-overlay.md`.

## Sensitivity contract (secure HITL credentials, P1)

A `user_request` service request carries a server-owned sensitivity contract,
`UserRequest.sensitive: Option<SensitiveInputSpec>`
(`magician/src/magician_v2/user_requests/sensitive.rs`). It is metadata only —
never a value — and is serialized into the pending and history shards (so a
client reading a pending request sees it). **It is published on every
announcement:** `hitl.requested` carries it as `input_schema.sensitive` for
the `user_request` source (`hitl_requested_event`, written last so a
producer's own `input_schema` cannot shadow it) and for the `agentic` source
(`emit_hitl_requested_for_agentic_pause` → `hitl_input_schema_for_pause`, which
keeps the spec even for a protected app whose question is hidden), so a client
masks by the spec and never by the request-type name or the wording. `None`
means an ordinary request.

```rust
SensitiveInputSpec {
    kind: Option<SensitiveKind>,        // whole-answer kind: login_identifier / password / otp / other
    fields: Vec<SensitiveField>,        // per-field kinds for a form; an absent field is ordinary
    provenance: SensitiveProvenance,    // producer / typed_input / form_schema / heuristic
    one_time: bool,                     // one bound use (browser JIT, OTP)
    collection_deadline_ms: i64,        // material is unavailable after this even if never taken
    challenge_id: Option<String>,       // the challenge this ask answers (P4), else None
    revision: u32,
    expected_destination: Option<String>, // where the answer is delivered (P4): host / origin / program
}
```

**Classification** happens once, at acceptance
(`UserRequestService::accept_request_with_sensitive_receiver` →
`user_requests::classify::classify_sensitive`; also on restore of a pending
row written before the contract), and the result is stored on the request;
nothing downstream re-derives it. Only a request that collects a free-text
value is classified: `input_type ∈ {text, guidance, password, otp, form}`, or
no `input_type` at all with no options — or with options where one of them sets
`requires_input`, because such an option collects free text exactly as a typed
ask does. A request typed `choice`, `multi_choice`, `confirmation`,
`external_action`, or `file_path`, one whose options all merely decide and which
carries no `input_type` (the shape of every approval, notification, and relay
producer), and the one-time fill confirmation never get a spec, so their wording
can neither turn a decision into a cancel nor shorten their window. (The
presence of options alone must never read as "decision": an option that sets
`requires_input` collects free text.) A
decision answer to a request that does have a spec keeps its decision and
carries no sensitive answers. Precedence: (1) a spec set by a
trusted in-process producer is kept untouched; (2) `request_type ==
secure_browser_input` → `password`, one-time, `producer`; (3)
`context.input_type == "password"` → `password` (`typed_input`; one-time when
`credential_lifetime == "one_time"`), `"otp"` → `otp`, one-time; (4) the request
CARRIES `input_schema.questions`, whatever its `input_type` says — a
single-question call keeps `text`; the chat dispatcher promotes such a single question to a
form when its SCHEMA says sensitive — a typed `password`/`otp` field or an
explicit `sensitive: true` — so the answer arrives per field, never on wording,
which would mask a decision worded that way and leave the operator unable to
answer it. A question that offers OPTIONS is never classified by wording at all,
for the same reason: a question typed `password`/`otp` or flagged
`sensitive: true` is flagged with `form_schema` provenance; by wording, a
question whose prompt reads like a one-time code is `otp` (this wins over the
word "password" — "one-time password" is a code), one whose prompt or id names
a password is `password` (the form schema has no typed password field yet),
and one whose prompt or id otherwise reads like a secret is `other`, all with
`heuristic` provenance, and when a password field is present a sibling text/email question
whose id or prompt reads like a login identifier is flagged `login_identifier`
(one authentication bundle, plan §3.3) — an email in a form with no password
stays ordinary; (5) otherwise a question worded like a one-time code → `otp`
(one-time), or like another secret → `other`, `heuristic`. The wording
heuristics live in `magician_v2::secrets::classify` (`is_secret_param_name`
for identifiers — substrings for `password`/`passwd`/`passphrase`/`secret`/
`token`/`credential`/`cvv`/`otp`, whole-word `pin`, and a key by what kind of
key its name says with separators collapsed: `api_key`, `apiKey`, `x-api-key`,
`access_key`, `private_key`, `auth_key`, `license_key`, `signing_key`,
`encryption_key`, `master_key`, `session_key`, `shared_key`; never bare `key`,
so `sort_key` and `keyword` stay ordinary — and `looks_like_secret_prompt_text`
/ `looks_like_otp_prompt_text` for prose — whole words plus "api key",
"access key", "private key", "license key", "credential(s)", so "tokens" in a
sentence is not "token"; and a bare "code" counts as a one-time code when the
sentence says where it came from or how long it is — "the 6-digit code from your
authenticator app", "the code we texted you" — while "area code", "zip code" and
"which code path" stay ordinary, because for an untyped `text` ask the wording
is the only protection there is) and are shared with the agentic executor; they
can only raise sensitivity, never lower a typed classification. The collection
deadline is `created_at + timeout_secs`; one-time material clamps both it and
the request's own timeout to 180 s.

**Custody before resolution.** In `UserRequestService::respond_scoped`, a
request with a spec strips `input` before the durable commit, lifecycle
publication, or the ordinary oneshot see it. The browser one-time path keeps
its private oneshot receiver; every other sensitive answer is deposited in the
service-owned `SensitiveCustody` (`user_requests/custody.rs`: in-memory,
zeroizing, never persisted or restored) **after** the resolution commits, and
the public response carries one `SensitiveAnswer` per value. Acceptance
requires a scoped responder (the legacy unscoped `respond()` proves no scope
and resolves as `cancelled`), a live asker, and both the request and collection
deadlines; the value must be non-empty, ≤ 4096 bytes, and control-free. A form
ships every answered field as one JSON object in `input`; the service deposits
the spec-flagged fields (one deposit each) and renders the rest back into
`input`. A decision answer (cancel, an option id) to a sensitive request keeps
its decision and deposits nothing. Material leaves custody only through the
in-process `UserRequestService::take_sensitive(reference, request_id,
principal, workspace)` — one shot, bound to the request and its scope (a
mismatch does not consume), held at most 15 minutes after deposit and never
past the collection deadline; `retire_sensitive_for_request` forgets a
request's deposits when its owning execution is gone. The restart/orphan path
(`emit_orphan_resolution`) classifies a recovered row that has no spec and
refuses material for any request with one, not only the browser types.

**Decided at pause time for an agentic ask (P3).** An agentic pause decides its
spec when it is raised — `sensitive_spec_for_pause(input_type, question, hint,
parameter, pending_inputs, now)` in `execution/agentic/executor.rs`, stored as
`AgenticPauseState.pending_sensitive` and persisted with the pause record —
with the same rules the request service applies: the typed widget first
(`UserInputType::Password` → `password`; the new `UserInputType::Otp` →
`otp`, one-time), then a form's questions (`collected_form_field_sensitivity`,
including the login-identifier bundle), then wording and the parameter name
as `heuristic` compatibility detection for a `Text`/`Guidance` ask; a decision
prompt (choice, confirmation, external action, tool authorization, sandbox
override, diff approval) is never classified — but an external action's own
optional NOTES are free text that reaches the goal the model reads on every
later decision and is persisted in the pause record, so text there that reads
as a verification code (the shared extractor's cue rules) is replaced by a
marker telling the model to ask for it with `need_user_input`. One-time material gets the
service's 180 s collection clamp; any other sensitive ask keeps the day-long
window an agentic pause has today. `UserInputType::Otp` is the model-facing
`need_user_input` type `otp` (and the per-question kinds `password`/`otp` of
a form); its answer rides `UserInputValue::Password` — an exact string, never
a number — so every client that can answer a password can answer a code, and
the plane/MCP elicitation refuses it exactly as it refuses a password. The
spec also rides the pending-pause listing (`PendingPauseInfo.pending_sensitive`)
and is read back at resume: its kind is the answer's sensitivity (the resume
never re-derives what the pause decided and every client masked by — including
the interpreter skip and the re-ask card's `previous_answer`), and an
answer that arrives after `collection_deadline_ms` is refused — the material
is dropped, never retained — and the pause is re-asked with a fresh window
and `revision + 1`.
A resume that ends the run there instead — an abort, a denied authorization,
exhausted retries, a lost approval — retires the scope the paused run kept.

**Carried into the agentic run.** When a HITL answer resumes an agentic
execution, the sensitivity decided at collection is recorded beside every
resolved-input key the answer added or replaced
(`AgenticPauseState.resolved_input_sensitivity`, copied to
`AgenticContext.resolved_input_sensitivity` on resume and into the
continuation frame on a nested pause; a form's fields are keyed
`"<input id>.<field id>"`). `collected_input_sensitivity` maps a `Password`
value to `password` (or `otp` under an `Otp` ask) and a `Text` or `Guidance`
value to `otp`/`other` by prompt wording — the same rule the pause applied
(shared with request classification in `magician_v2::secrets::classify`);
`collected_form_field_sensitivity` classifies a form's questions. From then on
`format_resolved_inputs_for_llm`, the answer summary, and `sync_ephemeral_secrets`
honour the flag first and the name heuristic second, so a value stored under a
fallback key such as `text_input` is redacted and vaulted like any other.

**Vault before retention (P3).** Every resume path (`continue_resumed_execution`,
`execute_agentically_resume_exact_inner`) and the run's entry
(`build_run_setup`, before `sync_ephemeral_secrets`) call
`vault_flagged_resolved_inputs`: each flagged resolved input is registered in
the store under the run's ephemeral scope — a code (`otp`) as one-time
material through `register_one_time` (one bound submission,
`ONE_TIME_MAX_RETENTION_MS` = 10 min), anything else through
`register_ephemeral_bounded` with `PASSWORD_RETENTION_MS` = 30 min (long
enough for an approval wait or a second pause in the same login; short enough
that an abandoned run does not hold a password until its scope dies) — and
the context keeps only the reference `[REF:<key>]` (a form field under
`<input>.<field>`). The LLM block, the answer summary, the durable pause
record and the continuation frame therefore carry references by
construction. `sync_ephemeral_secrets` must not clear and re-register the
scope on every loop entry (that would reset a consumed code to available and
lose a password collected before a later pause): it registers
only a raw value still sitting in `resolved_inputs` — a record written before
P3, or a secret-named parameter no flag covers — bounded like a password
collected today, and never a reference. The scope guard clears the scope at
the run's end but keeps it across a continuation boundary
(`AgenticOutcome::requires_continuation`): a pause, a sleep (including the
`SleepUntil` hint that concludes run setup), waiting on children. A run that
moves worker finds an empty scope, its
reference stays unresolved, `prepare_action_with_secrets` refuses the dispatch
with the unresolved-reference error, and the model is told to ask again — the
designed failure.
**Authenticated dispatch (P4).** A reference becomes a value in exactly one
place — `prepare_action_with_secrets` through `secrets::sinks::lower_references`
— and only where an adapter delivers the value to its destination and owns
what comes back (the *approved sinks*): an HTTP header or the HTTP body, a
process's stdin, the typed browser fill (`browser__secure_prompt_fill` with
`fields[].value` set to the reference), and a pack parameter the capability
declares as a credential input. A reference anywhere else — file content or
path, SQL, a sub-goal or delegation context, a handover, a sleep reason, a
shell command line or environment, an HTTP URL or query string, a pack's
command or environment, an `agent-browser` argument or page command — refuses
the action with the sink named and the typed operation to use instead
(`LoweringError::UnsupportedSink`), before anything is read. Passwords and
identifiers lower at preparation (a plain scoped read) — unless a challenge
bound them (below), in which case only a sink delivering to exactly that
destination lowers them. **A one-time code is lowered immediately before
dispatch** (`lower_one_time_material_for_dispatch`, after every check that
could still pause or refuse the action, and never for a run already
cancelled): `reserve_one_time` with a claim naming the operation and the
destination (the HTTP origin, the program, the browser origin) plus the
challenge the run can still see standing there — an expired, spent,
superseded or differently bound code refuses here with a reason and the model
is told to ask for a fresh one — then the lowered request is proven buildable
the way dispatch will build it (for a pack, through the provider's own
lowering, so its `resolve_params` applies), then `consume_one_time` as the
submission starts. A
request that cannot be built releases the reservation (a proven
pre-dispatch failure);
nothing after `consume` ever releases — a timeout after the send, a reset, a
lost worker — because nothing proves the destination did not receive the
code. A reservation is never held across an approval wait: the pre-dispatch
recheck is the reserve itself. The typed browser fill reserves per field
after the material's own destination check, rechecks the page (same single
target, same origin and document) immediately before consuming, and
consumes as the fill starts; a declined confirmation, a page that changed, a
cancel or a deadline release (`Referenced` releases what it still holds when
dropped) — a selector the adapter cannot resolve *after* that point spends
the code, the recorded residual. A ONE-TIME code the store bound to exactly the
fill's origin (a challenge named the destination) needs no second "use
once" tap; a password bound to that origin still asks, because the fill does
not spend it — it outlives the fill under the run's scope. Several fields naming one reference are a segmented input: the
material is reserved once, split one character per field inside the fill,
spent once — the model never holds a per-digit reference. The CLI lane
answers a governed login hook's *declared* prompts
(`auth.lifecycle.login_prompts`, MagicRun 0.1.75) from the run's material
under the standard keys (`password`, `otp`, `login_identifier`), a code
reserved for the program and consumed as it is written to the PTY, each
prompt at most once per run; a prompt with no material — or material bound
elsewhere, expired or spent, with the store's reason — settles the login as
a typed `authentication_required` result. So does a login that FAILS after a
code was written: the code is spent at the write, so the outcome says to ask
for a fresh one instead of reporting a flat failure. A contract that declares
prompts without asking for a PTY is refused rather than run with a null stdin
(the hook would block on its own prompt and report nothing about a credential).
What the program wrote is scrubbed with the run's delivered values before it
becomes a result — the same text also becomes the dispatch error, the log line
and a durable learning-evidence record, and MagicRun's own redactor knows only
the profile material it injected, never the HITL answer. The bridge reads the
SINGLE-VALUE keys (`password`, `otp`, `login_identifier`): a CLI prompt is
answered one value at a time, so a form bundle is not a CLI answer shape and
settles as a typed challenge rather than being mis-read. The plain `bash` action's only
sink is its `stdin` payload, piped and closed after the write on both the
blocking and the streaming shell path;
a shell is no verifiable destination, so bound material never goes there. BOTH
shell paths scrub what the process wrote — the streaming one per chunk, the
blocking one (taken whenever the run has no broadcaster, execution id or step
id) before its log line, its error and its result — because a tool fed a
credential on stdin commonly echoes it.
The `agent-browser` CLI's own credential intake (`auth … --password-stdin`)
is the only CLI stdin that lowers a reference, unbound passwords only.

**Destination binding and typed challenges (P4, plan §5.1).** A challenge is
raised only from a bound observation — an HTTP `401` with a `WWW-Authenticate`
header from the host that answered (the final URL when a followed redirect
moved a probe; `Basic` asks for a password, any other scheme for `other`
material), or a governed program's declared prompt, recorded by the governed
runtime itself (`PrimitiveExecCtx::pending_challenge`) — never from body
text, a bare status, or JSON a pack printed
(`secrets::challenge::AuthenticationChallenge`). It is recorded on the run
(`ActionExecutors::pending_challenge`); the next secure ask that answers it
binds its spec to it (`challenge_id`, `expected_destination`) and takes it,
and a re-ask after an expired window keeps the binding. "Answers it" is the
ask's own kind for a single value, or ANY sensitive field's kind for a form:
a login bundle (§3.3) declares no whole-answer kind, and its password field
is what answers the `401`, so the binding covers every sensitive field the
bundle collects.
The material that answer becomes is registered bound to the destination,
whatever its kind: a
code through the store's own binding (`register_one_time`, refused on
`BindingMismatch`), a password or identifier through a companion entry
beside it in the same scope (`sinks::binding_key`, `bound_destination`),
which every sink checks before lowering — an HTTP action delivers a bound
password only to that origin, the fill only on that origin, the CLI bridge
only to that program, and a shell (no verifiable destination) never. One
vocabulary: the origin `scheme://host[:port]` for HTTP and the browser, the
program path for a CLI. A claim never copies the *registered* challenge back
to the store — that would assert nothing. It names the challenge the run can
still SEE standing at that destination when there is one
(`DispatchResolver::observed_challenge`), which is how a code collected for
the challenge an origin raised before is refused against the one standing now
— the destination alone cannot tell two challenges apart. A
claim that can assert no observation — the ordinary case after a resume, where
the run's in-memory observation is gone — names none, and the store admits it;
the challenge half of `OneTimeBinding::admits` is deliberately advisory there,
and says so. A new observation is a new challenge. An HTTP request carrying a lowered or
provisioned credential never follows a redirect
(`HttpAction::carries_credential`); a cross-origin redirect ends the attempt
with `destination changed` and the credential unforwarded. The browser lane
raises no automatic challenge — a login page is the model's own observation,
and `browser__secure_prompt_fill` is the typed operation that answers it;
universal login detection is not a claim. Dedupe of one live ask per
challenge is structural: an agentic run pauses once and a chat turn awaits
one ask.

**Observation filtering (P4).** Every value a dispatch delivers joins the
run's scrub set (`ActionExecutors::delivered_secret_values`, keyed per
delivery so a fresh code never hides an old one) *before* the adapter runs,
and every later observation of the run is sanitised with the set: action
results and failure texts (a failing login script that echoes what it was
fed), the log line, streamed shell chunks, the browser dispatcher's results
and errors, CDP traces and HAR archives before they reach disk — so a code the store dropped at
`consume` is still redacted when a page, a response body or a process
echoes it. The set is in memory, per run, and starts empty after a resume;
what a resumed run delivers is scrubbed from then on. After a typed fill
delivered a secret, image captures (`screenshot`, `pdf`, `record`) through
the browser dispatcher are withheld (`capture_withheld`) until the page has
demonstrably moved on: a navigation command, or a text observation of the
page (`snapshot`, `text`, `html`) that no longer contains a delivered value
— a click or key press proves nothing (a "show password" toggle is a click)
and clears nothing. After a split delivery no text observation can prove
anything either (six single digits match no scrub), so every command that
returns page content is withheld until a navigation command
(`CAPTURE_WITHHELD_PAGE`); commands that only act still run. The
`capture_reference` and `screenshot_preview` compiled handlers capture their
own headless sessions, never the page a secret was typed into.

The pause deadline bounds *collection* (enforced at resume, above);
retention after collection is the store's own cap, because a code that
arrives late in its window still needs time to be sent.

**Chat (P3).** A chat `need_user_input` whose answer is sensitive never reaches
the model. `ChatService::dispatch_chat_need_user_input` takes each provided
answer from service custody (`take_sensitive`, one shot, bound to the request
and the session's scope) into the session's turn hold
(`chat::turn_secrets::ChatTurnSecrets`: in memory, zeroizing, 15 min), retires
the request's custody, and returns
`{"status":"answered","sensitive":[{reference, kind, field, status: held |
unavailable, placeholder: "[REF:<key>]"}], "note": …}` — the key is the form
field id or the kind (`password`, `otp`, `login_identifier`, `secret`). Every
pack sub-run the turn starts (`dispatch_capability_pack` →
`run_chat_pack_inline` → `AgenticContextOverrides.seeded_sensitive_inputs`)
is seeded with the held values as flagged inputs, which the run vaults at
entry; the turn's end (`clear_active_chat_run`, `cancel_chat_run`, session
archive) drops the hold, so a later turn asks again. A turn
cancelled while its ask is still open withdraws the ask (resolved `cancel`
on channel `chat_turn_cancelled`) and retires its custody, so nothing waits
for an operator to answer into custody with no consumer left. The service's
custody sweep (every accept and every response) drops untaken material past
its 15-minute hold and flips the request's history `sensitive[].status` from
`provided` to `expired`.
Retiring a request's custody does the same with `unavailable`, so the history
row never claims `provided` for material that was discarded.

**Mixed forms and the plane.** The plane never asks a sensitive question:
`service_input_type` refuses a request whose spec is set (whatever its typed
shape — a `text` ask whose wording names a code included) and `pause_input_type`
refuses an agentic pause whose `pending_sensitive` is set, both with the
credential message;
the typed refusals in `InputForm::new` remain the belt for records restored
from before the spec existed. That refusal does not ANSWER the ask: the plane's
race (the operator's channel against the MCP form) is skipped for a request that
collects a CREDENTIAL — a spec, or one of the two legacy browser types — and
only for that one, because `service_input_type` also refuses a conditional
option, an unreadable specification and an authorization that needs the review
UI, and those must keep aborting at once instead of parking the caller for the
request's whole window. For a credential ask the instant refusal would win the
race and cancel the ask before the owner could act, so the ask stays open for the
authenticated UI, exactly as it would for a caller with no plane route at all, and the MCP
call waits for that answer like any other blocking ask. A form answering a request whose spec flags
fields travels as one JSON object of every answered field — from the web
responder (`sensitive_transport_for_pending`) and, defensively, from the plane
`wait` responder (`execution::plane::input::service_response`, which takes
the request) alike. `respond_scoped` deposits the flagged fields into custody and
keeps the ordinary ones usable as `input` text (`id: value` lines), so a login
form's "remember me" still reaches the consumer while the identifier and
password do not. A form with no such spec renders as text but withholds any
field whose id reads as a secret, on both responders. Lifecycle events
(`HitlRequested`/`HitlResolved`, the generic resolution outbox) carry request
metadata and the decision only, never a value.

The public `UserResponse` never carries a sensitive value. Instead it carries
`sensitive: Vec<SensitiveAnswer { reference, kind, status, field }>` — one per
sensitive value, `status ∈ {provided, cancelled, expired, unavailable}` — and
`input` carries no sensitive value for such answers (only the ordinary fields
of a mixed form, rendered as `id: value` lines). Older shards load with
`sensitive` absent/empty; neither key is emitted when empty. Plan:
implementation plan.

**Urgent delivery through registered channels (P5, plan §6.1).** A
*critical* request — one whose published spec is set, or that names a
collection deadline; never one a model merely called urgent — is projected
by the delivery coordinator (`hitl_delivery::DeliveryCoordinator`, one
subscription to `HitlRequested`/`HitlResolved`) onto the owner's verified
private destinations: registered mobile push (the attention push, driven
from the same decision) and the channel bots enabled in
`hitl.critical_delivery` (Kapso WhatsApp, the Telegram bot), resolved to
addresses through `envoy.owner_identities` — never the last inbound sender.
What goes out is a value-free card (the bound host, the kind of thing
needed, a trustworthy deadline, the `/attention?attention=1&attention_item=` link behind
normal login); what is kept is a routing record per destination, written
before any send, with `queued → claimed → provider_accepted` and every
failure state tracked separately. A bot never receives an address on the
feed: it *claims* the delivery over the authenticated API and reports the
provider's answer. The request's own origin channel is deduplicated against
the fan-out in one place (`relayed_by_origin`); every retry rechecks the
request first; `HitlResolved` retires everything outstanding and the cards
that were sent. Attention stays the authoritative pending request whatever
the providers do. Full contract, states, settings and provider limits:
[critical-request-delivery.md](critical-request-delivery.md).

**Inbound verification-code retrieval (P6, plan §6.2).** A pending
single-value `otp` ask is a verification challenge (a form with a code field
is the person's whole form, never one); for as long as it is open, the resolver
(`verification_codes::VerificationCodeResolver`) watches only the sources
the owner granted the `verification_codes` purpose — an Observe email
account, the local Messages store, an AgentMail inbox, a permitted Android
phone — extracts a code deterministically from a message whose provider
receive time, sender domain (against the P4 binding) and provider
authentication match the challenge, and answers the ask through its own path
(`respond_scoped` for a request, the API's resume for an agentic pause):
first response wins, the value enters custody as a typed answer would, the
record keeps the resolver's channel, the model sees only status. Two eligible
codes, two live code asks in one scope, or a message this path refuses
(recovery codes, authenticator setup, resets, promotions), leave the ask to
the person; a message or a code that answered an earlier challenge never
answers a later one; the prompt says what retrieval is doing
(`VerificationRetrievalStatus`). The
Android companion answers the ask itself over its paired credential and
returns status only — digits never ride a tool result. Full contract:
[verification-code-retrieval.md](verification-code-retrieval.md).

## Canonical respond endpoint

```
POST /api/magician/v2/hitl/{correlation_id}/respond
Body: { source, value, channel?, task_id?, execution_id? }
```

- `source` (string): dispatcher arm in `respond_hitl_handler` (`magician-api/src/web_api.rs`). Must match
  the emit source.
- `value` (`AgenticResumeValue`): `{type: "text", value}`, `{type: "confirmation", confirmed}`,
  `{type: "choice", selected_id, other_value?}`, `{type: "multi_choice", selected_ids}`,
  `{type: "password", value}`, `{type: "external_action_completed", guidance?}`,
  `{type: "file_path", paths}`, `{type: "guidance", advice}`, `{type: "form", answers}`,
  `{type: "aborted"}`.
- `task_id` (conditional): required for `clarification` and `plan_approval`.
- `execution_id` (conditional): durable runtime identity. V3 planning keeps a distinct `planexec_*` id;
  legacy ask-loop may reuse the workflow id.

`form` is several questions in **one** pause: `input_schema.questions` (cap 3), one `correlation_id`, one
POST. An answer may set `skipped: true` without failing the run; abort remains abort. Planning batches of
two or more clarifications emit this envelope instead of N `HitlRequested` events. Chat `need_user_input`
waits on `UserRequestService::ask` (`source: "user_request"`). Magios/Magdroid chat that cannot render a
form open Attention; their Attention surfaces render stacked fields with Skip / Skip all, grant blocks,
chain eyebrows, destructive confirmation banding, file-path multiplicity labels, and external-action
instructions. Web, Magios, and Magdroid chat composers post `mode: accept_in_scope` from the Accept tab
so in-tree file edits skip permission HITL (`plan` from Plan; Do omits `mode`). Accept does **not** skip
`diff_approval` park; that skip is Autopilot / `apply_code_proposal` only. `apply_patch` paths come from
the unified diff, so an out-of-tree hunk still prompts.

Task-plan approval is bound to the displayed immutable `plan_id` (the correlation id). Planner refresh,
edit, and restore mint a new revision; stale or concurrent decisions return conflict and cannot mutate
the newer graph. Plan, index, and task projection writes are journaled and recovered together. Diff
decisions authenticate principal/workspace, task/execution provenance, apply root, destinations, and the
complete displayed review revision against the in-process trust authority. Apply takes a cross-process
record lock and rechecks that revision after claiming the decision; concurrent or stale apply/reject
cannot report a fresh success or roll back a winner. Pause-state hydration requires a workspace-bound
bearer. Cross-scope and legacy unscoped pause records are concealed rather than serialized. Approval
auto-resume requires one exact scoped pause and never falls back to a cycle id.

| `source` | Handler call |
|---|---|
| `user_request` | `UserRequestService::respond_scoped` |
| `approval` | `ApprovalService::resolve_approval` |
| `plan_approval` | `ArtifactV2Service::approve_task_plan` / `reject_task_plan` |
| `clarification` | V3 `submit_task_plan_clarification` first; V2 `AskLoopApi::submit_clarification` fallback |
| `diff_approval` | `CodeChangeProposalStore` apply/reject when `proposal_id`/correlation id resolves to a proposal; otherwise native `FileEditTransaction` apply/reject |
| `agentic` / `inner_loop` / `escalation` (default) | `resume_agentic_execution_with_scope` |

Legacy per-source URLs (`/approvals/{id}/resolve`, `/user-requests/{id}/respond`,
`/executions/{id}/agentic-resume`, `/v3/tasks/{id}/plan/clarifications/{q}/respond`) are **410 Gone**.

`EscalationListener` projects canonical `HitlRequested` into `ChatMessageContent::Escalation`, retaining
`input_type`, full `input_schema`, and a server-authored action per option. Ordinary answers post
`/hitl/{correlation_id}/respond`; max-iteration continuation alone uses
`/executions/{execution_id}/execution/agentic-continue` so it receives a fresh budget. The production
host starts this projection after ChatStoreSink is available. Old persisted cards still deserialize;
clients route a missing action contract to the matching Attention record instead of guessing from an
option id or translated label.

An already-answered pause returns **410 `pause_state_gone`** (agentic) or **409 `already_resolved`**
(clarification, via `TriggerError::AlreadyResolved` → `task_api_v3`). `respondToHitl` treats both as
**soft-success dismissal**. `resolved-elsewhere` `HitlResolved` is **correlation-scoped**, so resolving
one agent's card does not strand a sibling pause on the same `execution_id`. A `reask_required` (200,
`resumed:false`) re-prompts; validation re-asks atomically replace the visible question, hint, previous
answer, and `pause_state_id` before another submission (unscoped pauses can rotate their durable key when
a rejected answer is consumed and re-stored).

### Planning projection and resolution ordering

`make test-preplan-flow-live-eval` qualifies the planning contract against a running service: a
clarification's exact correlation must appear in both `/v3/tasks/{task}/plan` `pending_questions` (Plan
Inspector) and `/v2/feed/attention` with matching task, execution, source and prompt; after the canonical
response both projections drop it and `/v3/events?category=hitl&backfill_only=true` retains the matching
`hitl.resolved`. `make eval-preplan-flow-live-interactive` reads answers from `/dev/tty`.

Planning executions persist their own durable Ask/Plan records and are not Artifact-V2 execution-tree
roots, so Attention must not rely only on walking execution events. Progress projection replaces
same-planning-execution `clarification` / `plan_approval` rows with cards from the authoritative latest
plan. `eliciting` exposes every pending question; `draft` exposes the exact immutable `plan_id`;
`planning` / `approved` / `rejected` / `failed` expose neither. The canonical emitter and this projection
share one presentation adapter. Clarification response persistence reprojects both surfaces before
asynchronous replanning continues. Once elicitation has resolved every actionable slot, the snapshot
suppresses a later confidence-budget hold (a plan must not be stranded in `planning` without a
presentable clarification). Returned resume errors settle the planning execution as failed. Approving and
rejecting a Draft are both task-aggregate mutations; the task clock advances with the plan decision so the
derived Attention projection outranks the Draft card it removes. Progress publication and
`/v2/feed/attention` reads share the same injected `FeedStore` instance — do not open another DuckDB
handle; an already-open instance can serve an older WAL view.

**Clarification robustness.** `slot_graph/rewriter.rs::is_sentinel_question_text` (in
`normalise_open_questions` and defensively in `elicitation.rs`) drops open-questions whose WHOLE trimmed
text is a null-ish sentinel (`None`/`null`/`nil`/`undefined`/`nan`/`n/a`). The `/feed/attention`
clarification projection defaults `input_type` to `text` when the payload lacks it. Unknown ask-loop
question IDs fail with `InvalidResume`. `hitlRequestFromCanonicalEvent` does not descend the `payload`
layer on `/events` backfill, so a reloaded clarification renders from the feed projection.

**Response identity.** Each unresolved clarification is its own attention item; displayed prompt and
`pause_state_id` come from the same `hitl.requested` correlation. Agent-scoped pause keys
(`agent:<agent>:...`) are storage selectors, not execution IDs; canonical responses prefer the real
runtime `execution_id`. Successful agentic responses persist a scoped `hitl.resolved` with task identity.
The UI treats a conflict as success only when the backend reports `already_resolved`.

**A resolution settles only the requests before it.** Three consumers order by time, not by id alone:
the HITL lifecycle journal (`realtime_events.rs`,
`reconcile_scoped_hitl_{with,without}_persistence`) admits a `HitlRequested` on a resolved key as a fresh
request only when it is newer than the resolution (an older one is a replay); the attention builder
(`resolved_hitl_at` / `hitl_is_unresolved`) keeps the latest `hitl.resolved` per id and surfaces a
request only when its own timestamp is later; and the resume handler emits the pause's `HitlResolved` when
the answer is admitted — before the loop runs — because a resolution stamped after the loop would land
after the run's next ask and hide it. A validation re-ask or loop failure that restores the pause
re-requests it (`emit_hitl_requested_for_restored_pause`) under a newer timestamp.

**Cancel resolves pending clarifications.** `ArtifactV2Service::update_task_status` runs
`resolve_pending_clarifications_on_terminal` whenever a task IS terminal: it emits
`HitlResolved { outcome: "cancelled" }` for each pending question and clears `plan.pending_questions`. It
runs BEFORE the same-status short-circuit, so re-asserting a terminal status heals an orphaned
clarification. Best-effort and idempotent.

## Operator surfaces

Three places show pending HITLs and consume the same canonical events:

1. **`/attention` page** (`routes/(app)/attention/+page.svelte`) — full-page host for the reusable
   Attention inbox. Lists every pending HITL. A horizontally scrollable lane bar exposes All, Requests,
   Approvals, Escalations, Failed, and Messages using true backend totals. Previous/Next advances
   FeedStore and message cursors; the visible page stays bounded. Toggle to "All" backfills resolved
   history (paired by `correlation_id` from `/events?category=hitl`) showing outcome + decision + elapsed
   time.
2. **Top-bar Attention signal** (`shell/TopBar.svelte`, `attention/AttentionRain.svelte`) — the badge
   opens the compact center over the current route. While the count is non-zero, a bounded
   pointer-transparent set of attention/importance icons falls from the control and fades within 94px. No
   timer-driven DOM growth; follows theme tokens; disabled under reduced motion. Shared alert icon. The
   mounted top bar owns a balanced `attentionStore.start()` / `stop()` so the badge and cascade stay live
   without opening another surface.
3. **Chat typing-bubble pill** (`chat/+page.svelte`) — flips from bouncing dots to a "Waiting on you"
   pill when a HITL fires for the in-flight chat turn (`chat_turn_id` in `pendingHitlStore`). Click opens
   the same modal.

All other surfaces show **count + CTA**, never response widgets: `PlanModePane.svelte` ("N items are
waiting on you → Open Attention →"), the Square HUD `CompactCrewStatus.svelte`; TopBar Attention icon +
count badge opens the compact center.

## Frontend stores

| Store | Role |
|---|---|
| `pendingHitlStore` | Live HITL pendings via NDJSON `/events?category=hitl`. Source of truth for `pendingHitlCount` badge, `/attention` pending list, chat-pill lookup. |
| `attentionStore` | Polled attention projection (HITL + failed runs + running indicators + bot-auth synthesised rows). 15s `/feed/attention` poll + canonical `HitlRequested` / `HitlResolved` for instant in-place row drop. |

Both consume the same backend events; they project differently. See
`docs/components/unified-ui/README.md`. `AttentionDisplayRow.origin` is internal provenance for
dedup/activation and is not rendered as `VIA FEED`.

## Reusable frontend inbox contract

Canonical inbox lives under `ui/unified-ui/src/lib/attention/`. `/attention` consumes these APIs
directly. Shell compact centers and individual-item launchers must reuse the same model and controller,
not a HITL-only subset.

- **Model (`model.ts`)**: `AttentionDisplayRow` is the normalized row. `attentionRowAliases`,
  `attentionRowDedupeKey`, `attentionBusRow`, `attentionFeedRow`, `collectAttentionFeedItems`,
  `buildAttentionRows`, `mergeAttentionRows`, and `attentionRows` normalize bus, feed, and channel
  follow-up inputs. Bus rows merge first and win alias overlap. A valid explicit `metadata.source` is
  retained on the row and on the adapted `HitlRequest` used for response routing.
- **Presentation (`AttentionInboxSurface.svelte`)**: controlled filter/list/row surface. Accepts rows,
  source/search filters, page limit, loading/feedback state, and controller busy keys. Emits activation,
  filter/search, pagination, Skill Evolution, rollback, and channel follow-up events. Failed dismissal,
  review links, feed-origin hints, message follow-up actions, and special row actions stay in the shared
  row presentation.
- **Controller (`controller.ts`)**: `activateAttentionRow(row, options)` is the individual-launch API.
  `createAttentionItemController(options)` adds busy/error state plus bulk diff, Skill Evolution, and
  rollback. Activation uses an existing `HitlRequest` or hydrates a thin feed row from
  `/executions/{id}/pause-state`, invokes `respondToHitl`, and drops every row/request alias from both
  `pendingHitlStore` and `attentionStore` only after success. The optimistic `attentionStore` drop
  updates loaded rows, actionable counts, lane totals, and lane page totals in one transition so category
  tabs cannot keep a stale non-zero beside an empty inbox. Failed dismissal sends raw `FeedItem.id` for
  durability; the row dedupe key drives the optimistic drop.
- **Foreign-projection opener (`openHitlPrompt.ts`)**: execution-panel rows, draft-plan responses,
  approval records, canonical event-stream entries, and Today/Activity projections carry or derive a
  complete `HitlOpenTarget` and invoke the prompt directly. The target includes canonical respond id,
  source, input type/schema, prompt, identifiers, and origin scope. This path never searches the
  paginated Attention feed. Parsing requires exact `(principal, workspace)` ownership, a source-specific
  canonical identifier, and the source's workflow/execution key. Conflicting or cross-source aliases are
  rejected before a prompt opens; diff approvals alone may also carry the distinct pause id they resume.
  Resolution preserves validation reasks, snapshots scope, and drops validated request aliases from both
  pending stores only after a successful response.

Planning targets carry two identities: `workflow_id`/`task_id` is the responder key; `execution_id` is
the durable `planexec_*` event-stream identity. Plan approval compares the request correlation to the
exact current `plan_id` while holding the per-task write guard. A stale notification or concurrent losing
approve/reject receives `already_resolved` and cannot mutate a newer plan. Clarification falls back to
the legacy ask loop only for a V3 404 and only after the owning execution's principal/workspace matches
the request. Diff approval records bind task/execution provenance at stage time. The backend uses that
persisted owner for durable resolution and task reconciliation; a client execution id may agree but
cannot redirect the decision. Legacy records without an owner require a matching live diff pause before
any mutation.

`openAttentionItem(id)` is limited to ids originating from the Attention store (the center and Citizen
Inspector), legacy route/native compatibility, and forced-miss recovery. A Vitest source-contract scans
production launchers and fails if another foreign-dataset surface calls `openAttentionItem` /
`openAttentionForItems` directly or introduces another legacy `openAttentionRoute` caller outside the
documented Today digest path. The controller snapshots `(principal, workspace)` before hydration or an
action. A completion from an older scope cannot open a prompt, mutate current scope rows, publish
feedback, or trigger a refresh. Prompt cancellation leaves the row pending and is never treated as
failed/message/rollback dismissal. A canonical `reask_required` outcome re-enters `respondToHitl` with
the clarified question and hint; it is not terminal success and is not ignored.

Approval records and canonical events retain up to five bounded pending-action descriptions so review
prompts say what will execute. Today, Town Square, Command Palette, and Citizen Inspector open that
review prompt; they do not call the approval mutation directly. The approval store uses `validating` as a
reversible in-flight shadow and reconciles 404/409 outcomes from the durable record. Choice prompts start
with no selection; diff prompts focus review content rather than Apply; Enter is handled only from inside
the active modal. Exactly one prompt host: the root-owned `AttentionPromptModal` subscribed to
`attentionPromptStore`. Reusable Attention surfaces call `respondToHitl`; they do not mount another
modal/store host or start another poll. `/attention` remains responsible for its `attentionStore`,
meeting, channel follow-up, and resolved-history lifecycle.

### Global compact center

`AttentionCenter.svelte` is mounted once in `(app)/+layout.svelte`, beside the other global overlays and
outside both route content and floating chrome — not clipped by route overflow and not inheriting
Square's floating chrome. Public launcher contract from `$lib/attention`:

- `openAttentionCenter()`, `closeAttentionCenter()`, `toggleAttentionCenter()`.
- `openAttentionItem(id)` accepts any canonical or alias id, including raw `FeedItem.id`, and routes the
  matching row through `activateAttentionRow`. If the row is not loaded, the center hydrates it through
  the scoped exact Attention endpoint; arbitrary foreign-dataset ids still resolve as not found.
- `returnToAttentionCenter()` clears an item selection while retaining the center.
  `findAttentionRowByAlias()` is the shared alias lookup.

Same lane tabs as the full inbox; skeleton rows while a page is being established; a link to
`/attention`. The center shares the top bar's `attentionStore` refcount so the 15s
`/feed/attention` poll and canonical `HitlResolved` refresh remain singletons.

The current route is canonical state. SvelteKit shallow routing writes `attention=1` and, for an
individual selection, `attention_item=<id>`. URL updates clone the current URL (path, unrelated query,
hash). Closing deletes only those two owned keys. The center derives state from SvelteKit's page URL, so
direct links and Back/Forward use the same flow as clicks. Route-bearing launchers use
`parseAttentionRouteIntent` / `openAttentionRoute` before generic navigation. `/attention` and the
retired `/approvals` alias (301 redirect, query preserved; `approval_id` / `correlation_id` normalized to
`attention_item`) therefore open the compact center or exact item over Today, Town Square, digest rows,
and other current surfaces instead of navigating to the full inbox. The full `/attention` page remains an
escape hatch from the command palette and the compact center footer.

The desktop bridge has one ID-based contract. Actionable native notifications call `open_app_at` with
`/attention?attention_item=<encoded-correlation-id>`; Tauri normalizes that into the bounded singleton
Attention window and the always-mounted center resolves through the same exact endpoint as browser deep
links. No parallel typed-payload IPC or pending-intent queue. Deep-link parsing preserves query and
fragment state; approval `source_url` values carry their feed-item alias so external consumers select the
same prompt. Compact page size is six. Prev/Next navigate finite slices. Crossing the loaded boundary
uses serialized `attentionStore.loadMore()` and backend lane cursors. Concurrent cursor requests
coalesce; an exhausted cursor disables Next. Store refresh is backend-capped at 200 rows. Pending
channel/message follow-ups merge through `channelFollowUpToAttentionRow`. The center independently
follows `fetchChannelFollowUpsPage(6, cursor)`, dedupes by `annotation_id`, and caps its local channel
buffer at 200. It does not add another poll. A successful channel action removes the item from the
compact buffer immediately and decrements the local total.

Direct `attention_item` hydration checks locally loaded alias sets, then `GET
/api/magician/v2/feed/attention/{urlencoded-item-id}`. The detail lookup matches raw feed id plus
canonical `correlation_id`, `pause_state_id`, `approval_id`, and `request_id` metadata aliases within the
resolved principal/workspace. It reads the durable Attention union without walking cursor pages, honors
dismissal, and falls back to authoritative V3 task attention when a notification arrives before
projection materialization. For canonical sources with no task/execution artifact, the runtime
broadcaster's scoped pending-HITL registry supplies the exact request synchronously with event delivery.
Keyed lifecycle commit, pending-registry mutation, canonical persistence enqueue, and delivery share one
serialized ordering. The lifecycle authority is a scoped SQLite store
(`pending_hitl_lifecycle.v3.sqlite3`): exact reads and writes use one scoped primary key, ready startup
performs no full replay, and the one-time legacy JSONL import completes behind a durable downgrade fence
before producers start. Durable resolution tombstones take precedence over an asynchronously stale V3
projection after a crash. A corrupt or unavailable authority preserves known in-memory keys but fails
unknown exact lookups closed. `HitlResolved` remains authoritative across races and restarts. Stored
canonical HITL projections (`hitl.requested`, `input.requested`, confirmation/max-iteration pauses,
pending user requests) are returned only when current V3 attention or the pending registry corroborates
them; taskless registry hits are checked before V3 so a task-store outage cannot block an unrelated
bot-auth prompt. A true miss returns 404; a direct open then clears its URL selection so it cannot leave
an invisible frozen overlay. Realtime resolution also closes detached exact prompts through the durable
lifecycle authority even when their row was never in the capped feed state. Scope changes cancel active
selection and clear stale item URL state.

The center is an opaque, theme-token dialog (560px wide, at most 78vh) with internal scrolling, backdrop
close, focus capture/trap/restoration, and a dedicated `overlayCoordinator` priority. The singleton
`AttentionPromptModal` registers at priority 1 and traps/restores focus. The center is retained beneath
that prompt: first Escape cancels the prompt, a subsequent Escape closes the center.

### Server pagination

`GET /feed/attention` accepts `limit` (1..=200) that cursor-pages the attention lanes (`requests`,
approvals, escalations, failed, running); absent, it falls back to `per_section` (default 5) for legacy
callers. The response carries a **`totals`** block independent of the returned page. Pill-facing `counts`
are global after stale/dismissed filtering, not page-local. `attentionStore` sends `limit` (default 25),
fetches cursor pages via `loadMore()`, and refreshes the currently expanded window up to the backend cap
of 200. `GET /feed/attention/{item_id}` is the non-paginated detail companion for notification and other
direct-item deep links. It returns one currently visible scoped `FeedItem` — including a live taskless
canonical HITL — or `404 attention_item_not_found`; it does not change the loaded list or cursor.
Canonical HITL rows are live-state reads, not blind FeedStore returns: the endpoint rejects stale stored
projections after resolution and consults the durable keyed lifecycle authority for taskless pending
state.

The backend cursor remains `<updated_at>:<urlencoded-id>`; paging is a store-level `(updated_at DESC, id
DESC)` keyset query. Each lane fetches `limit + 1`; its exact SQL total and page select run in one read
transaction and apply the same scope, thread, and indexed dismissal predicates, so equal timestamps and
filtered rows cannot create gaps or page-local badge counts. Task archive/delete mutations remove
obsolete task rows; startup recovery reconciles legacy or interrupted state outside the request path. V3
task-progress mutations maintain the `requests` projection in FeedStore's attention-only table (kept out
of the Activity feed). Publishes are serialized per task and generation-checked; FeedStore projection
failures are logged and queued for retry without failing an already committed task mutation. Skill
Evolution gates, rollback recommendations, and post-promotion regressions are durable FeedStore
projections maintained by the detached learning sync; request handling never walks their filesystem
histories.

FeedStore uses cloned connections to one already-open DuckDB instance, so readers see committed WAL state
without independent file handles or forced per-write checkpoints. Identical ordinary items and
equal-generation attention snapshots are no-ops. Scope materialization validates the derived database and
quarantines recognized storage corruption before rebuilding projections. Because historical
field-id/dictionary corruption can terminate in native DuckDB before returning a Rust error, a feed
database larger than 512 MiB is also quarantined before open. Quarantine retention is bounded to one
database and its sidecars within a 512 MiB total budget; authoritative task and HITL storage repopulates
the active Attention projection. Dismissal imports are timestamp-merged under the scoped write lock with
a plain update-or-insert sequence. They deliberately do not use DuckDB `INSERT .. ON CONFLICT` (native
`MERGE INTO` buffered-index replay can `SIGSEGV` while binding a persisted dismissal index). Bootstrap
drops the old secondary index over `(principal, workspace, dismissed, id)`; the primary key already owns
exact dismissal lookup.

When an execution is still `waiting_for_user`, the V3 read path can synthesize a requested-input summary
from persisted agentic `hitl.requested` rows even if the normal progress snapshot was interrupted. Used
by `/tasks?type=internal` and task progress reads so max-iteration / agentic pause prompts do not leave a
task visibly paused with no pending attention item. The `requests` lane and Today "Needs You" filter
V3-attention with `valid_attention_task_ids` = user tasks ∪ **non-terminal** internal tasks
(terminal-status check normalizes case/whitespace and accepts legacy `canceled`). A prompt surfaces only
while its backing task can still accept a submission. Live internal HITL stays; a deleted or
already-terminal internal-task prompt is dismissed on the next poll; user-facing failure alerts are
unaffected.

## Response modal contract

The response modal (`AttentionPromptModal.svelte` + `requestAttentionInput`) switches on `input_type`:

- `confirmation` → `{id: "confirm" | "deny"}` choice from `schema.confirm_label` / `deny_label`
- `choice` → radio of `schema.options`
- `multi_choice` → checkbox list gated by `schema.min_selections` / `max_selections`
- `text` / `password` → input (multiline if `schema.multiline`)
- `external_action` → choice + optional notes
- `file_path` → text input, comma-separated for `schema.multiple`
- `guidance` → multiline notes
- `diff_approval` → `DiffStrip` with Apply / Reject. Schema may carry `proposal_id`
  (`CodeChangeProposal`) or `transaction_id` (`FileEditTransaction`); responders prefer `proposal_id`
  when present.

Submit maps to a typed `HitlResponseValue` and POSTs canonically. On `reask_required`, the Attention item
controller reopens this same singleton modal with the clarified prompt; the pending row drops only after
the subsequent canonical response succeeds. See `ui/unified-ui/src/lib/hitl/respondToHitl.ts`.

## Secure-credential support matrix (P7)

What the secure-credential claim
(plan §8–9) covers. **Live** means
`make test-secure-hitl-qualification` drives the runtime end to end against the fixture login service and
sweeps every storage class for the canary; **unit** means tests below a whole execution. Anything not
listed is unsupported: a reference there refuses and no challenge is inferred.

| Surface | Verified |
| --- | --- |
| One secure answer collected and delivered | **Live** (a two-field identifier + password bundle has no journey) |
| Canonical respond (`source: agentic`), including replay/other-scope/superseded (`409 superseded_ask`) refusals | **Live** |
| HTTP delivery through the compiled `http` pack — body and headers | **Live** |
| HTTP delivery through the native `Http` action | Unit |
| HTTP refusal — reference in URL/query | **Live** + unit |
| HTTP refusal — header name, content type, other sinks | Unit |
| `401 WWW-Authenticate` challenge binding an ask to its origin | **Live** |
| One-time code: one bound submission, fresh ask after rejection | **Live** |
| Automatic retrieval from an owner-permitted email account | **Live** (resolver contract, fixture reader) |
| Provider readers (Gmail, AgentMail, Messages) | Unit |
| Browser typed fill (`browser__secure_prompt_fill`) | Unit |
| CLI / governed process (MagicRun lifecycle prompt → PTY) | Unit |
| Shell `stdin` | Unit |
| Critical-request delivery to registered channels | Unit (`make test-hitl-delivery`) |
| Web / desktop prompt rendering, masking, retrieval status | Unit (vitest) |
| iOS / Android prompts, Android notification handoff | Unit; device journeys are owner-verified |
| A delegated child's ask | **Not supported**: the child's `HitlRequested` is journalled and its run ends `waiting_for_user`, but its terminal debt is not discovered, so nothing is projected and the delegation timeout cancels it |

Known limits:

- **HTTP Basic**: the header sink substitutes the typed value, so `Authorization: Basic [REF:password]`
  sends it raw; it does not build the base64 `identifier:password` pair.
- The lane uses **synthetic credentials only** and proves destination receipt and canary absence; it
  proves nothing about a real provider account, browser, phone or bot channel.

### Rollout posture (§9)

- **Order.** Acceptance guards ship before any client advertises secure input. MagicVault pins are
  reviewed literal revisions ([dependency mapping](jit-browser-credentials.md#magicvault-dependency-mapping)).
- **Rollback** disables the automation and keeps the refusal: turn `hitl.verification_codes.enabled` off
  (every code is typed), leave the sink refusals in place. Never restore a route that accepted a
  credential as ordinary text.
- **Metrics** are aggregate counters and safe error classes only (requested / accepted / expired /
  consumed / denied / unavailable, per scope and kind) — never a code fragment, value hash, or credential
  identifier.
- **Historical data.** These controls do not erase plaintext that may already exist. Any assessment names
  storage classes and time ranges without printing values; any purge or rotation is a separate procedure
  with explicit confirmation, and nothing under `data/`, `magician_data_v3/` or the runtime data root is
  deleted without it.

## Adding a new HITL source

1. Mint `broadcaster.emit(RuntimeTransportEvent::HitlRequested { source: "your_source", correlation_id, …
   })`. Use `emit`, not `emit_transport_only`.
2. Add a dispatch arm in `respond_hitl_handler` (`magician-api/src/web_api.rs`) that maps `body.value` to
   your service call and emits `HitlResolved` on success.
3. (Optional) Add an attention-summary group in `build_attention_summaries` (`artifact_v2/service.rs`) so
   the row surfaces in `/feed/attention`.
4. (Optional) If the source can be inferred from the V3 `attention_id` prefix, update
   `artifact_v2/progress.rs::attention_message` so the frontend adapter routes the respond POST to the
   new arm.
5. Add the source to `HitlSource` in `ui/unified-ui/src/lib/hitl/types.ts` and to both `KNOWN_HITL_SOURCES`
   lists (`lib/hitl/adapters.ts`, `lib/attention/model.ts`).

## Restart durability

A magician restart kills every in-flight coroutine, including `UserRequestService::ask().await` and
chat-delegate teardown watchers. Three recovery layers sit behind the canonical bus.
`ArtifactV2Service::reconcile_stale_active_root_execution` finalizes an execution as `orphaned_execution`
when `is_orphaned_user_pause` holds (waiting-on-user AND no `FullPauseStore` pause AND no durable
`paused_from_state` — a live/resumable pause is never destroyed). The resume handler calls
`finalize_orphaned_user_pause_by_execution_id` so a click on a stuck prompt clears it instead of 404ing.
That finalizer writes through `persist_external_execution_outcome`, which crosses the execution-runtime
boundary (`run_execution_job`), never the `_inner` variant: the outcome projection must start on a fresh
stack, not atop the actix worker's HTTP + resume chain (which overflows). The scoped pause admission guard
is held across the await; a source-scanning test keeps the finalizer off `_inner`.

**1. `UserRequestService` durable pending.** `ask()`
writes the affected scope's pending shard under
`scopes/<principal>/<workspace>/user_request_pending.json` via atomic
temp+rename after every mutation; the root-level file is only a bounded
legacy migration source. On startup, `with_pending_persist_path` replays the
scope shards: entries still within their `timeout_secs` deadline get a freshly
spawned timeout task for the remaining window, while expired ones fire a
synthetic timeout inline (records default-on-timeout response + emits
`HitlResolved`). Restored entries hold a `oneshot::Sender` whose receiver was
dropped at restore time — `respond_scoped` falls through cleanly when the
original caller's coroutine is gone. Each request is capped at 512 KiB, 32K
context nodes, and 128 options; each pending/history scope shard is capped at
512 rows and 16 MiB. Ordinary restores also enforce the 30-day age ceiling,
while sealed app notifications retain only through their independently bounded
absolute deadline. Exact-id, logical-scope membership, and canonical
physical-scope ownership indexes keep hot scoped listing, admission, response,
and persistence bounded by that 512-row shard. The physical owner key uses the
same normalized Artifact V2 directory segments as the path provider: recovery
quarantines a shard whose rows resolve to another owner, and distinct logical
scope strings that alias one physical directory cannot overwrite each other.
Exact duplicate persisted IDs deduplicate only when their complete row and any
private app generation agree; a conflicting duplicate quarantines every
implicated physical scope. Sealed expiry can authorize redaction/deletion of a
malformed or expired app pending body, but never replacement of a different
same-ID history owner. If a scope's bounded startup reconciliation write fails,
recovery removes that
scope from live pending/history and aborts its restored timeout tasks. Startup
still discovers all scopes through the provider's eager filesystem listing
API; that deployment-wide enumeration, unlike scoped hot operations, has no
aggregate scope-count ceiling.

Before reading either durable history or pending state, the service takes the
non-waiting exclusive writer lease
`<storage_root>/.user-request-store-writer.flock`. The same guard covers every
scope shard and both legacy compatibility files, and is retained by the service
plus detached timeout/publication/cleanup tasks that can still rewrite those
owners. A competing upgraded process skips recovery and fails durable
accept/response operations closed; it never loads a snapshot that it cannot
exclusively maintain. A service with no persist paths stays purely in memory
and does not create or require the lease, even if it carries a workspace layout.
Durable builders are one-shot and must configure history before pending;
repeating either persistence builder, reversing that order, or adding/rebinding
a workspace layout after a persisted owner was loaded skips the later
configuration and fences mutations instead of replacing a live durable owner
or switching storage models with partially recovered authority.

This sentinel is an upgraded-writer exclusion boundary, not an old-binary
downgrade fence: binaries released before this contract do not acquire it.
Deployments must drain every old UserRequest writer before starting the first
upgraded writer, and must not overlap old and upgraded versions during rollback.

**2. Per-source orphan recovery sweeps.** Producers that schedule work AFTER the user answers cannot rely
on `ask().await` returning.

- **`MemoryConsolidator::recover_orphan_clarification_answers`** walks user-request history for the
  agent's scope, finds resolved `memory_clarification` records whose `memory_question_key` has neither a
  `learning_memory_clarification_answered`/`_skipped` learning event NOR a matching `LearningCandidate`,
  reconstructs `MemoryClarificationQuestion` + provenance from saved `context`, and replays
  `process_memory_clarification_response`. Runs before every batch sweep; 60s recency buffer avoids
  racing the live coroutine.
- **`ChatService::recover_orphan_delegate_terminals`** walks every `<scope>/ui/chat_turn_events/*.jsonl`.
  For each non-terminal `chat.delegate.status_changed` whose `task_state.json` is actually terminal,
  emits a synthetic terminal event via the canonical broadcaster so `ChatTurnEventSink` writes it into
  the projection (`recovery_origin: "startup_orphan_sweep"`).
- **`AuthHitlBroker`** persists `last_known` to `<storage_root>/bot_auth_hitl_cache.json`. Combined with
  a defensive `prev = None, curr = Ok|Unsupported → Transition::Exit` in `classify_transition`, a bot
  that went `NeedsAuth` → `Ok` during downtime resolves on the next 15s poll.

**3. Frontend reconciliation watchdog.** `chatTurnEventsStore` opens each SSE in a `while !aborted` loop
with exponential backoff (1s → 30s), and a single global `setInterval` polls REST `fetchEventsPage` every
30s for any subscription idle longer than 60s. Catches a healthy-looking SSE that dropped an event
mid-stream; event-id dedup merges only new rows.

Every terminal execution path reads `FullPauseStore::pending_hitl_resolutions_for_execution` and emits
the typed canonical resolution **before** clearing pauses. Ordinary agentic prompts preserve
`correlation_id == AgenticPauseState::hitl_correlation_id()` (`PendingPauseInfo.hitl_correlation_id`);
diff approvals preserve their proposal or transaction id and `source: "diff_approval"`. The lookup merges the restart-hydrated disk index, so this
ordering still works after a process boundary. Artifact V2 startup reconciliation folds each terminal
execution's canonical history and appends `expired` or `cancelled` for any unmatched execution-owned
request. It does not expire independently owned `user_request` or `bot_auth` records.


### Failure-mode coverage

| Failure | Recovery layer | Worst-case latency |
|---|---|---|
| Magician died mid-`ask()`, user answers via UI | A (durable pending; `respond_scoped` resolves) | Immediate on next click |
| Magician died mid-`ask()`, user never answers, deadline expires during downtime | A (synthetic timeout on restore) | Immediate at next boot |
| Memory clarification answered via UI but watcher died before processing | B (consolidator orphan sweep) | One sweep tick (~5min) |
| Chat-inline delegate completed but teardown watcher died | B (chat orphan sweep on startup) | Immediate at next boot |
| Bot auth resolved out-of-band while magician was down | B (broker cache + defensive transition) | One 15s poll cycle |
| SSE dropped mid-stream, frontend never reconnected | C (SSE auto-reconnect) | 1-30s (backoff window) |
| Server dropped an event mid-stream, SSE looks healthy | C (REST watchdog) | 60-90s |
| Execution cancelled while a HITL is still pending | `cancel_execution` emits `HitlResolved { outcome: "cancelled" }` per pending pause before clearing them | Immediate |
| Execution became terminal but the process died before its HITL resolution persisted | Artifact V2 startup reconciliation appends resolutions for unmatched execution-owned requests | Next boot |
| HITL prompt re-derived for a deleted / terminal **internal** task | `requests` + Today "Needs You" via `valid_attention_task_ids` (user ∪ non-terminal internal) | Next attention poll (≤15s) |

### Synthetic markers (audit / triage)

- User-request history from `normalize_restored_history_records` carries `channel: "service_restart"`.
  `emit_orphan_resolution` treats these as recoverable, not a real prior response.
- Synthetic chat-delegate terminals carry `recovery_origin: "startup_orphan_sweep"`.
- Synthetic user-request timeouts on restore use `channel: "timeout"` (same semantics as a normal
  timeout).

## Card resolution / reappearance contract

The `/feed/attention` requests lane and Today "Needs You" lane are **re-derived live on every poll** from
each execution's `events.jsonl` (`build_attention_summaries`). A card is hidden ONLY when a paired
resolution is found in that same log.

- **Durable resolution.** `RuntimeTransportBroadcaster::emit()` must mirror canonical `hitl.resolved`
  into the origin execution's `events.jsonl`. On a `runtime_canonical_event_scopes` miss it reconstructs
  `CanonicalEventScope` from the event's own fields (`self_describing_canonical_scope` needs
  principal/workspace/task_id/execution_id — all carried by `HitlRequested`/`HitlResolved`). Any
  resolution emit path (diff_approval respond arm, orphan pause reconciler) must supply a real `task_id`
  (recovered from `CodeChangeProposal` / `PendingPauseInfo.task_id`); a `None` task_id silently voids the
  durable write on a registry miss.
- **Generic pairing.** The projection builds ONE source-agnostic `resolved_correlations` set (every
  `hitl.resolved`, keyed `correlation_id` then `pause_state_id`) and applies `hitl_is_unresolved` to
  EVERY pending group (user_request, clarification, plan_approval, input.requested). A pending event
  survives only while no resolution shares its id.

Id-namespace rule: a single pause must key its request row(s) and its resolution on the SAME id. The
diff_approval pause emits two request rows — `AgenticWaitingForUser` → `input.requested` and
`hitl.requested{source:"diff_approval"}` — so the proposal id (`ccp-<uuid>`) is stamped as
`correlation_id` on BOTH (and on the resolution) so the whole card drops together. Each pending group
stamps an explicit `source` on its `V3AttentionSummary` so the UI routes the response to the right
dispatcher arm; the `input.requested` catch-all excludes `user_request`/`clarification`/`plan_approval`
so one pause renders exactly one card.

### Authoritative state gate

Event-pairing is a fragile proxy: a dropped `hitl.resolved` strands the card, and the feed serves cards
from a cached `tasks/<id>/indexes/attention_projection.json` that does not re-run
`build_attention_summaries`. Authoritative signal is store state:

- A `user_request` card (`source=="user_request"` or `attention_kind=="user_request.pending"`) surfaces
  ONLY while its id is in `UserRequestService::list_pending_for_scope`.
- A `diff_approval` card (`source=="diff_approval"` or a `ccp-`-prefixed id) surfaces ONLY while
  `CodeChangeProposal.status == Pending`. Applied / Rejected / PartiallyApplied → drop.
- A generic `agentic`/`escalation` card surfaces only while the storage key its `pause_state_id`
  addresses (the canonical id less its ask suffix) remains in `FullPauseStore`. This also applies to legacy cached `input.requested`/`waiting_for_confirmation` cards
  without a stamped source. Terminal owning tasks always drop these execution-bound prompts.

The gate runs in TWO places: `build_attention_summaries` (fresh projections) and a post-filter in
`list_attention_items` (`attention_is_still_actionable`) that applies to cached snapshot **or** live
projection, so every poll reflects current store state and can clear a stale cached card whose task no
longer updates. Fail-open: `user_request` cards are gated only when the service is wired; the per-task
`CodeChangeProposalStore` read is authoritative (empty result correctly drops a resolved
`diff_approval`); `FullPauseStore` membership is fail-open when the store is not wired. Clarification and
plan-approval groups keep the event-based `hitl_is_unresolved` path above. Terminal-task rule: a
`diff_approval` card ALSO drops when its owning task is terminal (`completed`/`failed`/`cancelled`). A
proposal can be left `Pending` when its task is cancelled, and that orphaned card is no longer actionable
(a genuine review sits on an active `waiting_for_user` task). Owner-briefings are NOT gated on task
status: a briefing execution may finish while its `user_request` stays pending.

### Failed items are non-actionable — Today parity

`FeedItemStatus::Failed` cards (`execution.failed` / `agent.cycle.failed` / `agent.goal.failed` /
`agent.circuit.opened`) are terminal REPORTS, not actionable requests. The attention bar and `/attention`
page route them into a dedicated `failed` lane and exclude them from the actionable pill count. Today
`/feed/today` "Needs You" must match: it does NOT fold `Failed` items into `needs_you`. Only
`FeedItemStatus::NeedsAction` (HITL / approvals / escalations) is actionable.

### Durable dismissal of failed items

Dismissing a failed report is SERVER-SIDE and durable. A per-scope `AttentionDismissedState` store
(`ui/attention_dismissed_state.json`, keyed by raw `FeedItem.id`, same model as `today_visibility_state`)
is written by `POST /feed/attention/dismiss` (and `/undismiss`). `feed_attention` filters dismissed
`FeedItem.id`s out of EVERY lane and decrements the counts. A failure dismissed on any device stays
dismissed everywhere. The frontend keeps optimistic in-memory +
`localStorage['attention:dismissed-failed']` removal for instant UX and as a fallback against an older
backend; the server store is source of truth. Dismiss must POST the raw `FeedItem.id`, not the
correlation-based dedupe key.

On iOS, failed dismissal and every actionable HITL answer use a per-card optimistic transaction: row,
lane/page totals, canonical badge count, and pending-HITL tracker change before the network response. A
shared bounded tombstone prevents a racing Attention fetch — or a Today projection of the same HITL
correlation — from flashing the row back. Success commits it for a short reconciliation grace period;
transport or non-2xx restores the original lane index and all projections and surfaces a dismissible
error. Different cards may resolve concurrently; a duplicate action for the same card is coalesced. HTTP
`status=reask_required` is not committed: iOS restores the live card and refreshes its revised prompt.
This is an eager UI transaction with compensating rollback, not a durable offline/force-quit outbox.

## Agentic failure and HITL mapping

- **Permission walls escalate; give-up is terminal.** The live agentic terminal (`Decision::Yield →
  YieldDisposition::Failed`) honours `OnFailureMode::AskUser` for Auth/Permission yield blockers
  (`escalate_to_user`, source `agentic`/`escalation`) so a re-auth / sandbox-access HITL can unblock the
  wall. Generic cannot-proceed give-up completes as `Failed` / `CannotProceed` with
  `AgenticExecutionCompleted{cannot_proceed_reason}` — not an interview. Budget and max-iterations keep
  their Continue HITL. A yield with both concrete `completed[]` work and explicit `open[]` remainder is a
  terminal partial success even when that remainder has a structural blocker; the blocker is a caveat on
  a useful result, not an automatic resume request. Pure blocked yields with no completed/open split
  retain failure/HITL behavior.
- **Path access → sandbox-override HITL.** A path outside the boot `file_sandbox` yields recoverable
  `ExecutionError::PathAccessDenied` (not a hard `Step`), routed into `sandbox_override` HITL
  (`input_type` `file_path`). On approval the root is threaded through `SESSION_FILE_SANDBOX_ROOTS` so
  the compiled `read_file` retry succeeds. Out-of-workspace **writes** stay a scope boundary (actionable
  error, not a HITL loop — the diff-approval apply path is not widened).
- **Budget exhaustion is resumable.** `BudgetExhausted` carries a `pause_state` and maps to a Continue /
  Cancel HITL (`ExecutionPauseKind::Budget`), mirroring `max_iterations_reached`, instead of terminal
  `ExecutionFailed`. It does not count as a circuit-tripping cycle failure.
- **Anomalies auto-resolve.** A healthy harness cycle marks matching open anomalies `Resolved`. Sandbox /
  stuck-HITL / tool-unavailable failures classify into dedicated `AnomalyKind`s from the cycle `detail`.

### External-write HITL (gated default-OFF)

A `write_file`/`edit_file` to a path OUTSIDE the scoped workspace can escalate to the `sandbox_override`
HITL and, once the operator approves a root R, stage + apply there — double-gated (sandbox-override →
diff-approval) and containment-checked within R (`apply_transaction` canonicalizing guard). The
transaction carries the approved `apply_root`; the diff-approval rationale shows the ABSOLUTE destination
+ an `OUTSIDE WORKSPACE` marker. **Gated OFF by default** (`config::external_writes_enabled` /
`MAGICIAN_ALLOW_EXTERNAL_WRITES`): with it off, `apply_root` is always `None` and an out-of-workspace
write returns a soft actionable error.

Always-on regardless of the flag: a **runtime-store deny-fence**
(`native_executors::validate_file_action_runtime_store_fence`) hard-denies agent native-file writes to a
scope's trusted stores (`<scopes>/<p>/<w>/{transactions,code_change_proposals,runtime/pause_states}`) so
a transaction's trusted `apply_root` cannot be forged via the native-file path. **Do not enable the flag
in a multi-agent deployment** until the trusted stores are moved out of the agent-reachable `scopes` tree
(the `shell` tool bypasses the file fence).

## Actionability training worker cadence

`AttentionActionabilityTrainingWorker::spawn` waits out boot before its first pass (ten minutes, or the
configured interval if shorter): a full-scope training scan inside the startup I/O storm can hold one of
the attention store's few read connections for tens of seconds on a large cold database. The
candidate-binding lookup in the training join uses an index-usable `IN` list, not an `OR` of string
concatenations.

