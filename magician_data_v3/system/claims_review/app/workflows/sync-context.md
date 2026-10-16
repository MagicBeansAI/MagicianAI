Sync bounded correction, entity, and commitment context into this package.

Call each action at most once, with a limit no greater than 20:

- `evidence_data.list_commitments`, forwarding the required `audience_kind`
  and `audience_id`, mapping supplied `commitment_status` to the tool's
  `status`, and forwarding supplied `after_term_id`. Omit the tool status when
  `commitment_status` is absent; never invent a second `status` input.
- `evidence_data.list_evidence_records`, forwarding the required explicit
  `agent_id` plus supplied `after_record_id`.
- `evidence_data.list_entities`, forwarding the same explicit `agent_id` plus
  supplied `after_entity_key`. Never derive or union agent ids.

Never pass principal/workspace, never retry with broader filters, and preserve
any source partial-page or `scan_truncated` truth in the workflow result.
Map commitments as: `term_id` <- `commitment_id`; `status` <- `status`;
`direction` <- `direction`; `term_text` <- `terms`; audience fields <-
`audience.kind` and `audience.id`; `named_person` <- `confirmed_by`;
`source_claim_id` <- null; `expected_revision` <- `revision`; `created_at` <-
`recorded_at`; and `synced_at` <- current UTC.

Map evidence as: `record_id` <- `evidence_id`; `state` <- `status`; `summary`
<- `summary`; `created_at` <- `first_seen_at`; and `synced_at` <- current UTC.
The package schema intentionally has no claim/source/provenance fields and the
host projection omits raw source references and metadata; never infer or
reconstruct them.
Map entities as: `entity_key` <- `entity_key`; `entity_kind` <- `entity_type`;
`label` <- `canonical_name`; and `synced_at` <- current UTC. The package entity
schema has no free-form summary field to reconstruct from omitted evidence.

Reuse the source's affirmatively-sensitive suppression: absent records stay
absent. Deleted tombstones also stay absent. This package does not infer, join,
or reconstruct suppressed or deleted context.
