# History Native Route

The `/history` route renders its stable product UI directly in Svelte instead
of building a `MuijComponent[]` tree for `MuijRenderer`.

The native route owns the agent picker, refresh action, episode pagination,
episode cards, selected-episode detail drawer, and memory-update cards. Episode
pagination stays server-side through the agent episodes endpoint using
`limit`/`offset`, and the selected page size is reflected in the route query.

Agent labels include a compact role label when available so the picker is easier
to scan across similarly named agents. Episode selection opens a right-side
detail drawer and resets when paging, changing agents, or refreshing into a page
that no longer contains the selected episode.

MUIJ remains reserved for generated, published, replayable, or live agent-owned
surfaces. The history route is a stable hand-authored product page and should
continue using native Svelte controls for theming, spacing, responsiveness, and
page-specific interactions.
