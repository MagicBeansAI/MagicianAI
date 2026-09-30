# Decision packs

Versioned, vendor-neutral question packs for `magician-decision`.
Layout: `<pack_id>/<version>.json`.

## `tool_action_judge`

The shared action rail uses one pack for every authorized tool: `next_action`
selects a complete call, then `evidence_sufficient` and `action_applicable`
review that exact call. Decision Engine constructs the candidates from schemas,
current evidence and validated continuation plans. Missing content or inadequate
confidence requests the selected generative planner.

Thresholds belong to `decision-engine.yaml`, keyed per model. Packs contain
question instructions and contrastive criteria, with no provider-specific fields
or execution authority. Browser, Android and CUA have no separate judge packs.

See [the living contract](../../../docs/components/magician/structured-decision.md).
