# Harness SRE Agent

Relay (`harness-sre`) is the autonomous reliability agent for the company-loop
harness. It exists to catch the class of failure where agents run cycles,
produce briefings, but do not move tasks, anomaly repairs, backlog promotion, or
program state forward.

## Runtime Shape

- System template:
  `magician_data_v3/system/agent_templates/agents/harness-sre/definition.agent.yaml`
- Default scoped seed:
  `magician_data_v3/scopes/anonymous/default/agent_runtime/agents/harness-sre/definition.agent.yaml`
- Required default delegate seeds:
  `magician_data_v3/scopes/anonymous/default/agent_runtime/agents/internal-system-analyst/definition.agent.yaml`
  and `magician_data_v3/scopes/anonymous/default/agent_runtime/agents/cto/definition.agent.yaml`
- Program spec:
  `scopes/<principal>/<workspace>/programs/harness_reliability.md`
- Default cadence:
  `7 * * * *`

The scoped runtime definition carries `principal: anonymous` and
`workspace: default` for the default seed. Live runtime roots, including
`~/MagicianNotes`, own their active copies; seed files should not overwrite an
operator-edited runtime copy. Container startup copies the default program and
required default scoped agent definitions into the runtime root only when those
files are missing.

## Runtime Control

The Crew page exposes the effective global company-loop state. It reads
`GET /api/magician/v2/harness/runtime` and writes
`PUT /api/magician/v2/harness/runtime` with `{ "enabled": true|false }`.
The write persists `harness.paused` in the live `magician-config.yaml`; no
restart is required. A successful mutation also refreshes the shared
`AgentResources` config snapshot; admission and background gates continue to
reload the file and fail closed. Turning the loop off blocks scheduled and
manual harness starts, the steward tick, and autofix dispatch. It also clears
queued harness triggers across scopes and requests cancellation of active
harness cycles.

`MAGICIAN_HARNESS_PAUSED` remains an emergency process/file override. A Crew
mutation removes that key from both live runtime env files and makes config
authoritative. Missing or malformed live config fails closed and is reported by
the status endpoint, so a config fault cannot silently start Relay or peers.

`max_tasks_per_cycle` is enforced at both the compiled `create_task` handler and
the harness-native task producers (`create_task`, `reassign_task`, and
`promote_backlog_item`), not only in the prompt. Manual and scheduled harness
dispatch thread the cap through `AgenticContextOverrides`; spawned tasks are
tagged with their source execution and task id, and both provider paths count
every matching tag. A process-wide count-and-create gate closes concurrent
harness calls, while missing cycle provenance fails closed.

Relay is intentionally configured as a low-cost watchdog, not a broad forensic
worker: one spawned task per cycle, one delegation hop, a small prompt budget,
no LLM memory-consolidation rules, and hourly scheduled cadence. Scheduled cron
ticks coalesce when the same agent/goal already has active or queued work, so a
restart or long cycle cannot build a token-burning backlog. Scheduled harness
cycles are also capped at eight concurrent root dispatches
(`HARNESS_ROOT_PERMIT_LIMIT`, `magician-core/src/local_resource_governor.rs`)
process-wide across
cron, wake/catch-up, manual, and promoted-queue entry points. Admission happens
before prompt assembly or V3 task activation, so a capacity rejection cannot
leave an orphan running task.

Delegated work has a second process-wide boundary: per-level delegated-child
pools (`DELEGATED_CHILD_PERMITS_BY_LEVEL` = `[4, 4, 2, 1]`, so four root
children at level 0) and one `internal-system-analyst` child at a time. Relay may create only
one delegated child total per root execution. Its full and flat provider
schemas expose `maxItems: 1`, while the runtime rechecks the durable root child
set so repeated calls cannot bypass the schema. A delegated permit is held for
the child's execution future and released on completion or cancellation.

`constraints.max_tokens_per_cycle` is a runtime budget. Usage is accumulated in
an execution-scoped meter, persisted in pause state, and restored on resume.
The operation router charges responses before parsing, so malformed outputs and
tool-internal router calls still count; Pi coding turns charge input plus output
tokens. Reasoning/cache fields are not double-counted, and missing provider usage
fails closed while a hard cycle budget is active. Delegated children use their
own definition budgets.

Internal analytics remains available to Relay and Sonar. DuckDB work is
serialized to one process-wide query, configured with one execution thread,
bounded memory/temp storage, deadline interruption, and 10,000-row / 4 MiB
materialized-result limits. The guard stays on the blocking thread until the
query actually exits, so an async timeout cannot release capacity while an
orphan query continues consuming CPU.

## Evidence Contract

Every harness-enabled personal-agent prompt with a loaded program context gets a
deterministic `### Harness Health` block from
`append_harness_program_context`. It is best-effort and read-only. It
summarizes:

- harness scheduler entries, due/missed entries, and cron entries missing
  `next_run_at`;
- open anomalies, `FixDispatched` anomalies, and repair dispatches whose linked
  task is missing, unreadable, failed, cancelled, completed without anomaly
  closure, or still running/planning after the attention window;
- proposed/promoted/delivered backlog counts, promoted items with missing task
  linkage, and terminal promoted tasks still awaiting an evidence-based delivery
  review.

A promoted task reaching `completed`, `failed`, `cancelled`, or retryable
`ready` does not silently close its directive. The owning harness calls
`inspect_backlog_delivery` for bounded output previews and material artifact
evidence, then calls `review_backlog_delivery`: `accepted` moves a genuinely
completed outcome to `Delivered`; `rework` returns the item to `Proposed` with
optional corrected scope. Acceptance fails closed when persisted material
delivery evidence is absent.

Delivery-review behavior is owned by the declarative CPO and Harness SRE
personas. Rust supplies completion enforcement, tool execution, and structured
health facts such as `delivery_review=pending`; it does not append a second
delivery-policy prompt to generated tasks. The system templates and active
materialized scope definitions must keep those behavior-bearing persona blocks
in sync while preserving scope-specific metadata and operator settings.

Fresh recurrence of a `FixDispatched` anomaly reopens the anomaly while keeping
its dispatch timestamp and task id, so the issue is visible but autofix cooldowns
still prevent immediate duplicate repair tasks.

Relay must treat that block as the first input for a cycle. If the block is
healthy, it can record a compact program-state update and stop. If it shows a
stuck state, Relay should take one concrete action.

## Delegation Model

Relay delegates for evidence, not ceremony:

- `internal-system-analyst` (Sonar) receives narrow forensic asks that need logs,
  task/execution JSONL, DuckDB analytics, LLM-call telemetry, or trace
  correlation. Delegations should include exact principal/workspace, agent id,
  goal id, anomaly signature, backlog id, task id, execution id, and time window
  when known.
- `cto` receives scoped engineering repair work only after Relay has a concrete
  defect or acceptance criteria.

Relay also has harness action tools auto-granted by `kind: personal` +
`harness`, including `create_task`, `promote_backlog_item`,
`propose_backlog_item`, `inspect_backlog_delivery`,
`review_backlog_delivery`, and `update_program_state`.

## App Package Wrapper (plan 2.2)

The template directory also carries the first first-party app package at
`magician_data_v3/system/agent_templates/agents/harness-sre/app/`
(`harness-sre-reliability`). It is the platform-layering plan's cheapest
agents-as-data dogfood and is deliberately a wrapper, not a move:

- The package's `run_audit` workflow binds this definition by name as its
  runner (`agent: harness-sre`). Installation review resolves the definition
  through the scoped agent definition store, checks it permits the Task
  surface, and seals its digest; runtime revalidation fails closed if the
  definition drifts from the sealed review.
- The YAML template and the program doc stay exactly where they live; nothing
  about the scheduled autonomous loop, its cadence, or its tool grants depends
  on the package being installed.
- The package owns one small entity/view/action surface (`reliability_audit`
  records) with no dependencies, no memory contributions, and no LLM
  operations. See
  [app primitive catalog](app-primitive-catalog.md#first-party-package-harness-sre-reliability-plan-22)
  for the binding mechanics and the publication flow.

## Non-Goals

Relay is not another business executive and should not scrape market data,
write product directives, or generate broad status reports. It should not create
duplicate repair tasks for fresh or already-running repair dispatches, reopen
dismissed incidents without new evidence, or propose structural agent changes
without repeated evidence.
