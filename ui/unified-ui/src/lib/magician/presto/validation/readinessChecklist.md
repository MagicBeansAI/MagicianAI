# Presto GAUI Route Readiness Checklist

Use this checklist before marking a route as migration-ready.

## Required scenarios

- `load`: route renders GAUI components on first load without fallback UI.
- `reconnect`: websocket disconnect/reconnect preserves or recovers route state.
- `errors`: route surfaces API/runtime errors through GAUI alerts and recovers after refresh/retry.
- `empty`: route renders explicit empty state when no data is returned.
- `stale_cycle`: stale agent/cycle payloads are cleared and do not reappear after route/agent/cycle transitions.
- `long_running_updates`: route remains responsive under sustained updates (streaming, polling, or repeated interactions).

## Pass criteria per route

- Required component types from `routeContracts.ts` are present.
- No duplicate `component.id` values in rendered GAUI tree.
- Component IDs follow deterministic ID policy (`[a-z0-9._:-]+`).
- No legacy route-owned fallback UI widgets are used.
- Route-level interactions are wired (`action`, `submit`, `change` where applicable).

## Evidence to capture

- Route path
- Date/time of validation run
- Data shape used (normal, empty, error)
- Reconnect behavior notes
- Remaining known gaps
