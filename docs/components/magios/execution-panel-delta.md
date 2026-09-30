# Execution Panel Delta (iOS)

How Magios consumes the `ExecutionPanelDelta` realtime event, and the decoding
rules that keep a single unexpected field from silencing the whole surface.

## Wire shape

The backend builds the payload in
`magician-learning/src/execution_panel/v3_adapter.rs` and ships it over the
realtime WebSocket. iOS models it in `magios/Shared/ExecutionPanelTypes.swift`:

```
ExecutionPanelDeltaEventData
└── state: ExecutionPanelState
    ├── overview: ExecutionPanelOverview   ← task id, title, status, progress
    ├── run:      ExecutionPanelRunState   ← summary, recent activity, activity log
    ├── output:   ExecutionPanelOutputState
    └── debug:    ExecutionPanelDebugState
```

## Consumers

Two, both driven off the realtime stream:

| Site | Uses | Effect when a delta is dropped |
| --- | --- | --- |
| `ChatViewModel.updateExecutionPanel` | `overview.status`, `overview.title`, run activity | The chat task card stops updating |
| `BackgroundEngine` | `overview.isTerminal`, run activity | The Live Activity never updates and never ends |

Both decode through `decodeExecutionPanelDelta(from:)`. Use it rather than
calling `JSONDecoder` directly — see below.

## Task status is a `String`, deliberately

`ExecutionPanelOverview.status` is a plain `String`, **not** a Swift enum.

The backend's `TaskStatus`
(`magician/src/magician_v2/storage/task_models.rs`) has nine variants —
`pending`, `planning`, `ready`, `running`, `paused`, `completed`, `failed`,
`cancelled`, `deferred` — and `map_task_status` falls back to `pending` for
anything it does not recognise, so `pending` is the *default*, not an edge case.

`overview` is only ever decoded as a child of `ExecutionPanelDeltaEventData`.
A closed enum therefore fails open in the worst possible direction: one
unmodelled status makes the *entire delta* throw, so the chat card and the Live
Activity both stall — with no error surfaced to the user. A closed enum also
re-arms that failure every time the backend grows a variant.

This mirrors `TaskV3.status`, which is a `String` for the same reason, with
`statusLabel` / `statusColor` mapping the known values and defaulting for the
rest. **Do not "tighten" either of these into an enum.** Presentation should
switch on the known values and fall through to a sensible default.

`ExecutionPanelOverview.isTerminal` is the one place that interprets status,
mirroring the backend's `TaskStatus::is_terminal` (`completed`, `failed`,
`cancelled`). An unknown status is treated as non-terminal — a run we do not
understand is better left showing progress than falsely declared finished.

## Decode failures must be loud

`decodeExecutionPanelDelta(from:)` wraps the decode and logs the underlying
`DecodingError` on failure. Do not replace it with a bare `try?`: a decode
failure here is always client/backend contract drift, and a silently dropped
delta hides it.

## Tests

`magios/MagiosTests/ModelAndUtilityTests.swift`:

- `testExecutionPanelDeltaDecodesEveryBackendTaskStatus` — every backend status,
  plus an invented future one, must decode without discarding the delta.
- `testExecutionPanelOverviewTerminalStatuses` — pins the terminal set and that
  an unknown status is not terminal.

When the backend adds a status, add it to the first test's list. It should pass
without any other change; if it does not, something has been narrowed.
