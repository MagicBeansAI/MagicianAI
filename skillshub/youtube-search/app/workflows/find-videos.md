Find recent YouTube coverage for a topic using the `youtube-search` tool.

Call the `youtube-search` tool once with the caller's `topic` as `query` and a
bounded `limit` (default 10). Do not widen the run into general research; this
workflow exists to exercise the reviewed ToolSkill binding, not to replace
`content_search`.

Finish by projecting exactly one `video_coverage` record:

- `topic`: the caller's topic, unchanged.
- `best_video`: the title and url of the single most relevant result. If the
  tool returned an error object or no items, say that plainly in `best_video`
  instead of inventing a video.
- `captured_at`: the current UTC time.

Do not create additional records and do not call any other tool.
