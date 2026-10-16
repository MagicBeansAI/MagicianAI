Sync one bounded page of the scope's meeting threads.

Call `meetings_data.list_threads` exactly once. Forward only supplied `status`,
`text` and `after_thread_id` values. Clamp `limit` to 20. Never pass
principal/workspace, and never widen or retry an empty or `scan_truncated`
page.

Project one `meeting_thread` per returned row: `thread_id` <- `thread_id`;
`session_id` <- `session_id`; `title` <- `title`; `agent_id` <- `agent_id`;
`status` <- `status`; `created_at` <- `created_at`; `updated_at` <-
`updated_at`; and `synced_at` <- current UTC. The rows are already
newest-first over a stable ordering — preserve it rather than re-sorting.

`next_cursor`, when present, is the only admissible `after_thread_id` for the
following page. Do not synthesize one from a thread id you merely saw.
