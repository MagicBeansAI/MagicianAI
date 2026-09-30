Build one bounded research-plan projection for the supplied topic. Use
`time_math__date_range` only to normalize the explicit date range, then use
`research_outline__normalize` only to normalize the proposed outline. Apply
the private `plan-brief` procedure. Use
`research-planner-worker__agent_as_tool` only with its declared typed request
and result. Browser work is limited to the four declared singleton leaves in
the current run-owned isolated session: `browser__snapshot`,
`browser__navigate`, `browser__scroll`, and `browser__click`. Navigate only to
the reviewed `https://example.com` origin. Scroll or click only with an opaque
reference from a fresh same-session snapshot, and snapshot again after an
action invalidates the observation. Do not delegate, type, pass browser/agent
IDs, selectors, scripts, files, downloads, or raw session controls, mutate
another installation, directly write memory, or call an undeclared action.
