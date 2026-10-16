# Eval lanes (`magician_surfaces::evals`)

Backend of the `/evals` page ([unified-ui / evals page](../unified-ui/evals-page.md)):
`Makefile` `## eval:` annotations become lanes, each run is an internal
execution, and every run leaves one uniform record.

Design: 2026-07-26 evals page design ·
plan: 2026-07-26 evals page plan

## Module map

| module | owns |
| --- | --- |
| `magician-surfaces/src/evals/registry.rs` | pure Makefile annotation parser (no I/O) |
| `magician-surfaces/src/evals/readiness.rs` | live service probes; fail-closed decision |
| `magician-surfaces/src/evals/options.rs` | bounded lifecycle repeat, partition, configured-profile options |
| `magician-surfaces/src/evals/run.rs` | `EvalRun`, the uniform run record |
| `magician-surfaces/src/evals/store.rs` | append-only per-scope run history |
| `magician-surfaces/src/evals/cost.rs` | read-time join from the LLM ledger |
| `magician-surfaces/src/evals/runner.rs` | starting a lane; every run is recorded |
| `magician-surfaces/src/evals/executor.rs` | production `EvalExecutor` over the task system |
| `magician-surfaces/src/evals/provider_replay.rs` | cross-provider interrupted-history and clean-checkpoint conformance |
| `magician-api/src/evals_api.rs` | `/api/magician/v2/evals/{lanes,runs,spend,report,web-researcher/judge,{lane}/run}` |

`magician/tests/eval_lane_contract.rs` checks the real Makefile both ways
([inverse contract](#the-inverse-contract-every-eval-target-must-be-annotated)).

## Discovered lanes

`/evals` has no UI allowlist: lanes are the `Makefile`'s `## eval:` annotations,
served with readiness by `GET /api/magician/v2/evals/lanes`.
Cargo-test and bash harness targets write `report.html` / `report.json` through
`scripts/write_eval_harness_report.py` so the page deep-links them like the
Python evaluators. Lane-specific gates:
[chat-mode](chat-mode.md) / [plane](plane.md),
[memory-evals](memory-evals.md#ann-shadow-eval).

## Harness conformance chat lane

`scripts/eval-harness-conformance-live.py --lane chat` runs
`scripts/fixtures/harness_conformance/chat_cases.json` under the `magician`
baseline and every installed harness engine ([plane](plane.md) owns the chat
mouth), grading effect plus proof of who answered; `tools_expected` cases also
need a tool-lineage fact for the turn. Tools reached through the mouth bridge
(`ChatService::build_mouth_bridge`) emit the same chat tool events, so a case
may expect a tool the plane does not execute.

| case | turns | effect | oracle |
| --- | --- | --- | --- |
| `pong` | 1 | answer contains `PONG` | no tools |
| `task_lookup` | 1 | answer contains the setup task's secret | task read |
| `task_update` | 1 | setup task title carries the nonce suffix | task update |
| `agent_roster` | 1 | answer names `web-researcher` | roster read |
| `remember_recall` | 2 | turn 2 recalls the nonce colour | memory write/read; deferred write `partial` |
| `tool_result_recall` | 2 | turn 2 names the secret turn 1 looked up | turn 1 answers only `DONE` (leak fails); task deleted after turn 1; turn 2 no tools |
| `streaming` | 1 | answer contains `runtime` | SSE, ≥ 2 `token` frames; `codex` exempt (`partial`, documented) |
| `thread_create` | 1 | UI thread `HC-<nonce>` exists | `create_chat_thread` via bridge; matched in `GET /api/magician/v2/ui-threads?q=…` by name (case-insensitive) or slug id |
| `create_task` | 1 | task `HC-<nonce>` in `/v3/tasks` | `create_task` via bridge; no setup task |

`task_exists` / `thread_exists` take `title`, poll with the task-effect bound,
and record `task_id` / `thread_id` and `task_deleted` / `thread_deleted`; the
rig deletes probes (even after a failed turn) unless `--keep-artifacts`. No
`persona_switch` case: `switch_personality` bridges, but tonal presets give
`answer_contains` no marker.

Per-case options (result field in brackets):

- `delete_task_after_turn: N`: delete the setup task after turn N
  (`task_deleted_after_turn`, `task_readable_after_delete`); a refused delete or
  readable task fails as `precondition: …` (`probe task still readable after delete`).
- `no_tools_turn: N`: tools on turn N fail the case even with the effect
  present (`tools used on the no-tools turn: <names>`) [`no_tools_ok`].
- `turn_answer_must_not_contain: {turn: N, value: …}`: `withheld_value` absent
  from turn N, checked before the effect (`the withheld value leaked into an
  earlier answer: turn N answered with …`) [`no_leak_ok`].
- `stream: true`: turns use `POST …/messages/stream`; the answer is the `done`
  frame's message (`usage.provider` proves the mouth), else the joined `token`
  frames; turns record `token_events`.
- `min_token_events: N`: fewer frames fails (`too few streamed token frames:
  turn T streamed K token frame(s), wanted at least N`) [`stream_ok`].
- `engines_exempt: [...]`: that shortfall grades `partial`
  (`engine cannot stream (documented)`).

Live runs and `--regrade` share `grade_case_result`, so a regraded report reads
exactly as a fresh run.

## Harness conformance run lane

`scripts/eval-harness-conformance-live.py --lane run` runs
`scripts/fixtures/harness_conformance/run_cases.json` per engine, switching
the RUN engine (`execution.harness_engine`, `PUT /api/magician/v2/plane/engine`) and restoring `current` and `run_model`
from `GET /plane/engines` in `finally`, Ctrl-C included (the PUT resets an
absent `harness_model` to `default`). It measures the parent-engine rule
(`docs/archive/plans/2026-09-14-parent-engine-routing.md`): text-only
background operations follow the run's engine; tool-carrying requests never do.

Per engine and case: create approved `HC-<nonce>`
(`agent_id: personal-assistant`), `POST /v3/tasks/{id}/execute`, poll
`GET /v3/tasks/{id}/executions/{execution_id}` to `completed | failed | cancelled`,
then read:

- effect: `task_field_contains`;
- proof: the journal (`GET /v3/events?task_id=&execution_id=&
  backfill_only=true`) has an `execution.progress` fact with
  `payload.kind == "harness_turn_settled"` per external turn, its
  `payload.engine` naming the engine under test; the baseline has none, only
  `llm.succeeded` on `agentic_decision`;
- trace: `POST /analytics/llm/facts/query` on `llm_calls` `WHERE task_id = …`
  (5 s before creation to now + 1 min; polled every 5 s up to 90 s) as
  `analytics_rows` / `analytics_sql`, plus unfiltered `window_rows` (JSON only).

| case | effect | oracle |
| --- | --- | --- |
| `run_task_update` | title carries `-run-updated` | effect plus proof |
| `run_parent_follow` | same | plus `trace: true`. External engine: an `agentic_decision*` row on a `harness-*` provider is `fail` (`tool_call_rode_harness`); no other operation on `harness-<family>` is `partial` (`no_text_op_on_parent`, or `no_text_op_observed` when only tool-carrying rows exist). `magician`: any harness provider is `fail` (`baseline_op_rode_harness`). No rows is `inconclusive` (`no_analytics_rows`) |

Families: `claude_code -> harness-claude_code`, `codex -> harness-codex`,
`codex_app_server -> harness-codex`, `grok -> harness-grok`,
`agy -> harness-agy`. Results add `trace_ok` (floor held) and `trace_followed` (rule held) to `effect_ok` /
`proof_ok`; the reason names the operations and providers seen (and a harness
provider seen only in the unfiltered window); `--regrade` re-derives them.

Waiting on a person (`waiting_for_user`, `waiting_for_confirmation`, `paused`,
…) for two consecutive polls: cancel (`POST /v3/executions/{id}/cancel`),
`fail` (`run_waited_for_input`; no clarification answerer). Still running at `--turn-timeout-secs`: cancel,
`inconclusive` (`run_timeout`). Probes deleted unless `--keep-artifacts`.
Verdict rule and report are the chat lane's (`decide_verdict` also takes
`trace_rows`, `trace_ok`, `trace_followed`).

Run: `make test-harness-conformance-live-eval HARNESS_CONFORMANCE_LANE=run`
(`HARNESS_CONFORMANCE_ENGINES=magician,codex` narrows) or `--engines
magician,claude_code,codex,codex_app_server,grok,agy`. Needs the runtime,
`MAGICIAN_BEARER_TOKEN` with session scope (engine PUT is owner-only),
signed-in CLIs, analytics. `--self-test --lane run` is provider-free.

## Harness conformance plane lane

`scripts/eval-harness-conformance-live.py --lane plane` (`scripts/eval_harness_plane.py`) checks that an external harness
holding only a `plt_` terminal grant can run a task through
`POST /api/magician/v2/plane/mcp` and answer the run's question in its own
session. Each case mints a grant (`POST /plane/grants`) and revokes it in
`finally` (`DELETE /plane/grants/{id}`); no global setting changes.

- `scripted_door` (engine `scripted`): a stdlib MCP client (`initialize` with
  `capabilities.elicitation.form`, `Mcp-Session-Id`) checks `tools/list` has
  `create_task`, `run_task`, `wait_for_run`, creates a `run: "manual"` task (else it dispatches at once),
  launches it, and answers the `elicitation/create` frame on the SSE
  `tools/call` with `--plane-answer-word`. Cross-session answers and replays are
  refused (`202` is the only accept); calls after revoke get `401`/`403`. Gates:
  `session_established`, `catalog_advertises_run_tools`,
  `task_created_via_door`, `run_launched`, `elicitation_observed`,
  `answer_accepted`, `cross_session_refused`, `replay_refused`,
  `run_completed`, `effect` (the answer word in the task's file and reply),
  `revoke_refuses_calls`.
- `cli_door` (`claude_code`, `codex`, `grok`, `agy`): each CLI targets the door
  as its engine does (claude: `--mcp-config` + `--strict-mcp-config`,
  `--tools ""`; codex: isolated `CODEX_HOME` whose `config.toml` sets
  `bearer_token_env_var`; grok: isolated `GROK_HOME`, native tools denied; agy:
  `agy mcp add --header`, removed in teardown), holds only the grant (no
  `MAGICIAN_BEARER_TOKEN`), and must create `HC-<nonce>` via `create_task`.
  Missing/signed-out CLI: `cli_unavailable`. Gates: `cli_exit_ok`,
  `task_created` (in `/v3/tasks`), `via_plane` (`create_task` in its trace).

Tasks are kept. Run: `make test-plane-harness-live`
(`HARNESS_CONFORMANCE_ENGINES=scripted,codex`) or `--engines scripted
--turn-timeout-secs 600`; `--api-base-url` defaults to
`http://127.0.0.1:3002`. `make test-plane-harness-eval` is provider-free.

## Harness conformance voice lane

`scripts/eval-harness-conformance-live.py --lane voice`
(`scripts/eval_harness_voice.py`) drives a realtime session like the meeting
responder: `POST /media/sessions`
(`surface_type: web_desktop`) → WebSocket `/media/voice/{id}/control` →
`session.start {realtime_profile}` → `session.ready` → one user turn → frames
until `delegate_to_chat.done` or timeout → `session.end`.

Profiles: `voice_realtime_openai_backend` (GPT Realtime) and
`voice_realtime_gemini_38_live` / `voice_realtime_gemini_38_live_thinking`
(Gemini 3.8 Live) delegate via model-initiated `delegate_to_chat`;
`voice_realtime_gpt_live_1` (GPT Live 1) via Live-initiated
`session.delegation.created`.

`--voice-driver` defaults to `auto`, which resolves to `speech`: `say` → ffmpeg → 24 kHz mono PCM16 frames,
push-to-talk on a `turn_detection: none` profile. `text` sends
`user.text {request_response: true}`, Realtime only. The title is three
everyday two-syllable words and the prompt names no product, so speech-to-text carries it.

Reports are by outcome: `expected`, `did`, `result` (reached if the task exists
as asked or as transcribed) and `path` (`delegated`, `self_served`, `declined`,
`errored`; reported, not judged). `proof` gates: `delegation_succeeded` (`delegate_to_chat.done.success`); `voice_call_row` (a
successful `llm_calls` row, `capability = "voice.realtime"`,
`execution_id = voice:<session>`, naming the chat turn); `voice_call_priced`
(has a cost); `hands_on_turn` (`tool.call.started` and
`tool.result.projected` for `create_task` under the delegate call); `effect`.
Speech also reports `heard` (`transcript.user`). Per-profile summary:
reached/runs plus paths taken. Artifacts deleted unless
`--keep-artifacts`; evidence keeps non-delta control frames.

`create_task` must be hot on the voice surface; deferred, mouths decline the
`tool_search` hop (Gemini loses the turn to the reconnect it forces).

Run: `make test-voice-harness-live` (`HARNESS_CONFORMANCE_RUNS=3`,
`HARNESS_CONFORMANCE_ENGINES=voice_realtime_gpt_live_1`) or `--runs 3
--turn-timeout-secs 180`. Needs the runtime, `MAGICIAN_BEARER_TOKEN`, the OpenAI
key, `say`, `ffmpeg`, `websocket-client`. `make test-voice-harness-eval` is
provider-free.

## Memory lifecycle runs

The live lifecycle lane advertises `run_options: "memory_lifecycle"`;
`POST /evals/{lane}/run` accepts optional `profiles` (≤ 3 distinct configured
names), `repeats` (1–3) and `partition` (`all`, `development`, `validation`).
An empty body keeps existing behavior. Unknown fields, invalid values and
options for other lanes are `400` before execution; the wrapper preflights
profiles against runtime config before any model call.

Both lifecycle lanes pass a generated `EVAL_RUN_ID` and bind history to that
run's `runs/<id>/report.html` only. Options are additive fields in history
schema 1. Ledger spend is unknown (separate evaluator process); the report keeps
router estimates. See the [lifecycle contract](memory-lifecycle.md#focused-qualification).

## Runtime performance and resource lane

`make test-runtime-performance-live-eval` gates latency, retained memory,
storage amplification and stack risk (not correctness) across web research,
pre-plan HITL, live Tutor (browser and `ios_tutor_overlay`), concurrent
Attention / Follow-up / resource-governor polls, and retrieval under writer
contention.

It attaches to the listening Magician PID (never starts or restarts it), samples RSS, sizes
the Attention DB and scope surfaces with bounded symlink-free scans (budget
exhausted: inconclusive), and checks a bounded `magician.log` suffix for
stack-overflow, SIGABRT and unexpected-exit. Crash, restart and
source-evaluator failures always fail.

The first fully passing run is captured atomically as the baseline
(`$(COVERAGE_BASE_DIR)/evals/runtime-performance/live/baseline/report.json`); comparisons use
per-metric relative ceilings plus absolute noise headroom.
`RUNTIME_PERFORMANCE_LIVE_BASELINE=...` picks another (cleared: measure only);
`RUNTIME_PERFORMANCE_LIVE_CAPTURE_BASELINE_IF_MISSING=false` requires one.

`make test-admission-300-task-eval-harness` proves the hard live-agent cap admits
300, rejects 301 and returns to zero after guards drop (not the owner-gated
live soak). Blocking-pool admission (R12) is restart-bound (`runtime.scale.overrides.blocking_admission_permits`,
16 permits on the `current`/`auto` profiles; `MAGICIAN_BLOCKING_ADMISSION=off`); the governor snapshot exposes the
`blocking_admission` gauge (in-flight, high-water, wait). Do not shrink Tokio
`max_blocking_threads`.

`make test-runtime-performance-eval-harness` is provider-free. Reports:
`evals/runtime-performance/{deterministic,live}/latest` (directory hrefs end in
a slash, keeping sibling evidence inside the declared directory).

## The `## eval:` annotation

```make
## eval: kind=live requires=ollama,magician_binary report=evals/memory-temperature
## desc: Gate real/synthetic memory recall
test-memory-temperature-live-eval:
	@…
```

| field | required | values |
| --- | --- | --- |
| `kind` | **yes** | `harness` (self-contained) or `live` (talks to a real service) |
| `requires` | no | comma-separated, no spaces: `ollama`, `magician`, `magician_binary`, `magicutor`, `provider_keys` |
| `report` | no | literal repo-relative directory (**no `$(VAR)`**, no `..`, not absolute) |
| `## desc:` | no | separate line between annotation and rule |

`kind` is **never defaulted** (`harness` is the kind run unattended): a bad
value (`kind=liv`), key (`KIND=`, `kinds=`), spacing (`kind = live`) or omission
yields `EvalKind::Unknown`, which cannot run. `requires` is a sorted,
de-duplicated **set**; a space in it (`requires=ollama, magician`) is reported,
not silently shortened.

### Placement rules — read this before adding a lane

**An annotation must sit directly above its target, or above that target's
`.PHONY:` line.** The parser skips `## desc:`, prose comments, `.PHONY:` and other
dot-targets; **a blank line ends the search**, as does any other non-rule
(`ifeq`, `include`, `export FOO`, an assignment), so an orphan never drifts onto
another target.

```make
## eval: kind=harness report=evals/monitor
## desc: Golden monitor traces
# Provider-free golden eval for the Recurring Monitors change ledger.
.PHONY: eval-monitor-golden
eval-monitor-golden:
	@…
```

That is legal; a blank line before the rule orphans it, as does the trailing
form `eval-x: ## eval: kind=harness` (trailing `##` is help text here).

### The inverse contract: every eval target must be annotated

`eval_lane_contract` detects eval targets by **report directory, not name**
(Makefile variables pointing into `evals/`, then recipes expanding them).

| assertion | catches |
| --- | --- |
| `no_annotation_is_orphaned` | an annotation that reached no rule |
| `every_annotated_lane_names_a_real_target` | a renamed target |
| `every_target_writing_an_eval_report_is_annotated` | an undeclared eval target |

(`every_annotated_lane_parses_without_error` sees only existing lanes.)

### Targets that deliberately are not lanes

Listed with reasons in `ALLOWED_UNANNOTATED` (`eval_lane_contract.rs`), itself
checked for stale entries; add a new non-lane there with its reason:

- **`test`, `test-verbose`**: aggregates feeding `run-all-tests-with-report.sh`.
- **`benchmark-chat-context-retrieval`**: `test-chat-context-retrieval-live-eval`
  without gating flags (`--require-hybrid`, `--require-coalescing`,
  `--max-concurrent-wall-p*-ms`), so always exit 0; the gating lane is annotated.
- **`benchmark-media-*`**: same shape; write to
  `data/magician_v2/media_evals/results/`.

### Nothing is ever dropped

A malformed field returns the lane with `parse_error`; an undeclared `kind` is
`EvalKind::Unknown`; an annotation reaching no rule, or a near miss (`##eval:`,
`## Eval:`, `## eval :`, trailing form), is an `OrphanedAnnotation` (line, text,
reason). A missing lane must never look like one that does not exist.

## Why uniformity is produced OUTSIDE the lanes

Lane reports share no schema; the runner shells the `make` target and keeps
what ran, when, how long and the verdict. `EvalRun::report_href` links the
lane's own report.

## Why cost is joined at read time, never stored

`EvalRun` **has no cost field** (`run.rs` test `a_run_record_carries_no_cost`
rejects any serialized `cost`/`usd` key): the repricing LLM ledger is the only
source of truth. `cost.rs` joins on `EvalRun::task_id` at read
time, batched (`costs_for_runs`, one ledger query per page), with **no error
type**, so a ledger failure cannot be `unwrap_or_default()`ed into free lanes.

## The unknown-never-zero rule

| situation | answer |
| --- | --- |
| ledger has rows for the run's task | `Known(sum)` |
| ledger has no rows (no LLM calls) | `Known(0.0)` |
| ledger unreachable or unusable answer | `Unknown` |
| run has no `task_id` | `Unknown` |

`CostValue` is tagged (`{"kind":"known","usd":0.83}` / `{"kind":"unknown"}`):
an unknown has no number. `join` returns unknown for an empty task set rather
than query it; a `cost_usd: NULL` row makes its task `Unknown` for good; and
`LlmLedgerCosts` returns `Err` when it may not have seen everything (truncated
result, unsafe task id, failed chunk).

### The aggregate form: a partial total is a FLOOR

`SpendTotal::known_usd` sums known runs; **if `SpendTotal::unknown_runs > 0` it
is a lower bound** (`SpendTotal::is_floor()`, rendered `≥$1.20`).
`/evals/spend` refuses ranges past the ledger's 31-day window
(`400 range_too_wide`). `total.known_usd` counts each task once; `by_lane`
figures can sum higher, and the total is the right one.

## Why a lane's verdict cannot be read from `ShellState::last_exit_code`

**`last_exit_code` is `Some(0)` whenever the tool call succeeded** (Pack/DuckDB
hardcode it), and `AgenticOutcome::Success` only means the agent reported.
`executor::exit_code_from_evidence` uses, in order:

1. the native shell executor's `Command failed with exit code {code}: …` (the
   only real process status);
2. the agent's reported code, only if every code it mentions agrees;
3. a `Success` whose last command was *this lane's* target with tool exit 0.

Otherwise `EvalCommandOutcome::indeterminate` records
`EvalRunStatus::Interrupted` (neither `Failed` nor `Passed`); pass rates use
`is_judgement()` runs only. The goal says *do not fix this* and
`EVAL_MAX_ITERATIONS` is 8, so an agent cannot repair a failing eval into a pass.

## Readiness fails closed

`lane_readiness` counts a requirement met **only** on `ProbeResult::Up`; `Down`,
`Unknown` and missing entries block. `Down` needs service evidence (failed HTTP
status, non-executable binary, no key); **transport errors and timeouts are
`Unknown`**. `magician` is the server over HTTP, `magician_binary` is
`./$(MAGICIAN_BIN)` on disk, `magicutor` a separate service.

## Every run leaves a record — by construction

`RunRecord`'s **only** write site is its `Drop` (no `finish()`, `write()` or
`commit()`); moved into the supervising future up front, it persists on early
return, `?`, panic or tokio drop (so `EvalRunStore` is synchronous).
`SIGKILL`/abort leaves no record.

A new `EvalRun` starts `Interrupted`; there is no `Running` variant. History is
append-only at `evals/runs/<lane_id>/<run_id>.json` under the scope root, and
`EvalRunStore::append` refuses to overwrite a `(lane_id, run_id)`. "Is this lane
running?" queries the task system by execution title
(`execution_title_for_lane`, prefix `eval lane: `).

## Known limitations

- **`provider_keys` cannot name a provider**: any credential a non-Ollama
  profile names counts (an Anthropic lane is ready with only `OPENAI_API_KEY`).
- **`magician` means up, not new enough** (`GET /health` on `MAGICIAN_BASE_URL`).
- **No `network` token**: an internet-only lane reads ready offline.
- **A renamed execution title orphans an in-flight run** (a second can start).
- **`report_dir` cannot contain a make variable** (e.g.
  `$(COVERAGE_BASE_DIR)/evals/x`); write the literal path.

## Endpoints

| route | notes |
| --- | --- |
| `GET /api/magician/v2/evals/lanes` | lanes + readiness + last run + `orphaned` + `probes` |
| `GET /api/magician/v2/evals/runs` | server-paged; `total` is the filter count |
| `GET /api/magician/v2/evals/spend` | explicit range required; > 31 days refused |
| `POST /api/magician/v2/evals/{lane}/run` | `202` + `{task_id, run_id}`; `409` not-ready / already-running, `422` unrunnable |
| `GET /api/magician/v2/evals/report/{tail}` | lane-gated passthrough to a lane's report |
| `POST /api/magician/v2/evals/web-researcher/judge` | eval-only gate: judges an answer against bounded text from its cited pages |

Data routes resolve scope via `resolve_required_scope` (missing: `400`). The
report passthrough is deliberately **not** scope-bound (navigation carries no
bearer); its path must lie in a lane's validated `report=` directory. Specific
paths register **before** `/evals/{lane}/run` in `magician-bin/src/main.rs`, so
no lane id shadows them.
