This action runs the reviewed `recipes/sync-queue.json` reconciliation recipe.
It uses no model to choose tools, transform records, or decide completion.

Sync one bounded page of learning candidates into this console's queue.

Call the `internal_data` tool's `list_learning_candidates` action exactly
once. Pass the caller's `state` filter when one was supplied, and keep
`limit` at or below 25, defaulting to 25. The recipe forwards only the declared
`limit` and `state` input fields. Do not call any other `internal_data` action, do not
pass a principal or workspace (the runtime scope is executor-owned), and do
not retry with different filters if the page comes back empty — an empty
queue is a valid result.

Then project one `learning_candidate` record per returned candidate:

- `candidate_id`: the core candidate `id`, copied verbatim.
- `candidate_type`, `state`, `title`, `summary`, `risk_level`,
  `source_agent_id`, `review_required`, `created_at`, `updated_at`: copied
  from the returned candidate.
- `synced_at`: the current UTC time.

Do not editorialize summaries, do not invent fields the read did not return,
and do not create records for candidates you did not read. If the read
fails, finish with no records and state the failure plainly.

Match existing app records by `candidate_id`, retain their record IDs, and
update only the fields listed above with optimistic record revisions. Keep
records outside this page. A nullable `source_agent_id` remains null. New
records use stable IDs, and the complete page is committed atomically through
the Apps terminal transaction owner. An empty page completes successfully
without a mutation receipt.
