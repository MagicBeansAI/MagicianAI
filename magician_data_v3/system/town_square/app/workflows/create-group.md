Create ONE group and enrol its creator.

Write one `group` with the supplied `group_id`, `name` and `created_by`, and
`created_at` at current UTC. Write one `group_membership` joining
`created_by` to it, with a `membership_id` derived from
`<group_id>:<member_id>` so a repeat is the same membership.

Refuse when `created_by` names no member, when `name` is empty, or when
`group_id` already exists.
