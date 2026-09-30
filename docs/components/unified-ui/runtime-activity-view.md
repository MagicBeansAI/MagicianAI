# Runtime Activity View

Before the full API is ready, the root layout's `StartupGate` shows startup
status and delays child-screen mounting. It polls the coarse
`/api/magician/v2/startup` status every 500 ms only while the service explicitly
reports initialization; older servers and offline connections retain normal
client behavior. See [runtime startup](../magician/startup.md).

`ui/unified-ui/src/routes/(app)/runtime/ActivityView.svelte` — one live surface
for every unit of work in flight: agent steps, background distillation and
consolidation, LLM dispatch, governed child processes, capability invocation.

Backed by the `Activity` transport family (`ActivityStarted` /
`ActivityFinished` / `ActivityProgress` / `ActivityCost`), documented field by
field in [v2-websocket-events.md](../magician/v2-websocket-events.md). The page
opens one NDJSON stream on the operator's own scope and never polls.

## Three tiers, one gate

| Tier | Answers |
|------|---------|
| Header strip | How much is running, how fast, how slow, how many failed |
| Lanes | What is happening right now, grouped by workload class |
| Stream | The complete record — the tree of spans and their log lines |

All three read the same retained window and apply the same filters. That is a
correctness requirement, not a nicety: a header reporting totals for work the
stream below is hiding reads as a quiet runtime when it is a filtered one.

**There are two filters and both apply to all three tiers**: the subject pin and
the kind chips. They meet in one predicate (`spanVisible`) that every tier calls,
so a third filter cannot be added to one tier and forgotten in the others.

A consequence of making the kind chips per-span: turning off `agent` does not
hide an agent step's LLM calls, it **promotes them to the root**. That is the
same rule the subject pin already follows — a span whose parent was filtered away
renders at the root rather than disappearing — and it is what lets the chips mean
the same thing in the header as they do in the stream.

## Undeclared is a state, never a default

Two absences are rendered as themselves and must stay that way:

Lane keys are the wire values verbatim — `foreground_chat`, `ambient`, … — the
same strings the `workload_class` parquet column stores. The view title-cases
them for display and groups on the raw value, so a lane and an analytical
rollup are the same bucket. A class the server sends that the client's order
list does not know still gets a lane rather than disappearing.

- **`workload_class: null`** gets its own dashed **Undeclared** lane, labelled
  as an instrumentation gap rather than as a category. A root that forgot to
  declare is exactly what the view exists to surface; inventing a plausible
  class for it hides the gap. See the inheritance rules on `ActivityStarted` —
  a child takes the nearest declaring ancestor's value, so a growing undeclared
  lane means a *root* is missing its declaration.
- **`outcome: "closed"`** is counted apart from errors *and* apart from
  successes, under its own "undeclared end" figure. The layer watched the span
  end; it did not watch it succeed. There is deliberately no success-rate
  number on this page, because any such figure would have to decide what
  `closed` means and the honest answer is that it does not know.

## Cost, and the three ways it can be absent

Cost arrives out of band, after the span it belongs to has already closed, and
attaches by `activity_id`. Three absences are rendered as themselves:

- **A span with no cost at all** — it made no model call, or the call was issued
  outside any instrumented span and so published nothing. Nothing is shown.
- **An unpriced call** publishes nothing rather than a zero. The header's per
  commodity totals simply do not include it. A `0` would say the work was free;
  the truth is that the price is unknown.
- **`local`** renders as the word `local`, never as `$0.00`. The work ran on the
  operator's own hardware and money was never the unit, which is a different
  statement from a vendor charging nothing.

Two rules the numbers themselves follow. **Commodities are never added** —
`usd`, `local` and `tavily_credit` count different things, so they sit side by
side in the header and on the row. And **a non-zero amount never renders as
zero**: spend below the four-decimal floor is shown at six decimals rather than
rounded into a `0.0000` that reads as free.

A span can receive several costs — an agent step that retries, a chunked
summarisation, a fallback to a second provider — and they are accumulated per
commodity rather than overwritten. Hovering a cost shows the token counts behind
it, when the provider reported them.

## Pinning a subject

Clicking an `agent_id` in a lane pins every tier to that subject. Because the
dimensions are inherited, pinning an agent keeps the whole subtree its run
opened, not just the root that declared the id.

Consequences that are deliberate:

- The header's `running` and `spans/s` are the **filtered** counts, over the
  filtered window. `held` is not filtered — it is the retention figure, how
  full the in-memory window is, not a count of what is on screen.
- A span that matches the pin while its parent does not renders **at the
  root**. Nesting it under a parent that was itself filtered away would make it
  unreachable from any rendered row — counted by the header, absent from the
  stream.
- Unspanned ("loose") progress rows are hidden while a pin is set. They carry
  no agent, thread or task at all, so they can never be the pinned subject's
  work. Clearing the pin brings them straight back, and the pin chip is always
  visible while one is set.
- The pin is cleared by `resetBuffers`, alongside every other buffer that
  outlives a window: a Clear or a scope switch removes every row the pinned
  subject had, and leaving the pin set would render an empty view that looks
  like a quiet runtime. `subjectFilter` is declared *above* `resetBuffers` for
  that reason — a `let` is in the temporal dead zone until its declaration runs,
  so a reset reached during instance setup would throw rather than clear.

## Reachability of the dimensions

`workload_class`, `agent_id`, `model` and `operation` have declaring spans in
the runtime today, and `thread_id` is declared by `chat_turn` and `agentic_run`.
`task_id` rides the wire and the client model but is declared only by
`agentic_run`, so the task pin is reachable only for dispatched task work. That
absence is visible rather than inferred: the ids simply do not appear.

## Telling one model call from another

Every model call in the process opens a span named `llm_dispatch` — it is the
single dispatch boundary, so the name is identical on all of them by
construction, as are `kind` (`llm`) and `target`. A stream of a hundred calls
therefore read as a hundred copies of one line.

`operation` is what distinguishes them, and it is rendered right after the span
name in both the lanes and the stream, so a row reads `llm_dispatch
agentic_decision` rather than `llm_dispatch`. It is the only dimension that
does **not** inherit: it names one call, not a subtree, so a child span never
borrows its parent's. Absence therefore means "this span dispatched nothing",
not "someone forgot to instrument" — the opposite of what an absent
`workload_class` means.

Stream rows carry `model` and per-commodity cost on the right, under the same
rules the lanes already followed: `local` renders as the word, commodities sit
side by side and are never added, and an unpriced call shows nothing rather
than a zero. The lanes show a short live window; the stream is where the record
is read, and it was previously the one tier that could not answer "which model,
and what did it cost".

## Automatic storage maintenance

Database repairs appear in the system workload lane as `Database maintenance`
with operation `database_compaction`; start, progress and terminal outcomes use
the ordinary scoped activity family. Storage settings also polls the lightweight
`/storage/maintenance` status endpoint every ten seconds while visible. This
shows the last run after reconnecting without repeatedly scanning inventory.
