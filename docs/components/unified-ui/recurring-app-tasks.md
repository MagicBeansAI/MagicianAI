# Recurring App execution history

Scheduled App behaviors reuse one internal task. The task's details endpoint
returns 25 enriched executions by default, newest first, with an opaque
`next_execution_cursor` and `execution_total`. Both the expanded row and shared
task drawer offer **Load older executions**. Older payloads are fetched only on
that action; live refreshes merge current executions into the loaded history
without dropping the reader's older pages.

The task drawer and expanded row show the recurring interval, latest run status
and next eligible time. An active or unsettled run is described as waiting for
settlement; its previous scheduled timestamp is not presented as an imminent
launch. If a whole new history page arrives between refreshes, the fresh cursor
keeps the intervening runs reachable instead of silently skipping them.

Run selection remains tied to the exact execution ID, including its log and
outputs. The recurring task stays under Internal tasks and links to Apps for
App-owned controls. Generic Delete/Retry controls do not gain authority over App
workflows. See [the runtime contract](../magician/recurring-app-tasks.md).

`make test-app-recurring-ui` covers cursor serialization, lazy loading and the
existing task drawer behavior. `make check-ui` checks the Svelte/TypeScript
surface.
