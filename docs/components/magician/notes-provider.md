# Notes Provider

The notes provider layer stores user-visible Markdown notes without changing
canonical Magician runtime storage.

## Boundary

Notes are projections and captures for humans and agents to read. They are not
the source of truth for:

- tasks or internal tasks;
- execution history;
- artifact V2 runtime state;
- memory tiers;
- ledgers or resource authority.

Changing the notes provider or notes root only changes where new notes are
written. Existing notes are not deleted, but can appear lost until the user
switches back or migrates them. Indexed Audio Notes are the exception: their
private index envelope retains the provider root chosen at capture time, so
playback and deletion still resolve the original files after a settings change.

## Settings

Notes settings are scoped by `(principal, workspace)` and stored at:

```text
scopes/{principal}/{workspace}/notes/settings.json
```

That logical path is resolved through the active workspace file provider: under
`magician_data_v3` with `local_file`, under `<Space>/.magician/runtime` with
`silverbullet_space`. The browser Settings page is the only editor (including
whether capture is enabled); requests use the signed-in scope.

The one setting that matters is `space_path`, the filesystem root Magician reads
and writes. Fresh setup defaults to one folder per workspace:
`$MAGICIAN_ROOT_DIR/MagicanNotes/spaces/<principal>/<workspace>` (normally
`~/MagicianNotes/MagicanNotes/spaces/<principal>/<workspace>`). The stored
provider id is `silverbullet` (a historical name). Web, iOS and Android open the
library inside Magician. `local_url`, `server_url` and `public_origin` in older
settings files no longer decide how clients open notes; the backend still
validates them and uses `public_origin` (else `server_url`) for `open_url` and
the provider-health probe.

`MAGICIAN_NOTES_SPACE` relocates the forest (the parent of `spaces/`). An
explicit `space_path` that is the forest, its `spaces/` directory, or
`spaces/<principal>` resolves to the per-workspace child (even if it does not
exist yet) so search never walks sibling folders; saving persists the child
path. HOME, `/` and the Magician runtime root are refused. Changing `space_path`
does not move files. An unsafe path is marked unavailable and writes fall back
to Local Markdown. Path comparison resolves existing symlinks before applying
parent components.

Supported provider ids: `local_markdown` and `silverbullet`. `local_markdown` is
always kept as the fallback so writes continue when the notes folder is not
configured or unavailable.

## Provider health

A SilverBullet space is a folder, so `status_for_provider`'s directory checks
pass whether or not any server is running.

- **Reading the status never writes.** `GET /notes/providers/status` uses
  `directory_is_writable` (`access(2)` `W_OK` on Unix, the read-only flag
  elsewhere). `writable_probe` (which creates and removes files) is reserved for
  `unavailable_write_reason` on the write path. A full or read-only-remounted
  filesystem therefore reads as writable until the write probe says otherwise.
- `NotesProviderStatus.health` appears only for providers that depend on more
  than their directory (absent on Local Markdown): server reachability and
  whether the `sb` CLI is on PATH.
- The server probe is bounded at two seconds; any HTTP response (including
  401/404) counts as reachable.
- The `sb` CLI is reported, never enforced — nothing in the write path shells
  out to it. The PATH lookup is cached per process.

## Provider Registry

Writes go through a registry of two filesystem-backed providers:

- Local Markdown: scoped root under `magician_data_v3`, created lazily on first
  write.
- SilverBullet Space: configured Space folder, used only when explicitly
  configured and writable.

If the requested/default provider is unavailable, writes fall back to
`local_markdown`. Write responses include:

```text
requested_provider
provider
used_fallback
fallback_reason
path
absolute_path
open_url
```

`open_url` is provider-aware: Local Markdown returns a `file://` URL; SilverBullet
prefers `public_origin`, falls back to legacy `server_url`, and drops the `.md`
suffix. Future providers fill the same field with their native deep link.

Disabled notes reject writes before any note root is created.

## Agent tools

Six compiled tools share the notes store: `create_note`, `append_note`,
`open_note`, `search_notes`, `save_selection_to_note`, `publish_task_to_note`.
They build `NotesSettingsStore` from the workspace on `AgentResources`; every
provider write serializes on a process-wide lock, so tools and HTTP are the same
writer.

The tools surface rather than smooth over:

- **Where it actually landed** — `provider`, `used_fallback`, `fallback_reason`,
  so an agent cannot claim a SilverBullet save after a silent fallback.
- **Whether retrying can help** — failures carry `retryable`, mirroring the
  HTTP layer's `io::ErrorKind` classification (`TaskNotFound` is permanent; an
  `Io` error reaching it is not).

A read-only tool reports `notes_read_failed`, never `notes_write_failed`.

`open_note` resolves one note by path and returns its open URL (`open-root`
opens the whole folder). It takes no write lock and creates nothing; a path under
no configured provider is `NotFound`. Providers are searched default-first.

`publish_task_to_note` delegates to `publish_task_note`, which owns scope
ownership and "only a settled task may be published", so tool and task card
cannot disagree.

Grants are per-agent in `definition.agent.yaml`; the six notes tools are granted
to the personal assistant in the seed template. Scoped agent definitions do not
inherit template edits, so an existing scope sees a new tool only after
re-seeding.

### Selection capture

`save_selection_to_note` files a passage that belongs to something else (page,
document, message) with its source, and is the tool form of the same
`capture_selection` the browser extension and desktop overlay call.
`append_note` takes prose the agent wrote; quoting it would misattribute it.

A capture is an append with provenance built on `append_note`, so daily-page
naming, provider selection and fallback cannot drift. Invariants:

- **Idempotent on `capture_id`** via an HTML-comment marker in the note. A
  deliberate second capture carries a new id. The tool passes no `capture_id`
  (a model inventing one could suppress a capture it meant to make).
- The dedupe read and the append run under one hold of the process-wide write
  lock, so concurrent sends of one id cannot both append.
- The retry check reads every configured provider's copy of the target page
  (the write is fallback-aware, so the read must be too).
- The read is fail-closed: only a missing page means "not captured"; any other
  stat/read error fails the capture.
- Without `target_path`, the dedupe also checks yesterday's default page
  (midnight retries); the append still lands on today's. A named `target_path`
  checks only that page.
- `capture_id` must be short and alphanumeric plus `-`, `_`, `.`, `:`, so it
  cannot close the comment early. Every surface sends a UUID.
- The source line is a Markdown link with an angle-bracketed destination (URLs
  with `)` stay one link); title brackets are escaped. Absent provenance fields
  are omitted.

## Projection seam

Product logic for the two projections lives in
`magician_v2/notes_projection.rs`, with a one-way dependency on the provider
layer in `notes.rs`. The seam owns:

- the capture decision: `capture_append_plan` (selection and capture-id
  validation, daily-page naming, quoted block with provenance and marker), the
  resend-once answer, and the dated Inbox page names (today's for the write,
  yesterday's for the look-back);
- the task-page projection: naming, tags, projection date, asset names, timeline
  bounding (newest 20 executions, 12 spelled-out steps in standard mode,
  omissions reported), Markdown rendering;
- the memory-promotion projection (bounded stable value, candidate fingerprint
  id, review-gated candidate request) called by `magician-api`'s promote handler.

The provider layer keeps the registry, selection and fallback, settings,
storage and the write lock; `NotesSettingsStore::capture_selection` /
`publish_task_note` keep orchestration (scope checks, retry read, provider
writes, index commits) and call the seam's decision functions.

## Search

Browsing and editing routes (all refuse paths leaving the root, symlinks, and
anything not a real directory or Markdown file):

- `GET /notes/tree` (`list_note_tree`) lists one directory; folders carry
  `has_children`. `POST /notes/tree` (`folder`, `name`) creates a folder;
  `DELETE /notes/tree` deletes a folder, refusing the root and folders
  containing a symlink.
- `GET /notes/file` (`read_note_file`) reads a note; `POST` creates one (empty
  `folder` = notes root); `PUT` (`path`, `markdown`) replaces one; `DELETE`
  removes one.
- `GET /notes/backlinks` (`list_note_backlinks`) lists notes linking via
  `[[Page]]` or a Markdown link to a `.md` file.

Every create, save and delete refreshes the search index before returning.

`search_notes` / `POST /notes/search` find notes by keyword, typo and, when
embeddings are stored, meaning; each hit includes its matching lines. Index:
one LanceDB table at `scopes/{principal}/{workspace}/notes/hybrid-index` (path,
text, content hash, embedding, stored full-text index).

- A search opens the table once per process, checks out the latest version and
  queries the stored index — no `memory://` copy, no rebuild per query.
- Changed rows are embedded and merged in the background, refreshing the
  full-text index once. A per-folder watcher merges after an outside edit has
  been still for about a second (`notes watch refreshed` records timings).
  Startup does not re-embed unchanged notes.
- A query runs BM25 with LanceDB's automatic typo distance plus, when the
  embedding model answers in time, a vector search; results are fused. If the
  model is down, search stays on the fuzzy keyword index.
- The Markdown files are the only copy of note text. SilverBullet's own HTTP
  surface and CLI are not used for search.

Matching rules: body, title and path (minus the extension) are all admitted
together, so a page under `Clients/Acme/` answers "Acme". Keyword terms are
ANDed, case-insensitive substring; a title/path match of every term outranks a
body-only hit; within a note the line with the most terms comes first. Asking
for a provider not configured for the scope is refused rather than searched as
empty.

Search walks the same boundary-safe roots as Observe traversal — a notes root
is shared with runtime state (model caches, credentials, worktrees) that must
never surface.

Every growing axis is bounded: entries scanned, note size, concurrent reads,
terms per query, results, snippet length. `scan_truncated` and `more_available`
report the two that trim real results.

The web surface is `/notes`: toolbar with search, then the notes folder.
Transcripts live under `Audio Notes/` (voice icon); uploading recordings stay in
the pending list. The page states truncation and caps, and renders matched
terms by splitting lines into segments (never assembling HTML).

## Capture surfaces

`POST /notes/capture-selection` is the one entry point, reached by:

- **Browser extension** — a selection-only context item in `magicutor/extension`,
  registered at all three MV3 lifecycle points (ephemeral service worker). It
  calls Magician directly, not through the desktop host gateway, so capture
  works with the desktop app closed. It keeps its own scope (defaulting to the
  web UI's default), because notes writes require a scope.
- **Desktop contextual assist** — a `save_to_notes` action that runs directly
  rather than through select-then-generate (keeping text is not a draft).
- **`save_selection_to_note`** — the agent-facing form.

The overlay action id and the extension's default scope are literals crossing a
language boundary; `make check-notes-capture-surface` (part of
`make check-all`) fails on drift.

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

All endpoints derive scope from the workspace-bound bearer. Principal/workspace
fields in older payload shapes are compatibility assertions, not selectors.

## Audio Notes

Chat dictation on iOS and web is ephemeral by default (tap to start/stop, or
press-and-hold): the recording is removed after STT and only the transcript
follows the composer's three-second cancelable auto-send. **Keep dictation
recordings** (Voice settings → Dictation) opts in; the mic then shows a saving
disclosure, and archiving is independent of cancelling auto-send. Ambient
Dictation and Thinking Map speech are never archived.

**iOS outbox.** The stopped `.m4a` is copied off the main actor into a
data-protected, backup-excluded outbox: at most 50 records, 200 MB total, 24 MB
per recording, with capacity reserved for the later multipart copy and
including quarantined files. Multipart bodies stream to file. A background
`URLSession` survives suspension/relaunch (retry tasks use `earliestBeginDate`;
foreground activation also drains). Relaunch accepts one task per valid record;
orphan/duplicate tasks are canceled and cannot mutate state.

Each row owns retry count, next attempt, failure, destination URL and captured
scope. Restored destinations must be a credential-free, query-free origin
matching the configured service (or HTTP loopback in development); others are
quarantined. Retryable network/408/425/429/5xx failures back off; permanent 4xx
and exhausted/30-day windows move only that row to **Needs attention** (explicit
Retry/Discard under Menu → Audio Notes; Discard is disabled while the background
session owns an upload). Interrupted STT recovers as an audio-only note.
Oversized recordings and a full outbox are rejected visibly. Transcripts are
client-bounded below the server multipart limit.

**Web outbox.** Opt-in and browser-local: the Blob enters a bounded IndexedDB
outbox before STT with capture-time scope and a stable UUID. A ten-minute
transcript lease keeps other tabs from treating active STT as crashed; expired
captures recover audio-only. Atomic upload leases prevent duplicate workers
across tabs. Failures back off or remain under `/notes` for Retry/Discard. It is
a foreground queue: uploads stop when the browser process dies and resume when
an app page next opens.

**Durable receipt.** An outbox deletes a recording only after a non-redirected
2xx decodes as a receipt whose `note_id`, byte count, provider, note path and
audio path match the record (a proxy/login `200` is retried, not trusted). The
backend binds the UUID to the recording's byte length and BLAKE3 digest; a
same-id/different-audio collision is a conflict. RFC 3339 timestamps compare by
instant.

Uploads omit an explicit provider unless one was captured, so the scoped
default and fallback apply. Layout:

```text
Audio Notes/
  2026-08-01/
    20-56-24-147-a1b2c3d4111142228333123456789abc.md
    20-56-24-147-a1b2c3d4111142228333123456789abc.m4a
```

With `local_markdown` this is below the scope's Notes root (default
`scopes/<principal>/<workspace>/notes/space/Audio Notes/`); with SilverBullet,
below the Space. The page embeds/links the recording, includes the transcript
when available, carries `audio-note`, `voice-note`,
`audio-note/date/YYYY-MM-DD` and `audio-note/time/HH-MM` tags, and records
`recorded_at`, `recorded_date`, `recorded_time`, `source_surface`, MIME type
and duration — all from the device's capture timestamp (with UTC offset), not
upload time.

Archive policy lives in `magician_v2::audio_notes_seam` (`audio_note_layout`,
`audio_note_extension`, `render_audio_note_markdown`, `audio_note_newest_first`,
`audio_note_ref_from_index`); `notes.rs` keeps registry, settings, idempotency
binding and write orchestration. Dictation/live-call UX lives in
`magician-api::media_ux`.

**Write protocol.** Audio then Markdown are written via same-directory temp
files, flushed and atomically renamed under the Audio Notes write boundary; the
Markdown page is published last. `notes/audio-index/<note_id>.json` commits only
after both files exist; index failure rolls back new files. A retry after a
crash adopts byte-identical remnants and repairs only the missing file;
different existing content is a conflict, never overwritten. The index powers
newest-first pagination, transcript search, authenticated playback, deletion,
and read-only `internal_data` `list_audio_notes` / `read_audio_note` (bounded
previews in lists, full transcript on read; recording bytes never reach model
context). Its private envelope snapshots the provider root; that absolute path
is not in API or agent models.

API models expose provider-relative paths only. Recording responses are
authenticated, range-capable, private/no-store. iOS (**Menu → Audio Notes**) and
web (`/notes`) show saved and pending/failed records with play, server-paged
search, Retry and confirmed Delete/Discard. Changing the search text resets the
server offset; deleting a loaded row repairs the next offset.

## Task Projections

Task projections publish a human-readable Markdown page into the selected Notes
Provider without making notes the canonical task database. `Tasks/` is a
projection layer; canonical task, internal-task, chat and execution outputs
remain under the active `ArtifactV2Workspace` runtime root and are read through
it, so projections behave the same for `local_file` and `silverbullet_space`.
Pages and copied assets are written through Notes Provider selection/fallback.

```text
POST /api/magician/v2/notes/publish/task/{task_id}
```

```json
{
  "provider": "silverbullet",
  "mode": "standard",
  "include_assets": true,
  "principal": "anonymous",
  "workspace": "default"
}
```

Projection modes:

- `compact`: task goal and primary user output only.
- `standard` (default): final answer, bounded execution and step timeline, up
  to four important user-facing intermediate outputs, selected copied
  files/screenshots.
- `diagnostic`: standard plus broader registered output coverage and, only with
  `include_assets`, a raw canonical task-record asset. Never selected implicitly
  unless the scoped default is changed.

Backfill populates `Tasks/` after changing providers or migrating storage:

```text
POST /api/magician/v2/notes/publish/tasks/backfill
```

```json
{
  "provider": "silverbullet",
  "mode": "standard",
  "include_assets": true,
  "only_unpublished": true,
  "offset": 0,
  "limit": 25,
  "principal": "anonymous",
  "workspace": "default"
}
```

It considers completed canonical tasks, caps each request at 100 publishes, and
defaults to missing pages. A registry entry whose page was removed is repaired
at the same path. `only_unpublished: false` refreshes existing projections.

```text
Tasks/
  2026-06-22/
    task_abc123-zepto-price-check.md
    task_abc123.assets/
      out_primary-final.md
      out_chart-chart.png
```

Frontmatter: schema, task id, principal/workspace, thread id, agent id, status,
lifecycle, projection mode, canonical created/due/completed/source-updated
timestamps, publication timestamp, projection date, and tags (`magician/task`,
`date/YYYY-MM-DD`, `task/<status>`, `agent/<agent>`, `lifecycle/<kind>`,
optional priority, `due/YYYY-MM-DD` for a valid ISO due prefix, normalized task
tags). The projection date is the completed instant, else source-updated — never
publish/backfill time.

`notes/task-note-index/<task_id>.json` (scoped, machine-owned) links the task
to its provider root and stable page path for pagination, search, repair and
reopen; it never mutates the task or makes the page authoritative. Republishing
updates the same page. Copied assets are individually and collectively
size-bounded with content hashes and source-output provenance.

Observe orders pages by completion time (else source-updated) and labels
completion, due and publication separately, so a backfilled page does not look
newly completed.

Terminal (completed, failed, cancelled) task cards expose **Publish to Notes**;
the backend rejects non-terminal snapshots. Web: task card and open task panel.
iOS: the card's **Actions** sheet and the note-plus control in the task view.
Notes Settings controls default mode, asset copying and opt-in automatic
publishing, which runs only after a completed task has no pending or failed
synthesis.

## Observe And Memory Promotion

`/observe` has two deliberately separate Notes roles:

- **Sources → Notes** is an observation input. It scans Markdown from the
  scoped Local Markdown root and any explicitly configured, boundary-safe Space;
  new and edited notes enter the same Observe selection, intent scoring,
  handoff, resurfacing and Worth a look pipeline as other sources. Traversal is
  iterative, bounded to 20,000 directory entries, skips symlinks and
  provider-internal directories, reads only bounded UTF-8 `.md`/`.markdown`,
  and pages with a durable scoped offset checkpoint. A provider-relative source
  identity plus content revision skips unchanged notes but admits edits.
- **Published notes** is the output/discovery panel for task projections (web
  Observe **Notes** pane). It does not turn task pages into observation inputs.

Web and iOS Published Notes share search, server offset/limit pagination with
5/10/20/50 page sizes, provider-aware open, publishing the next batch of 25
unpublished completed tasks with remaining-work feedback, and memory promotion.
On iOS, opening a page reuses the protected embedded WebView and accepts only
the configured exact Notes origin.

**Promote to memory** never writes a note into a memory tier. It creates a
low-risk, review-required `memory_fact` learning candidate targeting
`user.knowledge`, with note projection, task, agent, provider path and source
version as provenance. Text is bounded, volatile publication metadata is
excluded, and identical content resolves to the same candidate. Learning review
is the only bridge into model-facing memory.

## Layout

Default local Markdown root:

```text
magician_data_v3/scopes/{principal}/{workspace}/notes/space/
```

Standard directories, created lazily on first write:

```text
Inbox/
Captures/
Tasks/
Threads/
Artifacts/
Sources/
```

Writes are direct filesystem writes to the configured notes folder. The command
palette's **Notes** command opens `/notes`.

Task output preview, download, open-file, open-folder and export APIs resolve
canonical files through `ArtifactV2Workspace`. The internally named
`silverbullet_space` runtime storage root is independent of the notes-only Space.

## SilverBullet Setup

There is no SilverBullet install, sidecar or notes server to set up. The
`setup-silverbullet-notes`, `run-silverbullet-notes` and
`stop-silverbullet-notes` targets and their scripts were removed; `make
setup-all` no longer installs anything for notes. The default Space is a plain
folder, resolved at runtime:

```text
${MAGICIAN_NOTES_SPACE:-${MAGICIAN_ROOT_DIR:-${MAGICIAN_STORAGE_PATH:-$HOME/MagicianNotes}}/MagicanNotes}/spaces/<principal>/<workspace>
```

- Keep each Space in a dedicated strict subfolder of `MAGICIAN_ROOT_DIR`; never
  serve the runtime root. A shared Space from an earlier install is migrated into
  the per-workspace folder (contents move; `space_path` is rewritten).
- Choosing a Space never touches the canonical workspace-storage selector: it
  must not move task, ledger, memory or execution storage.

Magician serves the notes library itself. `make run-all` does not start a
separate notes process, and there is no listener on port 3021 to health-check.

## Protected web and iOS access

Web (`/notes`), Magios (**Menu → Notes**, `NotesBrowser.swift`) and Magdroid
(`NotesScreen.kt`) browse, edit and search the library natively through the
authenticated `/notes/tree`, `/notes/file`, `/notes/backlinks` and
`/notes/search` routes. There is no separate Notes hostname, Cloudflare Access
application, notes tunnel ingress or embedded SilverBullet web view: the
`notes-access`, `ensure-notes-tunnel` and `notes-tunnel-status` targets and
`scripts/ensure-notes-access.sh` were removed.

### Recovery and credential drills

None remain. `notes-recovery-drill` (`scripts/drill-notes-recovery.sh`) and the
`notes-token-rotation-*` targets were removed with the notes server and its
Access service token.

## Assets

Task projection copies selected user-facing output assets into sibling
`{task_id}.assets/` folders through the same provider selection/fallback,
rejecting asset names with path components. Assets are size-capped; oversized or
missing files stay as output references instead of failing the projection.

## Path Safety

Note-relative paths must stay under the selected provider root. Absolute paths,
parent components, root/prefix components and provider-root escapes are rejected
before writing. For missing child directories, validation checks the deepest
existing ancestor against the canonical provider root, so lazy directory
creation works while symlink escapes are rejected.

## Current Limitations

- General Markdown notes are not indexed into semantic memory retrieval. Audio
  Notes have a scoped transcript index and read-only agent actions but are not
  injected into prompts automatically.
- The Tauri settings window has no folder picker; paths are text fields.
- Browser background upload cannot continue after the browser process is
  terminated; the outbox resumes when an app page next opens.
