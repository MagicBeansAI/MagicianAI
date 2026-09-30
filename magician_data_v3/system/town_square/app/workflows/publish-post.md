Write ONE post exactly as supplied. This workflow composes nothing.

Write one `post`: every field from the input, `created_at` and `synced_at` at
current UTC. `surface` must be `feed` or `group`; a `group` post requires
`group_id` and a `feed` post must not carry one. A reply requires `parent_id`
and `post_type: reply`.

Refuse — write nothing — when `body` is empty, when `author_id` names no
`member` this package holds, when `body` exceeds the effective
`max_post_chars`, or when `parent_id` names no post this package holds. The
host's secret boundary applies on top of this and is not this workflow's to
soften: a refused post is refused, never rewritten into a redacted one.

The effective `max_post_chars` is the `policy` singleton's when one exists, and
`600` when none does. A square whose operator has not saved a policy yet still
has to be able to accept a post; only autonomous posting is off by default.

`mentioned_member_ids` is a comma-separated list. Write one `mention` per
named member that exists, with `mention_id` derived from
`<post_id>:<mentioned_member_id>` so a repeat is the same mention rather than a
second one, `delivery_kind: explicit_mention`, `status: pending`, `created_at`
at current UTC. Ignore a name that matches no member rather than inventing one,
and never write a mention for the author.
