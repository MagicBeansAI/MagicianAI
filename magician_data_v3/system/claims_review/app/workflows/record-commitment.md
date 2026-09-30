Record exactly one request to create an UNCONFIRMED owner commitment.

Project one `review_decision`: copy `request_id`, `claim_id` as `target_id`, and
`expected_revision`; set `decision_id`, `actor_ref`, and `reason` to null,
`target_kind` to `claim`, `decision` to `record_commitment`, `apply_state` to
`recorded`, and `decided_at` to current UTC. Set `payload_json` to canonical
JSON containing exactly `request_id`, `claim_id`, and `expected_revision`.
Emit the platform canonical compact encoding (deterministic sorted object keys,
no insignificant whitespace).

Only `magician.claims-decision` may apply this claim-targeted payload. The
destination derives an UNCONFIRMED commitment from the already confirmed
claim; no model or package workflow supplies a replacement term or audience,
or creates a binding/confirmed term. No current consumer performs the signed
application automatically. Do not create a receipt or mutate the host register.
