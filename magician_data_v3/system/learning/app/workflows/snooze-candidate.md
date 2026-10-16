Record one snooze decision in this console's review ledger.

Take the caller's `candidate_id`, optional `candidate_type`, and `reason`.
Project exactly one `review_decision` record:

- `candidate_id`: copied verbatim from the input.
- `candidate_type`: copied when supplied, otherwise null.
- `decision`: `snooze`.
- `reason`: copied verbatim from the input.
- `decided_at`: the current UTC time.

This action records the owner's decision in the durable ledger this package
owns; that record is designed to be the exact source head a
`magician.learning-decision` contribution proposes: the reviewed
learning-decision contribution port (plan 2.5 apply path) carries the
decision to the core `LearningStore` behind an owner-signed decision
envelope, where snooze records its deferral in the core decision log
without changing the candidate's state. The package manifest does not
declare contribution ports yet, so that feed is designed-for rather than
wired. Do not create additional records and do not restate the candidate's
content beyond the fields above.
