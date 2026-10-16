Sync one bounded page of transcript claims (pending by default, or the explicitly requested status) and, optionally, one exact
claim detail into this package.

Call `evidence_data.list_pending_claims` exactly once. Forward only supplied
`status`, `audience_kind`, `audience_id`, `text`, and `after_claim_id` values.
Clamp `limit` to 20. Never pass principal/workspace and never widen or retry an
empty or `scan_truncated` page.

Project one `claim_summary` per returned claim using this exact mapping:
`claim_id` <- `claim_id`; `status` <- `status`; `claim_text` <-
`stated_text`; `speaker_name` <- `speaker`; `audience_kind` and `audience_id`
<- the two fields of `audience_ref`; `expected_revision` <- `revision`;
`created_at` <- `extracted_at`; `context_excerpt` <- null; and `synced_at` <-
current UTC. An absent `audience_ref` projects null `audience_kind` and `audience_id`: the
statement remains visible without a guessed relationship. Relationship-bound
commitment actions remain unavailable until a real binding exists. Preserve source wording and revision exactly.

If `claim_id` was supplied, call `evidence_data.read_claim` exactly once for
that id and project one `claim_detail`: `claim_id` <- `claim.claim_id`;
`transcript_id` <- `utterance_context.transcript_key`; `utterance_id` <-
`utterance_context.segment_key`; `speaker_id` and `speaker_name` <-
`utterance_context.speaker`; `claim_text` <- `utterance_context.stated_text`;
`audience_kind` and `audience_id` <- `claim.audience_ref`; `extractor_id` <-
`claim.extracted_by`; `expected_revision` <- `claim.revision`; `created_at` <-
`claim.extracted_at`; `prior_context` and `following_context` <- null; and
`synced_at` <- current UTC. Do not read details for every list row. A missing,
unfiled, or refused detail creates no detail record. Neither action writes host
evidence.

Project the selected detail into `claim_summary` too, using its current status
and revision, even when it is outside the list page. If it overlaps the page,
write that summary only once using the later detail read.

In the same transaction upsert the singleton `claim_sync_page` (`page_id: current`)
with the supplied `after_claim_id` or null, source `next_cursor` or null, a JSON
array of this source page's exact claim IDs, and `synced_at`. Preserve an empty
page and its continuation; never infer a cursor from the local projection table.
