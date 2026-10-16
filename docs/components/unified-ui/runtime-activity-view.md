# Runtime Activity View

Before the full API is ready, the root layout's `StartupGate` shows startup
status and delays mounting child screens. It polls the coarse
`/api/magician/v2/startup` status every 500 ms only while the service reports
initialization; older servers and offline connections keep normal client
behavior. See [runtime startup](../magician/startup.md).

`ui/unified-ui/src/routes/(app)/runtime/ActivityView.svelte` is one live surface
for every unit of work in flight: agent steps, background distillation and
consolidation, LLM dispatch, governed child processes, capability invocation.

It is backed by the `Activity` transport family (`ActivityStarted` /
`ActivityFinished` / `ActivityProgress` / `ActivityCost`), documented in
[v2-websocket-events.md](../magician/v2-websocket-events.md). The page
subscribes to the app's event hub ([event-hub.md](event-hub.md)) with the
`/v3/events` `category=activity` rule, reads history with one
`/v3/events?category=activity&backfill_only=true` request (again on every hub
resync), and never polls. It holds no stream of its own.

## Three tiers, one gate

| Tier | Answers |
|------|---------|
| Header strip | How much is running, how fast, how slow, how many failed |
| Lanes | What is happening now, grouped by workload class |
| Stream | The complete record: the span tree and its log lines |

All three read the same retained window and apply the same two filters, the
subject pin and the kind chips, through one predicate (`spanVisible`). This is
a correctness rule: a header counting work the stream hides reads as a quiet
runtime when it is a filtered one.

Kind chips filter per span: turning off `agent` does not hide an agent step's
LLM calls, it **promotes them to the root**, the same rule the subject pin
follows.

## Undeclared is a state, never a default

Lane keys are the wire values verbatim (`foreground_chat`, `ambient`, …), the
same strings the `workload_class` parquet column stores. The view title-cases
them for display and groups on the raw value. A class the client does not know
still gets a lane.

- **`workload_class: null`** gets its own dashed **Undeclared** lane, labelled
  as an instrumentation gap. A child takes the nearest declaring ancestor's
  value, so a growing undeclared lane means a *root* is missing its
  declaration.
- **`outcome: "closed"`** is counted apart from errors and successes, as
  "undeclared end": the span ended but was not seen to succeed. There is
  deliberately no success-rate number on this page.

## Cost, and the three ways it can be absent

Cost arrives after the span closes and attaches by `activity_id`.

- **No cost at all**: the span made no model call, or the call ran outside any
  instrumented span. Nothing is shown.
- **An unpriced call** publishes nothing rather than a zero, and is left out of
  the header totals. A `0` would say the work was free.
- **`local`** renders as the word `local`, never `$0.00`.

**Commodities are never added**: `usd`, `local` and `tavily_credit` sit side by
side. **A non-zero amount never renders as zero**: spend below the
four-decimal floor shows at six decimals, never as `0.0000`. Several costs on
one span (retries, chunked summaries, provider fallback) accumulate per
commodity. Hovering a cost shows its token counts when the provider reported
them.

## Pinning a subject

Clicking an `agent_id` in a lane pins every tier to that subject. Dimensions
are inherited, so the pin keeps the whole subtree the run opened.

- The header's `running` and `spans/s` are **filtered** counts. `held` is not
  filtered: it is how full the in-memory window is.
- A span that matches the pin while its parent does not renders **at the
  root**, so it is never counted by the header yet unreachable in the stream.
- Unspanned ("loose") progress rows carry no agent, thread or task, so they are
  hidden while a pin is set. The pin chip stays visible.
- `resetBuffers` clears the pin with every other buffer (Clear or a scope
  switch). `subjectFilter` is declared *above* `resetBuffers`, since a `let` in
  the temporal dead zone would throw during instance setup.

## Reachability of the dimensions

`workload_class`, `agent_id`, `model` and `operation` have declaring spans in
the runtime; `thread_id` is declared by `chat_turn` and `agentic_run`.
`task_id` rides the wire but is declared only by `agentic_run`, so the task pin
reaches only dispatched task work.

## Telling one model call from another

Every model call opens a span named `llm_dispatch`, with the same `kind`
(`llm`) and `target`. `operation` tells them apart and is rendered right after
the span name in lanes and stream (`llm_dispatch agentic_decision`). It is the
only dimension that does **not** inherit, since it names one call, so absence
means "this span dispatched nothing", not missing instrumentation.

Stream rows show `model` and per-commodity cost on the right, with the same
rules as the lanes: `local` as the word, commodities never added, nothing for
an unpriced call.

## Automatic storage maintenance

Database repairs appear in the system workload lane as `Database maintenance`
with operation `database_compaction`, using the ordinary scoped activity
family. Storage settings also polls the lightweight `/storage/maintenance`
status every ten seconds while visible, so the last run shows after a
reconnect without rescanning inventory.
