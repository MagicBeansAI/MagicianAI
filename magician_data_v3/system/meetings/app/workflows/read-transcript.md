Read one bounded page of a meeting thread's transcript.

Call `meetings_data.read_thread` exactly once with the supplied `thread_id`,
forwarding `session_id` and `before_message_id` only when they were supplied.
Clamp `limit` to 20.

Project one `transcript_line` per returned message: `message_id` <-
`message_id`; `thread_id` <- the result's `thread_id`; `session_id` <- the
result's `session_id`; `speaker` <- `speaker`; `line_text` <- `text`;
`transcript` <- `transcript`; `line_at` <- `created_at`; and `synced_at` <-
current UTC. Messages arrive oldest-first within the page — preserve that
order.

`skipped_non_text` counts runtime rows the host declined to project. Do not
attempt to recover them and do not treat their absence as a gap in what was
said. `next_before_message_id`, when present, is the only admissible cursor for
the next (older) page; an unknown cursor is refused by the host rather than
restarting at the newest page, so never retry a refused cursor with a
different one.
