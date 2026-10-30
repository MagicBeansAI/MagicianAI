# Notes Provider

Notes are Magician's own notes: plain Markdown files Magician reads and writes,
browsed in the web app at `/notes`, kept apart from canonical runtime storage.
There is no notes server, port, hostname or sidecar (SilverBullet is retired).

A new note's file name is its slugged title plus `.md`; a title that already
ends in `.md` / `.markdown` keeps that name (`plan.md`, not `plan-md.md`).

## Note links

Every note link the server produces is the one canonical **relative** link
built by `notes::note_link(path, workspace)`:

```text
/notes?path=<encodeURIComponent(path)>&workspace=<encodeURIComponent(workspace)>
```

`path` is the note's path inside the notes library (what `/notes?path=` opens);
`workspace` is the scope's workspace id (omitted only when empty). The encoder
matches JavaScript's `encodeURIComponent` byte for byte, so the web helper
`noteHref(path, workspace)` (`ui/unified-ui/src/lib/notes/noteLinks.ts`) emits
the identical string. No host, no `file://` path, no notes-server origin: the
link works on whatever origin the owner reached Magician through, and opening
it still goes through sign-in and scope, so a forwarded link grants nothing.

- `open_url` on write results (`NoteRef`, `NoteLocation`), search hits,
  observed notes, task-note index entries and `notes_data` results is this
  link. Attachments (non-Markdown) get `open_url: null`. Task-note index
  records rebuild the link on read, so records written with an older URL never
  hand it out.
- Tool guides (`create_note`, `open_note`, `search_notes`, `notes_data`) tell
  models to copy `open_url` verbatim as a relative link.
- `GET /notes/file` resolves `path` in the library root first and then in the
  fallback folder, so a link to a note a provider fallback wrote still opens.
- Web `/notes` honors `workspace`: a different workspace the session can enter
  is switched to (existing scope switch) before any note is read; one it
  cannot enter shows "This note is in a workspace you're not signed into" and
  fetches nothing. Signed-out visitors go through sign-in and return to the
  same URL. Opening a note inside Notes rewrites the address bar to its
  canonical link (`replaceState`), and **Copy link** copies
  `window.location.origin` + the canonical link.

## Boundary

Notes are projections and captures, never the source of truth for tasks or
internal tasks, execution history, artifact V2 runtime state, memory tiers, or
ledgers and resource authority. Changing provider or root only redirects new
notes; old ones are not deleted but can look lost until the user switches back
or migrates them. An Indexed Audio Note's private index envelope keeps
its capture-time provider root, so playback and deletion still find it.

## Settings

Scoped by `(principal, workspace)` at
`scopes/{principal}/{workspace}/notes/settings.json`, resolved through the
workspace file provider (`local_file`). The browser Settings page is the only
editor (capture on/off included), uses the signed-in scope and shows one
location, the "notes folder"; saving selects it (`default_provider:
silverbullet`) and keeps any stored `local_markdown.root`.

| Setting / input | Behaviour |
| --- | --- |
| `notes_folder.space_path` | Root Magician reads and writes. Default `$MAGICIAN_ROOT_DIR/MagicanNotes/spaces/<principal>/<workspace>` (normally `~/MagicianNotes/MagicanNotes/spaces/<principal>/<workspace>`). |
| `MAGICIAN_NOTES_SPACE` | Relocates the forest (parent of `spaces/`). |
| Provider ids | `local_markdown` (always the fallback) and `silverbullet` — the persisted id of the notes folder, kept because settings, index records, Observe `source_ref`s and search-index keys store it. `sb`, `silverbullet_space`, `notes` and `notes_folder` normalize to it. |
| Legacy keys | Older files name the section `silverbullet` and carry `local_url` / `server_url` / `public_origin`: the section is read as `notes_folder`, the URL keys are ignored (never validated), and the next save writes `notes_folder` only. Nothing moves. |

`resolved` reports `local_markdown_root`, `notes_folder_path` and
`notes_folder_write_safe`.

A `space_path` naming the forest, `spaces/` or `spaces/<principal>` resolves to
(and is saved as) the per-workspace child, even before it exists, so search
never walks siblings. HOME,
`/` and the Magician runtime root are refused; an unsafe path is unavailable and
writes fall back to Local Markdown. Comparison resolves existing symlinks before
parent components. Changing `space_path` moves no files.

## Provider health

- **Reading status never writes.** `GET /notes/providers/status` and
  `status_for_provider` use `directory_is_writable` (`access(2)` `W_OK` on Unix,
  the read-only flag elsewhere); only `unavailable_write_reason` uses
  `writable_probe` (creates and removes files), so a full or read-only
  filesystem reads as writable until a write probes it.
- Status is filesystem facts only (folder present, writable); there is no
  server to probe.

## Provider Registry

| Provider | Id | Role |
| --- | --- | --- |
| Notes (label "Notes") | `silverbullet` | The configured folder, used when configured and writable; the one location users see. |
| Fallback folder | `local_markdown` | Scoped root under the runtime root, created on first write; fallback only, shown as "fallback folder" only while in use. |

An unavailable provider falls back to `local_markdown`. Write responses carry
`requested_provider`, `provider`, `used_fallback`, `fallback_reason`, `path`,
`absolute_path` and `open_url` (the canonical relative link, see
[Note links](#note-links)). Disabled notes reject writes before any root is
created.

## Agent tools

`create_note`, `append_note`, `open_note`, `search_notes`,
`save_selection_to_note`, `publish_task_to_note` build `NotesSettingsStore` from
the workspace on `AgentResources`. All provider writes share one process-wide
lock with HTTP.

- Results report `provider`, `used_fallback`, `fallback_reason`; failures carry
  `retryable` from the HTTP layer's `io::ErrorKind` classification
  (`TaskNotFound` permanent, `Io` not). Read-only tools report
  `notes_read_failed`, never `notes_write_failed`.
- `open_note` maps a path to its canonical link (`open-root` opens the
  folder), searching providers default-first; it takes no write lock and creates nothing. A path
  under no configured provider is `NotFound`.
- `publish_task_to_note` delegates to `publish_task_note` (scope checks; settled
  tasks only), so tool and task card agree.
- Grants live in `definition.agent.yaml`; the seed template gives all six to the
  personal assistant, and existing scopes see new tools only after re-seeding.
- Goal workspaces ([access rules](../../features/goal-workspaces.md#who-can-see-a-workspace)):
  every tool checks the acting agent first. `search_notes` drops hidden
  matches after ranking, before the limit, and reports `withheld_count`;
  `open_note` and the writers answer `notes_permission_denied` before any
  provider call. Apps (`notes_data`) get no workspace content.

### Selection capture

`save_selection_to_note` files a passage from elsewhere with its source; it is
the tool form of `capture_selection` (also used by the extension and overlay).
`append_note` is for the agent's own prose. A capture is an `append_note` with
provenance:

- **Idempotent on `capture_id`** (HTML-comment marker; short, alphanumeric plus
  `-`, `_`, `.`, `:`; surfaces send a UUID). A deliberate second capture needs a
  new id. The tool passes none, since a model-invented id could suppress a real
  capture.
- Dedupe read and append share one lock hold; the read checks every configured
  provider and fails closed (only a missing page means "not captured"; any
  other stat/read error fails the capture).
- Without `target_path`, dedupe also checks yesterday's page (midnight retries)
  and appends to today's; a named `target_path` checks only itself.
- The source line is a Markdown link (angle-bracketed destination, escaped
  title); absent fields are omitted.

## Projection seam

`magician_v2/notes_projection.rs` depends one way on `notes.rs` (registry,
selection, fallback, settings, storage, write lock) and owns:

- capture: `capture_append_plan`, the resend-once answer, dated Inbox names;
- task pages: naming, tags, dates, asset names, timeline bounds (newest 20
  executions, 12 spelled-out steps in standard mode, omissions reported),
  rendering;
- memory promotion: bounded value, fingerprint id, review-gated candidate
  (called by `magician-api`'s promote handler).

`NotesSettingsStore::capture_selection` and `publish_task_note` keep
orchestration (scope checks, retry read, provider writes, index commits) and
call the seam's decision functions.

## Search

Browse/edit routes refuse root escapes, symlinks, and anything not a real
directory or Markdown file; every create, save and delete refreshes the index.

| Route | Behaviour |
| --- | --- |
| `GET /notes/tree` | `list_note_tree`: one directory; folders carry `has_children`. |
| `POST /notes/tree` / `DELETE /notes/tree` | Create (`folder`, `name`) / delete a folder; delete refuses the root and folders holding a symlink. |
| `GET /notes/file` | `read_note_file`; `POST` creates (empty `folder` = root), `PUT` (`path`, `markdown`) replaces, `DELETE` removes. |
| `PATCH /notes/front-matter` | `update_note_front_matter`: `{path, set: {key: "value"}, unset: [key], expected_revision}` changes only those top-level front-matter keys (other lines, order, comments, line endings and the body kept; a note without front matter gets a block). 409 `note_revision_conflict` with `current_revision` when the note no longer matches `expected_revision` (`revision` = `blake3:<hex>` of the content, returned by every note read). |
| `GET /notes/backlinks` | `list_note_backlinks`: `[[Page]]` or Markdown links to a `.md`. |

`search_notes` / `POST /notes/search` match keyword, typo and (with embeddings)
meaning, returning matching lines. Index: one LanceDB table at
`scopes/{principal}/{workspace}/notes/hybrid-index` (path, text, hash,
embedding, stored full-text index), opened once per process; each search checks
out the latest version (no `memory://` copy, no per-query rebuild). Markdown
files are the only copy of note text.

- Changed rows are embedded and merged in the background (one full-text
  refresh); a per-folder watcher merges once an outside edit has been still
  ~1 s (`notes watch refreshed` logs timings); startup skips unchanged notes.
  Note writes (create, save, front-matter, delete) queue the same refresh and
  reply without waiting for the embedder.
- BM25 with LanceDB's automatic typo distance, fused with vector search when the
  model answers in time; if it is down, search stays on the keyword index.
- Only a different embedding model or contract rebuilds the index. When Ollama
  looks down (a search's 200 ms readiness check; a refresh waits out the
  configured health timeout) or an embed fails, the embedded index is kept and
  the next refresh with Ollama embeds just the changed notes.
- Body, title and path match (`Clients/Acme/` answers "Acme"); terms are ANDed
  case-insensitive substrings; title/path hits on every term outrank body-only,
  and the line with most terms leads. An unconfigured provider is refused, not
  searched as empty.
- Walks the same boundary-safe roots as Observe (plus `Workspaces/`, which
  Observe skips), so co-located runtime state (model caches, credentials,
  worktrees) never surfaces. The index folder embeds every note, so no agent's
  file tools, shell or coding box can read it. Scan size, note size,
  concurrent reads, terms, results and snippets are bounded; `scan_truncated`
  and `more_available` flag trimmed results.

Web `/notes`: search toolbar over the notes folder; transcripts under
`Audio Notes/` (voice icon), uploads still in the pending list. It states
truncation and caps and highlights by segmenting lines, never assembling HTML.

## Capture surfaces

`POST /notes/capture-selection` is the one entry point, used by the browser
extension (`magicutor/extension`, selection-only context item, registered at
all three MV3 lifecycle points; calls Magician directly, not via the desktop
host gateway, so it works with the desktop app closed; own scope,
default the web UI's), the desktop contextual-assist `save_to_notes` action
(run directly, not select-then-generate), and `save_selection_to_note`.
`make check-notes-capture-surface` (in `make check-all`) fails if the overlay
action id or extension default scope drift.

## API

```text
GET  /api/magician/v2/notes/settings
PUT  /api/magician/v2/notes/settings
GET  /api/magician/v2/notes/providers/status
POST /api/magician/v2/notes/providers/open-root
POST /api/magician/v2/notes
GET  /api/magician/v2/notes/audio?offset=0&limit=20&q=optional
POST /api/magician/v2/notes/audio
GET  /api/magician/v2/notes/audio/{note_id}
GET  /api/magician/v2/notes/audio/{note_id}/recording
DELETE /api/magician/v2/notes/audio/{note_id}
POST /api/magician/v2/notes/append
POST /api/magician/v2/notes/search
POST /api/magician/v2/notes/capture-selection
POST /api/magician/v2/notes/publish/task/{task_id}
POST /api/magician/v2/notes/publish/tasks/backfill
GET  /api/magician/v2/notes/published-tasks?offset=0&limit=20&q=optional
GET  /api/magician/v2/notes/published-tasks/{task_id}
POST /api/magician/v2/notes/published-tasks/{task_id}/promote-memory
```

Scope comes from the workspace-bound bearer (`resolve_required_scope`); a
payload or query `workspace` is a compatibility assertion, not a selector.

## Audio Notes

Chat dictation (iOS, web; tap or press-and-hold) is ephemeral by default: the
recording is deleted after STT; only the transcript reaches the three-second
cancelable auto-send. **Keep dictation recordings** (Voice settings →
Dictation) opts in (the mic shows a saving disclosure) and archives
independently of auto-send. Ambient Dictation and Thinking Map speech are never
archived.

| Outbox | Contract |
| --- | --- |
| iOS | The stopped `.m4a` is copied off the main actor into a data-protected, backup-excluded outbox: ≤ 50 records, 200 MB total, 24 MB each (reserved multipart space and quarantined files count); multipart bodies stream to file. Background `URLSession` (retries via `earliestBeginDate`; foreground activation also drains) survives relaunch, which accepts one task per valid record and cancels orphan/duplicate tasks. Each row owns retry count, next attempt, failure, destination URL and captured scope. Restored destinations must be credential-free, query-free origins of the configured service (or HTTP loopback in development), else quarantined. Network, 408, 425, 429, 5xx back off; permanent 4xx, exhausted or 30-day-old rows move only that row to **Needs attention** (Retry/Discard, Menu → Audio Notes; no Discard while the background session owns an upload). Interrupted STT recovers audio-only; oversize and full-outbox are rejected visibly; transcripts are client-bounded below the server multipart limit. |
| Web | Opt-in bounded IndexedDB outbox, entered before STT with capture-time scope and stable UUID. A ten-minute transcript lease stops other tabs treating active STT as crashed (expired captures recover audio-only); atomic upload leases prevent duplicate workers. Failures back off or wait under `/notes` for Retry/Discard. Foreground only: stops when the browser dies, resumes when an app page next opens. |

**Durable receipt.** A recording leaves the outbox only after a non-redirected
2xx receipt matches `note_id`, byte count, provider, note path and audio path (a
proxy or login `200` is retried). The backend binds the UUID to byte length and
BLAKE3 digest; same id, different audio is a conflict. RFC 3339 times compare by
instant.

Files land at `Audio Notes/2026-08-01/20-56-24-147-a1b2c3d4111142228333123456789abc.{md,m4a}`
below the Space or the Local Markdown root (default
`scopes/<principal>/<workspace>/notes/space/Audio Notes/`); uploads name a
provider only if one was captured. The page embeds recording and transcript,
tags `audio-note`, `voice-note`, `audio-note/date/YYYY-MM-DD`,
`audio-note/time/HH-MM`, and records `recorded_at`, `recorded_date`,
`recorded_time`, `source_surface`, MIME type and duration from device capture
time with UTC offset, not upload time.

`magician_v2::audio_notes_seam` (`audio_note_layout`,
`audio_note_extension`, `render_audio_note_markdown`, `audio_note_newest_first`,
`audio_note_ref_from_index`) holds archive policy; `notes.rs` holds registry,
settings, idempotency and write orchestration; `magician-api::media_ux` holds
dictation and live-call UX.

**Write protocol.** Audio then Markdown via same-directory temp files, flushed
and atomically renamed under the Audio Notes write boundary (Markdown page
last); `notes/audio-index/<note_id>.json` commits only after both exist, and an
index failure rolls back new files. A crash retry adopts byte-identical
remnants and repairs only the missing file; differing content is a conflict,
never overwritten. The index drives newest-first paging, transcript search,
authenticated playback, deletion and read-only `internal_data`
`list_audio_notes` / `read_audio_note` (bounded previews in lists, full
transcript on read; recording bytes never reach model context). Its private
envelope snapshots the absolute provider root, which no API or agent model
carries; APIs expose provider-relative paths only. Recordings are
authenticated, range-capable, private/no-store. iOS (**Menu → Audio Notes**)
and web (`/notes`) list saved and pending/failed records with play,
server-paged search, Retry and confirmed Delete/Discard; new search text resets
the server offset, and deleting a loaded row repairs the next offset.

## Task Projections

Publishes a readable page without making notes the task database; canonical
task, internal-task, chat and execution outputs are read through the active
`ArtifactV2Workspace` root. Pages and copied assets go through provider
selection/fallback.

```text
POST /api/magician/v2/notes/publish/task/{task_id}
{"mode": "standard", "include_assets": true, "workspace": "default"}
```

| Mode | Contents |
| --- | --- |
| `compact` | Goal and primary output. |
| `standard` (default) | Final answer, bounded timeline, up to four important intermediate outputs, selected files and screenshots. |
| `diagnostic` | Standard plus broader outputs and, with `include_assets`, a raw task-record asset. Only if the scoped default is changed. |

`POST /api/magician/v2/notes/publish/tasks/backfill` takes the same body plus
`"only_unpublished": true, "offset": 0, "limit": 25` and fills `Tasks/` after a
provider change or storage migration: completed tasks only, at most 100 per
request, missing pages by default (removed pages repaired in place;
`only_unpublished: false` refreshes existing projections).

Pages land at `Tasks/<YYYY-MM-DD>/<task_id>-<title slug>.md` (slug up to 72
characters, `task_note_path`), assets in a sibling `<task_id>.assets/`. Frontmatter: schema,
ids (task, thread, agent), principal/workspace, status, lifecycle, mode,
canonical created/due/completed/source-updated times, publication time,
projection date, tags (`magician/task`, `date/YYYY-MM-DD`, `task/<status>`,
`agent/<agent>`, `lifecycle/<kind>`, optional priority, `due/YYYY-MM-DD` for a
valid ISO due prefix, normalized task tags). The projection date is
completion, else source-updated, never publish/backfill time. Observe orders
pages the same way and labels completion, due and publication separately, so a
backfill never looks newly completed.

`notes/task-note-index/<task_id>.json` (scoped, machine-owned) maps a task to
its provider root and stable page path (pagination, search, repair, reopen),
which republishing updates in place; it never mutates the task, and the page is
never authoritative. `{task_id}.assets/` rejects names with path
components; assets are size-capped singly and in total, with hashes and
provenance; oversized or missing files stay references.

Terminal task cards (completed, failed, cancelled) offer **Publish to Notes**
(web card and panel; iOS **Actions** sheet and note-plus); non-terminal
snapshots are rejected. Settings sets default mode, asset copying and opt-in
auto-publish, which runs only for a completed task with no pending or failed
synthesis.

## Observe And Memory Promotion

`/observe` keeps two roles apart:

- **Sources → Notes** (input): new and edited Markdown from the scoped Local
  Markdown root and any explicitly configured, boundary-safe Space feeds the
  normal Observe pipeline (selection, intent scoring, handoff, resurfacing,
  Worth a look). Traversal is
  iterative, ≤ 20,000 directory entries, skips symlinks, provider-internal
  dirs and `Workspaces/` (goal text stays out of content search, resurfacing
  and the taste profile; `read_observation_note` answers nothing for an older
  `Workspaces/` ref), reads bounded UTF-8 `.md`/`.markdown`, and checkpoints a
  durable scoped
  offset; identity plus content revision skips unchanged notes.
- **Published notes** (output, the **Notes** pane): task projections, never
  inputs. Web and iOS share search, server offset/limit paging (5, 10, 20, 50),
  open through the canonical note link, publish-next-25 (with remaining-work
  feedback) and promotion.

**Promote to memory** never writes a memory tier: it creates a low-risk,
review-required `memory_fact` candidate for `user.knowledge` (bounded text;
note projection, task, agent, provider path and source version as provenance;
volatile publication metadata excluded; one candidate per identical content).
Learning review is the only bridge into model-facing memory.

## Layout

Local Markdown root `magician_data_v3/scopes/{principal}/{workspace}/notes/space/`
gets only `Inbox/` on first write (`ensure_standard_note_dirs`); `Tasks/` and
`Audio Notes/` appear when a task page or recording is written. The palette's **Notes** command opens `/notes`. Task output
preview, download, open and export resolve through `ArtifactV2Workspace`.

## Where notes live

Nothing to install or run: Magician serves the library and writes the folder
directly, and `make setup-all` / `make run-all` do nothing for notes. Default
folder:

```text
${MAGICIAN_NOTES_SPACE:-${MAGICIAN_ROOT_DIR:-${MAGICIAN_STORAGE_PATH:-$HOME/MagicianNotes}}/MagicanNotes}/spaces/<principal>/<workspace>
```

Keep each Space in a strict subfolder of `MAGICIAN_ROOT_DIR`, never the runtime
root. An older shared Space migrates into the per-workspace folder (contents
move; `space_path` rewritten). A Space choice never moves task, ledger, memory
or execution storage.

## Protected web and iOS access

Web (`/notes`), Magios (**Menu → Notes**, `NotesBrowser.swift`) and Magdroid
(`NotesScreen.kt`) use the authenticated `/notes/tree`, `/notes/file`,
`/notes/backlinks` and `/notes/search` routes. There is no Notes hostname,
Access app, tunnel, embedded viewer, or notes recovery/token-rotation drill;
links into a note are the canonical relative `/notes?…` link.

## Path Safety

Note-relative paths must stay under the selected provider root: absolute
paths, parent, root or prefix components and root escapes are rejected before
writing. Missing directories are checked via their deepest existing
ancestor against the canonical root, so lazy creation works and symlink escapes
fail.

## Current Limitations

- Notes are not in semantic memory retrieval; Audio Note transcripts are indexed
  and agent-readable but not auto-injected.
- The Tauri settings window has no folder picker; paths are text fields.
- Browser uploads stop with the browser; the outbox resumes on the next app
  page.
