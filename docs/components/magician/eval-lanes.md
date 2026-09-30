# Eval lanes (`magician_surfaces::evals`)

The backend behind the `/evals` page: `## eval:` annotations in the repo
`Makefile` become lanes, a lane runs as an ordinary internal execution, and
every run leaves one uniform record. The page is documented in
[unified-ui / evals page](../unified-ui/evals-page.md).

Design: 2026-07-26 evals page design ·
plan: 2026-07-26 evals page plan

## Module map

| module | owns |
| --- | --- |
| `magician-surfaces/src/evals/registry.rs` | pure Makefile annotation parser. Text in, lanes out, no I/O. |
| `magician-surfaces/src/evals/readiness.rs` | live service probes and the fail-closed decision over them. |
| `magician-surfaces/src/evals/options.rs` | bounded lifecycle repeat, partition and configured-profile options. |
| `magician-surfaces/src/evals/run.rs` | `EvalRun` — the uniform record one run leaves behind. |
| `magician-surfaces/src/evals/store.rs` | append-only per-scope run history on disk. |
| `magician-surfaces/src/evals/cost.rs` | the read-time join from the LLM ledger. |
| `magician-surfaces/src/evals/runner.rs` | starting a lane; the guarantee that every run is recorded. |
| `magician-surfaces/src/evals/executor.rs` | the production `EvalExecutor` over the task system. |
| `magician-surfaces/src/evals/provider_replay.rs` | production-backed cross-provider interrupted-history and clean-checkpoint conformance. |
| `magician-api/src/evals_api.rs` | `/api/magician/v2/evals/{lanes,runs,spend,report,web-researcher/judge,{lane}/run}`. |

`magician/tests/eval_lane_contract.rs` asserts, against the real Makefile, that
every annotation parses cleanly, names a real target, that **nothing is
orphaned** — and, in the other direction, that every target writing a report
under `evals/` carries an annotation.

## Discovered lanes

`/evals` has no UI allowlist. It discovers lanes from `## eval:` annotations
(`eval_lane_contract` checks their integrity). Cargo-test and bash harness
targets write `report.html` / `report.json` through
`scripts/write_eval_harness_report.py` so the page can deep-link the same way it
does for dedicated Python evaluators. Lane-specific gates live with their
owners: [chat-mode](chat-mode.md) / [plane](plane.md),
[memory-evals](memory-evals.md#ann-shadow-eval).

`report=` is optional. When present it is a literal repo-relative directory
(no `$(VAR)`, no `..`, not absolute).

The lane list is the set of `## eval:` annotations in the `Makefile`;
`GET /api/magician/v2/evals/lanes` returns it with readiness.

## Harness conformance chat lane

`scripts/eval-harness-conformance-live.py --lane chat` runs the fixture
`scripts/fixtures/harness_conformance/chat_cases.json` under the `magician`
baseline and every installed harness engine (see
[plane](plane.md) for the chat mouth). Every case is graded by effect plus
proof of who answered; `tools_expected` cases also need a tool-lineage fact
attributing the hands to the turn. A swapped mouth's hands are the plane's
executed tools and the native mouth's own tools reached through the mouth
bridge (`ChatService::build_mouth_bridge`); both emit the same chat tool
events on the turn, so the attribution reads either kind and a case may
expect a tool the plane does not execute itself (a chat-thread or task
creation, a persona switch).

| case | turns | effect | oracle |
| --- | --- | --- | --- |
| `pong` | 1 | answer contains `PONG` | no tools |
| `task_lookup` | 1 | answer contains the setup task's secret | task read |
| `task_update` | 1 | the setup task's title carries the nonce suffix | task update |
| `agent_roster` | 1 | answer names `web-researcher` | roster read |
| `remember_recall` | 2 | turn 2 recalls the nonce colour | memory write then read; deferred writes grade `partial` |
| `tool_result_recall` | 2 | turn 2 names the secret from the task turn 1 looked up | turn 1 must answer only `DONE` (a leaked secret fails), the task is deleted after turn 1 and turn 2 may use no tools, so only turn 1's tool result can answer; a transcript replay cannot |
| `streaming` | 1 | answer contains `runtime` | the turn rides the SSE route and must arrive as at least 2 `token` frames; `codex` is exempt (`partial`, documented) |
| `thread_create` | 1 | a UI thread titled `HC-<nonce>` exists | the mouth's own `create_chat_thread`, reached through the mouth bridge; the rig lists `GET /api/magician/v2/ui-threads?q=…` and matches the name (case-insensitive) or the id the title slugs to, then deletes the thread |
| `create_task` | 1 | a task titled `HC-<nonce>` exists in `/v3/tasks` | the mouth's own `create_task` through the mouth bridge; no setup task, the rig deletes the task it finds |

The existence effects name the title they look for: `task_exists` and
`thread_exists` take `title` (rendered with the case's nonce), poll for it
for the same bound the task effects use, and record `task_id` /
`thread_id` (with `task_deleted` / `thread_deleted`) in the result artifacts;
`--keep-artifacts` leaves both behind. A turn that failed after the tool ran
still gets one lookup by title, so the probe the mouth created is cleaned up
either way. There is no `persona_switch` case: `switch_personality` bridges,
but the shipped presets are tonal and none defines a literal marker a reply
must carry, so an `answer_contains` oracle would grade the model's tone,
not the switch.

Three per-case options build that oracle:

- `delete_task_after_turn: N` — once turn N has answered and its events are
  read, the setup task is deleted (`task_deleted_after_turn`,
  `task_readable_after_delete` in the result artifacts); cleanup then skips it.
  The delete is a precondition, not a cleanup: a refused delete, or a task
  that still reads afterwards, grades `fail` with `precondition: …` (`probe
  task still readable after delete`), since a re-lookup could otherwise pass
  as recall.
- `no_tools_turn: N` — hands on turn N fail the case (`tools used on the
  no-tools turn: <names>`) even when the effect is present; the result's
  `no_tools_ok` carries the check and `--regrade` re-derives it.
- `turn_answer_must_not_contain: {turn: N, value: …}` — the value (rendered
  with the case's nonce and secret, kept as `withheld_value` in the artifacts)
  must be absent from turn N's answer; a leak fails the case (`the withheld
  value leaked into an earlier answer: turn N answered with …`) ahead of the
  effect, because a recall of something an earlier answer already spelled out
  can be a transcript replay rather than tool-result memory. The result's
  `no_leak_ok` carries the check and `--regrade` re-derives it.

The live run and `--regrade` grade through one function
(`grade_case_result`), so a regraded report reads exactly as a fresh run.

Three more shape the streaming oracle:

- `stream: true` — every turn goes to `POST …/messages/stream` and is read
  as SSE; the answer is the `done` frame's assistant message (the same
  response the synchronous route returns, so `usage.provider` still proves the
  mouth), or the concatenated `token` frames when `done` carries none. Each
  turn records its `token_events`.
- `min_token_events: N` — a streamed turn with fewer than N `token` frames
  fails the case (`too few streamed token frames: turn T streamed K token
  frame(s), wanted at least N`) even when the effect is present; the result's
  `stream_ok` carries the check and `--regrade` re-derives it from the
  recorded counts.
- `engines_exempt: [...]` — roster engines for which that shortfall grades
  `partial` with `engine cannot stream (documented)` instead of `fail`.

## Harness conformance run lane

`scripts/eval-harness-conformance-live.py --lane run` runs the fixture
`scripts/fixtures/harness_conformance/run_cases.json` under the `magician`
baseline and every installed harness engine, switching the RUN engine
(`execution.harness_engine`, `PUT /api/magician/v2/plane/engine`) the way
the chat lane switches the mouth. It reads `GET /plane/engines` first
(`current` and `run_model`) and restores both in `finally`, Ctrl-C
included; the PUT resets an absent `harness_model` to `default`, so the
restore sends the saved model too. This is the live measurement of the
parent-engine rule (`docs/archive/plans/2026-09-14-parent-engine-routing.md`):
a run's text-only background operations follow the engine that runs it,
and a tool-carrying request never does.

A case is a task description, not chat turns. Per engine and case the rig
creates a probe task titled `HC-<nonce>` (`agent_id: personal-assistant`,
approved), executes it (`POST /v3/tasks/{id}/execute`), polls
`GET /v3/tasks/{id}/executions/{execution_id}` to a terminal status
(`completed | failed | cancelled`), then reads three things:

- the effect off the task itself (`task_field_contains`, polled for the same
  bound the chat lane uses);
- the proof off the run's journal (`GET /v3/events?task_id=&execution_id=&
  backfill_only=true`): an external engine leaves one `execution.progress`
  fact with `payload.kind == "harness_turn_settled"` per turn, and its
  `payload.engine` must name the engine under test; the baseline leaves none
  and its native loop shows as `llm.succeeded` on `agentic_decision`;
- the trace off the LLM facts (`POST /analytics/llm/facts/query`, relation
  `llm_calls`, `WHERE task_id = …`, window from five seconds before the task
  was created to a minute past now). Facts materialise behind the run, so the
  rig polls every 5 s for up to 90 s until a row for the task appears. The
  rows land in the result artifacts (`analytics_rows`, with the SQL as
  `analytics_sql`), alongside the same window with no task filter
  (`window_rows`, JSON only) so an operation that ran outside the task's
  trace context is still visible.

| case | effect | oracle |
| --- | --- | --- |
| `run_task_update` | the probe task's title carries `-run-updated` | effect plus proof: the run's turns were decided by the engine under test |
| `run_parent_follow` | same run | plus `trace: true`: on an external engine no `agentic_decision*` row rides a `harness-*` provider (`fail`, `tool_call_rode_harness`) and at least one other operation rides `harness-<family>` (`partial` otherwise: `no_text_op_on_parent`, or `no_text_op_observed` when only tool-carrying rows exist); on `magician` no row rides a harness provider (`fail`, `baseline_op_rode_harness`); no rows within the poll is `inconclusive` (`no_analytics_rows`) |

Engine to provider family: `claude_code -> harness-claude_code`,
`codex` and `codex_app_server -> harness-codex`, `grok -> harness-grok`,
`agy -> harness-agy`. The result carries `trace_ok` (the floor held) and
`trace_followed` (the rule held) beside `effect_ok` and `proof_ok`; the
reason names the operations and providers seen, and names a harness
provider that appears only in the unfiltered window when the rule reads as
unfollowed. `--regrade` re-derives the trace from the recorded rows through
the same grader.

A run that settles into a state that needs a person (`waiting_for_user`,
`waiting_for_confirmation`, `paused`, …) on two consecutive polls is
cancelled (`POST /v3/executions/{id}/cancel`) and graded `fail`
(`run_waited_for_input`); the lane has no clarification answerer for a run
the way the chat lane has for a turn. A run still going at
`--turn-timeout-secs` (the run deadline here) is cancelled and
`inconclusive` (`run_timeout`). Either way the probe task is deleted unless
`--keep-artifacts`. Verdict rule and report are the chat lane's
(`decide_verdict` gained `trace_rows`, `trace_ok`, `trace_followed`; the
HTML renders the artifacts where a chat row renders its turns).

Run it with `make test-harness-conformance-live-eval HARNESS_CONFORMANCE_LANE=run`
(optionally `HARNESS_CONFORMANCE_ENGINES=magician,codex`) or directly:
`python3 scripts/eval-harness-conformance-live.py --lane run --engines
magician,claude_code,codex,codex_app_server,grok,agy`; it needs the
runtime, `MAGICIAN_BEARER_TOKEN` with the session scope (the engine PUT is
owner-only), signed-in CLIs, and the analytics layer. `--self-test --lane
run` is provider-free and exercises the grader, the poll and cancel paths,
the engine switch and the report on canned payloads.

## Harness conformance plane lane

`scripts/eval-harness-conformance-live.py --lane plane` (module
`scripts/eval_harness_plane.py`) measures the door from the outside: an
external harness that holds nothing but a `plt_` terminal grant must be able
to run a Magician task through `POST /api/magician/v2/plane/mcp` and answer
the run's question in its own session. No global setting is switched; every
case mints its own grant with the session bearer (`POST /plane/grants`) and
revokes it in `finally` (`DELETE /plane/grants/{id}`). Two case kinds:

- `scripted_door` (engine `scripted`): a stdlib MCP client, no SDK. It
  `initialize`s with `capabilities.elicitation.form`, keeps the
  `Mcp-Session-Id`, checks `tools/list` advertises `create_task`,
  `run_task` and `wait_for_run`, creates a nonce task through the door
  (`run: "manual"`, because a door-created task otherwise dispatches at
  once), launches it with `run_task`, and drives `wait_for_run`, reading the
  `tools/call` answer as SSE and answering the `elicitation/create` frame
  from the fixture word (`--plane-answer-word`). Around the real answer it
  proves the door's ownership rules cheaply: the same prompt answered from a
  second session must be refused, and the accepted answer replayed must be
  refused (`202` is the only accept; anything else counts as refused). After
  the run completes it revokes the grant and proves a further call is
  refused (`401`/`403`). Gates: `session_established`,
  `catalog_advertises_run_tools`, `task_created_via_door`, `run_launched`,
  `elicitation_observed`, `answer_accepted`, `cross_session_refused`,
  `replay_refused`, `run_completed`, `effect` (the answered word is in the
  task's file and its reply — the word exists only in the answer, so a run
  that guessed cannot pass) and `revoke_refuses_calls`.
- `cli_door` (engines `claude_code`, `codex`, `grok`, `agy`): each installed
  CLI is pointed at the door exactly the way its engine points it — claude
  with `--mcp-config` + `--strict-mcp-config` and `--tools ""`; codex with an
  isolated `CODEX_HOME` whose `config.toml` names the door and
  `bearer_token_env_var`; grok with an isolated `GROK_HOME` and its native
  tools denied; agy after `agy mcp add --header`, removed again in teardown —
  holding the grant and nothing else (no `MAGICIAN_BEARER_TOKEN` in the
  child's environment), and asked to create the nonce task `HC-<nonce>` with
  `create_task`. A CLI that is not installed or signed in reports
  `cli_unavailable`. Gates: `cli_exit_ok`, `task_created` (the title is read
  back from `/v3/tasks` — the effect from Magician's side) and `via_plane`
  (`create_task` in the CLI's own tool trace).

Tasks are kept for inspection. Run it with `make test-plane-harness-live`
(`HARNESS_CONFORMANCE_ENGINES=scripted,codex` narrows the matrix) or
directly: `python3 scripts/eval-harness-conformance-live.py --lane plane
--engines scripted --turn-timeout-secs 600`; the port is whatever
`--api-base-url` says (default `http://127.0.0.1:3002`). `make
test-plane-harness-eval` is provider-free and runs the module's contracts —
SSE framing, elicitation answers, both graders, the CLI launch shapes — on
canned payloads.

## Harness conformance voice lane

`scripts/eval-harness-conformance-live.py --lane voice` (module
`scripts/eval_harness_voice.py`) drives a realtime voice session the way the
meeting responder does, without a person speaking: `POST /media/sessions`
(`surface_type: web_desktop`) → the control WebSocket
`/media/voice/{id}/control` → `session.start {realtime_profile}` →
`session.ready` → one user turn → frames until `delegate_to_chat.done` (or
the timeout) → `session.end`. Profiles: `voice_realtime_openai_backend`
(GPT Realtime, model-initiated `delegate_to_chat`),
`voice_realtime_gpt_live_1` (GPT Live 1, Live-initiated
`session.delegation.created`), `voice_realtime_gemini_38_live` and
`voice_realtime_gemini_38_live_thinking` (Gemini 3.8 Live, model-initiated
`delegate_to_chat`). Two drivers (`--voice-driver`): `speech`, the default
everywhere — `say` (unhurried) → ffmpeg → 24 kHz mono PCM16, streamed as
binary frames with push-to-talk on a `turn_detection: none` profile and
trailing silence for a server VAD — and `text`, which sends `user.text
{request_response: true}`; a typed prompt reaches GPT Realtime as a system
item the model may decline to act on, GPT Live 1 only as commentary and
Gemini not at all, so `text` is selectable only for Realtime. The title is
three everyday two-syllable words so speech-to-text can carry it, and the
prompt is the request itself with no product name (transcribers mangle it):
whether the mouth delegates, loads the hand and does it, or declines is its own
decision.

Each run is told as a story and counted by outcome, not by method:
`expected` (a task titled with the probe title), `did` (what the mouth did,
in order — delegated to Magician and what Magician's turn ran and said;
loaded the hand with `tool_search` and called it itself; declined, with its
words; or errored, with the refusal or session error), `result` (reached:
the task exists — as asked, or exactly as the transcriber heard it in any
user turn of the call, since Magician can only act on the words that reached
it; or not reached, naming any task that exists under another title), and
`path` (`delegated`, `self_served`, `declined`, `errored`). A turn that ends
in the task the person asked for is `reached` by any path; how it got there
is reported, not judged. Underneath, `proof` keeps the evidence gates:
`delegation_succeeded` (`delegate_to_chat.done.success`), `voice_call_row`
(a successful `llm_calls` row with `capability = "voice.realtime"` for
`execution_id = voice:<session>` naming the chat turn), `voice_call_priced`
(that row carries a cost — a mouth billed by the clock, as GPT-Live is, has
no free sessions, so an unpriced one is a bill nobody recorded),
`hands_on_turn` (`tool.call.started`
and `tool.result.projected` for `create_task` on the delegated chat turn —
read under the voice turn the call row names and under the delegate call id,
on the voice channel session of the probe's UI thread), and `effect`. For
speech the report also carries `heard`, whether any `transcript.user` had
the three words: a garbled word the model still acted on correctly is the
transcriber's fact, kept beside the outcome. The summary per profile is
reached/runs with the paths taken. Tasks and threads are deleted unless
`--keep-artifacts`; the retained evidence keeps every non-delta control
frame.

The hand (`create_task`) must be hot on the voice surface: when deferred, mouths
decline the `tool_search` hop or, on Gemini, lose the turn to the reconnect it
forces.

Run it with `make test-voice-harness-live` (`HARNESS_CONFORMANCE_RUNS=3`,
`HARNESS_CONFORMANCE_ENGINES=voice_realtime_gpt_live_1`) or directly:
`python3 scripts/eval-harness-conformance-live.py --lane voice --runs 3
--turn-timeout-secs 180`; it needs the runtime, `MAGICIAN_BEARER_TOKEN`,
the OpenAI key, `say` and `ffmpeg`, and the `websocket-client` package two
other evals already use. `make test-voice-harness-eval` is provider-free
and runs the module's contracts on canned frames.

## Memory lifecycle runs

The live lifecycle lane advertises `run_options: "memory_lifecycle"`.
`POST /evals/{lane}/run` accepts optional `profiles` (up to three distinct configured
names), `repeats` (1–3) and `partition` (`all`, `development`, `validation`). Empty
bodies retain existing behavior; unknown fields, invalid values and options for
other lanes return 400 before execution. Profile existence is preflighted against
the runtime config by the wrapper before model calls.

Both lifecycle lanes pass a generated `EVAL_RUN_ID` to Make and bind history to
that run's `runs/<id>/report.html`. Missing evidence never falls back to another
run. Run options are additive optional fields in history schema 1. Live model
spend remains unknown in the task ledger because the evaluator is a separate
process; its report preserves router estimates and missing usage explicitly.
See the [lifecycle contract](memory-lifecycle.md#focused-qualification) for
isolated profile comparisons, CLI controls and coverage boundaries.

## Runtime performance and resource lane

`make test-runtime-performance-live-eval` is the cross-flow measurement gate
for latency, retained memory, storage amplification, and runtime-stack risk.
It does not replace source-evaluator correctness. It runs the existing
direct/delegated web-research and pre-plan HITL lanes, live Tutor on the
browser surface and `ios_tutor_overlay`, concurrent Attention / Follow-up /
resource-governor polls, and memory/procedure retrieval under writer
contention.

The wrapper attaches to the already-listening Magician PID (it does not start
or restart a service). A process sampler records per-scenario and suite RSS.
Bounded scans measure the Attention database and the active scope's chat,
task, execution, and event surfaces; they never follow symlinks, and an
exhausted entry budget is inconclusive. Only a bounded suffix of
`magician.log` is inspected for stack-overflow, SIGABRT, and unexpected-exit
signatures. Crash, restart, and source-evaluator failures are always hard
failures.

The Make and `/evals` lane atomically captures the first fully passing run at
`evals/runtime-performance/live/baseline/report.json`. Failed or inconclusive
runs cannot become authority. Set `RUNTIME_PERFORMANCE_LIVE_BASELINE=...` to
select another baseline,
`RUNTIME_PERFORMANCE_LIVE_CAPTURE_BASELINE_IF_MISSING=false` to require an
existing one, or clear `RUNTIME_PERFORMANCE_LIVE_BASELINE` for a
measurement-only CLI run. Comparisons use metric-specific relative ceilings
with absolute noise headroom.

`make test-admission-300-task-eval-harness` proves the hard live-agent cap
admits 300, rejects 301, and returns to zero after guards drop. It is not the
owner-gated live soak. Blocking-pool admission (R12) is restart-bound
(`runtime.scale.overrides.blocking_admission_permits`,
`MAGICIAN_BLOCKING_ADMISSION=off`). The governor snapshot exposes the
`blocking_admission` gauge (in-flight, high-water, wait). Do not shrink Tokio
`max_blocking_threads`.

`make test-runtime-performance-eval-harness` is provider-free. Reports live at
`evals/runtime-performance/{deterministic,live}/latest`. Report-directory
hrefs are canonicalized with a trailing slash so sibling evidence stays inside
the declared directory.

## The `## eval:` annotation

One line above the target, declaring what the lane is:

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
| `report` | no | repo-relative directory, literal — **no `$(VAR)`**, no `..`, not absolute |
| `## desc:` | no | a separate line between the annotation and the rule |

`kind` is required and is **never defaulted**. A bad value (`kind=liv`), a bad
key (`KIND=`, `kinds=`), spaces around `=` (`kind = live`), or omitting it
yields `EvalKind::Unknown`, which has no runnable representation.
Defaulting to `harness` would put the mistake in the expensive direction:
`harness` is the kind the page may launch unattended.

`requires` is a **set**: sorted and de-duplicated, so `ollama,magician` and
`magician,ollama` describe the same lane. A space inside the list
(`requires=ollama, magician`) splits it into two fields and is reported as
such — the failure mode it prevents is a lane silently declaring *fewer*
preconditions than its author wrote.

### Placement rules — read this before adding a lane

**An annotation must sit directly above its own target, or directly above that
target's `.PHONY:` line.** On the way down to the rule the parser skips
`## desc:` lines, prose comment blocks, `.PHONY:`, and any other dot-target.

**A blank line ends the search.** So does anything else that is not a rule:
`ifeq`, `include`, a bare `export FOO`, an assignment. Deleting a rule together
with its recipe always leaves the blank line behind, which is what stops an
orphaned annotation drifting down onto an unrelated target.

Legal — the annotation reaches its rule through prose and a `.PHONY`:

```make
## eval: kind=harness report=evals/monitor
## desc: Golden monitor traces
# Provider-free golden eval for the Recurring Monitors change ledger.
.PHONY: eval-monitor-golden
eval-monitor-golden:
	@…
```

Not legal — the blank line orphans it:

```make
## eval: kind=harness

eval-monitor-golden:
	@…
```

Neither is the trailing form `eval-x: ## eval: kind=harness`. This Makefile
already uses `##` for trailing help text; the parser reports that form as an
orphan rather than treating it as a lane.

### The inverse contract: every eval target must be annotated

`eval_lane_contract` asserts both directions. Detection is by **report
directory, not by name**: Makefile variables whose value points into `evals/`,
then the recipes that expand them.

| assertion | catches |
| --- | --- |
| `no_annotation_is_orphaned` | an annotation that reached no rule |
| `every_annotated_lane_names_a_real_target` | an annotation whose target was renamed |
| `every_target_writing_an_eval_report_is_annotated` | an eval target nobody declared |

`every_annotated_lane_parses_without_error` only inspects lanes that *exist*,
so a vanished annotation would still pass it.
`no_annotation_is_orphaned` is the assertion that makes a missing row visible.

### Targets that deliberately are not lanes

These write into `evals/` — or would, under a wider rule — and sit in
`ALLOWED_UNANNOTATED` in `eval_lane_contract.rs` with the reason attached. The
list is itself checked for stale entries.

- **`test` and `test-verbose`** — aggregate suites. They only export the report
  variables into `run-all-tests-with-report.sh`; the children write the
  reports. They run lanes; they are not lanes.
- **`benchmark-chat-context-retrieval`** — a **benchmark, not an eval.** Same
  example as `test-chat-context-retrieval-live-eval` but without gating flags
  (`--require-hybrid`, `--require-coalescing`, `--max-concurrent-wall-p*-ms`),
  so it reports timings and always exits 0. Annotating it would put a permanent
  `Passed` on the page. The gating lane is annotated.
- **the `benchmark-media-*` twins** — same shape, and they write to
  `data/magician_v2/media_evals/results/` rather than under `evals/`. Listed so
  a later widening of detection to literal report paths does not re-derive the
  decision.

If you add an eval, annotate it. If you are sure it is not one, say why in that
list.

### Nothing is ever dropped

Every failure degrades to something the page can draw:

- a malformed field → the lane is still returned, carrying a `parse_error`;
- an undeclared `kind` → `EvalKind::Unknown`, with no runnable form;
- an annotation that never reached a rule → an `OrphanedAnnotation` carrying
  its line, its text, and *why*;
- a near miss (`##eval:`, `## Eval:`, `## eval :`, the trailing form) → also an
  `OrphanedAnnotation`, rather than being indistinguishable from never having
  written one.

A lane missing from the grid must never look the same as a lane that does not
exist.

## Why uniformity is produced OUTSIDE the lanes

Eval targets write mutually incompatible reports. None of them needs a shared
schema. A run is supervised from outside: the lane's `make` target is shelled,
and what comes back is an exit code and some output. That reduces every report
format to the same four facts — what ran, when, how long, and whether it
passed — which is what lets every lane gain status, duration, history, and a
trend line with no per-lane migration. The lanes' own reports stay theirs;
`EvalRun::report_href` deep-links out to them.

## Why cost is joined at read time, never stored

`EvalRun` **has no cost field.** `run.rs`'s `a_run_record_carries_no_cost`
test scans the serialized keys and fails on anything containing `cost` or
`usd`.

Money has exactly one source of truth: the LLM ledger. A run links to the
execution that produced it via `EvalRun::task_id`, and `cost.rs` joins on that
at read time. Copying a dollar figure onto the record would freeze it — and
the ledger **reprices** — so the copy would silently become a second, wrong
answer. A join that returns "unknown" is a visible gap; a stale number is not.

The join is batched (`costs_for_runs` takes a slice and issues one ledger query
for a whole page) and it has **no error type**: a ledger failure is data, not
an error a caller could `unwrap_or_default()` into a page of free lanes.

## The unknown-never-zero rule

| situation | answer |
| --- | --- |
| the ledger has rows for the run's task | `Known(sum)` |
| the ledger has no rows — the run genuinely made no LLM calls | `Known(0.0)` |
| the ledger could not be asked, or answered unusably | `Unknown` |
| the run has no `task_id`, so spend cannot be attributed | `Unknown` |

`Known(0.0)` and `Unknown` stay distinct all the way to the pixel. `CostValue`
has no numeric representation of "unknown", and it serialises as a tagged
object — `{"kind":"known","usd":0.83}` or `{"kind":"unknown"}` — so when the
answer is unknown **there is no numeric field to read**.

Consequences:

- `join` must not ask the ledger about an empty task set: "no rows" folds to a
  zero, so when there is nothing to attribute it returns unknown without asking.
- A ledger row with `cost_usd: NULL` is a call the ledger holds but cannot
  price. It becomes `Unknown` for that task, never `+0.0`. Once unknown, always
  unknown — a later priced row cannot restore confidence in a total that is
  already missing a piece.
- `LlmLedgerCosts` returns `Err` rather than a short answer whenever it is not
  certain it saw everything (a possibly-truncated result, an unsafe task id, a
  chunk that failed). A short answer is a smaller bill wearing the clothes of a
  total.

### The aggregate form: a partial total is a FLOOR

`SpendTotal::known_usd` is a sum over the runs whose cost is known. **If
`SpendTotal::unknown_runs > 0`, that sum is a lower bound, not a total** —
`SpendTotal::is_floor()` says so, and the page renders it `≥$1.20`, never
`$1.20`.

`/evals/spend` **refuses** a range wider than the ledger's 31-day window with a
`400 range_too_wide` naming the limit, rather than summing the part that fits.
`total.known_usd` counts each task's money once, while each lane counts it
within itself, so `by_lane` figures can sum to more than the whole-range total.
The whole-range total is the one that is right about how much was spent.

## Why a lane's verdict cannot be read from `ShellState::last_exit_code`

**`last_exit_code` is `Some(0)` whenever the *tool call* succeeded.** Pack and
DuckDB actions hardcode it. It is not the `make` command's exit code.

`AgenticOutcome::Success` is no better: it means the agent finished the goal it
was given, and that goal was to *report* a verdict, not to earn one.

`executor::exit_code_from_evidence` recovers the code from evidence, in
confidence order:

1. **the native shell executor's own failure string** (`Command failed with exit
   code {code}: …`) — the one exit code generated from the process's actual
   status;
2. **the agent's reported code**, accepted only when every code the report
   mentions agrees, so a report naming both `0` and `2` yields nothing;
3. **a `Success` outcome whose last command was *this lane's* target and whose
   tool exit code was zero.** The command check is load-bearing: without it any
   other action's hardcoded `Some(0)` reads as a passing eval.

No branch satisfied ⇒ `EvalCommandOutcome::indeterminate` ⇒ the run records as
`Interrupted`. An eval that was not judged reads as not judged.

`EvalRunStatus::Interrupted` is a third outcome: a cancelled or crashed run
learned nothing about the eval. Folding it into `Failed` puts a red mark on a
lane that was never judged; folding it into `Passed` is worse. Trend lines and
pass rates are computed over `is_judgement()` runs only.

The lane's goal says, explicitly, *do not fix this*. Handed a failing eval and
any room to act, the natural agentic move is to diagnose and repair it — and a
repaired eval reports `Passed` for code that has not changed.
`EVAL_MAX_ITERATIONS` is 8 to bound what an agent can *do* with a failing eval.

## Readiness fails closed

`lane_readiness` counts a requirement as satisfied **only** when a probe
explicitly said `ProbeResult::Up`. `Unknown` blocks, and a requirement absent
from the snapshot blocks.

Refusing a lane that would have worked costs a page reload; offering a lane
whose service we could not reach starts a run that dies partway through, after
spending model time or provider money.

`Down` and `Unknown` both block. A probe only returns `Down` on evidence the
service itself produced: an unsuccessful HTTP response, a binary that is
present but not executable, an environment with no configured key. **Every
transport error and every timeout is `Unknown`**.

The three magician-shaped requirements are separate tokens: `magician` is a
*server answering HTTP*, `magician_binary` is `./$(MAGICIAN_BIN)` *on disk*,
and `magicutor` is a different service entirely.

## Every run leaves a record — by construction

`RunRecord`'s **only** write site is its `Drop`. There is no `finish()`,
`write()` or `commit()` that can be forgotten: the record is moved into the
supervising future *before that future exists*, so an early return, a `?`, a
panic unwinding through it, and tokio dropping the task all converge on the
same line. Finishing a run only *mutates* the record; persisting it is not
something a caller does. This works because `EvalRunStore` is synchronous —
`Drop` cannot await.

The one thing it cannot survive is the process dying without unwinding
(`SIGKILL`, an abort). A run lost that way left no record, so it reads as never
having run, which is true.

A new `EvalRun` starts at `Interrupted`, not `Passed`. `EvalRunStatus` has no
`Running` variant — the store is *history*, append-only, at
`evals/runs/<lane_id>/<run_id>.json` under the scope root.
`EvalRunStore::append` refuses to overwrite an existing `(lane_id, run_id)`.

"Is this lane running right now?" is a **query against the task system**. An
in-memory registry of live runs dies with the process and is invisible to every
other client. The key from a live execution back to a lane is the execution
*title* (`execution_title_for_lane`, prefix `eval lane: `).

## Known limitations

- **`provider_keys` cannot name a specific provider.** The token means "this
  machine has *some* cloud credentials", judged against the env var names the
  configured non-Ollama profiles declare. A lane that bills Anthropic will read
  ready on a machine that only has `OPENAI_API_KEY`.
- **`magician` means "up", not "new enough".** The probe is `GET /health`
  against `MAGICIAN_BASE_URL`. There is no version handshake.
- **There is no `network` requirement token.** A lane that reaches the public
  internet without needing a local service declares nothing, so it reads ready
  offline and fails once started.
- **A renamed execution title orphans an in-flight run.** Renaming a run's
  title from the UI breaks the only key back to its lane, and the lane will then
  accept a second concurrent run.
- **`report_dir` cannot contain a make variable.**
  `$(COVERAGE_BASE_DIR)/evals/x` is how every report dir in the Makefile is
  actually written, and this parser is not `make` — write the literal
  repo-relative path.

## Endpoints

| route | notes |
| --- | --- |
| `GET /api/magician/v2/evals/lanes` | lanes + readiness + last run + `orphaned` + `probes` |
| `GET /api/magician/v2/evals/runs` | server-paged history; `total` is the filter count |
| `GET /api/magician/v2/evals/spend` | requires an explicit range; refuses one wider than 31 days |
| `POST /api/magician/v2/evals/{lane}/run` | `202` + `{task_id, run_id}`; `409` not-ready / already-running, `422` unrunnable |
| `GET /api/magician/v2/evals/report/{tail}` | lane-gated passthrough to a lane's own report |
| `POST /api/magician/v2/evals/web-researcher/judge` | eval-only semantic gate: judges an answer against bounded text fetched from its cited pages |

Every data route resolves scope through `resolve_required_scope` — a missing
scope is a `400`, never a silent read of somebody else's history. The report
passthrough is deliberately **not** scope-bound (it is followed by a plain
browser navigation carrying no bearer); what gates it instead is the Makefile —
the path must lie inside some lane's declared `report=` directory, and those
values were validated at parse time.

The specific paths are registered **before** `/evals/{lane}/run` in
`magician-bin/src/main.rs`, so a lane id can never shadow one of them.
