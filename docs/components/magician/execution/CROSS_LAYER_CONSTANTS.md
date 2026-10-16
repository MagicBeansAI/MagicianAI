# Cross-Layer Constants Reference

These values live only in the magician execution crate; there is no Magicutor
extension twin to keep in lockstep.

| Constant | Location | Value |
|----------|----------|-------|
| `PLAN_SCROLL_DEFAULT_PX` | `execution/constants.rs` | 400 px |
| `TEST_AUTOMATION_ATTRIBUTES` | `execution/constants.rs` | `data-testid`, `data-test-id`, `data-cy`, `data-test` |
| `STABILITY_SNAPSHOTS_REQUIRED` | `verified_executor/constants.rs` | 2 |
| `STABILITY_SNAPSHOT_INTERVAL_MS` | `verified_executor/constants.rs` | 50 ms |
| `STABILITY_MAX_WAIT_MS` | `verified_executor/constants.rs` | 2000 ms |

Paths are under `magician/src/magician_v2/`. Plan-level scroll is
intentionally 400 px.
