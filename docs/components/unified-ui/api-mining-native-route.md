# API Mining Native Route

`/api-mining` is a hand-authored Svelte route organized around what Magician
can do without a browser. Its four tabs are **Recipes**, **Learned APIs**,
**Activity**, and **Auth**.

The page starts with one bounded `GET /api/magician/v2/api-mining/overview`
request. It carries recipe summaries, capabilities grouped by the parent site
that taught them, process counters, non-secret auth metadata, the active grant
count, registry health, and the number of hidden telemetry calls. Registry index
v1.1 carries `parent_origin` in each capability summary, so site grouping reuses
the index instead of rereading every capability file. Raw trace telemetry counts
use a five-second, 128-scope cache invalidated by origin and full-scope purge.

The response is capped at 500 recipes, 250 sites, 2,000 origins and 2,000
capabilities. Its `truncation` object reports omitted counts, and the route shows
a bounded-overview badge rather than implying the rows are exhaustive. Before the
site cap, sites with retained capabilities sort ahead of observation-only and
telemetry sites, then by capability count and hostname. Auth metadata is loaded
only for retained origins, and the active-grant counter avoids cloning the
durable grant log. The Activity tab polls the small metrics endpoints every five
seconds while visible; detail and mutation endpoints are lazy.

Manual replay binds the form's recipe version through `expected_version`. The
input schema and pinned version both come from the inspected detail definition,
never from newer overview metadata. Overview refresh invalidates changed or
deleted definitions and their forms. A stale-version conflict clears the cached
form and detail and refreshes the overview; the user must inspect the new inputs
before retrying.

An uncertain write (including a lost or unreadable response, or a server/gateway
error without an execution outcome) shows an explicit do-not-retry warning. The
page submits once and blocks another run until the operator confirms checking
the destination. HTTP 200 alone is not shown as recipe success, and a failed API
write never gets a successful run-history badge. A successful replay with a
statistics-save warning stays a success and shows the warning separately.
Structured failure/fallback details appear in the replay notification. A busy
recipe returns `recipe_busy` at once instead of queuing behind an active replay
or approval wait.

## Tabs

- **Recipes** is first. Each row shows the task template, inputs, step count,
  origins, maturity, replay outcomes, last replay, and whether writes need
  approval. Its detail drawer lazily loads the complete versioned DAG, answer
  fields, run history, write grants, and cached projection rows. Operators can
  revoke grants, approve or purge cached rows, and submit a direct replay input
  form. An ungranted write is rendered as HTTP 409 and is never sent.
- **Learned APIs** groups API hosts under `parent_origin`, so an API or DSN
  called by a site is not presented as an unrelated product. Answer-bearing,
  dependency, and first-party relevance chips are on by default; third-party
  and unclassified chips are opt-in. Each capability shows relevance and
  `used_by_recipe_ids` alongside the existing method, URL, confidence,
  side-effect, sample, replay, edit/replay, and OpenAPI controls. A lazy Site
  details drawer lists the site's compiled multi-step workflows across all of
  its origins; workflow rows are deliberately read-only on this surface.
- **Activity** shows router, passive-validation, task-recipe, projection, and
  registry-health counters. The counters are process-local and reset on
  backend restart.
- **Auth** keeps the origin status badges and the existing bounded Refresh Auth
  flow. Credential values are never returned to or rendered by the page.

Noisy Origins and Learned Resources are not peer tabs. A
`N telemetry calls hidden` control opens the high-frequency origin review
drawer with Allow and Block actions. Projection rows appear only in the recipe
or capability detail drawer that owns them, with Approve and Purge rows
actions. Those drawers distinguish loading and load failure from a genuinely
empty projection set.

## Auth refresh

Refresh Auth starts a short-lived, backend-owned Magicutor CDP session using
the signed-in browser profile. The route displays starting, waiting, captured,
verifying, and terminal status without exposing credential values. The backend
verifies a safe concrete GET when origin policy permits; otherwise the UI says
that capture completed without automatic verification. The capture window is
bounded to 60 seconds and does not invoke an agent or LLM.

## Rendering boundary

All route chrome, sections, chips, alerts, buttons, tables, forms, drawers, and
modals are native Svelte. The OpenAPI viewer remains a Swagger UI embed because
it renders third-party OpenAPI documents; its surrounding modal is native.
