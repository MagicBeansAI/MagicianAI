Record exactly one owner request to reject a pending claim.

Project one `review_decision`: copy `request_id`, `claim_id` as `target_id`,
`expected_revision`, and `reason`; set `decision_id` and `actor_ref` to null,
`target_kind` to `claim`, `decision` to `reject_claim`, `apply_state` to
`recorded`, and `decided_at` to current UTC. Set `payload_json` to canonical
JSON containing exactly `request_id`, `claim_id`, `expected_revision`, and
`reason`. Emit the platform canonical compact encoding (deterministic sorted
object keys, no insignificant whitespace).

This local ledger is not the claim transition. Only the reviewed
`magician.claims-decision` seam may apply the owner-signed exact-revision
decision. No current consumer performs that conversion automatically. A
trusted application host must derive the signed proposal and receipt
`actor_ref` from the authenticated actor without rewriting this source row.
Never mark this row applied, create a receipt, or mutate host evidence.
