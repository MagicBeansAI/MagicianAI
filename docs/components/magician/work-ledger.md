# Work Ledger ("WEG-for-work")

A durable, loop-reachable record of prior work — one curated `work_outcome` per
terminal agentic run — built on the WEG evidence substrate and distilled into
the already-injected program-state + memory-tier channels.

Design + plan:
- `docs/archive/plans/2026-07-01-work-ledger-weg-for-work-design.md`
- `docs/archive/plans/2026-07-01-work-ledger-implementation-plan.md`

## Layers

1. **Ledger** — the WEG evidence store (`magician/src/magician_v2/evidence/`),
   `producer = "work_outcome"`. One record per run, run-grained id
   `evd:run:{root_execution_id}`, structured outcome fields under
   `EvidenceRecord.metadata` (`serde_json::Value`, `#[serde(default)]`).
   `EvidenceRecord::work_outcome_id(root)` and `from_work_outcome` plus a
   `producer_spec` entry; ledger records bypass cross-run compaction merge
   (`producer == "work_outcome"`). Inherits dedup/lifecycle/views/claims/
   dashboard/CLIs from the substrate unchanged.
2. **Distill** — a pure `work_outcome_from_summary` projector
   (`execution_summary.rs`) plus fail-soft `spawn_work_ledger_write`
   (root-gated via `task_execution_updates_apply_to_root`) writes exactly one
   `work_outcome` per terminal ROOT run (idempotent upsert by `evd:run:{root}`),
   covering yield dispositions `Completed`/`PartialSuccess`/`Failed`/
   `RetryTransient` and the non-yield terminal arms.

   For harness agents, `LearningProgramStateBridge`
   (`learning/work_ledger_program_state.rs`) maps open-loops/next-actions into
   a whitelist-only `ProgramStateUpdate` (`open_loops`/`next_action_hints`/
   `last_run_summary`), routed through the existing auto-apply gate
   (harness-self, revertible, owner-notified). Non-harness agents skip.
   Standing facts continue to distill into memory tiers via episode append →
   `upsert_by_name`.

   Synthetic yield builders in `execution/agentic/decision.rs` mine the run's
   last text artifact (`mine_substantive_next_steps`) for "Next Steps"/
   "Recommendations"/"Open Items" rather than boilerplate. They skip
   tool-capture artifacts (`text/*` only; `artifact_type`/`name` guards against
   `*inline_result*` / `<tool>_inline_*`) and fall back to boilerplate only
   for a genuine no-output degenerate loop.
3. **Retrieval** — always-on: ranked memory-tier + program-state injection at
   run start. On-demand: the `magician_work_ledger` query tool
   (`execute_work_ledger` in `harness_provider.rs`, pack-def
   `execution/embedded_pack_defs/magician_work_ledger.yaml`), granted to C-suite
   harness agents, read-only, scope-isolated, backed by `load_native_evidence`
   filtered to `producer=="work_outcome"` with `agent_id`/`kind`/`since`/`limit`.
   Harness read providers late-bind onto `ScopedCapabilityResolver` per scope
   (register-if-absent). The global file sandbox is augmented at boot with the
   runtime scopes root (`FileSandboxConfig::augment_with_scopes_root`) so
   harness `read_file` can reach `durable_artifacts/` under the runtime root.
4. **Repo projection** — one-way `docs/worklog/` export via
   `render_agent_worklog_markdown` (`evidence/worklog.rs`) and the
   `worklog-export` CLI in `magician-bin/src/main.rs`. `docs/` is not indexed;
   agents reach the ledger via the tool/tiers, not the doc. The export path is
   not a tracked directory.

## Known limits

- **Program-state write concurrency:** `apply_update` → `write_runtime_state`
  is an unlocked read-modify-write (the revert path uses CAS; apply does not),
  so the per-run distill producer and the `update_program_state` tool can
  last-writer-wins-clobber `open_loops`.
- **Single focus area:** `WorkOutcomeInput` has no focus-area discriminator, so
  a multi-program harness agent's bookkeeping always lands on focus-area #1.
