Sync the owner's upcoming calendar meetings.

Call `meetings_data.upcoming_meetings` exactly once with no arguments. The read
serves a shared 60-second cache and never forces a refresh; do not call it more
than once per run to try to warm it.

Project one `upcoming_meeting` per returned event: `event_id` <- `event_id`;
`title` <- `title`; `starts_at` <- `start`; `ends_at` <- `end`; `meet_url` <-
`meet_url`; `live_now` <- `live_now`; `account` <- `account`; and `synced_at`
<- current UTC. Keep at most `limit` rows when one was supplied, clamped to 20,
preserving the returned chronological order. An event without an `event_id`
creates no record.

`errors` lists per-account failures. Leave the accounts that worked projected;
never blank the whole set because one account failed, and never invent an event
to stand in for a failed account.
