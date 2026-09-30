Sync meeting takeaways into this package.

Call `meetings_data.read_takeaways` exactly once, forwarding `thread_id` only
when it was supplied. Clamp `limit` to 20.

Project one `meeting_takeaway` per returned row: `takeaway_key` <- `key`;
`thread_id` <- `thread_id`; `title` <- `title`; `meeting_date` <- `date`;
`summary` <- `summary`; `decisions` <- the returned `decisions` list rendered
as one markdown bullet list, or null when the list is empty; `action_items` <-
the same rendering of `action_items`; `updated_at` <- `updated_at`; and
`synced_at` <- current UTC.

`retention_max_entries` is the writer's own bound. An older meeting missing
from the page is retention, not a read failure: never report it as an error and
never retry with a wider filter to look for it.
