# HITL / Attention contract

Every human-in-the-loop ask uses one **canonical envelope** and one
**canonical endpoint**: one badge, one inbox, one response modal.

## Sources (what kinds of HITL exist)

| `source` | When fired | Emit site |
|---|---|---|
| `agentic` | `need_user_input` / `cannot_proceed`, `waiting_for_confirmation` / `max_iterations_reached` | `native_catalog.rs` + executor escalation paths |
| `inner_loop` | Legacy (`InnerLoopUserInput` retired, nothing emits); still accepted by respond (with `agentic` / `primitive` / `escalation`) and feed | `magician-api/src/web_api.rs`, `feed_api.rs` |
| `user_request` | `UserRequestService::ask`, the channel-agnostic ask | `user_requests/service.rs` |
| `approval` | `AgentConstraints.requires_approval`; never announced (the pause is the ask), only resolved/expired | `agents/approval_service.rs` |
| `plan_approval` | A task plan moves to `Draft` | `artifact_v2/service.rs::emit_plan_approval_requested` |
| `clarification` | Planner needs a slot value (V3 plan; V2 ask loop, in its own scope) | `artifact_v2/service.rs::emit_v3_planning_clarification_needed` + `realtime_events.rs::clarification_queued` |
| `escalation` | Loop, sandbox override, tool authorization, max iterations | executor escalation paths |
| `diff_approval` | Gated code changes (`FileEditTransaction` / `CodeChangeProposal`) | `execution/compiled_handlers/staged_file_edit.rs`, `execution/file_edit/proposal.rs` |
| `mcp_oauth` | MCP OAuth sign-in awaiting its callback | `magician_v2/mcp_oauth.rs::emit_pending` |
| `bot_auth` | Bot adapter (gmail/whatsapp/telegram/kapso) `NeedsAuth` / `AccountMismatch` | `magician-learning/src/bots/auth_hitl_broker.rs` |

Taskless asks (`mcp_oauth`; `user_request` from chat `need_user_input`,
`envoy.*`, `harness.notify_owner.*`, `memory_*`; `service_health`, `bot_auth`,
`approval`) project as `runtime_taskless_hitl` (one group per correlation id,
`metadata.taskless = true`, `metadata.hitl_request` open target; `approval` in the
approvals lane, the rest in requests) until `hitl.resolved`;
`GET /feed/attention` drops a resolved one and lists one a task-bound row also
carries only as the task's row; app-owner notifications never project. `mcp_oauth` never publishes an authorization URL:
`{ "type": "external_action_completed" }` reopens sign-in in the host browser,
`dismiss` cancels. `GET /bots/auth` (`list_bots_auth`) polls emit
`HitlRequested` / `HitlResolved` on transitions via
`AuthHitlBroker::record_snapshot`.

### Surfacing guarantees

- Every persisted agentic pause emits `hitl.requested` regardless of routing;
  the observability / agent-routing gate applies only to the legacy
  `AgenticWaitingForUser` / `…ForConfirmation` / `AgenticMaxIterationsReached` twins.
- A delegated child's ask projects onto the parent (`task_id` = parent,
  `execution_id` = child, `metadata.delegated_agent_id`,
  `metadata.delegation_child = true`); the child stays suspended (parent
  `WaitingChildren`) for every owner-answerable input incl. `otp`, forms and
  diff approval; only tool authorization and sandbox override fail it
  (`agents::runtime::delegated_waiting_for_user_should_suspend`). Every durable
  ask-state event (`hitl.requested`, `hitl.resolved`, `input.requested`,
  `execution.waiting_for_user`, …) republishes `attention_projection.json`.
- A `waiting_for_user` execution with no answerable ask becomes a
  `waiting_without_question` escalation (source `v3_task_stalled_wait`,
  escalations lane, shown once 60 s old per `stalled_wait_attention_is_due`;
  `metadata.attention_reason`, `metadata.actions = ["cancel", "open_task"]`, `metadata.waiting_since_ms`;
  panel event `run.waiting_without_question`).
- A pipeline run outside V3 planning enters `WaitingUser` only with a
  clarification; otherwise, or on a guardrail breach, it fails
  `execution.failed` (`failure_kind: "question_undeliverable"`), settled 2 s
  later outside the resume listener so a resume that already left
  `WaitingUser` is left alone.

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

Mirrored by `RuntimeTransportEvent::HitlResolved { correlation_id, source,
outcome ∈ {responded, expired, cancelled, dismissed}, decision: Option<String>,
… }`.

**A correlation id is one ask.** The lifecycle journal is one-shot per id: a
`HitlRequested` under a resolved id is refused. A pause's storage key names its
slot (`<execution>:<plan>:<step>`, `agent:<agent>:<goal>:<cycle>`,
`<execution>::<ask id>` with no plan/step, never random) and is reused by the
step's next question, so the canonical id is
`AgenticPauseState::hitl_correlation_id()` = `<storage key>~<ask id>` (ask id:
12 hex chars minted and persisted with the pause; `~` never occurs in a key): a
re-ask (OTP retry, confirmation after a password) is a new request, retrieval
challenge and delivery. Legacy markers (`AgenticWaitingForUser`,
`AgenticResumed`, `input.requested`) keep the bare key in `pause_state_id`; the
snapshot resolves either spelling. Resume splits the posted id
(`split_hitl_correlation_id`): **409 `superseded_ask`** (pause restored) if the
key holds a later ask; a bare key selects whatever it holds.

**Persistence.** Emit via `RuntimeTransportBroadcaster::emit`, not
`emit_transport_only`, so `map_v2_realtime_event` writes per-scope
`events.jsonl` for `/api/magician/v3/events?category=hitl` backfill.
`HitlResolved` must carry the request's scope, since the `/v3/events` tail
filters by exact `(principal, workspace)` (`emit_approval_resolved_event` /
`emit_approval_expired_event` stamp `ApprovalRequest.{principal,workspace}`;
resolver scope only for legacy approvals); otherwise an `anonymous/default`
overlay card resolved from the web strands
(`docs/components/unified-ui/notification-overlay.md`).

## Sensitivity contract (secure HITL credentials, P1)

`UserRequest.sensitive: Option<SensitiveInputSpec>`
(`magician/src/magician_v2/user_requests/sensitive.rs`) is server-owned
metadata, never a value, stored in pending and history shards and published as
`input_schema.sensitive` for `user_request` (`hitl_requested_event`, written
last so a producer cannot shadow it) and `agentic` pauses
(`emit_hitl_requested_for_agentic_pause` → `hitl_input_schema_for_pause`, only
once publicly answerable: after a direct store commits, or from
`publish_terminal_loop_pause` after activation; kept when a protected app hides
the question).
Clients mask by the spec only; `None` is ordinary.

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

### Classification

Once, at acceptance (`UserRequestService::accept_request_with_sensitive_receiver`
→ `user_requests::classify::classify_sensitive`, or on restoring an older
row). Only free-text asks qualify (`input_type ∈ {text, guidance, password, otp, form}`,
or untyped with no options or an option setting `requires_input`); decision shapes (`choice`,
`multi_choice`, `confirmation`, `external_action`, `file_path`, the one-time
fill confirmation) never get a spec, and decision answers keep their decision.

1. A trusted in-process producer's spec is kept.
2. `request_type == secure_browser_input` → `password`, one-time, `producer`.
3. `context.input_type == "password"` → `password` (`typed_input`; one-time if
   `credential_lifetime == "one_time"`); `"otp"` → `otp`, one-time.
4. Carries `input_schema.questions` (whatever `input_type` says; a single
   question stays `text`, but chat promotes it to a form when its schema is
   sensitive): a question typed `password`/`otp` or `sensitive: true` →
   `form_schema`; questions with options are never classified by wording;
   else `heuristic` wording → `otp` (wins: "one-time password" is a code),
   `password` (prompt or id names one) or `other`. A login-identifier-like
   question beside a password field → `login_identifier` (plan §3.3); without
   a password it stays ordinary.
5. Otherwise code-like wording → `otp` (one-time), other secret wording →
   `other`, `heuristic`.

`magician_v2::secrets::classify` (shared with the executor) only raises
sensitivity. `is_secret_param_name` matches
`password`/`passwd`/`passphrase`/`secret`/`token`/`credential`/`cvv`/`otp`,
whole-word `pin`, and key kinds with separators collapsed (`api_key`,
`apiKey`, `x-api-key`, `access_key`, `private_key`, `auth_key`, `license_key`,
`signing_key`, `encryption_key`, `master_key`, `session_key`, `shared_key`),
never bare `key` (`sort_key`, `keyword`). `looks_like_secret_prompt_text` /
`looks_like_otp_prompt_text` match whole words (plus "api key", "access key",
"private key", "license key", "credential(s)"; "tokens" ≠ "token"); "code"
counts only with its origin or length, so "area code", "zip code" stay ordinary. Collection deadline =
`created_at + timeout_secs`, clamped (with the timeout) to 180 s for one-time
material.

### Custody

`UserRequestService::respond_scoped` strips `input` from a spec'd answer before
the durable commit, lifecycle event or oneshot see it. Except the browser
one-time path (private oneshot), each sensitive value goes, after the
resolution commits, to `SensitiveCustody` (`user_requests/custody.rs`: in memory, zeroizing, never
persisted), reported as one `SensitiveAnswer`; flagged form fields deposit
separately, decisions deposit nothing.

- Needs a scoped responder (unscoped `respond()` resolves `cancelled`), a live
  asker, unexpired deadlines, and a non-empty, ≤ 4096-byte, control-free value.
- Material leaves only via
  `UserRequestService::take_sensitive(reference, request_id, principal, workspace)`:
  one shot, request- and scope-bound (a mismatch does not consume), ≤ 15 min,
  never past the collection deadline. `retire_sensitive_for_request` forgets a
  gone execution's deposits.
- On restart `emit_orphan_resolution` classifies spec-less rows and refuses
  material for spec'd ones.

### Agentic asks

`sensitive_spec_for_pause` (`execution/agentic/executor.rs`) decides at pause
time into `AgenticPauseState.pending_sensitive`: typed widget
(`UserInputType::Password` → `password`, `UserInputType::Otp` → `otp`,
one-time), then form questions (`collected_form_field_sensitivity`), then
wording and parameter name for `Text`/`Guidance`; decision prompts (choice,
confirmation, external action, tool authorization, sandbox override, diff
approval) are never classified. External-action notes that
read as a code are replaced by a marker telling the model to use
`need_user_input`.

- One-time asks get the 180 s clamp; others keep the day-long window. `otp`
  answers ride `UserInputValue::Password`; plane/MCP elicitation refuses both.
- The spec rides `PendingPauseInfo.pending_sensitive` and is read back at
  resume, never re-derived (incl. the re-ask card's `previous_answer`). An
  answer after `collection_deadline_ms` is refused, its material dropped, and
  re-asked with a fresh window and `revision + 1`; a resume that ends the run retires its scope.
- Sensitivity per resolved-input key
  (`AgenticPauseState.resolved_input_sensitivity` →
  `AgenticContext.resolved_input_sensitivity`; form fields
  `"<input id>.<field id>"`; `collected_input_sensitivity`) is honoured by
  `format_resolved_inputs_for_llm`, the answer summary and
  `sync_ephemeral_secrets` before the name heuristic (so `text_input` values
  are still redacted).

**Vault before retention.** `vault_flagged_resolved_inputs` runs on every
resume path (`continue_resumed_execution`,
`execute_agentically_resume_exact_inner`) and at run entry (`build_run_setup`):
`otp` → `register_one_time` (`ONE_TIME_MAX_RETENTION_MS` = 10 min), else
`register_ephemeral_bounded` (`PASSWORD_RETENTION_MS` = 30 min). The context,
LLM block, summary, pause record and continuation frame hold only
`[REF:<key>]` (form fields `<input>.<field>`); `sync_ephemeral_secrets` never
clears and re-registers the scope (that would revive a spent code), registering
only raw values left in `resolved_inputs`, bounded like a password, never a
reference. The scope survives continuation
(`AgenticOutcome::requires_continuation`: pause, sleep incl. `SleepUntil`,
children) and clears at run end; on another worker it is empty,
`prepare_action_with_secrets` refuses, and the model asks again (by design).

### Authenticated dispatch

References lower to values only in `prepare_action_with_secrets` via
`secrets::sinks::lower_references`, at an approved sink:

| Approved sinks | Refused with `LoweringError::UnsupportedSink` before anything is read |
|---|---|
| HTTP header or body; process stdin; the typed browser fill (`browser__secure_prompt_fill`, `fields[].value` = the reference); a pack parameter declared as a credential input | file content or path, SQL, sub-goal or delegation context, handover, sleep reason, shell command line or environment, HTTP URL or query, pack command or environment, `agent-browser` argument or page command |

Passwords and identifiers lower at preparation unless challenge-bound (then
only toward that destination). One-time codes lower just before dispatch
(`lower_one_time_material_for_dispatch`), after every check that could pause or
refuse, never for a cancelled run: `reserve_one_time` (claim = operation,
destination, observed challenge; expired, spent, superseded or mis-bound
refuses) → prove buildable (packs via `resolve_params`; failure releases) →
`consume_one_time` at submission, after which nothing releases. No reservation
spans an approval wait.

- **Browser fill** (reservation, page recheck, `Referenced` release on drop,
  segmented inputs, "use once"):
  [one-time browser credentials](jit-browser-credentials.md#referenced-material-through-the-typed-fill).
- **CLI:** declared login prompts (`auth.lifecycle.login_prompts`, MagicRun
  0.1.75) are answered once each from `password`, `otp`, `login_identifier`
  material, codes consumed as written to the PTY; missing material or a failed
  login after a code settles `authentication_required`. No forms, no prompts
  without a PTY.
- **Shell:** only `bash` `stdin`, piped and closed after the write; never
  bound material. Blocking and streaming paths both scrub process output.
  `agent-browser` `auth … --password-stdin` is the only CLI stdin that lowers
  (unbound passwords).

**Destination binding and typed challenges (plan §5.1).**
`secrets::challenge::AuthenticationChallenge` comes only from a bound
observation: HTTP `401` + `WWW-Authenticate` from the answering host (`Basic` →
password, else `other`) or a governed program's declared prompt
(`PrimitiveExecCtx::pending_challenge`); never body text, bare status or pack
output. Held in `ActionExecutors::pending_challenge`, it binds the next
answering ask's `challenge_id` and `expected_destination` (kept on re-ask); for
a form, any sensitive field's kind answers it and the binding covers every
sensitive field. One live ask per challenge is structural (one pause, one chat
ask).
Codes register bound (`register_one_time`, refused on `BindingMismatch`);
passwords and identifiers via `sinks::binding_key` / `bound_destination`,
checked by every sink before lowering.
Destination = origin `scheme://host[:port]` (HTTP, browser) or program path
(CLI); never a shell. A claim names the challenge still observed
(`DispatchResolver::observed_challenge`); with none (usual after resume) it is
admitted, so the challenge half of `OneTimeBinding::admits` is deliberately
advisory. Credential-carrying HTTP never follows redirects
(`HttpAction::carries_credential`; cross-origin → `destination changed`). The
browser lane raises no automatic challenge; universal login detection is not
claimed.

**Observation filtering.** Delivered values join the per-run in-memory scrub
set (`ActionExecutors::delivered_secret_values`, keyed per delivery, empty
after resume) before the
adapter runs; results, errors, logs, learning evidence, process and shell
output, CDP traces and HAR archives are sanitised. After a typed fill, image captures (`screenshot`,
`pdf`, `record`) are withheld (`capture_withheld`) until navigation or a clean
`snapshot`/`text`/`html` (a click proves nothing); after a split delivery, all
content-returning commands until navigation (`CAPTURE_WITHHELD_PAGE`);
act-only commands still run. `capture_reference` and
`screenshot_preview` use their own headless sessions. The pause deadline
bounds collection only; retention after it is the store's cap.

### Chat, forms and the plane

**Chat.** Sensitive chat `need_user_input` answers never reach the model:
`ChatService::dispatch_chat_need_user_input` moves them via `take_sensitive`
into the turn hold (`chat::turn_secrets::ChatTurnSecrets`, in memory,
zeroizing, 15 min) and returns
`{"status":"answered","sensitive":[{reference, kind, field, status: held | unavailable, placeholder: "[REF:<key>]"}], "note": …}`
(key = form field id or `password`, `otp`, `login_identifier`, `secret`). The
request's custody is retired; pack sub-runs (`dispatch_capability_pack` →
`run_chat_pack_inline`) are seeded via
`AgenticContextOverrides.seeded_sensitive_inputs` and vault at entry; turn
end (`clear_active_chat_run`, `cancel_chat_run`, archive) drops the hold; a
cancelled turn withdraws its ask (`cancel`, channel `chat_turn_cancelled`).
Untaken custody expires at 15 min (history `sensitive[].status` `provided` →
`expired`; retired → `unavailable`).

**Plane.** Never asks a sensitive question (`service_input_type`,
`pause_input_type` and `InputForm::new` refuse) and skips its operator-channel
race only for credential asks (spec or legacy browser type), leaving them to
the authenticated UI while the MCP call waits; its other refusals (conditional
option, unreadable spec, review-UI authorization) still abort at once. Spec'd forms
travel as one JSON object (`sensitive_transport_for_pending`,
`execution::plane::input::service_response`); `respond_scoped` deposits flagged
fields, keeping the rest as `id: value` lines. Spec-less forms still withhold
secret-named fields. Lifecycle events carry metadata and the decision only;
public `UserResponse` carries only
`sensitive: Vec<SensitiveAnswer { reference, kind, status, field }>`
(`status ∈ {provided, cancelled, expired, unavailable}`) plus ordinary form
`input`, omitting empty keys.
Implementation plan.

**Urgent delivery (plan §6.1)**: `hitl_delivery::DeliveryCoordinator` sends
critical requests (published spec or deadline) as value-free cards
(`/attention?attention=1&attention_item=`) to the owner's verified destinations
(`hitl.critical_delivery`, `envoy.owner_identities`):
[critical-request-delivery.md](critical-request-delivery.md). **Inbound
verification-code retrieval (plan §6.2)**:
`verification_codes::VerificationCodeResolver` answers an open single-value
`otp` ask from `verification_codes`-granted sources; the model sees only
`VerificationRetrievalStatus`:
[verification-code-retrieval.md](verification-code-retrieval.md).

## Canonical respond endpoint

```
POST /api/magician/v2/hitl/{correlation_id}/respond
Body: { source, value, input_type?, channel?, task_id?, execution_id? }
```

- `source`: dispatcher arm in `respond_hitl_handler`
  (`magician-api/src/web_api.rs`); must match the emit.
- `value` (`AgenticResumeValue`): `{type: "text", value}`,
  `{type: "confirmation", confirmed}`,
  `{type: "choice", selected_id, other_value?}`,
  `{type: "multi_choice", selected_ids}`, `{type: "password", value}`,
  `{type: "external_action_completed", guidance?}`, `{type: "file_path", paths}`,
  `{type: "guidance", advice}`, `{type: "form", answers}`, `{type: "aborted"}`.
- `channel` (default `web`): attribution only, except
  `verification_code_resolver` (refused over HTTP) and `android_notification`
  (needs the paired device, `401 paired_device_required`, and beyond a
  `confirmation` or a `choice` without `other_value`, not typed
  `otp`/`password`, the code grant,
  `403 device_not_permitted_for_verification_codes`). Others:
  `ios_notification` (legacy buttons), `ios_push` / `android_push` (answer
  modes), `web_push`.
- `task_id`: required for `clarification`. `plan_approval` uses an explicit
  `task_id`/`execution_id`, else the pending `hitl.requested` in scope, else
  `400 plan_approval_requires_task_id`. Approval over a notification channel
  also starts the run (`TaskApiV3::execute_task`, as
  `POST /v3/tasks/{id}/execute`): `200 {accepted: true, reason: "run_started"}`
  or `reason: "run_not_started"` (approval stands); re-approval starts nothing.
- `execution_id`: durable runtime identity (`planexec_*` for V3 planning; the
  legacy ask loop may reuse the workflow id).

`form` = up to three `input_schema.questions`, one POST (`skipped: true`
allowed, abort stays abort), used by planning batches of ≥ 2 clarifications.
Chat `need_user_input` waits on `UserRequestService::ask`
(`source: "user_request"`). Mobile chat without forms opens Attention (stacked
fields, Skip / Skip all). Composers post `mode: accept_in_scope` from Accept (in-tree
edits skip permission HITL; `plan` from Plan); only Autopilot /
`apply_code_proposal` skips the `diff_approval` park; out-of-tree
`apply_patch` hunks still prompt.

Integrity: plan approval binds the displayed immutable `plan_id` (edits mint a
revision; stale decisions conflict; plan, index and projection writes are
journaled together). Diff decisions authenticate scope, provenance, apply
root, destinations and revision, and apply locks and rechecks, so a stale
decision cannot win. Pause hydration needs a workspace-bound bearer;
cross-scope and legacy unscoped pause records are concealed (404), never
serialized; approval auto-resume needs one exact scoped pause, never a cycle id.

| `source` | Handler call |
|---|---|
| `user_request` | `UserRequestService::respond_scoped` |
| `approval` | `ApprovalService::resolve_approval` |
| `plan_approval` | `ArtifactV2Service::approve_task_plan` / `reject_task_plan` |
| `clarification` | V3 `submit_task_plan_clarification`; V2 `AskLoopApi::submit_clarification` fallback |
| `diff_approval` | `CodeChangeProposalStore` apply/reject if a proposal, else `FileEditTransaction` |
| `agentic` / `inner_loop` / `escalation` (default) | `resume_agentic_execution_with_scope` |

Legacy URLs (`/approvals/{id}/resolve`, `/user-requests/{id}/respond`,
`/executions/{id}/agentic-resume`,
`/v3/tasks/{id}/plan/clarifications/{q}/respond`) are **410 Gone**.

`EscalationListener` renders `HitlRequested` as
`ChatMessageContent::Escalation` with a server-authored action per option
(answers post `/hitl/{correlation_id}/respond`; max-iteration continuation uses
`/executions/{execution_id}/execution/agentic-continue`, fresh budget). A
"User request resolved" row posts only when
`chat_surface_renders_agent_event("hitl.requested")` (checked before any
session lookup) and the session holds the `Escalation` card with that
`request_id`; cards without an action contract open the Attention record,
never guessing from an option id or label.

Already answered: **410 `pause_state_gone`** (agentic), **409
`already_resolved`** (clarification, `TriggerError::AlreadyResolved` →
`task_api_v3`); `respondToHitl` treats both as soft success. `resolved-elsewhere`
is correlation-scoped (sibling pauses stay). `reask_required` (200,
`resumed:false`) re-prompts, atomically replacing question, hint, previous
answer and `pause_state_id` (an unscoped pause may rotate its key).

### Planning projection and resolution ordering

`make test-preplan-flow-live-eval` proves live that a
clarification shows in `/v3/tasks/{task}/plan` `pending_questions` and
`/v2/feed/attention`, leaves both when answered, and stays resolved in
`/v3/events?category=hitl&backfill_only=true`
(`make eval-preplan-flow-live-interactive` reads answers from `/dev/tty`).

Planning executions are not Artifact-V2 tree roots, so progress builds their
cards from the latest plan (`eliciting` → pending questions, `draft` →
`plan_id`, other states neither; one presentation adapter with the emitter), reprojected on every answer before replanning; a resolved
elicitation suppresses a later confidence-budget hold; resume errors fail the
planning execution; plan decisions advance the task clock. Progress and `/v2/feed/attention` share one injected
`FeedStore`; never open another DuckDB handle.

- `slot_graph/rewriter.rs::is_sentinel_question_text` (`normalise_open_questions`,
  `elicitation.rs`) drops sentinel-only questions
  (`None`/`null`/`nil`/`undefined`/`nan`/`n/a`); `input_type` defaults to
  `text`; unknown ask-loop IDs fail `InvalidResume`.
  `hitlRequestFromCanonicalEvent` does not descend `payload` on backfill, so a
  reloaded clarification renders from the feed projection.
- One item per clarification (prompt and `pause_state_id` from one
  correlation). `agent:<agent>:...` keys are storage selectors; responses
  prefer the real `execution_id` and persist a scoped `hitl.resolved`. The UI
  treats a conflict as success only for `already_resolved`.
- **A resolution settles only earlier requests** in the journal
  (`reconcile_scoped_hitl_{with,without}_persistence`) and builder
  (`resolved_hitl_at` / `hitl_is_unresolved`); resume emits `HitlResolved` on
  admission, and a restoring re-ask re-requests with a newer timestamp
  (`emit_hitl_requested_for_restored_pause`).
- **Cancel resolves pending clarifications**:
  `ArtifactV2Service::update_task_status` runs
  `resolve_pending_clarifications_on_terminal` for any terminal task (before
  the same-status short-circuit), emitting `HitlResolved { outcome: "cancelled" }`
  per question and clearing `plan.pending_questions`; best-effort, idempotent.

## Operator surfaces

- **`/attention`** (`routes/(app)/attention/+page.svelte`): lanes All,
  Requests, Approvals, Escalations, Failed, Messages, cursor paging, resolved
  history paired by `correlation_id` from `/events?category=hitl`.
- **Top bar** (`shell/TopBar.svelte`, `attention/AttentionRain.svelte`): badge
  opens the compact center (icon cascade while non-zero, off under reduced
  motion); owns a balanced `attentionStore.start()` / `stop()`.
- **Chat pill** (`chat/+page.svelte`): "Waiting on you" for the in-flight turn
  (`chat_turn_id` in `pendingHitlStore`).
- `PlanModePane.svelte`, `CompactCrewStatus.svelte`: **count + CTA** only.

## Frontend stores

| Store | Role |
|---|---|
| `pendingHitlStore` | Live pendings (NDJSON `/events?category=hitl`); drives `pendingHitlCount`, the pending list and the chat pill. |
| `attentionStore` | 15s `/feed/attention` poll (HITL, failed, running, bot-auth) plus `HitlRequested` / `HitlResolved` for instant drop. |

See `docs/components/unified-ui/README.md`. `AttentionDisplayRow.origin` is
never rendered as `VIA FEED`.

## Reusable frontend inbox contract

All launchers reuse `ui/unified-ui/src/lib/attention/`:

| Module | Owns |
|---|---|
| `model.ts` | `AttentionDisplayRow`; `attentionRowAliases`, `attentionRowDedupeKey`, `attentionBusRow`, `attentionFeedRow`, `collectAttentionFeedItems`, `buildAttentionRows`, `mergeAttentionRows`, `attentionRows` merge bus and feed rows (`AttentionRowOrigin` = `bus` | `feed`; bus wins; `metadata.source` routes). |
| `AttentionInboxSurface.svelte` | Controlled filter/list/row surface. |
| `controller.ts` | `activateAttentionRow(row, options)`, `createAttentionItemController(options)` (busy/error, bulk diff, Skill Evolution, rollback): hydrate via `/executions/{id}/pause-state`, `respondToHitl`, and only after success drop every alias from both stores (rows, counts, lane totals in one transition). |
| `openHitlPrompt.ts` | Other surfaces open a complete `HitlOpenTarget` directly, never searching the paginated feed (exact `(principal, workspace)`, source-specific id; conflicting aliases rejected; aliases drop only after success). |

Planning targets carry `workflow_id`/`task_id` and `execution_id`
(`planexec_*`). Plan approval compares the correlation to the current
`plan_id` under the per-task write guard (losers get `already_resolved`).
Clarification falls back to the legacy ask loop only on a same-scope V3 404.
Diff approvals use the owner bound at stage time (a client execution id cannot
redirect it; ownerless legacy records need a matching live diff pause).
`openAttentionItem(id)` is limited to Attention-store ids, legacy/native
compatibility and forced-miss recovery (a Vitest source contract fails on
other `openAttentionItem` / `openAttentionForItems` callers, or
`openAttentionRoute` callers outside the Today digest). The controller
snapshots `(principal, workspace)` first, ignoring an older scope's
completion. Prompt cancellation leaves the row pending (never a dismissal);
`reask_required` re-enters `respondToHitl` with the clarified prompt. Approval
records keep up to five pending-action descriptions; Today, Town Square,
Command Palette and Citizen Inspector open the prompt, never approve directly;
the approval store's `validating` is a reversible shadow reconciled on 404/409.
Choice prompts preselect nothing; Enter acts only inside the modal. One prompt
host: the root `AttentionPromptModal` on `attentionPromptStore`; reusable
surfaces mount no other host or poll.

### Global compact center

`AttentionCenter.svelte` (once, in `(app)/+layout.svelte`; shares the
`attentionStore` refcount) is driven by `$lib/attention` launchers:
`openAttentionCenter()`, `closeAttentionCenter()`, `toggleAttentionCenter()`,
`openAttentionItem(id)` (any alias, incl. raw `FeedItem.id`),
`returnToAttentionCenter()`, `findAttentionRowByAlias()`.

The URL is the state (`attention=1`, `attention_item=<id>`; closing deletes
only those keys; `parseAttentionRouteIntent` / `openAttentionRoute`). `/attention` and the
retired `/approvals` (301; `approval_id` / `correlation_id` →
`attention_item`) open the center over the current page; native notifications
call `open_app_at` with `/attention?attention_item=<encoded-correlation-id>`
(no other IPC); approval `source_url`s carry their feed-item alias.
Page size six (`ATTENTION_CENTER_PAGE_SIZE`); `attentionStore.loadMore()` is
serialized (cap 200, `MAX_ATTENTION_FETCH_LIMIT`). The center does not fetch
channel follow-ups. 560 × 680 px dialog (capped to the viewport) with focus
trap at `overlayCoordinator` priority 5; the prompt is its only allowed child
overlay (first Escape cancels the prompt, the next closes the center).

Hydration checks loaded aliases, then
`GET /api/magician/v2/feed/attention/{urlencoded-item-id}` (feed id or
`correlation_id` / `pause_state_id` / `approval_id` / `request_id` in scope;
honors dismissal; falls back to V3 task attention or the pending-HITL
registry); a miss is 404 and clears the selection. Lifecycle commit, registry
mutation, persistence enqueue and delivery share one serialized order. The
lifecycle authority is
scoped SQLite `pending_hitl_lifecycle.v3.sqlite3` (no startup replay; legacy
JSONL import completes behind a downgrade fence before producers start).
Tombstones beat stale V3 projections; an unavailable authority keeps known
in-memory keys and fails unknown lookups closed. Stored projections return only if V3 attention or the registry
(taskless first, so a task-store outage cannot block bot auth) corroborates;
an unprojected task-bound ask is served from its pending `HitlRequested` while
its task exists; a V3 hit takes the question as `title` and gains
`metadata.correlation_id`. Realtime resolution closes detached exact prompts;
a scope change cancels the selection.

### Server pagination

`GET /feed/attention` takes `limit` (1..=200) to cursor-page the lanes
(`requests`, approvals, escalations, failed, running), else `per_section`
(default 5); `attentionStore` sends `limit` (default 25). **`totals`** are
page-independent; `counts` are global after stale/dismissed filtering.
`GET /feed/attention/{item_id}` returns one visible scoped `FeedItem` or
`404 attention_item_not_found`.

- Cursor `<updated_at>:<urlencoded-id>` over a `(updated_at DESC, id DESC)`
  keyset; each lane reads `limit + 1` plus its total in one transaction.
- V3 task-progress mutations maintain the `requests` projection in FeedStore's
  attention-only table, kept out of Activity (per task, generation-checked;
  failures retry without failing the mutation); archive/delete removes task
  rows. Skill Evolution gates, rollback recommendations and regressions are
  FeedStore projections from learning sync; requests never walk their files.
- FeedStore clones connections to one DuckDB instance; recognized corruption
  or a feed database over 512 MiB is quarantined (one database plus sidecars,
  512 MiB budget) and rebuilt from task and HITL storage. Dismissal imports never use `INSERT .. ON CONFLICT` (native
  `MERGE INTO` replay can `SIGSEGV`). Bootstrap drops the old
  `(principal, workspace, dismissed, id)` index.

`waiting_for_user` executions get a requested-input summary synthesized from
persisted `hitl.requested` rows (`/tasks?type=internal`, progress). The
`requests` lane and Today "Needs You" keep only `valid_attention_task_ids` =
user tasks ∪ **non-terminal** internal tasks (case/whitespace-normalized;
legacy `canceled` is terminal), so a deleted or terminal internal-task prompt
drops on the next poll.

## Response modal contract

`AttentionPromptModal.svelte` + `requestAttentionInput` by `input_type`:
`confirmation` → `{id: "confirm" | "deny"}` (`schema.confirm_label` /
`deny_label`); `choice` → radios (`schema.options`); `multi_choice` →
checkboxes (`schema.min_selections` / `max_selections`); `text` / `password`
(`schema.multiline`); `external_action` → choice + notes; `file_path`
(comma-separated if `schema.multiple`); `guidance` → notes; `diff_approval` →
`DiffStrip`, `proposal_id` (`CodeChangeProposal`) over `transaction_id`
(`FileEditTransaction`). Submit maps to `HitlResponseValue`
(`ui/unified-ui/src/lib/hitl/respondToHitl.ts`); on `reask_required` the same
modal reopens and the row drops only after the next response succeeds.

## Secure-credential support matrix

Coverage of the secure-credential claim
(plan §8–9).
**Live** = `make test-secure-hitl-qualification` end to end against the fixture
login service, sweeping every storage class for the canary; **unit** = below a
whole execution. Unlisted surfaces are unsupported: references refuse, no
challenge is inferred.

| Surface | Verified |
| --- | --- |
| One secure answer collected and delivered | **Live** (no identifier + password journey) |
| Respond (`source: agentic`), replay/other-scope/`409 superseded_ask` refusals | **Live** |
| HTTP via compiled `http` pack (body, headers) | **Live** |
| HTTP via native `Http` action | Unit |
| HTTP refusal: reference in URL/query | **Live** + unit |
| HTTP refusal: header name, content type, other sinks | Unit |
| `401 WWW-Authenticate` origin binding | **Live** |
| One-time code: one bound submission, fresh ask after rejection | **Live** |
| Retrieval from an owner-permitted email account | **Live** (fixture reader) |
| Provider readers (Gmail, AgentMail, Messages) | Unit |
| Browser typed fill (`browser__secure_prompt_fill`) | Unit |
| CLI (MagicRun lifecycle prompt → PTY) | Unit |
| Shell `stdin` | Unit |
| Critical-request delivery | Unit (`make test-hitl-delivery`) |
| Web / desktop rendering, masking, retrieval status | Unit (vitest) |
| iOS / Android prompts, Android notification handoff | Unit; devices owner-verified |
| Delegated child's ask | Unit ([Surfacing guarantees](#surfacing-guarantees)); no live lane |

Known limits: **HTTP Basic** `Authorization: Basic [REF:password]` sends the
value raw (no base64 `identifier:password`); **synthetic credentials only**, no
claim about real accounts, browsers, phones or bots.

### Rollout posture

- Acceptance guards ship before any client advertises secure input; MagicVault
  pins are reviewed literal revisions
  ([dependency mapping](jit-browser-credentials.md#magicvault-dependency-mapping)).
- Rollback: `hitl.verification_codes.enabled` off, sink refusals kept; never
  restore a route that took credentials as ordinary text.
- Metrics: aggregate counters (requested / accepted / expired / consumed /
  denied / unavailable) and safe error classes per scope and kind; never
  code fragments, value hashes or credential identifiers.
- Existing plaintext is not erased; assessments name storage classes and time
  ranges, never values; a purge is a separate, explicitly confirmed
  procedure, and nothing under `data/`, `magician_data_v3/` or the runtime data
  root is deleted without it.

## Adding a new HITL source

1. `broadcaster.emit(RuntimeTransportEvent::HitlRequested { source: "your_source", correlation_id, … })`
   (`emit`, not `emit_transport_only`).
2. A `respond_hitl_handler` arm (`magician-api/src/web_api.rs`) that calls your
   service and emits `HitlResolved`.
3. Optional: a `build_attention_summaries` group (`artifact_v2/service.rs`);
   for V3 `attention_id` prefixes, `artifact_v2/progress.rs::attention_message`.
4. `HitlSource` (`ui/unified-ui/src/lib/hitl/types.ts`) and both
   `KNOWN_HITL_SOURCES` lists (`lib/hitl/adapters.ts`, `lib/attention/model.ts`).

## Restart durability

`ArtifactV2Service::reconcile_stale_active_root_execution` finalizes an
execution as `orphaned_execution` when `is_orphaned_user_pause` (waiting on the
user, no `FullPauseStore` pause, no `paused_from_state`; a resumable pause is
never destroyed); resume calls
`finalize_orphaned_user_pause_by_execution_id`, so clicking a stuck prompt
clears it. The finalizer uses `persist_external_execution_outcome` via
`run_execution_job`, never `_inner` (actix stack overflow; source-scan test),
holding the scoped pause admission guard across the await.

**1. Durable pending.** `UserRequestService::ask()` writes
`scopes/<principal>/<workspace>/user_request_pending.json` (atomic
temp+rename; the root-level file is only a legacy migration source), replayed
at startup by `with_pending_persist_path` (remaining timeout, or an inline
synthetic timeout with `HitlResolved`). Restored entries' dropped receivers let
`respond_scoped` fall through cleanly.

- Limits: 512 KiB, 32K context nodes, 128 options per request; 512 rows and
  16 MiB per scope shard; 30-day restore ceiling (sealed app notifications use
  their own); no aggregate scope-count ceiling.
- Recovery quarantines shards with another physical owner's rows and every
  scope in a duplicate-id conflict (identical rows dedupe); aliased logical
  scopes cannot overwrite each other; sealed expiry never replaces another
  same-ID history owner; a failed startup write drops that scope and aborts
  its timeout tasks.
- Exclusive writer lease `<storage_root>/.user-request-store-writer.flock`: a
  competing upgraded process skips recovery and fails durable writes closed;
  builders are one-shot, history before pending, else mutations are fenced; no persist paths = in-memory, leaseless. Not
  a downgrade fence: drain old writers first, never overlap versions.

**2. Per-source orphan sweeps.**

- `MemoryConsolidator::recover_orphan_clarification_answers` replays
  `process_memory_clarification_response` for resolved `memory_clarification`
  records whose `memory_question_key` lacks a
  `learning_memory_clarification_answered`/`_skipped` event and
  `LearningCandidate` (from `context` into `MemoryClarificationQuestion`;
  before each batch sweep, 60s recency buffer).
- `ChatService::recover_orphan_delegate_terminals` scans
  `<scope>/ui/chat_turn_events/*.jsonl` and writes a synthetic terminal
  (`ChatTurnEventSink`) for a non-terminal `chat.delegate.status_changed` whose
  `task_state.json` is terminal.
- `AuthHitlBroker` persists `last_known` to
  `<storage_root>/bot_auth_hitl_cache.json`; `classify_transition` maps
  `prev = None, curr = Ok|Unsupported → Transition::Exit`.

**3. Frontend watchdog.** The shared WebSocket event hub
(`lib/realtime/eventHub.ts`) reconnects with backoff capped at 30 s and, when
it cannot resume, each watched chat turn re-reads its projection over REST;
`chatTurnEventsStore` also polls `fetchEventsPage` every 30 s for
subscriptions idle over 60 s.

Terminal paths emit resolutions from
`FullPauseStore::pending_hitl_resolutions_for_execution` **before** clearing
pauses (`PendingPauseInfo.hitl_correlation_id`; diffs keep their id and
`source: "diff_approval"`; the lookup includes the restart-hydrated disk
index). Startup reconciliation appends
`expired`/`cancelled` for unmatched execution-owned requests, never independent
`user_request` or `bot_auth`.

### Failure-mode coverage

| Failure | Recovery layer | Worst-case latency |
|---|---|---|
| Died mid-`ask()`, user answers | 1 | Next click |
| Died mid-`ask()`, deadline passed while down | 1 (synthetic timeout) | Next boot |
| Memory clarification answered, watcher died | 2 (consolidator) | One sweep tick (~5min) |
| Chat-inline delegate done, watcher died | 2 (chat sweep) | Next boot |
| Bot auth fixed while down | 2 (broker cache) | One 15s poll |
| Event stream dropped | 3 (reconnect) | ≤ 30 s backoff |
| Event dropped, stream healthy | 3 (REST watchdog) | 60-90s |
| Cancelled with HITL pending | `cancel_execution` emits `HitlResolved { outcome: "cancelled" }` per pause | Immediate |
| Terminal, died before resolution persisted | Artifact V2 startup reconciliation | Next boot |
| Prompt for a deleted / terminal **internal** task | `valid_attention_task_ids` | Next poll (≤15s) |

### Synthetic markers (audit / triage)

- `channel: "service_restart"`: restored history
  (`normalize_restored_history_records`), recoverable by
  `emit_orphan_resolution`. `channel: "timeout"`: timeouts on restore.
- `recovery_origin: "startup_orphan_sweep"`: synthetic chat-delegate terminals.
- `channel: "task_ended"`: owner escalations (`harness.notify_owner.escalation`)
  dismissed when their task fails or is cancelled
  (`ArtifactV2Service::close_owner_escalations_if_ended` →
  `UserRequestService::close_task_escalations`, decision `dismiss`,
  `HitlResolved { outcome: "cancelled" }`; terminal path and every
  failed/cancelled status change, idempotent). Briefings and a completed task's
  escalations stay until acknowledged or two hours. Every acknowledge-only
  request — one option, no input, the same option as `default_on_timeout`
  (briefings, escalations, run-ended and crew notices, Relentless notices) —
  waits at most `ACKNOWLEDGE_ONLY_MAX_SECS` (2 h): `request_deadline_ms` caps
  it, so admission, restore and every expiry check agree, and the timeout
  answers it (`HitlResolved { outcome: "expired" }`), clearing Attention, the
  badge, the notch and the phones.

## Card resolution / reappearance contract

The requests lane and Today "Needs You" are re-derived each poll from
execution `events.jsonl` (`build_attention_summaries`); a card hides only on a
paired resolution there.

- `RuntimeTransportBroadcaster::emit()` mirrors `hitl.resolved` into the origin
  `events.jsonl` (on a `runtime_canonical_event_scopes` miss,
  `self_describing_canonical_scope`). Resolution paths must supply a real
  `task_id` (`CodeChangeProposal` / `PendingPauseInfo.task_id`); `None`
  silently voids the write.
- One `resolved_correlations` set (`correlation_id`, then `pause_state_id`)
  gates every group via `hitl_is_unresolved`. Request and resolution share one
  id (a diff_approval's `input.requested` and
  `hitl.requested{source:"diff_approval"}` both carry `ccp-<uuid>`); groups
  stamp `source` on `V3AttentionSummary`, and the `input.requested` catch-all
  excludes `user_request`/`clarification`/`plan_approval`: one pause, one card.

### Authoritative state gate

A lost `hitl.resolved` would strand a card (the cached
`tasks/<id>/indexes/attention_projection.json` is not re-derived), so store
state decides:

- `user_request` (or `attention_kind=="user_request.pending"`): while listed by
  `UserRequestService::list_pending_for_scope`.
- `diff_approval` (or `ccp-`): while `CodeChangeProposal.status == Pending` and
  the task is non-terminal.
- `agentic`/`escalation` (and legacy unsourced
  `input.requested`/`waiting_for_confirmation`): while the storage key its
  `pause_state_id` addresses (canonical id less `~<ask id>`) is in
  `FullPauseStore` and the task is non-terminal.

In `build_attention_summaries` and `list_attention_items`
(`attention_is_still_actionable`, cached or live, every poll). The
`user_request` and `FullPauseStore` gates fail open when unwired; the per-task
`CodeChangeProposalStore` read is authoritative (empty drops). Clarification and
plan approval keep event pairing; briefings ignore task status.

### Failed items are non-actionable — Today parity

`FeedItemStatus::Failed` cards (`execution.failed`, `agent.cycle.failed`,
`agent.goal.failed`, `agent.circuit.opened`) are reports in the `failed` lane,
uncounted and not in `/feed/today` `needs_you`; only
`FeedItemStatus::NeedsAction` is actionable.

A failure also clears itself: the live Attention read
(`attention_lane_facade` → `FeedStore::list_attention_lane_page_since`) leaves
out `failed`-lane rows last updated more than `FAILED_ATTENTION_LIFESPAN_MS`
(2 h) ago — the same lifespan as an acknowledge-only request — in the page, its
total and therefore the badge and the notch. Internal readers keep calling
`list_attention_lane_page`, which applies no cutoff. A daily job started at boot
(`magician-bin` `main.rs`, beside the feed materializer) then deletes
failure rows older than `FAILED_FEED_RETENTION_MS` (7 days) and the
dismissals they leave without a row (`FeedStore::prune_failed_items`), for
every scope that already has a feed database, so they do not accumulate.

### Durable dismissal of failed items

`POST /feed/attention/dismiss` (and `/undismiss`) writes per-scope
`AttentionDismissedState` (`<scope>/ui/attention_dismissed_state.json`, keyed by the
raw `FeedItem.id`, never the dedupe key, like `today_visibility_state`);
`feed_attention` filters it from every lane and count on every device. Web also
drops the row optimistically and in
`localStorage['attention:dismissed-failed']`. iOS applies dismissals and
answers as per-card optimistic transactions with a bounded tombstone (failure
or `reask_required` restores the row); not an offline outbox.

## Agentic failure and HITL mapping

- **Permission walls escalate; give-up is terminal.** `Decision::Yield →
  YieldDisposition::Failed` honours `OnFailureMode::AskUser` for
  Auth/Permission blockers (`escalate_to_user`); otherwise `Failed` /
  `CannotProceed` (`AgenticExecutionCompleted{cannot_proceed_reason}`). A
  yield with `completed[]` work and an `open[]` remainder is a terminal partial
  success; a pure blocked yield keeps failure/HITL.
- **Path access.** Outside the boot `file_sandbox`,
  `ExecutionError::PathAccessDenied` becomes a `sandbox_override` HITL
  (`input_type` `file_path`); approval adds the root to
  `SESSION_FILE_SANDBOX_ROOTS` for the `read_file` retry. Out-of-workspace
  **writes** stay an error.
- **Budget and max iterations** keep a Continue / Cancel HITL:
  `BudgetExhausted` carries a `pause_state` (`ExecutionPauseKind::Budget`), is
  not `ExecutionFailed`, and does not trip the circuit.
- **Anomalies** auto-resolve on a healthy harness cycle; sandbox, stuck-HITL
  and tool-unavailable failures have dedicated `AnomalyKind`s.

### Preference alignment holds

A hold (`preference_alignment::ask`) is an agentic `confirmation` pause with an
`AlignmentAsk`, so any client can approve or deny; the reply table, ledger,
voice and deadline rules are in
[preference alignment](preference-alignment.md#owner-answers-p5). HITL-facing
rules:

- The `prompt` is generic and `input_schema` carries only
  `escalation_trigger: "preference_alignment"`, `confirm_label`, `deny_label`,
  `alignment` (`alignment_ask.v1`: `ask_id`, `purpose`, `choices[{id,label}]`,
  `free_text`, `deadline_at_ms`) and, unattended, `collection_deadline_ms`, on
  every re-ask and listing too (`AlignmentAsk::hitl_input_schema`,
  `PendingPauseInfo::into_user_facing` → `alignment_hold`). The private reason
  is served only to the owner by
  `GET /hitl/{correlation_id}/alignment?surface=web|ios|android`; the re-ask
  HTTP `question` is the generic prompt too.
- Besides confirmation the endpoint takes `{type: "choice", selected_id}`,
  `{type: "choice", selected_id: "respond", other_value}` and
  `{type: "text"}` / `{type: "guidance"}`; free text never consents. An
  unoffered choice or empty reply re-asks; a reply after `deadline_at_ms`
  ends the run without the act (60 s sweep resolves `expired`).
- Consent (`allow_once`, `proceed_without_check`) and lasting changes come only
  from `web`, `ios`, `android`, `desktop` (`403 private_surface_required`);
  `voice` must name `storage_key~ask_id`
  (`409 alignment_ask_identity_required`) and gets
  `alignment.review_requested` without approval. The server stamps the
  channel; a refused reply keeps the hold. Only consent settles gates listed in
  `alignment.also_covers`; lasting changes re-ask at
  `alignment.stage: "confirm_wording"`, which saves only with the private
  readback token and never admits the act.

### External-write HITL (gated default-OFF)

With the flag on, an out-of-workspace `write_file`/`edit_file` escalates to
`sandbox_override`, then stages and applies in the approved root (sandbox
override → diff approval; containment-checked by `apply_transaction`; diff
shows the absolute path and `OUTSIDE WORKSPACE`). **Off by default**
(`config::external_writes_enabled` / `MAGICIAN_ALLOW_EXTERNAL_WRITES`):
`apply_root` is `None` and the write returns an actionable error. An always-on
**runtime-store deny-fence**
(`native_executors::validate_file_action_runtime_store_fence`) blocks agent
writes to `<scopes>/<p>/<w>/{transactions,code_change_proposals,runtime/pause_states}`,
so `apply_root` cannot be forged that way.
**Do not enable the flag in a multi-agent deployment** until those stores leave
the agent-reachable `scopes` tree (the `shell` tool bypasses the file fence).

## Actionability training worker cadence

`AttentionActionabilityTrainingWorker::spawn` delays its first pass by ten
minutes (or the configured interval if shorter), keeping its full-scope scan
off the store's scarce read connections during boot; the binding lookup uses
an index-usable `IN` list.

## Service-health notices

`realtime_events/service_health.rs` raises one scoped `service_health` ask per
failing service (provider profile, `Decision Engine`, `Decision model:<id>`);
success clears it, **Dismiss** answers it. Auth, credit and rate-limit failures
raise at once; unavailable or timed-out only after `UNAVAILABLE_GRACE_MS`
(2 minutes) without success.

## Answering on every surface

Answered anywhere, cleared everywhere. Plans:
`docs/archive/plans/2026-10-01-asks-everywhere-and-cost.md`,
`docs/archive/plans/2026-10-01-hitl-surface-bug-sweep.md`.

- **Web.** Task panels, chat drawers and `/t/{name}/tasks` answer in place
  (`answerAsk` → `answerTaskAsk`; secrets and grants hand off to the focused
  prompt); `ComposerAskDock` shows the thread's asks ("N of M"); `/hud` mounts
  its own `AttentionPromptModal`; every ask in a run gets a control
  (`moreAsks`, `ASK_HANDOFF`). Form options post `selected_ids`; the plan
  inspector posts `{type: choice, selected_id}`; links use
  `?attention_item=<id>`. A plan approval the planner dock shows stays there.
- **One selector.** `src/lib/attention/pendingAsks.ts` merges pending asks and
  answerable feed rows (`metadata.hitl_request`), deduped on every alias, and
  picks the thread (`scope.thread_id`, `ui_thread_id`, then the task's).
  Feed rows: requests, approvals, escalations (failed and stalled waits
  skipped). Badges count only asks `hitlRequestFromCanonicalEvent` parses
  (others are neither counted nor offered); LiveTaskCard
  "Needs you" needs a pending ask (`pendingHitlTaskIds`) or draft plan.
- **Already answered.** 404 / 409 / 410 drop the card; `postHitlResponse`
  returns `{ok: true, alreadyClosed: true}` (one toast per 3 s). On
  `HitlResolved` an open prompt closes "Answered elsewhere"
  (`attentionStore.resolutions`, `resolutionIds`), open task panels re-read,
  and chat escalation cards grey out (request or pause id).
- **Review links.** Same-origin `input_schema.context.review_href` (Relentless
  `accepted_review`, `budget_extension`, `capture_review`) →
  `schema.review_href` / `review_label`, a `Review →` link in
  `HitlPromptFields` that never answers.
- **Android** refreshes on `PendingHitlTracker.changes` and polls every 15 s
  while a sheet is open (closing it "Answered elsewhere" once the ask is gone
  or `already_resolved`), folds `escalation_resolved` into the card, links
  `magican://attention?item=<id>`, and offers Review plan · Reject · Approve ·
  Approve & Run for `plan_approval` (a failed start keeps the approval).

### Push

Content-free (correlation id, deep link, value-free answer hints; never the
question or option labels), and shown while the app is open (`willPresent`,
`onMessageReceived`).

- **Collapse/expiry.** `apns-collapse-id` = correlation-id digest;
  `attention_resolved` removes it. `expires_at` = ask deadline, else now + 1 h
  (`MAGICIAN_ATTENTION_PUSH_TTL_MINUTES`, clamped 5 min–24 h), enforced by
  `apns-expiration` / FCM `android.ttl`, Android `setTimeoutAfter` and sweeps,
  iOS silent `attention_expired` (an in-process timer, so the app also sweeps
  on open and on any push; Android below API 26 relies on sweeps and the silent
  push). The ask stays in Attention.
- **Answer buttons.** `push_answer_actions(source, input_type, schema)` sets
  `actions: approve_reject` only for a non-destructive, non-sensitive
  `plan_approval` `confirmation` (iOS `MAGICAN_ATTENTION_APPROVAL`, Android
  `AttentionActionReceiver`; unlock required), posting
  `{source, value: {type: confirmation, confirmed}}` to `/hitl/{id}/respond`
  (failure: "Couldn't send your answer"). Agentic, secret, max-iteration and
  budget asks open Attention; `actions` is kept for builds predating
  `answer_mode`.
- **Answer modes.** `push_answer_mode(source, input_type, schema)`
  (`hitl_delivery/coordinator.rs`, `PushAnswer` via
  `PushSink::attention_requested`) sets `answer_mode` (absent = open the app;
  APNs root key, FCM string): `confirm`
  (`confirmation` from `plan_approval`/`user_request`), `choice` (1–4 options,
  none `requires_input`, from `agentic`/`user_request`/`plan_approval`, with
  `option_count`; `allow_other` ok) or `text` (single-line, same sources,
  `{type: text, value}`). Never for `sensitive`, `destructive`, `protected` or
  form schemas, any other source (approval, clarification, MCP OAuth, …), or
  authority-granting `user_request`s (`tool_authorization`,
  `sandbox_override`, `*_approval`, `secure_*`). The phone fetches
  `GET /feed/attention/{correlation_id}` (`title` = question) with its device
  credential, re-checks the mode and posts
  `{source, input_type, value, channel: ios_push | android_push, execution_id?, task_id?}`; already answered =
  `409 {accepted: false, reason: already_resolved}`.
- **Transport.** `aps.mutable-content: 1` on `attention_requested` (iOS
  Notification Service Extension). Needs APNs (`MAGICIAN_APNS_*`, `.p8`) and
  FCM credentials; rustls (APNs HTTP/2 needs ALPN). iOS Debug omits push;
  `make ios-debug-build-push` / `ios-debug-run-push` use sandbox push.
- **Browsers (Web Push).** `PUT /web-push/subscription`
  ([v2 API guide](v2-api-guide.md); `web_push.rs`) stores a
  `PushSubscription` (platform `web`, kind `application`, ≤ 16 per scope,
  oldest replaced). On only when `MAGICIAN_WEBPUSH_VAPID_PUBLIC_KEY` (base64url
  uncompressed P-256 point) and `MAGICIAN_WEBPUSH_VAPID_PRIVATE_KEY`
  (base64url raw 32-byte scalar) load; `MAGICIAN_WEBPUSH_SUBJECT` = VAPID `sub`
  (default `https://magican.ai`). Body: RFC 8291 `aes128gcm` of
  `{kind: "attention_requested", correlation_id, source, expires_at, web_path: "/attention?attention_item=<url-encoded correlation_id>", answer_mode?, option_count?}`
  or `{kind, correlation_id}` (resolved/expired); withdrawals reach only
  subscriptions sent the ask (in memory, 8192 pairs, lost on restart).
  Headers: `TTL`, `Urgency: high` / `normal`, `Topic` = 32-char base64url
  SHA-256 of the correlation id,
  `Authorization: vapid t=<ES256 JWT, aud = push-service origin, 12 h>, k=<public key>`.
  https endpoints on known push-service hosts only (`PUSH_SERVICE_HOST_SUFFIXES`:
  FCM, Mozilla, Windows, Apple; others never fetched); 404/410 drops the
  registration like APNs `BadDeviceToken`. Answers use `channel: web_push`.
  Browser half: [browser notifications](../unified-ui/web-push.md)
  (`src/service-worker.ts`, `Notification.maxActions` ≥ 2 for buttons).

## Task-spawn ceiling across pauses

The task-spawn ceiling is serialized with every pause and restored on
continuation, so cold recovery compares the same run identity. Explicit null =
unrestricted launch; a legacy omitted field denies new spawns. Permissions
beyond that zero cap are in the pause authorization hash, without turning a
restrictive ceiling into an elevated approval.
