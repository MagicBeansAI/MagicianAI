# Changelog - Unified UI

## [Unreleased]

### 2026-10-16 — 0.1.44

- `/coding-runs` is a reserved first-party route root, so an app manifest can no longer declare a route that poses as the Work in progress page.

### 2026-10-15 — 0.1.43

- `/coding-runs` is "Work in progress": coding runs, crews and Work tasks with Follow; notch dots follow the same items.
- An archived task's chat card settles as "Archived" instead of reading "Running".
- Optional Clarity app recording; public landing pages with a GitHub link and site metadata.
- Super god mode (debug): Settings and the VibeDev ⚡ toggle start or stop it; a red banner with Turn off shows while it is on.

### 2026-10-15 — 0.1.42

- Server-paged scheduling and delivery follow-through; the CLI Agent drawer matches the shared shell dimensions.

### 2026-10-12 — 0.1.41

- God mode: a Settings panel sets the default (with confirmation) and can forget stored approvals; the chat orb toggles it per chat and glows red while on. Approval prompts offer allow once / for this session / always.
- Observe shows Join, Listen, Ignore and Cancel for WhatsApp calls, with a toggle; Settings lists the WhatsApp and Meet realtime engines.
- Work's /budget menu adds a +24 hours grant.

### 2026-10-12 — 0.1.40

- New shell: an icon left rail for Apps, LLM, Memory, Notes, Quality, Settings and account; the top bar keeps Chat, Work, Observe and Today.
- Chat and VibeDev composers share a Talk / Code switch; VibeDev gains an empty state with starter prompts and recent runs.
- Chat has a collapsible history rail; VibeDev sessions open in `/vibe` (old links redirect).
- Archived chats, tasks, VibeDev runs and projects can be browsed read-only, paged by the server.
- VibeDev app options load only the tabs you open and keep selections (first Tools load about 9x faster).
- Fixed task card menus while scrolling, notes drawer overlap and the engine picker size.

---

Older entries: `docs/archive/changelogs/unified-ui.md`
