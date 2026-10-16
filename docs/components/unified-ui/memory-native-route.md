# Memory Native Route

The top-level `/memory` route renders its product UI directly in Svelte. It
does not build Overview or User Memory as `MuijComponent[]` or mount them
through `MuijRenderer`.

## Native Sections

The native route owns:

- A search box above the tabs. It posts to
  `POST /api/magician/v2/memory/search` for the selected agent and shows the
  matches `search_memory` would return, with scope, tier and semantic type. The
  lookup does not count as a use.
- The header, refresh controls, agent counts and tab strip.
- Memory synthesis task selection, themed checkboxes, select-all, result
  tables and error state.
- The agent list, selected-agent details, tier tables, tier deep links and
  environment-knowledge expansion.
- User Memory promoted-memory and skipped-item tables, removal controls, and
  server-side pagination through `/api/magician/v2/memory/user-knowledge`
  `promotions_limit`/`promotions_offset` and `skips_limit`/`skips_offset`.
- The Preferences subsection, which pages confirmable user-knowledge entries
  (`preferences`, `research_findings`) through `GET /memory/entries` at 20 rows
  per page. The request always sends that tier allowlist, so accounts and other
  rows cannot appear. `ServerPager` shows only past one page. Confirm and scope
  edits stay on the same list.

The Observability tab mounts `MemoryObservabilityDashboard` lazily, only when
selected.

When shadow memory-effect records justify Canary or Enforced, `/memory` loads
`GET /memory/effect-review` and shows a banner above the tab strip; Stay /
Switch posts to the same path. The same choice also reaches the Attention bar as
a `memory_effect_review` HITL request. Collect-only reviews show no banner.

## The entry contract lives in its own module

`MemoryEntry`, `MemoryScopeDraft` and `MemoryTrust` are declared in
`src/lib/memory/memoryEntry.ts`, not on `MemoryEntryCard.svelte` (a Svelte 5
instance script cannot re-export a type). The card, the `/memory` page and
`preferenceEntries.ts` all import from it.

`conflict` is part of that shape. When a stated preference disagrees with
observed behaviour, the entry carries a sentence describing it, and the card
renders it at full opacity, not the 0.8 of other secondary lines. The card then
offers Keep and Edit. Edit focuses topics so the owner can fix prose or scope on
the card. Keep keeps the written memory (confirming an inferred one, otherwise
`POST …/keep-conflict`, which 204s without changing trust).
`conflict_agree`/`conflict_disagree` render as `N/M opened anyway` only when
both arrive in the list JSON (`conflict_agree` / `conflict_disagree` on
`GET /memory/entries`).

## Routing Behavior

The route keeps the tab query parameter:

- `?tab=overview` loads the registry and selected-agent memory.
- `?tab=user-memory` selects User Memory and loads promoted memories, skipped
  entries and the Preferences page as paged server slices, never the whole
  store. Tab changes write the query, so the Preferences pager can be linked.
- `?tab=preferences` mounts `PreferencesTab` (`src/lib/preferences/`): the
  owner's preferences exactly as preference checks see them, grouped as rules,
  goals and tastes with strength, source and last check use; add, edit,
  confirm, retire and forget (forget asks first, since it also erases the
  wording from history), plus each preference's history. Edits send the
  revision on screen; a changed preference reloads and says so. A preference
  stored as `key: {json}` shows as a title and its fields
  (`presentStatement`, keeping acronyms such as UI and API uppercase, fields
  sorted by key as on iOS and Android); editing, checks and history keep the
  stored wording. Buttons and headings use the memory page's tokens (`.btn`
  language, display-font headings). See
  [preference alignment](../magician/preference-alignment.md#owner-view-and-edits).
- `?tab=observability` mounts the observability dashboard on demand.
