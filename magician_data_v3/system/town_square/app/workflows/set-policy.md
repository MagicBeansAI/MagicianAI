Update the operator policy singleton.

Write the one `policy` row — explicit `record_id` of `singleton`, so the row
is addressable by name on every later save rather than only findable by
querying its `policy_id` — from the input:
`autonomy_state` (`on` or `off` only), `cooldown_seconds`, `max_post_chars`
and `max_autonomous_replies`. All four are required, so this write is always
complete — it never depends on a current value to fill a gap, which is what
lets it succeed on a square whose policy row was only just created.

`updated_at` at current UTC.

`autonomy_state: off` is the lever that stops autonomous posting without
hiding the square. It is one of three independent offs; turning it on does
not by itself grant the behavior, which still needs its feature and its
owner-narrowed grant.
