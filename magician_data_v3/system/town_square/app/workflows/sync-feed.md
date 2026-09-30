Refresh nothing from outside; this workflow only re-stamps what the package
already holds so a surface poll has a defined result.

This package OWNS the feed — there is no host store to re-read. Set
`synced_at` to current UTC on at most `limit` (clamped to 20) of the newest
`post` rows and project them. Never invent a post, never alter a `body`,
`author_id`, `created_at` or `parent_id`, and never delete a row.
