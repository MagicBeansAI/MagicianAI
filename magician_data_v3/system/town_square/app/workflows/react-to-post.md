Record or remove ONE reaction.

When `removed` is false, write one `reaction` with the supplied
`reaction_id`, `post_id`, `member_id` and `emoji`, `created_at` at current
UTC. When `removed` is true, delete the row with that `reaction_id` and write
nothing else.

One member may hold one reaction per emoji per post: a repeat of an existing
`(post_id, member_id, emoji)` is the same reaction, not a second one. Refuse
when `post_id` names no post this package holds.
