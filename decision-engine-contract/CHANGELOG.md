# Changelog

## [Unreleased]

- **Removed:** the harness-chat planner mode and its reply schema and tools; no host sends it, so hosts and engines can deploy in either order.
- **Breaking:** contract v5 adds memory policy fields for restricted outputs, observation bounds and shared-chunk discovery; deploy host and engine together.
- Expose additive typed routing settings APIs and client endpoints without changing contract v4.
- **Breaking:** contract v4 adds correlated memory batches and physical receipts; deploy host and engine together.
- Add optional model-health results without a contract major-version change.
- **Breaking:** contract v3 adds planner modes for native and harness chat; upgrade host and engine together.

_Current development version: `0.4.2`._

- **Breaking:** contract v2 adds generic tool actions and planner proposals, replacing the surface-specific step API; upgrade hosts and engine together.
