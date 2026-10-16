# Preference alignment

Before an outward or commitment act (send, invite, submission, disclosure,
order, booking, purchase), Magician checks it against the owner's
preferences, goals and standing instructions, and on a material conflict
nudges, offers an alternative, asks for clarification, or holds it for
confirmation.

**Status:** enforced. The repo seed and live config set `mode: enforce`; an
absent section means `mode: off`. Admission is wired into the native loop and
the shared action executor; holds are answered on web, iOS and Android. Open
items: backlog.
History: design plan,
frozen v1 contracts,
execution tracker (archived).

## Responsibilities

- **The judge (a model) decides meaning:** applicability, materiality, how to
  intervene. No domain or keyword rules.
- **The host decides everything else:** identity, evidence integrity, evidence
  force, valid transitions, admission.

## Explicit and inferred preferences

| Evidence (`AssertionStatus`) | Typical source | Can hold | Can offer a choice and wait |
|---|---|---|---|
| `direct_owner_statement` | Owner told chat to remember it, verified against the owner's turn | yes | yes |
| `confirmed` | Confirm endpoint, approved taste proposal, conflict answer | yes | yes |
| `inferred` | Consolidation, learning, channel patterns | no | only under a choice-based owner policy |
| `inferred_untrusted` | Meeting or screen capture | no | no (advisory) |
| `unknown_provenance` | Profile line with no authorship record | no | no (advisory) |

`preference_alignment::contract::validate_decision` enforces this table
whatever the model returns, lowering an answer to what its evidence can carry
(a `ForceDowngrade`). Inferred preferences feed every check; one that causes
an intervention may be offered for remembering, via exact-wording
confirmation.

## Code

Host side: `magician/src/magician_v2/preference_alignment/`. Pure core: the
`magician-alignment` crate (`intention`, `contract`, `composer`, `args`,
`eval_case`, `replay`, the `alignment-replay` binary), re-exported by
`magician` under the same paths; it depends only on serde, chrono and
`decision-engine-contract`. `make test-alignment-core` tests it in seconds;
`make replay-preference-alignment` replays recorded answers against a composer
change without building `magician` (shipping still needs a `magician` build).

| Module | Owns / enforces |
|---|---|
| `intention.rs` (`intention.v1`) | Read-only view over canonical memory, never a second store. `nothing_to_compare` needs `IntentionInventory::is_verified_empty` (complete coverage, nothing eligible). |
| `classify.rs` | Checks what `outward_dispatch_class` or `consequence_class_for_dispatch` classifies; `PrivateLocal` (incl. opaque file, HTTP, bash, DuckDB) is only steered, and unclassified effects fall through to it. **Published v1 limit:** coverage widens by classifying capabilities, never keywords. |
| `collect.rs` | `alignment_input.v1`. `action_binding` = `alignment_binding.v1:sha256(stable_confirmation_action_json)` with host `__*` identity removed, so identity a dispatcher injects after admission keeps the binding while any changed argument changes it (`None`, never `""`, if unserializable). `action_facts`: recipients from `resolve_effective_action`, arguments normalized (canonical key order, long strings cut). `build_inputs` batches the whole eligible inventory, explicit first, no top-k. |
| `judge.rs` | One bounded `pinned_json` call on `action_alignment_judge`, strict parse, validator. Every failure (no router, unbound route, missing prompt, timeout, provider error, malformed or rejected answer) is unresolved, unlike `memory_applicability`; verified-empty returns `NothingToCompare` with no call. `JudgeBackend::Scripted` is eval/test-only. `judge_batches` runs batches in parallel; `combine` folds them: cancel > confirmation hold > unresolved > clarification hold > choice hold > nudge > proceed, so a known explicit conflict never becomes a "could not be checked" waiver, and a hold over a failed batch says part was unchecked. Proceed over partial coverage is `Unresolved`. |
| `service.rs` | `check_action`, the single entry point (mode via `mode_for_scope`) → `NotChecked`, `Proceed`, `ProceedWithNudge`, `Hold`, `Cancel`, `Unresolved`. Telemetry `target: preference_alignment` records mode, classes, coverage, batch count, origin, disposition, failure cause, latency and downgrade, never intentions, arguments or explanations. The same record plus binding hash, generations and admission goes to `memory/users/preference_alignment_log.jsonl` (to `.jsonl.1` at 4 MiB); best effort, a failed append never changes the act. `ActionCheck::ticket` yields an `AdmissionTicket` (binding, origin, generation, mode); shadow tickets are never fenced. |
| `contract.rs` | `alignment_input.v1`, `alignment_decision.v2`, the validator. |
| `admission.rs` | Content-free `AdmissionRecord` (`alignment_admission.v1`; `decision_origin`: `model_action`, `nothing_to_compare`, `host_exception`, `owner_waiver`, `shadow`, `recovered_committed_batch`; generation, mode, policy version, execution, time), `verify_fence`, consume-once registry by owner, execution, binding (2-minute TTL, 4,096 entries; a dropped or expired entry only costs a second check). |
| `backstop.rs` | Admission in `execute_action_inner` for paths that skip the loop gate. |
| `ask.rs`, `hold.rs` | `alignment_ask.v1`, `resolve_reply`; `HoldSeed` (one ask for loop, plane, harness); `AlignmentHeld` error. |
| `ledger.rs` | `alignment_ledger.v1`, consume-once answers (`memory/users/preference_alignment_ledger.json`). |
| `receipts.rs` | Answers as confirmation JSON under `alignment_exception.v1:` (allow-once) or `alignment_check_waiver.v1:` (proceed-without-check); a prefixed token never equals a bare one, so an alignment answer never satisfies approval rules, criticality or the model's confirmation flag, and no other gate satisfies alignment. The prefix rides in `confirmation_action_json`, so stored pauses, effect ids and retry keys are unchanged. |
| `policy.rs`, `lasting_write.rs`, `notices.rs` | Owner policy, remember/replace, private notices of nudges and expired holds (`alignment_notices.v1`, `memory/users/preference_alignment_notices.json`). |
| `owner_turn.rs` | Host proof of what the owner said; a check's only owner evidence. |
| `eval_case.rs`, `journey_tests.rs` | Eval case (`preference_alignment_case.v1`), oracle and baseline scripted judges, journey lanes. |

**Reloads.** `install_config` publishes the section on every accepted load
(a rejected config never reaches it). If the reload endpoint's later router
check rejects the reload, the new mode stays with the old router; enforcing
then finds the judge unbound and holds (fail closed).

### The validator

`validate_decision` rejects unknown intention or span ids, contradictory
fields, ungrounded or unexplained interventions, a `cancel` or
`acknowledged_exception` not citing a current owner-turn span, a wait with
empty `holding_intentions`, holding intentions without a wait, holding ids
outside `supporting_intentions`, and any `alignment_decision.v1`. A rejection
is unresolved, never proceed.

- The force cap reads only `holding_intentions` and records them in
  `ForceDowngrade.rested_on`, so a lowered hold's nudge names only those. A
  hold's private detail and "change my preference" target come from the same
  list.
- A waiting `confirm` that cites explicit evidence but rests only on weaker is
  rejected (act held) when the cap would lower it to proceed; under a choice
  policy it becomes a waiting choice. A `clarify` or nudge waiting only on
  inferred evidence is lowered by design.
- **Limit:** structure, not meaning; a judge steered into naming an unrelated
  explicit rule as holding passes (the owner sees it in the private detail).
- The engine pack `action_alignment` answers per-intention questions only;
  `magician_alignment::composer` derives `holding_intentions` from those
  answers, and the composed decision goes through the same validator.

### Owner-turn evidence

- The chat dispatcher puts the owner's turn (id, bounded excerpt) in a
  task-local only on the direct chat surface (`Chat` with `Direct` or
  `ChatInline` source); `CheckRequest::for_run` makes it the one span a
  `cancel` or `acknowledged_exception` may cite. A run's goal is context only
  (a delegate's goal is its parent's words; a resumed goal is host-rebuilt).
- `merge_user_preference_with_event` copies that turn into the record's
  `memory_evidence`, making an explicit chat save `direct_owner_statement`.
  Saves from executor sub-runs, voice, shared rooms and public envoys are
  `inferred`. Tool ingress (`provenance::tool_fields`) drops `trust`,
  `confirmed*`, `owner_confirmed*` and downgrades `source_type:
  owner_confirmed`.
- **A chat "yes" counts once.** A yes to the chat's own `need_user_input`
  confirmation question on a consent surface (`CONSENT_SURFACES`) is recorded
  for that turn (`turn_owner_confirmations`, dropped with the turn). The next
  pack call takes it once: it appends the host-written line `The owner
  answered yes to this confirmation question: "<question>"` after the owner's
  message (before it, the act was still held; only the message is trimmed to
  600 characters, `owner_turn_with_confirmations`), and its sub-run goal says
  the owner already confirmed and not to ask again. The line never comes from
  model text; "only that act" rests on the judge matching the question to the
  action.
- On the direct surface the pack sub-run's goal also quotes the owner's
  request (≤600 characters). Agents ask at most once, never when the request
  acknowledges the preference; the gate counts that ask and does not ask again.
- In capture mode the captured-dispatch notice calls the capture the call's
  final outcome, not an error: a retry is captured again, and nobody is asked
  to turn capture mode off.

## Sources and generation (P1)

**Generation** (`generation.rs`): `memory/users/intention_generation.json`
beside `knowledge.json` (`intention_generation.v1`: generation, BLAKE3
fingerprint of the exact `knowledge.json` bytes, `bumped_at`). Bumped under
the `knowledge.json` flock on every `persist_user_knowledge` write and
`forget_memory` deletion; a failed bump is logged, never fails the write (a lost bump or a writer that
bypasses `persist_user_knowledge` shows as a fingerprint mismatch). Next =
`max(previous + 1, now in µs)`, never reused, compared for equality only. Read
under the shared flock with the bytes it describes: missing → 0, match →
value, mismatch → `Stale`, unparseable → `Corrupt`, unreadable →
`Unreadable`; an error never reads as 0. `reconcile_intention_generation`
repairs stale or corrupt under the exclusive flock; the inventory calls it
once.

**Native knowledge** (`sources/native.rs`, source `native_knowledge`), every
`knowledge.json` tier:

- **Kind** from `memory_kind` (`preference` → taste, `goal` → goal,
  `instruction` → standing instruction, `fact`/`observation` → none);
  unreviewed entries by tier (`preferences`, `goals`,
  `channel_writing_preferences` → taste, goal, standing instruction; other
  tiers → none). `goals` is not in `USER_MEMORY_TIERS` today. A standing
  instruction without direct or confirmed evidence presents as taste.
- **Eligible:** `active` (or missing), `pending_review`, `unresolved`;
  retired states, `valid_until` at or before now, or a future `valid_from` are
  excluded. Unknown lifecycle, unparseable validity date, app-governed entry
  (`app_source_eligibility`) or non-array tier → `omitted_ids`, coverage
  `partial`.
- **Authority**, only ever lowered: untrusted source, `trust: untrusted` or
  untrusted origin surface → `inferred_untrusted`; `owner_confirmed` with
  `confirmed_at`/`owner_confirmed_at` → `confirmed`;
  `explicit_user_statement`/`correction` with owner-turn evidence
  (`source_role: user`, `source_id`, non-empty `quote`) →
  `direct_owner_statement`; everything else (incl. `learning_candidate`) →
  `inferred`.
- **Identity** `memory_record_id` (legacy: `legacy:` + digest of tier and
  `memory_lifecycle::record_id`); revision `memory_lifecycle::digest`.
  Statement: key and value (structured as JSON); `memory_context` →
  conditions, `scope` → `MemoryScope`.

**Taste profile** (`taste_profile.rs`, source `taste_profile`): each
directive line is a taste (heading → `conditions: "profile section: …"`, id =
normalized-line digest, revision = profile version), `confirmed` if the
latest approved proposal for it is an addition, else `unknown_provenance`.
Checks pass the process's taste capture service so approved lines read
confirmed. `read_taste_profile_checked` keeps failures that
`TasteProfileLoader::load` reports as no profile: timeout or failure →
`unavailable` (`taste_profile`), unreadable proposal store → `partial`
(`taste_proposals`), no profile → complete and empty. The timeout is a
parameter; suggested 1 s, not the 5–100 ms prompt budget.

**App memory** (`app_memory.rs`) contributes nothing yet: its only read is a
bounded projection that cannot prove completeness.

**Inventory** (`inventory.rs`):
`collect_inventory(resolver, InventoryRequest)` reads both sources concurrently, merges coverage (weakest
wins; duplicate ids omitted), fills `StandingGeneration` (intention
generation, profile version, app generation). An unrepairable generation makes
it `unavailable` (`intention_generation`), intentions still listed.
Eligibility is structural: no topic, similarity or top-k filtering.

**Explicit saves:**

- Direct only if a user-role quote grounds the value
  (`owner_turn::quote_grounds_claim`: most content words are the owner's, any
  negation is the owner's); induced saves, paraphrases, unrelated citations
  are `inferred`. `provenance::ground_proposal` strips `confirmed*` and
  `owner_confirmed*` from consolidator proposals.
- Clearing or deleting a holding instruction (`save_preference(key, "")`,
  `forget_memory`) needs the owner's turn naming it
  (`owner_turn::owner_turn_may_retire`); else `kept`,
  `owner_instruction_needs_the_owner`. `forget_memory` deletes later array
  elements first so pointers never shift.
- Restating a key in the owner's turn supersedes differing live records
  (`memory_lifecycle: superseded`, `supersession_source: owner_restatement`;
  result carries `superseded`). Each passes the same `owner_turn_may_retire`
  guard, so a write with no owner turn naming the rule leaves it in force.

## Decision operation and configuration

### Judge

`preference_alignment.judge: engine` (default): `engine_judge.rs` sends each
check as one gate-mode batch of the Decision Engine `action_alignment` pack
(1.3.0, one item per intention) via the shared decisions runner. The engine marks every answer eligible;
`magician_alignment::composer` judges uncertainty against the owner's bars and
`validate_decision` checks the result (reference version
`typed:action_alignment@1.3.0/action_alignment_projection.v2`). Unresolved
(engine off or unreachable, an item unanswered, composer unable to settle)
with ≥1.5 s budget left → the generative judge answers; otherwise held.
`judge: generative` uses only the router judge.

The generative judge is router operation `action_alignment_judge`
(`data/magician_v2/prompts/action_alignment_judge_v2.0.0.json`, constants
`names::`/`versions::ACTION_ALIGNMENT_JUDGE`; v1.0.0 is kept and returns v1,
which is rejected), bound in seed `llm-router.yaml` to
`gpt61sol-responses-toolsany`, family `safety_and_review` in
`data/magician_v2/llm_observability/operation-families-v1.json`. Its ~6 s p95
(P8.8) misses the 4 s foreground budget, hence fallback. Input one variable
`alignment_input` (`alignment_input.v1`; the model never supplies ids); output
exactly one `alignment_decision.v2`, no prose, ≤2-sentence owner-facing
explanation, `holding_intentions` for any wait. The prompt teaches
principles, not keywords; its examples share no scenario with the eval corpus
(keep it so).

### Decision Engine

`decision-engine.yaml` `operations.action_alignment`: pack 1.3.0 (`relation`
choice per intention, `noul` questions, muting, `owner_decides`), gate on for
Jev only with per-question thresholds in `thresholds_by_model.jev` and
`allow_unqualified_gate: true`, shadow comparison against
`action_alignment_judge` off. Measured 2026-10-11 (Jev 1.13, bar 0.5; corpus
README): 2 explicit conflicts missed across dev and four held-out sets;
needless interruptions 7.2% dev and 12.5% on held-out 3 and 4 with pack
1.3.0; engine p95 0.57–0.81 s on pack 1.2.0. See
[structured decision](structured-decision.md#preference-alignment-action_alignment).

### `preference_alignment` in `magician-config.yaml`

Typed as `PreferenceAlignmentConfig` in `magician/src/config.rs`.

```yaml
preference_alignment:
  mode: "enforce"           # off | shadow | enforce (code default: off)
  enforce_scopes: []        # [{principal, workspace}] canaries while shadowing
  budgets:
    foreground_check_ms: 4000
    max_judge_calls_per_task: 40
    max_standing_batch: 24
  holds:
    foreground_deadline_secs: 86400
    background_deadline_secs: 86400
  recovery:
    allow_proceed_without_check: true
  composer:                 # bars for the typed-model composer
    threshold: 0.5
    explicit_benign: 0.6
    specified: 0.5
    soft: 0.5
    mute: 0.5
    decides: 0.7
  judge: "engine"           # engine (typed + generative fallback) | generative
```

| `mode` | Effect |
|---|---|
| `off` | No memory reads, no model calls; `enforce_scopes` ignored. |
| `shadow` | Full check recorded, always admits; `enforce_scopes` enforce as canaries. |
| `enforce` | Every scope enforces. |

- Composer bars (`magician_alignment::composer::ComposerBars`) are read every
  check and calibrated to one model; re-measure with `alignment-replay
  --bars` before changing models.
- `max_judge_calls_per_task` counts judge calls (one per batch), process-local
  (a restart resets it); past it a check is unresolved (`BudgetSpent`), never
  proceed; the owner can waive or cancel.
- Reloadable (unlike `approval_envelopes`). An unrecognised mode (`enforcing`,
  `on`) is `alignment_mode_invalid` on `preference_alignment.mode` and the
  mode in force stays, so a typo never switches enforcement off; unknown
  fields or a misspelled section fail the load.
- `enforce_preference_alignment_invariant` (in the
  `validate_and_apply_magician_config` chain: startup, reload, settings
  dry-run) → `preference_alignment_config_invalid`, whatever the mode, on a
  zero or over-ceiling budget (`foreground_check_ms` ≤ 60000,
  `max_judge_calls_per_task` ≤ 1000, `max_standing_batch` ≤ 256), a deadline outside 60 s–30 days, an empty or
  repeated canary scope, a bar outside 0.05–0.95 (`preference_alignment.composer`),
  or a `judge` other than `engine`/`generative` (`preference_alignment.judge`).
- If any scope enforces, `llm.router.operation_mapping.action_alignment_judge`
  must be bound with every selector arm naming a declared profile, else
  `alignment_enforce_route_required` with that property. The routing API
  refuses the same at save: `PUT/DELETE /llm/routing/action_alignment_judge`
  → 409 with that code and property
  (`config::preference_alignment_route_check`). A stale settings save → 409
  `decision_routing_changed_elsewhere`.
- A rejected reload keeps the last config; the error is a typed
  `PreferenceAlignmentConfigError` inside `anyhow::Error` (`downcast_ref`
  recovers code and property).

**Rollback:** `mode: off` hot-reloads, makes no model calls; pending holds
stay answerable until their deadline.

## Admission

**Loop gate.** `gate_preference_alignment` (`run_loop/phases/apply.rs`) runs
for the primary candidate after trust, the confirmation gate and the
allowlist, before the batch is admitted, so it sees only acts every other gate
would pass:

1. Mode `off`, no owner scope, or unchecked act → continue, no ticket.
2. Re-entry on a committed batch (`LoopProtectiveState::apply_reentered_committed_batch`,
   never persisted) → not re-judged; unfenced `recovered_committed_batch`
   ticket. An act the plan would fire again (`Refire` or nothing recorded,
   `GatedApply::alignment_recovery_would_fire`) is not fired: the batch
   settles not dispatched, keeping recorded or fired members, and the next
   iteration checks it on current intentions.
3. In-process rollback driver while enforcing → checked acts stop with
   `CannotProceed` (no pre-fire boundary). Shadow still runs there.
4. A prior owner receipt for this exact act → admitted only if its ledger
   record is consumed, with a `host_exception`/`owner_waiver` ticket carrying
   no generation. A used, expired, other-run or unreadable record admits
   nothing; the act is re-judged.
5. Else `check_action`: proceed/not checked → continue; nudge → continue plus
   owner notice; hold → pause with `AlignmentAsk::for_hold` (token = exception
   receipt); unresolved → `AlignmentAsk::for_unresolved_check` (retry, proceed
   without check, cancel; answering needs no judge call), or stop if
   `recovery.allow_proceed_without_check` is false; cancel → `CannotProceed`.
6. Before pausing, a receipt the confirmation gate just consumed is
   re-inserted, so neither gate asks twice.

The ticket travels on `GatedApply` to the host.

**Fence.** `dispatch_apply` runs `preference_alignment_fence` →
`verify_fence` at its last pre-fire boundary (the one re-polling
cancellation, pause, deadline, scope and budget; intents and activation
marker durable, nothing fired). It re-reads the intention generation (under
the knowledge file's shared lock), taste profile version and owner policy and
compares with the ticket: a change is `Stale`, a failed read `Unverifiable`;
owner answers and shadow tickets carry no fenced generation. It appends the
admission line to the alignment log. Fresh → registered for the backstop and
fired. `Stale`/`Unverifiable` → every member settles `NotDispatched`;
`note_alignment_fence` returns a model-facing "not executed" result
(`AdmissionExpiredBeforeIo`, boundary `NextIteration`); re-proposal re-checks,
and a second stale result for the same arguments bounces the act. No model
call in the fence.

**Backstop.** `execute_action_inner` (every dispatch path) calls
`backstop::backstop` after retrieval containment, before the outward gate.
Off, no owner scope or unchecked → continue; a registry entry for owner,
execution and binding → consume once and continue; anything else (plane
`tools/call`, plane approved captures, harness execute-on-resume) is judged
here, and an act whose arguments changed after admission has a new binding
and is judged again. In enforce mode it returns a `BackstopVerdict`: a cancel
or unwaivable unresolved check refuses, a hold carries its `HoldSeed`, and a
stopped act fails with `hold::AlignmentHeld` (never a success text). Refusal
and pause text name no preference or reason in tool results, chat tool rows,
action events, execution summaries, chat hints or a parent's failure text.

- **Plane and harness holds:** a hold in a live harness turn's `tools/call`
  (an `EndTurn` grant) is captured by `plane::dispatch`
  (`PlanePendingApproval.alignment`) and ends the turn; the loop raises the
  same ask (`harness_alignment_hold_outcome`). Allow-once consumes the ledger
  record, registers the admission, runs the act once under its `pltresume_`
  effect id and warm-resumes the harness; any other answer warm-resumes with
  "NOT executed" and what the owner said. Terminal grants (elicitation) and
  chat surfaces cannot pause, so they refuse as failures.
- While not `off`, checked follow-ups wait for their own turn (an aligned
  primary does not cover them) and checkable compiled chat calls take the
  executor path.
- **Delegation:** an in-context delegate shares parent execution, scope, mode
  and owner turn, and consumes a one-time answer through the same ledger
  record (parent and delegate cannot both use it); a spawned child keeps scope
  and mode, carries no owner turn and cannot consume another run's answer.
- **Owner turn:** passed to the pack sub-run (`run_chat_pack_inline` →
  `AgenticContextOverrides::alignment_owner_turn`) only on the direct surface
  with its typed invocation; saved with every pause, restored on resume; a
  free-text reply becomes the run's new owner turn. Compiled chat tools run in
  `owner_turn::with_owner_turn`.
- **Restart:** the pause index carries the ask (`PendingPauseInfo.alignment_ask`,
  dropped from the user-facing listing), so a hold stays owner-readable,
  swept and generically listed
  (`a_hold_survives_a_restart_readable_expirable_and_answerable`).
- **Effect tests** (`executor/tests/preference_alignment_effects.rs`, scripted
  judge via `ActionExecutors::preference_alignment_override`) count real
  transport calls for holds, shadow/off, allow-once and retried answers,
  changed orders, cancel and late answers, free text, unoffered choices,
  `hitl.requested` privacy, combined approvals and plane captures. Not
  covered: a harness process driving a whole plane turn.

## Owner answers (P5)

A hold is an ordinary `Confirmation` pause carrying an `AlignmentAsk` in
`AgenticPauseState::alignment_ask` (same `hitl.requested`,
`POST /api/magician/v2/hitl/{correlation_id}/respond`, ask identity — the
`ask_id` is the pause's own — journal, restart recovery). The kind stays
`confirmation` because reconnect and stateless-terminal recovery only know
existing kinds; unaware clients still get approve (the one consent) and deny
(cancel).

**The ask** (`alignment_ask.v1`): binding, execution, choices, holding
intentions, `created_at_ms`, `deadline_at_ms`, unattended flag (no chat
session). Hold choices: `allow_once`, `cancel`, plus `use_alternative` if
offered, `change_preference` if offered and a cited intention is native,
`remember_preference` if `remember_offer` names an inferred native one.
Unresolved: `retry`, `proceed_without_check`, `cancel`. The private `detail`
(explanation, act, cited intentions with id, revision, source, status,
statement) is saved with the pause and never published.

**Public face.** `hitl.requested` reaches chat, alerts, voice and the attention
log, so its `prompt` is generic ("An action needs your review before it runs.
Open Magician privately to see why and choose.") and its schema carries only
`escalation_trigger: preference_alignment`, the consent label, ask id,
non-revealing choice labels, `free_text: true`, deadline (and
`collection_deadline_ms` if unattended, so channel alerts show expiry), all
built by `AlignmentAsk::hitl_input_schema` for every announce and re-announce
(never the pause's private `question`). The re-ask HTTP response and the
pending-pause listing also carry the generic prompt. The chat escalation
listener offers the typed choices plus "Reply with what to do instead".
Detail: `GET /hitl/{id}/alignment?surface=web|ios|android`, run owner and
private surface only.

**Replies** (`ask::resolve_reply`, applied in
`execute_agentically_resume_with_validation`; the respond handler accepts a
`choice` or `text` besides `confirmation`):

| Reply | Result |
|---|---|
| approve, consent choice or its exact label | Ledger, then receipt; re-proposed act admitted once. Unrecordable → nothing runs. |
| deny, `cancel` | Run ends without the act. |
| `retry` | No receipt; re-checked. |
| `use_alternative` | No receipt; agent told not to run it and to ask what instead. |
| `change_preference` with wording, `remember_preference` | No receipt, no write; ask re-issued at its wording stage (exact statement, superseded wording, source revision). Without wording, re-asks for it. |
| `confirm_wording` with the readback token | Revision-checked lasting write; act stays unrun and is re-proposed. Without the token (bare approve, shared surface) nothing is written. |
| `keep_as_is` | Wording stage closes, nothing written; the act's choices return. |
| free text | No receipt; becomes the owner turn; never consent. |
| unoffered choice, empty reply | Hold re-issued. |
| anything after `deadline_at_ms` | Run ends without the act; timeout is never consent. |

Each resolution is appended to the alignment log, content-free.

**Ledger.** Keyed by ask: same answer returns the original, a different one
conflicts and records nothing; pruned ask ids are kept 31 days so a replay
never runs again. An exception carries no generation and covers one binding,
purpose and execution, lasts an hour, and is consumed under an exclusive
flock: delegates, cloned contexts and retried responses cannot use it twice,
and a sibling run cannot borrow it. A `proceed_without_check` waiver records
the generation, so a later save leaves it unused (`Stale`) and the act is
re-checked.

**Deadlines.** `holds.foreground_deadline_secs` with a chat session,
`holds.background_deadline_secs` otherwise (both 24 h). A late reply runs
nothing. A 60 s sweep (`web_api::expire_alignment_holds`, supervisor task,
covering restart-recovered holds via the persisted index) cancels the run
through `MagicianV2Orchestrator::expire_alignment_hold_execution`: the ask
resolves `expired` (not `cancelled`), the task outcome is persisted, the log
records `expired_unanswered`, and the owner gets a private notice.

**Who may consent.** `preference_alignment_api::alignment_answer_policy` runs
in the respond handler, and `resume_alignment_hold` enforces it on what a
reply resolves to, so every resume path is covered. Consent and lasting
changes only from `web`, `ios`, `android`, `desktop`; elsewhere (voice, bot,
unknown) a reply may cancel, retry, redirect or instruct, while consent
(including a typed "allow once") is `403 private_surface_required` or keeps
the hold and re-asks, and the words never become the owner turn. The channel
is server-stamped on `AgenticResumeRequest.answer_channel` /
`UserInputResponse.answer_channel`, never read from the body: the respond
route's `channel`, `plane` (plane `wait_for_run`), `approval` (approval
auto-resume, never allows a hold), `budget`, the verification-code resolver,
`voice`. Bot daemon and plane terminal bearers answer as `bot`/`plane`
whatever they claim, are never private for detail or notice reads, and cannot
write the policy. A `voice` answer must name its ask (`storage_key~ask_id`) or
gets `409 alignment_ask_identity_required`. **Limit:** an omitted `channel`,
and the legacy direct resume endpoint, count as `web`.

**Voice** (`voice.rs`) can cancel or redirect, never allow. A hold for a
session or scope with a live call sends `alignment.review_requested`
(`VoiceReview`: full correlation id, choices `cancel`, `retry`,
`use_alternative`, `keep_as_is`); the call says `GENERIC_ASK_TEXT` and to
allow in the app, naming no preference, reason or act. `VoiceControlSession`
answers only the owner's next utterance within 5 minutes that exactly matches
a choice naming the action
(never a bare "cancel"), through `ApiVoiceReviewSink` with channel `voice`, the
same policy and resume; any other utterance disarms it, approval-sounding
phrases go to the app, it applies only at the stage read out (a re-ask reads
"not applied"), and it closes with `alignment.review_closed` when the run
resumes or ends. **Limits:** the browser data-channel road receives the
envelope but no client speaks it; the voice catalog has no answer tool.

**Combined asks.** An act also needing another gate's approval (approval rule,
criticality, model confirmation flag, harness grant rule) is asked once:

- **Native loop:** the confirmation gate runs the check first with the
  requirements it would ask for; a hold lists them in `also_covers` (public
  projection: gate names; private: each reason) and consent reads "Allow this
  once and approve it". A non-holding check has no side effect (no receipt
  consumed, notice or admission), a shadow check is skipped, and the act
  pauses for the approval alone and is re-checked at dispatch.
- **Plane:** a harness approval capture runs the backstop; a hold rides it
  with `also_covers: [plane_approval]`; an outright refusal is refused, not
  asked.
- Only consent settles the other gates (pushing the bare approval with the
  alignment receipt); cancel, retry, words or a lasting change settle nothing.

## Owner surfaces

| Surface | Detail | Lasting changes | Code |
|---|---|---|---|
| Web | reason, cited preferences, readback | yes | `ui/unified-ui/src/lib/hitl/alignmentAsk.ts`, `AlignmentAskFields.svelte` |
| iOS | reason, cited preferences, readback | yes | `magios/Shared/AlignmentAsk.swift`, `Magios/AlignmentAskView.swift` |
| Android | reason, cited preferences, readback | yes | `magdroid/android/bridge/…/attention/AlignmentDetail.kt`, `magdroid/android/app/…/ui/AlignmentAnswer.kt`, `…/ui/PreferencesPage.kt` |
| Chat cards, channels | generic prompt, typed choices | no (no token) | `chat/escalation_listener.rs` |
| Live voice call | generic prompt; cancel, retry, redirect | no | `preference_alignment/voice.rs`, `magician-api/src/voice_control_handler.rs` |
| Web: Memory → Preferences | owner view; add, edit, confirm, retire, forget; history | yes | `ui/unified-ui/src/lib/preferences/` |
| iOS: Settings → Preferences → Your preferences | same | yes | `magios/Shared/AlignmentAsk.swift` (`OwnerPreferences`), `Magios/AlignmentAskView.swift` (`OwnerPreferencesView`) |

Clients keep the act's choices, lasting changes and reply apart, preselect
nothing, and send typed `choice` or `text` values, never labels. The hold's cancel reads "Cancel the
action" (closing the dialog leaves the hold waiting). **Settings → Engines →
Preference checks** reads `GET /preference-alignment/status`: the mode in
force and, separately, the saved file's dry-run verdict with `code` and
`property` (a rejected reload never reaches the service).

## Adaptation

**Owner policy** (`policy.rs`, `memory/users/preference_alignment_policy.json`,
`alignment_policy.v1`, one per owner scope): a global choice (`advisory`,
`choice_for_material`, `confirm_before_exception`; absent = reviewed defaults)
plus per-intention refinements, written whole at an expected revision under an
exclusive flock only via private-surface `PUT /preference-alignment/policy`
(a stale write is refused with the current policy); replies, cancellations
and ignored nudges never change it. `OwnerPolicy::apply` applies it;
`EffectivePolicy{global, version: "preference_alignment_policy.v1+r{rev}"}`
goes to the judge and the version to the admission record. A weaker global
never waives an explicit instruction (a `StandingInstruction` that may hold
stays `confirm_before_exception` unless a refinement names it). An unreadable
policy makes checks unresolved, not defaulted; the owner can overwrite it
(`replace_unreadable`).

**Remember and replace** (`lasting_write.rs`), alignment's only lasting
writes: remember marks an inferred native record `owner_confirmed`
(`owner_confirmed_at`, `confirmed_from: preference_alignment`,
`alignment_confirmation` stamp; wording unchanged); replace supersedes it
(`superseded_by`, `supersession_source: preference_alignment`) with a
successor keeping key, kind and applicability, the owner's wording as
`explicit_user_statement` and the reply as owner-turn evidence
(`direct_owner_statement`). Both must still match the read-back revision under
the knowledge flock (a changed or retired record is not written; the owner
chooses again), are idempotent per token, never touch profile- or app-sourced
intentions, and are not consent: the act is re-proposed and re-judged.

**Nudges** admit the act and record a private notice (`notices.rs`) with any
remember offer bound to the intention's revision.
`GET /preference-alignment/notices` on private surfaces, open count only on shared
ones; shown on web under Settings → Engines → Preference checks and on iOS
under Settings → Preferences → Notes from recent checks
(`PreferenceNotesView`). Remembering one needs the notice's token and the same
revision-checked write; a preference changed since is not saved.

## Owner view and edits

Private surfaces only (web, iOS, Android); bot daemon and plane terminal
bearers never; otherwise `403 private_surface_required`.

| Route (under `/api/magician/v2`) | What it does |
|---|---|
| `GET /preference-alignment/intentions?surface=` | The view (`view.rs`, `preference_view.v1`). |
| `POST /preference-alignment/intentions?surface=` | Add `{kind, label?, wording, conditions?}` → `201 {record_id}`. |
| `PATCH /preference-alignment/intentions/{id}?surface=` | `{action: edit\|confirm\|retire, expected_revision, wording?}`. |
| `DELETE /preference-alignment/intentions/{id}?surface=&expected_revision=` | Forget. |
| `GET /preference-alignment/history?surface=&offset=&limit=` | History, newest first (≤500 a page). |
| `GET /preference-alignment/intentions/{id}/history?surface=` | One record's history. |

- `build_view` uses the checks' `collect_inventory` and policy, plus grounds
  (evidence role and source), editability (native only), last-judged time,
  retired records (with `superseded_by`) and omitted ones with a reason
  (`unknown_lifecycle`, `app_governed`, `unreadable_validity`,
  `duplicate_id`, `unreadable_tier`); eligibility is shared via
  `sources/native.rs::eligibility`.
- Edits (`owner_edit.rs`) write `knowledge.json` under its flock as the owner:
  add → `preferences` tier, explicit `memory_kind`, direct statement; edit →
  supersede (`supersession_source: owner_surface`, via
  `lasting_write::supersede_with_owner_wording`); confirm → `owner_confirmed`;
  retire → `memory_lifecycle: retracted`; forget → remove and erase wording
  from history. A stale revision is `409 stale_preference` with
  `current_revision`; a retired record can only be forgotten.
- `DELETE /memory/user-knowledge/{path}` removing an owner-given instruction
  anywhere under the path from a bot daemon or plane terminal →
  `403 owner_instruction_needs_the_owner` (`owner_edit::removes_an_owner_instruction`).
- Check log lines record `judged: [{id, revision}]` and `policy_version`
  (ids and digests only), the source of "last checked".

## Analyst access

The internal analyst (`internal-system-analyst`, "Sonar") may read, never
change, alignment data.

- Read-only `internal_data` actions `alignment_intentions`,
  `alignment_history` (`record_id` narrows), `alignment_checks`,
  `alignment_policy`; its definition (template and seeded scope copy) denies
  `save_preference`, `forget_memory`, `update_memory_tier`, which its `local`
  trust would otherwise grant.
- Redacted by default (`redact.rs`): every wording field (`statement`,
  `conditions`, `wording_before`/`wording_after`, `explanation`, `quote`,
  `unresolved_facts`) → `[kind · N words · #digest]`; ids, kinds, strengths,
  lifecycles, revisions and timings stay; results say `"wording": "redacted"`
  or `"granted"`.
- `request_preference_wording {reason}` (`wording_grant.rs`) raises
  `alignment.wording_unlock` (`approve`/`decline`, declined after 15 minutes),
  answerable only from the owner's web, iOS or Android app (else `403
  private_surface_required`); approval grants wording to that run's execution
  id for 24 hours (`alignment_wording_grants.json`, journaled
  `wording_granted`); other runs stay redacted.

## Preference history

`memory/users/intention_history.jsonl` (`intention_history.v1`,
`history.rs`, rotated at 4 MiB) journals every intention-bearing change
(`sources/native.rs::bears_intention`: a default-kind tier or reviewed
`memory_kind` preference, goal or instruction, any lifecycle).
`persist_user_knowledge` diffs under the flock (`diff_intentions`), so every
writer is covered; `forget_memory` under its own. A failed append is logged and
the write stands; bookkeeping-only changes (timestamps, evidence lists) are
skipped.

- **Entry:** `at_ms`, `record_id`, `tier`, `op` (`added`, `edited`,
  `confirmed`, `superseded`, `retired`, `restored`, `scope_changed`,
  `forgotten`; events `policy_changed`, `taste_decided`, `wording_granted`),
  `actor`, revision and wording before/after. `policy_changed` (record
  `policy`, revisions `r{n}`) is written after the policy lock is released;
  `taste_decided` is keyed `taste:…` from the taste review handler.
- **Actor** (`actor.rs`): task-local `WriteActor` (`via`, `surface`, `cause`)
  via `update_user_knowledge_as`/`persist_user_knowledge_as`; outer caller
  wins; unnamed → `unknown`, never dropped.
- **Forget:** wording erased from every entry and the rotated copy
  (`wording_erased: true`); dated entries stay.
- **Durability:** appends and rotation lock, sync file and directory, and
  propagate rotation failures; erasure and grants use the shared durable
  writer (unique staging files).

## Evals

Corpus, schema, scorer: `data/magician_v2/evals/preference_alignment/` (see
its README). `make test-preference-alignment` runs the provider-free lane,
including the save-time routing refusal (`magician-api` `alignment`), the
engine settings codes (`decision-engine` `settings`) and the decision-routing
proxy:

- `EvalCase::to_alignment_input` applies host eligibility (a `superseded`
  intention is dropped; the current owner turn is the only citable span).
- Every dev case must have an outcome surviving the validator and force caps
  (a failure is a corpus defect, not a judge failure).
- The oracle scripted judge scores all cases through the real parse and
  validator; the always-proceed baseline must miss every intervention case.
- `journey_tests.rs` runs chat save to admission: owner-turn vs. non-owner
  saves (hold vs. nudge), clearing, same-key correction, owner cancel, fence
  staleness, shadow, backstop consume-once and widened arguments, private
  acts never judged.

`make test-preference-alignment-storage` runs journal rotation/concurrency,
erasure and wording-grant regressions on the real host's memory, locks and
durable I/O. Live-model lanes (governed judge over the corpus, held-out split,
real-model baselines) need an owner budget and are not run yet.
