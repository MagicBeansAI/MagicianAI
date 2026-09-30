# Memory Native Route

The top-level `/memory` route renders its owned product UI directly in Svelte.
It does not build Overview or User Memory as `MuijComponent[]` or mount those
sections through `MuijRenderer`.

## Native Sections

The native route owns:

- A search box above the tabs. It posts to `POST /api/magician/v2/memory/search` for the selected agent and shows the same matches `search_memory` would return, including scope, tier, and semantic type. The lookup does not count as a use.
- The page header, refresh controls, agent counts, and tab strip.
- Memory synthesis task selection, themed checkboxes, select-all, result tables,
  and synthesis error state.
- The agent list, selected-agent details, tier tables, tier deep links, and
  environment-knowledge expansion controls.
- User Memory promoted-memory and skipped-item tables, removal controls, and
  server-side pagination through `/api/magician/v2/memory/user-knowledge`
  `promotions_limit`/`promotions_offset` and `skips_limit`/`skips_offset`
  query parameters.
- The Preferences subsection pages confirmable user-knowledge entries
  (`preferences`, `research_findings`) through `GET /memory/entries` at 20
  rows per page. The request always sends that tier allowlist so accounts
  and other store rows cannot appear as Preferences. `ServerPager` is shown
  only when the confirmable total exceeds one page. Confirm and scope edits
  stay on the same paged list.

The Observability tab still mounts `MemoryObservabilityDashboard` lazily only
when the tab is selected, preserving the existing loading behavior for the
heavier analytics surface.

When shadow memory-effect records justify Canary or Enforced, `/memory`
loads `GET /memory/effect-review` and shows a banner above the tab strip.
Stay / Switch posts to the same path. The same choice also lands on the
Attention bar as a `memory_effect_review` HITL request. Collect-only
reviews do not show the banner.

## The entry contract lives in its own module

`MemoryEntry`, `MemoryScopeDraft` and `MemoryTrust` are declared in
`src/lib/memory/memoryEntry.ts`, not on `MemoryEntryCard.svelte`. A Svelte 5
instance script cannot re-export a type, so every consumer that wanted the
shape — the card, the `/memory` page, `preferenceEntries.ts` — was importing it
from a component. The module is the shape's home; the card is one consumer of
it.

`conflict` is part of that shape. When a stated preference disagrees with
observed behaviour, the entry carries the sentence describing the disagreement
and the card renders it at full opacity rather than at the 0.8 the other
secondary lines use. Surfacing a conflict is the point of recording one — the
card must not file it alongside the scope label as another muted detail.
When that line is present the card offers Keep and Edit — Edit focuses topics so
the owner can fix prose or scope on the same card, Keep keeps the written memory
(confirming inferred, otherwise `POST …/keep-conflict` which 204s without
changing trust), and `conflict_agree`/`conflict_disagree` render as
`N/M opened anyway` only when both arrive from the list JSON
(`conflict_agree` / `conflict_disagree` on `GET /memory/entries`).

## Routing Behavior

The route keeps the existing tab query parameter behavior:

- `?tab=overview` loads the registry and selected-agent memory.
- `?tab=user-memory` selects User Memory and loads that tab's paged slices.
  Tab changes write the same query so the Preferences pager can be linked.
  The tab requests promoted memories, skipped entries, and the confirmable
  Preferences page as paged server slices instead of loading the whole
  knowledge store into the browser.
- `?tab=observability` mounts the observability dashboard only on demand.
