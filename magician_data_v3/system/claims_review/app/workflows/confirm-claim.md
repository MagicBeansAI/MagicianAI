Record exactly one owner request to confirm a pending claim.

Project one `review_decision`: copy `request_id`, `claim_id` as `target_id`,
`expected_revision`, and `reason`; set `decision_id` and `actor_ref` to null,
`target_kind` to `claim`, `decision` to `confirm_claim`, `apply_state` to
`recorded`, and `decided_at` to current UTC. Set `payload_json` to canonical
JSON containing exactly `request_id`, `claim_id`, `expected_revision`, and
`reason`. Emit the platform canonical compact encoding (deterministic sorted
object keys, no insignificant whitespace).

This local ledger is not the claim transition. Only the reviewed
`magician.claims-decision` seam may wrap it in an owner-signed,
destination-head-bound envelope and call the canonical transition. No current
consumer performs that conversion automatically. A trusted application host
must derive the signed proposal and receipt `actor_ref` from the authenticated
actor without rewriting this source row, never from frame text. Never mark this
row applied, create a receipt, self-confirm for an extractor, or change the
expected revision.
