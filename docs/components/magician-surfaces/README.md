# Magician Surfaces

Provider-replay evaluation fixtures use the current GPT-6 Luna route while the
router retains GPT-5.6 Luna as an explicit fallback.

Near-free surface modules extracted from the magician monolith (thinking maps
and evals; the rest stay behind lib-side seams):

- `thinking_map` (~10.0k) — store, reducer, interpreter, coordinator. REST
  is `magician-api` `thinking_maps_api`.
- `evals` (~6.9k) — Makefile-derived eval lanes, run store, runner.
  Provider-replay reports group profiles by wire family
  (`provider_family`); `provider: xai` reports as `xai_responses`; `provider: sarvam` reports as `sarvam`.

Lifecycle Evals retain bounded run options and immutable per-run report links,
including failed outcomes. Standalone model calls remain unknown in task-ledger
costs; evaluator reports carry their own estimates. See the
[run/history contract](../magician/eval-lanes.md#memory-lifecycle-runs).

`counterparties/` is a three-line re-export; the store, consumers, and types
live in the magician lib. Progress-channel plumbing was never extracted: it
is `magician_v2::progress_channel_seam`.

## Lib-side seams

- `magician_v2::thinking_map_models`, `thinking_map_operations`,
  `tutor_map_context` — the crate re-exports these as
  `thinking_map::{models, operations, tutor_context}`.
- `magician_v2::progress_channel_seam` — ProgressChannel trait, router,
  surface routing, and types; chat consumes the seam in-lib.
- `magician_v2::counterparty_types` — identity/trust vocabulary
  (`CounterpartyRef`, `InboundIdentification`, `TrustedSignal`); the crate
  re-exports `counterparty_{consumers,store,types}`.

## Startup repair of ambiguous thinking-map history

`thinking_map::replay` resolves a duplicate group with more than one
replay-valid terminal branch by the served snapshot when exactly one branch
produced it; otherwise the sweep quarantines the map once
(`repair-quarantine.json`, counted as `maps_quarantined_ambiguous`) instead of
logging an ERROR on every boot. Details:
`docs/components/magician/live-thinking-map.md`.
