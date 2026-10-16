# Relentless

Relentless adds a fixed definition of done and independent artifact review
around the existing agent harness. It does not choose goals or script the
actor's tools. Design and implementation history: [Relentless
v1](../../archive/plans/2026-09-30-relentless-v1-design.md) (its [archive
gates](../../archive/plans/2026-09-30-relentless-v1-design.md#current-status-and-archive-gate)
are closed; this document owns current behavior). Live qualification of the
local-draft/capture rollout: [archived Work qualification
record](../../archive/plans/2026-10-07-work-quality-end-to-end-qualification.md).

**Status.** Qualified live: local-draft and capture-only handoffs, owner
answers and restart, code, sampled media, public research, OpenAI image
generation/editing, a Veo generation through the metered tool boundary, and
critic-triggered paid regeneration (a controlled initial defect, not a natural
defect or a second Veo dispatch). Known limits:

- Communication stays local draft or capture; live-channel delivery and
  production trust are separate owner decisions.
- Live guidance promotion, canary and adoption are unverified: live paired
  trials were rejected by the zero-regression gate, so nothing was dispatched.
- Question selection quality is model-dependent; pilots prove only
  answer/resume and crash recovery.
- Browser DOM controls exercised the real reply dialog; pointer automation is
  not qualified.

## Tasks, proposals and policy

A task owns the work; Relentless is an optional review and improvement policy
on it. The harness enriches context and proposes success criteria before
admission; independent feedback then guides revisions against them. Material
changes to goal or criteria need a new agreement. Uncertainty alone should not
interrupt the owner: the actor investigates, reuses answers, records reversible
assumptions and bundles material missing inputs before pausing through the
existing ask loop.

In OPC the coordinator delegates the outcome, the assigned task agent proposes
and executes, and Relentless supplies bounded review. Agents propose brief and
sources; manual setup can attach notes/documents or text files as frozen source
snapshots. The originating task and proposing agent stay visible.

### The proposal tool

`propose_relentless` (compiled; pack version 1.6.0) proposes text, code, research and
declared image/video/audio contracts: explicit representations, checks, source
provenance, sample coverage, media bounds and exact template slots. No use-case
name selects semantics.

- Trusted universal task-management substrate: deny rules remove it, untrusted
  agents never get it implicitly, surface-only agents keep their ceiling.
- `profiles` reads the qualified non-secret model catalog.
- Checks the scoped agent/task/execution, validates without a model call and
  creates an idempotent owner request via `UserRequestService`. No execution,
  outward or start authority; dismissal or timeout never admits work.
- The parameter declares an object schema and `param_type: object` so the
  runtime keeps the typed object before dispatch.
- `operation:propose_guidance` takes strict procedural text and terminal
  training run IDs from the agent's own scoped tasks (not trials), writing one
  idempotent, review-required `memory_procedure` candidate and a notice, with no
  model, qualification, activation or outward act. The
  qualification policy must include every training run; cases, targets, budgets
  and expiry need explicit review.

The owner reviews sources, requirements, models and budget. Admission binds the
exact proposal and preview contract hash (router or prompt changes invalidate a
preview) and seals the scoped origin with task ownership. HTTP delegates to
`magician_v2::relentless_service::RelentlessService`.

### Attaching a policy to an existing task

Owners attach the policy to a settled task at
`/work?view=quality&task=<task_id>`; the task keeps its goal, agent, thread and
outputs, and the root factory creates an idempotent bounded root under it.

- Checks the deployment-wide runtime writer floor (no per-workspace cutover
  seal), the reviewed task-manifest hash (excluding runtime timestamp and
  optional policy), exact goal and native actor. Paused controller attempts and
  pre-decided or settled tool calls stay under the authorization hash.
- Active executions and unsettled output synthesis block a new root.
- Interrupted admission exposes its run ID for an exact-brief retry; a newer
  task configuration or another output owner cannot reuse it.

### Recurring task policies

The task-manifest `relentless` policy is activated only by the verified owner
API. It binds reviewed context, proposal and resolved contract to an expiry and
a finite number of starts; maximum spend is the checked product of run count
and per-run ceiling.

- Manual and scheduled starts share task/root admission. Per-policy counts
  commit atomically with the root and survive restart, retries and revocation.
- The reducer rechecks at commit: changed intent, model/adapter pins, exhausted
  limits, expiry, unconfigured runtime ownership or unsupported overrides refuse
  dispatch, with no fallback to an ordinary worker.
- A protected `relentless_launch.json` snapshot and external output owner
  precede the root. The snapshot authorizes nothing; it only recovers its
  committed root/run identity after interrupted admission (changed context,
  pins, exhausted bounds or missing runtime wiring refuse fallback).
- Revocation affects future starts only; admitted run budgets and counters stay
  intact. The workspace shows runs consumed, expiry and aggregate ceiling, and
  can disable future starts.

## Owner workspace and API

The UI is **Work → More → Quality and review** at `/work?view=quality`
(`proposal` for a text draft). Stored `/relentless` links are normalized to
Work in-app; no redirect page remains. Backend notice/report payloads are
unchanged because durable notice replay needs an exact body per request ID. UI
and catalog scaling limits: [draft
workspace](../unified-ui/README.md#relentless-draft-workspace).

The owner API is `/api/magician/v2/relentless`: preview, task-backed admission,
start, scoped history/artifacts, cancellation, matched-attention resume and
exact artifact approval/decline. Ownership comes from the verified session with
its exact workspace binding, never query parameters; the first owner's adopted
`anonymous/default` root is a user scope; reserved event/quarantine sinks are
refused. A missing workspace binding returns 400, a mismatched asserted
`X-Principal`/`X-Workspace` 403, and a refused run transition (such as approving
the wrong artifact hash) 409.

| Group | Routes (under `/relentless`) |
|---|---|
| Admission | `POST /preview`, `GET /profiles`, `GET /work`, `GET /task-context/{task_id}`, `POST /task-policy/{task_id}`, `POST /runs`, `GET /runs` |
| Catalog | `GET /catalog`, `GET /proposals`, `GET /proposals/{id}` |
| Runs | `GET /runs/{id}`, `POST .../cancel`, `.../resume`, `.../review`, `.../budget-resets`, `.../reconcile-costs`; `GET .../lessons`, `.../report` |
| Artifacts | `GET /runs/{id}/artifacts/{hash}`, `.../evidence`, `.../file` |
| Guidance | `GET /guidance-policies`, `/guidance-proposals`, `/guidance-policies/{id}`, `/guidance-policies/{id}/evidence/{run_id}`; `POST /guidance-policies/preview`, `/guidance-policies/{id}` (activate), `.../revoke`, `.../resume` |
| Captures | `GET /campaigns`, `GET`/`POST /campaigns/{id}/captures`; `POST /campaigns/preview`, `/campaigns/{id}`, `/campaigns/{id}/captures/{capture}/review` |

- The owner-scoped admission UUID derives stable task/root IDs;
  compare-or-create repairs an interrupted root link, and the output-owner
  binding includes the frozen proposal hash.
- Admission and owner transitions finish in durable workers, so an HTTP
  disconnect strands nothing. Nothing dispatches before admission is durable.

### Catalog paging

Catalog endpoints take `limit` (default 20, max 100), `offset` (default 0) and
`q` (case-insensitive all-terms, max 512 bytes), and return `pagination` with
`total`, `limit`, `offset`, `has_more`. Filtering precedes paging; order is
deterministic with ID ties. Bad limits, groups or query fields return 400.

| Endpoint | Returns |
|---|---|
| `GET /catalog?view=inbox\|runs` | Compact rows, unfiltered counts, proposing-agent facets. `status` filters the group; `agent_id` filters proposals. Inbox: proposals by request ID, then runs newest first. |
| `GET /proposals/{request_id}`, `/runs/{id}` | Scoped full details. |
| `GET /campaigns/{id}/captures?status=awaiting_review\|captured\|rejected\|all` | Captures newest first, paged apart from policies. Rows keep binding hashes, omit file maps; approval reads the full stored binding. |

- Unverifiable runs list after healthy rows, by ID, as `kind:unavailable` with
  no invented status, spend or approval action; detail, admission and decision
  scans fail closed.
- Accepted internal evaluations with no admitted delivery policy, and held-out
  trials (by sealed guidance origin; never delivered even with a routing
  policy), show as complete "Accepted". Ordinary pending routes stay active;
  human or capture review stays in the inbox.
- Task rows return `run_count` and the latest run; policy/task pages sort by
  name/title then ID and exclude capture history.
- Pages are live views over scoped journals (concurrent writes can shift
  offsets); no derived catalog is authority.

### Owner reviews and answers

An accepted artifact needing human review creates an ask
(`relentless.accepted_review`) with **Approve draft** / **Reject draft** and a
`review_href`. Its id binds run and artifact hash, so an answer from any surface
(composer dock, task panel, Attention, push) records `OwnerReviewed` for that
draft only. `RelentlessService::start_review_answers` follows `HitlResolved` on
the runtime bus; a missed resolution is applied from request history at the next
projection. An answer for a draft no longer pending decides nothing; handoff,
refusal or cancellation closes the ask.

Material actor questions reach the Work inspector and Attention through the
durable event projector and route to the task answer flow. Only infrastructure
blockers get status-only resume, which cannot fabricate an answer. Answer
routing keeps both the pause storage key and its revision-bound correlation ID;
adapters accept the bare key or `key~<12 hexadecimal characters>` and reject
mismatched or malformed pairs.

Task-scoped requests keep publication debt until both the scoped HITL lifecycle
and the canonical task journal acknowledge them. Recovery mirrors an existing
lifecycle receipt into the journal; live transport alone is not a saved inbox
entry.

### Archiving

Soft archiving records `archived` under task admission/write exclusion, hides
the job from active catalogs and keeps files and history. Active roots, owner
waits and pending synthesis must settle first. Late terminal replay, including
after restart, keeps the archive and cannot reopen the job. Recovery keeps the
last report, output and review history and regenerates nothing; the
precomputed-output publisher and direct-output writer share the archive
admission fence (archived replay is a no-op, a new direct write is rejected),
and the record lock protects direct-write refs.

## Budgets

Defaults are **$100, 24 elapsed hours and 25 attempts** under
`execution.quality_round` (repo seed and live config); explicit proposal limits
win. Plateau stopping is opt-in contract policy; otherwise work runs to
acceptance, material input or an allowance stop.

Native proposals without per-attempt limits default to 1,024 iterations and 500
physical model calls (retries included), separate from spend. Admission protects
the full critic/retry quote and funds the actor from the rest, capped at its
call and paid-tool ceiling. Each physical call reserves its enforceable maximum
inside that parent and settles before more spending; unknown outcomes keep
their reservation. If funds cannot cover the first model call, admitted paid
tools and reviews, the run asks for cost continuation. Frozen contracts keep
their limits.

### Continuation after an allowance stop

An allowance stop pauses the same Work item with one durable "Review
continuation" notice per stop; dismissal or expiry leaves it paused. Terminal
billing is reconciled before the notice so the reviewed generation stays valid.

`POST /relentless/runs/{id}/budget-resets` needs verified owner identity, a
unique reset ID, the reviewed generation and the original contract hash.

- Only reached allowances reset, all of them if several block: cost gets a fresh
  allowance, attempts add to the count, time restarts at approval. Criteria,
  pins, tools, parent deadlines, outward authority and per-attempt quotas stay
  frozen.
- The journal records each approval, prior totals and deadline; usage stays
  cumulative, with current windows reported apart. Completed-work elapsed time
  stops at the final controller outcome; late receipts for already-funded
  operations cannot refill a new window.
- Idempotent; restart and agents grant nothing. Resumable stops keep unused
  review funding and undecided reservations and publish no learning or
  guidance-regression observations. Trials offer no extension; archived,
  cancelled, superseded or closed legacy roots cannot reopen.

An answered question resumes the original funded actor and review quote; its
planning view keeps that allowance even with zero unreserved run dollars, and
owner-approved extensions cannot enlarge frozen per-attempt funding. A native
loop that settles and exhausts its allowance with a declared candidate saved
still gets checks and review; without one the attempt fails (its loop segment
kept for retry controls) and the next fits the budget or raises the notice.
Unknown dispatch outcomes stay blocked for reconciliation.

Ordinary task budget GET/POST endpoints refuse externally owned roots with
`409 external_controller_budget_frozen`, reporting no ordinary ceiling as a
controller limit, writing no grant and resuming nothing; invalid ownership
records fail closed.

### The shared meter

The meter reserves actor and required-critic headroom together. Composite
adapter jobs reuse `BudgetLedger` inside the parent attempt: child calls reserve
within its maximum with their own durable starts and receipts, and the aggregate
parent receipt settles the main reservation.

- Extra calls, duplicate child IDs and expired/changed leases deny before I/O.
- Unknown usage survives restart at full ceiling and blocks new reservations
  and starts under the run lock, even with spare parent funding or a call
  reserved before the uncertainty; exact late usage can reconcile it without
  resetting spend.
- Inadmissible usage is recorded unknown before its error returns; an
  accounting-write failure cancels the live resource owner.
- Pre-I/O release needs proof of no durable start; composite results need an
  aggregate cost matching the child ledger.

## Metered tools

Native recipes may admit governed CLI tools through `metered_tools`, one
provider- and format-neutral contract: capability/action pin, argument grants,
work-area confinement, call ceiling and pricing policy.

- Preview pins the installed skill source and entrypoint (Work shows paid pins
  and limits); dispatch rechecks them. Admitted capabilities enter this boundary
  before legacy routing, so a bare name or unadmitted action cannot bypass
  metering through another dispatch path.
- Tool reservations share the actor's composite ledger. A fixed tariff charges
  a successful receipt at its reviewed rate (not an invoice); variable tariffs
  need an integer microusd charge in the receipt. Original generation and
  critic-driven regeneration share the run and cumulative budget.
- Unknown paid outcomes keep their reservation and block further paid calls;
  the underlying dispatch/receipt error is kept, and no refund or repeat is
  implied. Known invocations replay their sealed result without dispatch. An
  abandoned caller queued behind another tool cannot admit a paid dispatch.
- Declared outputs (regular files only; typed directory outputs are rejected
  before reservation or dispatch) are sealed with the receipt under the actor
  byte ceiling; replay restores a missing output but never overwrites later
  changes. A malformed output keeps its charge.
- Typed `workspace_path` actions get normalized relative paths and matching
  read/create grants, checked before reservation (sealed replay skips the
  create check); the actor workspace is the governed working-directory root,
  never submitted as a relative descendant, for profile-free and CLI-profile
  routes, and the skill must declare workspace execution.

The existing harness owns revision, acceptance, owner renewal and recovery.

## Contracts and review

`magician-relentless` (re-exported as `magician_v2::relentless`) validates
resolved contracts, pins independent critic configurations and aggregates
verdicts.

- Every criterion counts toward required validity or fixed-weight quality.
  Missing, malformed, stale, low-confidence or disagreeing required reviews are
  unknown.
- Grounding needs an independent whole-artifact claim inventory binding draft
  quotes to source quotes, checked exactly (no normalization or fuzzy match)
  against pinned source bytes. An empty inventory passes only with explicit
  independent confirmation.
- Criterion specs resolve from content-addressed bytes at admission. The seed
  check adapter supports file existence, UTF-8, byte bounds and JSON assertions;
  rubrics declare instructions and ordered score anchors. No domain checks are
  compiled in.
- Deterministic failures give file-specific findings (observed values, inclusive
  bounds, parse errors) that feed revision and reports but never change the
  contract or turn unknown into pass.

**Verdict validation.** Malformed quote bindings, omitted claim support and
score/inventory contradictions are rejected and may only use the frozen critic
retry allowance (same critic, artifact and sources; invalid verdict and error
in context, full re-audit). Feedback grants no extra call, changes no score or
gate and never turns unknown into pass. Valid negative or uncertain findings
cannot be rejection-shopped; final aggregation uses the same validator and
exposes its reason. Critic prompt `relentless-text-critic-v9` decodes JSON
transport once before exact quoting and carries the repair context; old
serialized requests without it stay readable. Attempt history shows
replay-derived validation reasons beside raw scores.

**The reducer** keeps feasible champions ahead of invalid drafts, accepts the
exact threshold-crossing candidate regardless of noise margin, and counts only
comparable attempts toward a plateau. Check-only runs need no model score.
Deadlines and cancellation prevent late acceptance; acceptance grants no
outward authority.

`review.unknown_review: revise_supported_defects` allows another bounded draft
when valid, confident reviews disagree and a qualified critic failed a required
criterion. It earns no retained or plateau credit, retires unused retry
headroom before reserving the next attempt (settled and uncertain costs stay
charged) and cannot rest on an uncertain claim; missing, malformed,
low-confidence or uncertain evidence still pauses. The
default `pause` is omitted from serialization to keep hashes; new UI contracts
use automatic revision.

### Routing and handoff

The owner can require review of every accepted draft or let the first frozen
critic recommend human review or automatic handoff. The recommendation is
separate from scores, uses the reserved review leg and comes only from the
accepted attempt's validated verdict under the current contract and source
epoch; identical bytes cannot inherit an earlier route (also on replay).
Missing or uncertain evidence falls back to human review. Approval binds exact
bytes; edited or stale payloads cannot reuse it.

The only handoffs are a local task draft or a communication capture (records
the artifact, sends nothing). A typed decision provider can use the same seam
once its cost and identity boundary is qualified; there is no unmetered
fallback. Accepted DoD and completed handoff are separate states: the final
answer publishes the accepted draft only after handoff; declining keeps
acceptance and artifact, fails the task and publishes the report.

## Artifact review recipes

`artifact_review` declares the exact candidate files, a positive
`maximum_prepared_bytes` ceiling for image/frame/audio/check evidence, and each
text, image, video or audio representation. Binary `source_files` carry base64
bytes declared in `sources`. Recipe identity is part of actor/critic config.
Preview resolves missing video executable pins and sampler hashes (supplied pins
must match), and admission checks combined code, probe, frame and audio
reservations against the ceiling.

- **Images and video.** PNG, JPEG, WebP; MP4/WebM sampled at explicit
  timestamps under frozen size, time and executable bounds, with frame process
  time clamped to the run's remaining deadline, into a private cache bound to
  artifact, recipe and source hashes and shared across critics and restart.
  Unseen intervals are unavailable evidence. Quote grounding needs text;
  perceptual reviews must cite inspected IDs or are malformed (keeping their
  paid receipt).
- **`media_probe`** freezes the ffprobe hash, timeout and result ceiling and
  reads only the sealed MP4/WebM with fixed options (no ffmpeg progress flags),
  under the same child ownership, deadline and cancellation draining as frame
  sampling; its cache is bound to input bytes and recipe. Typed `media_number`
  checks bound `duration_micros`, `video_width`, `video_height`,
  `video_stream_count` or `audio_stream_count` (inclusive integers, pixels and
  microseconds) on candidate containers only. Dimensions need exactly one video
  stream; absent facts are unknown; stream presence proves no heard content.
  Probe output counts toward the ceiling and is disclosed to critics
  ([FFprobe JSON output](https://ffmpeg.org/ffprobe.html)).
- **Audio** recipes declare ordered nonoverlapping `{start_secs,duration_secs}`
  segments, sample/transcript ceilings, timeout and exact STT provider/model and
  provider/ffmpeg pins. WAV/MP3 artifacts and optional MP4/WebM audio are
  sampled as mono 16 kHz PCM; samples, transcripts, levels and receipts are
  cached. The scoped media owner drains sampling, transcription and cache
  publication on cancellation; failed transcription keeps the WAV hash and
  levels with an unknown receipt. Only providers with the zero-cost review port
  appear in `audio_review_providers` (the loopback macOS Speech provider; no
  proxy, redirects or fallback); other STT and remote billed jobs are
  unqualified. Partial transcripts force required perceptual scores unknown;
  transcripts prove recognized speech only (not music, prosody, other tracks or
  unsampled intervals).
- **`native_harness.public_research`** admits `web_fetch` and, with
  `allow_search`, `web_search`, under request, byte, redirect and time limits
  shared across attempts and answer continuation. The fetcher pins public
  addresses per redirect and refuses credentials, proxies and private
  destinations; the retrieval fallback and browser are bypassed. Completed
  requests replay their sealed result; interrupted ones are never resent.
  Search results cannot ground claims; fetched pages become runtime-owned
  `collected:<request_hash>` sources (raw-body hash, exact extracted text) that
  enter the candidate hash, review input and quote coverage and that the actor
  cannot mint. Absent policies keep legacy hashes; no contract gains research
  tools automatically.
- **`artifact_review.verification`** pins a `ResolvedPolicy`, required/advisory
  commands, executables, a mandatory OS launcher, read-only roots,
  workspace-relative outputs, protected source hashes and the runner version
  (currently 3; a change needs a newly reviewed contract), referenced by a
  must-pass, unweighted `verification` criterion. Checks run on an ephemeral
  copy of every declared text file (including normally excluded config dirs)
  with network and ambient secrets denied and writes confined to the
  workspace; outputs and scratch cannot overlap source. Protected changes fail;
  other mutation or interruption is a typed unknown review, which never
  overwrites a late genuine result. Cancellation drains the process group and
  pipes before releasing ownership; an owned job publishes after caller
  disconnect. Dispatch markers and immutable caches prevent reruns; reading
  dispatch proof for an unknown reservation never permits another check. Output
  consumes the preparation allowance.

### Evidence inspection and preview

`GET /relentless/runs/{id}/artifacts/{hash}/evidence` returns candidate-bound
fetched sources, cached code results and audio receipts with a content hash; it
never runs checks or prepares media. The workspace adds audio playback, a JSON
contract editor (validated by server preview) and on-demand previews of
declared images, videos and audio from sealed bytes, checking size/type and
bound to the loaded selection (scope/selection changes discard responses and
revoke object URLs). Owner playback does not widen critic coverage; binary
artifacts use owner-scoped downloads and task output links to the exact run.

## Native actor

Proposals may opt into `native_harness` with `agent_id` (must match the task),
`maximum_iterations`, `maximum_physical_calls` and `capabilities`. The actor runs
the native task harness with its own attempt journal under the task/root;
direct drafting omits the field. Launch and answer continuation bind the task's
canonical event scope and scoped audit/credential store; missing audit
authority stops admission.

- Tools: local `files`, optional scoped `media_edit`, bounded
  `web_fetch`/`web_search`, in a durable per-operation workspace. The subset
  comes from the owner's catalog before the ceiling seals; denials and
  untrusted/surface-only owners narrow it, packs are never manufactured, and
  refresh only narrows. The closed catalog keeps each admitted tool's operation
  guide with its schema, and closed resource-bound invocations project admitted
  tools eagerly (grant/deny still apply, no implicit discovery).
- The parent quote covers every allowed call, critic headroom separate;
  dispatch closes and drains its permits before the aggregate receipt seals.
  An unsealed interrupted job keeps its ceiling and is never resent. Ordinary
  recovery cannot rebuild the attempt's budget carrier, so such entry is
  refused at setup.
- Frozen attempts ignore mutable task micro-goals; trace call IDs and
  tool-result identities are namespaced by attempt hash (ordinary task IDs keep
  their shape). The pins `relentless-native-actor-v9` and
  `relentless-text-critic-v9` are frozen per run; a different pin needs fresh
  admission, never a silent upgrade.
- Native entry uses the ordinary status ladder and execution controls, keeping
  the sealed control generation. A successful ending is recorded with its loop
  segment and iteration before the nonterminal seam reopens operator input;
  recovery can repeat that reopening without a model call, and older outputs
  prove nothing. The declared candidate is read with regular-file, symlink,
  UTF-8 and byte checks and passed straight to critics; neither file discovery
  nor output synthesis selects or rewrites it.
- Invalid or missing declared files seal a failed attempt; with known usage the
  next attempt gets the diagnostic (no owner question), keeping the champion
  and all limits and closing unused critic headroom. Unknown usage pauses.
  Over-ceiling candidates record `ActorFailed`, never sealed or sent to
  critics; a changed actor model pin is rejected. An actor at its iteration
  ceiling with a declared artifact still gets checks without another call.

### Media editing

Media guides load from the full compiled pack when discovery is truncated;
unknown parameters are rejected before ffmpeg. FFmpeg drops its banner and info
metadata but keeps errors (bounded in the model error envelope, so a silent
input's missing stream is visible) and progress. A runtime-only lease (work
area, byte ceiling, deadline, cancellation, executable identity) that model
JSON cannot create gates guarded calls, which declare input formats and an
output path, await completion and publish scratch only while the lease is live.
Cancelled callers leave processes and scratch with the media owner; the
controller joins those children before releasing its lease (renewing it during
cleanup), and normal completion drains before sealing a candidate or
checkpoint. Unguarded media calls keep background jobs.

### Native questions

The actor publishes owner questions through the durable pause store and a
nonterminal core checkpoint bound to the canonical body actually stored.

- A scoped pause reader inspects without consuming and refuses ambiguous
  authority, in-flight records, scope drift and integrity failures.
- Resume checks model/recipe, task/root, attempt, question, iteration ceiling,
  digest and the owner-question journal, refuses pending or unknown effects,
  uses the operation-owned work area (never a path from pause data), creates a
  private continuation binding ordinary pause decoding cannot, and resumes the
  original paid ledger.
- Validation re-asks get a fresh attention ID on the same pause key, pin their
  replacement body (request event, schema, timestamp) before publication and
  replace only the answered source via idempotent CAS; projection republishes
  those bytes under the pause lifecycle lock without a model call. A question
  published before its checkpoint recovers only from its sealed terminal receipt
  and a settled ledger; retained answered questions cannot prove a successor.
- A settled continuation records the prior pause digest and retires only that
  exact body; a newer same-key re-ask survives. Completion boundaries and
  answered pauses are retired from the durable result before another attempt.
- Continuation keeps admitted iteration and cost ceilings (no ordinary resume
  allowances); native stops publish no continue/cancel prompt. A terminal run
  closes obsolete questions after matching task, scope, root and
  actor-operation hash; durable HITL closure precedes pause retirement, so
  failures stay retryable and a replacement question survives. Legacy requests
  without a pause close only when their correlation key binds the same root
  and operation. Cold recovery scans scoped V3 authority by execution-prefixed
  keys with a key cursor, rechecking current
  lifecycle authority before closure.
- Question addresses bind root and attempt without pipeline identities. Pause
  reconstruction supplies the run's immutable actor router explicitly; workspace
  input rewrites refuse symlink replacement after a pause.
- Ordinary root pauses recover unchanged owner authority across restart from the
  scope-authenticated terminal receipt and committed journal, before
  destructive consumption; continuation, requested execution, ownership and
  unadopted lineage must match. Extra grants and forged, downgraded or stale
  bodies are refused; verification failure keeps the pause. See [pause
  persistence](pause-and-chat-persistence.md).
- Blocked attempts without a checkpoint show ordinary status; internal
  evaluation roots cannot ask.

**The HITL route** resolves the scoped external-output owner first (ordinary
tasks keep their responder), checks checkpoint and question, stores sensitive
answers in scoped custody and seals a receipt, detached from HTTP. HMAC
fingerprints pass identical replays; conflicting answers are refused.
Malformed form fields cannot evade the collection spec; skipped payloads are
discarded. Continuation needs the receipt, not raw answers; unknown-effect
resolution still needs the exact resolver. After restart, projection rebuilds
question/resolution events from receipts (new receipts keep the canonical
question; legacy ones need the existing proof), checking the broadcaster's
resolution proof before and after emitting; a conflicting or unavailable proof
keeps it pending, without delaying the deadline stop. Replay registers no
second secret and makes no model call.

## Owner input and checkpoints

`OwnerInputRecorded` adds a sealed UTF-8 answer for an exact pending attention
ID and advances the source epoch, keeping contract, reservation, attempt count
and deadline. Actors, critics and routing use a separately hashed evaluation
view; older reviews cannot establish acceptance there, though earlier drafts
remain revision references. Identical replays append
nothing; conflicting replies, wrong requests, unbound bytes, stopped runs and
completed/uncertain operations are refused. Legacy matched-attention resume
carries no answer.

Composite adapters may return `ActorOutput::Paused` with a hash-pinned
checkpoint; `ActorCheckpointed` parks the reservation and attention ID.
`resume_budgeted_adapter_job` verifies answer, lease, deadline, checkpoint and
unchanged ledger before one continuation claim per controller generation; a
later worker reclaims only if no call followed the pause; prior calls and costs
stay consumed. Outstanding or unknown calls prevent resumable checkpoints;
changed records, stale workers and stopped runs get no continuation, and parked
jobs cannot dispatch or publish. The controller passes updated evidence to
`ActorAdapter::resume` and later critics, keeping operation, quote and attempt
number. Recovery refuses missing or symlink-replaced work-area files rather
than creating a fresh area, even for a checkpoint staged before the journal
event. The native resource bridge validates recipe and ledger before claiming
continuation and publishes checkpoints only after dispatch drains.

## Text adapters and model profiles

Text adapters in `magician_v2::relentless_runtime` use the operation router. A
bounded route captures transport/config generation and pricing, needs explicit
context/output/timeout ceilings and a declared `model_family`, and disables
queue/provider retry spending. No tools, shell, browser or delivery.

Critics request a provider-native JSON schema pinned in their config hash;
strict parsing and core checks still decide, and malformed responses are never
repaired. One schema (`critic_response_format`) serves Anthropic Messages,
Gemini and OpenAI; core enforces score/confidence ranges because Anthropic's `minimum`/`maximum`
are left off the wire. Anthropic structured output uses `output_config.format`.

Durable markers precede I/O. Proven pre-I/O interruption readmits the attempt;
a started request without a saved response keeps its reservation and is not
resent.

Profiles (seed and live router, existing credential names):
`relentless-text-gpt61sol` (default of operation `relentless_actor`),
`relentless-text-gpt6luna`, `relentless-text-sonnet55` (default of
`relentless_critic`), `relentless-text-gemini31pro` (explicit selection only).

- Sonnet 5.5: adaptive thinking at `effort: low`, 16K output cap; the provider
  rejects `thinking: disabled`
  ([migration guide](https://platform.claude.com/docs/en/models/sonnet-5-5/migration-guide)).
- GPT profiles: `effort: medium`, 16K cap shared by reasoning and draft; no
  `openai_api_mode` (bounded routes admit only `model_family`; OpenAI defaults
  to Responses).
- Unfinished runs pinned to a profile no longer in the router refuse provider
  recovery (`OperationLlmRouter::bounded_route`: "bounded profile
  unavailable"); terminal output recovery keeps the sealed contract and
  receipts.
- Quotes reserve full physical capacity
  ([GPT-6.1 Sol](https://developers.openai.com/api/docs/models/gpt-6.1-sol),
  [GPT-6 Luna](https://developers.openai.com/api/docs/models/gpt-6-luna),
  1,050,000-token context;
  [Gemini 3.1 Pro](https://ai.google.dev/gemini-api/docs/models/gemini-3.1-pro-preview)),
  released only on receipts or proven non-performance.

## Durability and recovery

The scope-owned journal commits each transition atomically, fences renewable
controller leases by generation and rebuilds decisions from raw verdicts;
sealed blobs are hash-checked on recovery.

| Area | Rule |
|---|---|
| Controller | Reserves review budget before the actor, renews its lease and cancels child work at the deadline; uncertain remote jobs are parked, never recreated. Actor results and receipts commit together. Infra retries keep attempt number and critic config and reuse finished critic legs. |
| Stale workers | A late error against a newer lease generation publishes no blocker, expires nothing and keeps the successor's lease and reservations. Process ownership includes worker identity, so an old worker drains only its own local jobs; this identity changes no contract or hash. |
| Not-dispatched proofs | A proven-undispatched critic continues via `ReviewRestarted` (current unfinished leg, same input and quote, open allowance, new operation ID, no extra retry headroom or paid retry). Actor recovery reuses a journaled `OperationNotPerformed`, so a crash before `ActorRestarted` cannot strand the run (new operation ID, same input, quote and attempt). Unproven work stays blocked; historical events keep their budget semantics. |
| Zero-cost critics | Only a model-free check with its original zero-cost operation/input binding may rebuild from the sealed request and specs, reusing its operation and allowance; paid jobs need a sealed response or no-dispatch proof and are never resent. Unknown critic evidence never counts as acceptance. |
| Terminal billing | Stopped-run recovery and owner `reconcile-costs` settle only typed receipts or fully settled ledgers, under a fenced lease, with no resume, question or model call. Unallocated review headroom is released by a separate journal event, even when all calls are settled. Unknown receipts keep ceilings; cancellation, deadline or failure never implies a refund for an unknown call. |
| Per-record recovery | Damaged runs, campaigns, guidance policies or executions stay blocked and logged while healthy ones recover, never rebuilt from catalogs or given replacement work; trial cleanup still cancels every reserved child. Cancellation, terminal state and owner deadlines are never overwritten. |
| Worker exit | Owner wakes newer than the final journal read survive exit; only an unobserved external wake restarts a stopped worker, and errors do not self-restart. |

### Output ownership

The task factory registers a scoped output owner in
`external_output_owner.json` before creating the root. Terminal callbacks
(including pre-publication cancellation), startup synthesis recovery and the
summary scheduler defer to it, preventing unmetered model calls. The
precomputed-output finalizer checks the binding, uses the existing writers and
reducer and settles the root under lifecycle exclusion; completion replay skips
synthesis and reflection, and generic episode/reflection, startup repair and
memory backfill refuse controller-owned roots. Stopped runs project their draft
as failed/cancelled. Local output reuses the stable task output ID, so replay
overwrites with sealed bytes.

- Interrupted-runtime recovery returns a typed defer for owned roots (including
  sleeping retries) and the ordinary driver refuses them; an unreadable owner
  record does not mean absent.
- The terminal outbox cannot complete the task before review; a sealed
  nonterminal owner question still publishes its request without accepting a
  candidate. Cancellation wins.
- Pending human review projects to the task's waiting state against an exact
  observed state; failed projections retry and replay at startup without
  dispatching. Captures stay authoritative in the journal.
- The cancellation bridge carries task stops to the journal and children, even
  for accepted drafts pending while offline.

## Capture campaigns

Capture policies reuse the approval-envelope module and scoped counterparty
register. The owner reviews task contexts, contracts, sender/channel/message
kind, targets, expiry and finite total/day/recipient caps.

- A capture binds only after its sealed manifest and every blob re-verify; the
  manifest digest identifies the file map, not a blob.
- Pending captures reserve capacity; exact retries reuse the act. Review-event
  order determines simulated clean approvals, which stay per identity. Policy
  changes invalidate coverage; rejected audits suspend automatic capture until a
  successor policy.
- Templates render exact payloads from finite choices or hash-pinned values with
  allowed attachment hashes; novel content and first captures ask; automatic
  policy pins the normalized address. One durable notice per pending capture.
- Zero production trust: captures cannot send or submit. Live trust and
  receipt-fed envelope minting belong to the separate live-action rollout.

An admission or recurring policy may pin `capture_handoff` (campaign ID,
version, identity), outside the quality contract to avoid a hash cycle. The
admission snapshot and root owner key bind the target before root exposure
(including interrupted-admission retry); future starts revalidate the
campaign/task/contract binding. After acceptance the capture is recorded
through the verified path; a refusal keeps the local artifact, records an open
item and posts one notice, with no further actor/critic pass. Old receipts stay
readable after expiry without granting coverage; ambiguous recipient history
refuses. Run views show target, receipt and availability apart from local
acceptance.

## Guidance qualification

Procedural guidance can be qualified and promoted under an owner
evaluation/canary policy. `actor_guidance` is typed procedural text (title,
workflow, decision points, verification, failure modes) pinned in actor config
and admission snapshot. It is not a grounding source and changes no criterion,
critic, tool, model, budget or authority. The policy freezes candidate,
baseline, training runs, fresh held-out and canary suites, targets, phase cost
ceilings, adoption cap/expiry and regression rules.

- Trials need the targets' actor recipe, artifact kind and model/family (a
  text trial cannot qualify a native actor); pairs differ only in guidance.
  Child ceilings are reserved first.
- Positive held-out improvement and canary gates are required. Decimals compare
  exactly (inclusive, no epsilon, non-round-tripping fails closed), with the
  arithmetic version in the policy hash; legacy policies keep their sealed
  decisions.
- Unknown reviews or usage (missing receipts keep reservations),
  operator-assisted trials (steer redirect count), reused held-out inputs and
  training overlap cannot qualify; definitive case failures stop the suite
  without more model calls. Freshness audits exact goal/source content hashes
  (aliases and rubric edits ignored); the owner curates semantic independence.
  Trials publish no questions or lessons and need no handoff. Paused infra
  resumes the same cases and reservations (sharing the activation lock); failure
  needs a new candidate and suite.
- Passing guidance becomes an explicit-admission-only procedure, excluded from
  ambient retrieval and embedding/index refresh; approved recurring draft tasks
  adopt it with a durable slot reservation and pinned snapshot, capture-target
  policies do not. Task budgets, tools, review and outward gates still apply;
  admitted snapshots stay frozen; admission validates full provenance.
- `monitor_nonacceptance_is_regression` counts every nonaccepted terminal,
  cancellation included; missing review evidence is unknown. Regression, expiry
  or revocation stops adoption and archives the procedure; changed or retired
  guidance disables future adoption. Projection reloads the policy under its
  transition lock, so stale snapshots cannot activate after revocation, and
  never overwrites owner edits or revives retired guidance. Expiry runs on the
  recoverable policy worker without model calls; dismissing a notice changes
  nothing.
- Each admission persists an absolute `not_after` at policy expiry; controller,
  call and process deadlines enforce the earlier of it and the run wall clock
  (polling is not the fence; ordinary admissions omit it). Revocation, expiry
  and failure cancel trial children durably, and ordinary recovery rechecks the
  frozen policy. Known phase costs can settle after revocation; unresolved
  operations stay unknown. The policy evidence API/UI shows sealed
  trial/monitor snapshots by recorded hash and binding, never regenerated.

## Reports and lessons

`GET /relentless/runs/{id}/report` derives a typed report and Markdown download
from the journal, bound to the contracts and source epoch. It prefers retained
output over an earlier-source reference or the latest unreviewed candidate,
takes criterion results from matching attempt reviews, labels acceptance apart
from availability, and explains stop reasons,
unresolved checks, review and settled/reserved cost; it never confirms
delivery. Reports are hashed snapshots, not authority. Failed, cancelled and
exhausted outcomes include the report and artifact; accepted text handoffs keep
exact bytes.

Terminal runs seal `relentless-lessons-v1` evidence (scores, findings,
decisions, source epoch, artifacts, spend, outcome), retried idempotently into
the learning queue and exposed at `/runs/{id}/lessons` with no model call.
Lessons need review and cannot
auto-apply guidance or count as qualification, delivery or trust. Published
lessons are immutable; later settled spend is reported separately.

## Claim check before publication

Before a terminal report or accepted draft is published, `RelentlessService`
checks figures against the bound execution's tool results
(`checked_owner_report`, via `check_owner_text` and
`load_execution_grounding_corpus`), with no second judge call. A figure no tool
result contains publishes the markdown under a "Not fully verified" caveat.
Owner-approved bytes (`owner_approved == Some(true)`) publish exactly, and runs
without tool evidence are not checked.

## Attention after an exact review

Progress projections keep the latest execution-event time as
`source_updated_at`. Resolving a review after task completion advances the
Attention publication without changing status or granting work; the requests
index clears it and an older projection cannot resurrect it. Older snapshots
without the field stay readable.

## Tests

| Command | Covers |
|---|---|
| `make test-relentless` | Contract, review, reducer, budget, journal and scripted controller regressions, without the server test binary. |
| `make test-relentless-proposal-visibility` | Trusted discovery, denied/untrusted scopes and native proposal parameter coercion. |
| `make test-agentic-resource-guard` | Native resource integration, router/queue and async-hop seams, and the native-workspace boundary. |
| `make test-relentless-api` | Owner API and root-owner pause restore. |
| `make test-relentless-integrations` | Runtime Relentless, verification, media-edit and governed metered-tool paths. |
| `make test-ui-relentless` | Workspace UI tests under `src/lib/relentless/`. |
| `make test-anthropic-structured-output`, `make test-gemini-structured-output` | Provider structured-output mapping used by critics. |

Provider-free native regressions cover promotion, canary adoption, rollback,
revision recovery and question/paid-attempt continuation; they do not establish
real-model question quality.
