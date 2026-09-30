# Settings: workspaces

Server seam: `GET`/`POST /api/magician/v2/workspaces`,
`PATCH`/`DELETE /api/magician/v2/workspaces/{id}`, and
`DELETE /api/magician/v2/workspaces/{id}?purge=true`
([auth: workspaces](../magician/auth.md)).

Settings hosts `$lib/settings/WorkspacesPanel.svelte` (fed by
`$lib/stores/workspacesStore.ts`) as **Workspaces**, directly under the
bearer-bound workspace card. It renders only while signed in — every call rides
the session bearer, and the server resolves the principal from it.

**List.** Each workspace shows its name and id, a **Default** or **Current**
chip, its description, and the best-effort counts `GET /workspaces` returns:
agents, active tasks, and last activity.

**Create.** A name, an id and an optional description. The id is suggested
from the name (lowercase, accents stripped, anything else collapsed to `-`,
trimmed to 32) until edited, and validated with the server's own rule: 1–32
characters of `a-z`, `0-9`, `_` or `-` — ids are directory names. An id that
already exists is refused before submitting. A `409 workspace_pending_purge`
means the name belongs to a workspace deleted with its data; the panel says it
is free again after Magician restarts rather than showing the raw code.

**Rename.** The name and description of any workspace, including the default —
only its id is fixed. An emptied description is sent as `null`, which clears
it; the server distinguishes that from leaving it out.

**Delete.** Refused in the UI for the default workspace, which the server
protects, and for the workspace the session is bound to: deleting it from
inside would end the session under the page. The button says why on hover.

Deletion asks once and sends a plain `DELETE`. For an empty workspace that is
the end of it. For one that still holds data the server answers
`409 workspace_has_live_state`, which the panel turns into a second
confirmation rather than an error: it shows the counts, warns when tasks are
still active, and states the consequence — access ends now, and agents, tasks,
memory and files are removed the next time Magician starts. **Delete with
data** stays disabled until the workspace id is typed out exactly, and only
then sends `?purge=true`. So data is never removed by a single click, nor by
choosing a mode up front without knowing whether there is anything to lose.

The removal waits for the next start because the running service still holds
handles into a hydrated scope; see [auth: workspaces](../magician/auth.md). A
purged workspace leaves the list immediately — its registry row is gone — even
though its files are still on disk until then.

After any change the panel reloads its list and calls `onChange`, which the
settings page uses to re-read the session, so the workspace switcher above
picks up new and deleted workspaces.

Tests: `workspacesStore.test.ts` (ordering, error codes kept, create and
update bodies, `purge` only when asked, the slug rule, slug suggestion) and
`WorkspacesPanel.component.test.ts` (no delete for the default or current
workspace; an empty workspace deleted without purging; a workspace with data
purged only after the id is typed; the pending-purge message; a duplicate id
refused).
