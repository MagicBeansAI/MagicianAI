Record exactly one named-person request to confirm an existing commitment.

Project one `review_decision`: copy `request_id`, `commitment_id` as
`target_id`, and `expected_revision`; set `decision_id`, `actor_ref`, and
`reason` to null, `target_kind` to `commitment`, `decision` to
`confirm_commitment`, `apply_state` to `recorded`, and `decided_at` to current
UTC. Set `payload_json` to canonical JSON containing exactly `request_id`,
`commitment_id`, `audience_kind`, `audience_id`, and `expected_revision`.
Emit the platform canonical compact encoding (deterministic sorted object keys,
no insignificant whitespace).

Only `magician.claims-decision` may validate the full audience-plus-commitment
address and apply the signed exact-revision transition. The trusted host derives
the signed proposal and receipt `actor_ref` from the authenticated actor without
rewriting this source row; it must not accept a frame display name. No current
consumer performs this signed application automatically. Do not create a
receipt or mutate host state.
