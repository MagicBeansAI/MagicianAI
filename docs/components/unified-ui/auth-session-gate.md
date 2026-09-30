# Auth session gate, workspace switcher, and 401 re-gate

Server-side posture and the family/members model live in
`docs/components/magician/auth.md` — this page covers the unified-ui half.

## The gate (two stages, both hydration-aware)

Every Magician surface operates on a proven session scope, not only the
`(app)` group. Protection is the default: a route is open to a visitor only
when `isPublicRoute` (`src/lib/shared/publicRoutes.ts`) lists it — the
marketing pages (`/`, `/manifesto`, `/privacy`, `/terms`), `/login`, and the
static theme specimen sheet `/dev/theme-gallery`. Everything else, including
the pages outside `(app)` (`/warroom`, `/hud`, the overlays, the dev tools),
is gated. `publicRoutes.test.ts` derives every page from the route
files and asserts the public set is exactly that list, so a new page is
protected unless it is added there on purpose.

1. **Load-time presence gate** — the root `routes/+layout.ts` redirects to
   `/login?redirectTo=…` when the route is not public and no bearer is
   installed. It awaits `hasHydratedScopeBearer()` first: Tauri webviews
   start with empty `sessionStorage`, and the bearer may live in the native
   credential store — answering synchronously would misread a signed-in
   native surface as signed-out. Runs before any page component renders, so
   no unauthenticated shell + error toasts ever flash, and is a no-op on the
   server (SSR and prerender). `routes/(app)/+layout.ts` keeps only the app
   shell's host rule (local hosts and the Tauri origin).
2. **Mount-time liveness check** — `routes/(app)/+layout.svelte` calls
   `refreshScopeSession()` on mount; a present-but-dead token (revoked or
   expired) resolves null after a 401 and redirects to `/login` with the
   current location. Network failures are not auth verdicts: the shell
   renders and per-route error states speak for themselves. Each probe is
   fenced to the bearer revision that started it, so an older in-flight 401
   cannot erase a session minted by a concurrent login (and an older 200
   cannot confirm identity for a replacement bearer).

`/login` (`routes/login/+page.svelte`) is outside `(app)`: it signs in
via `loginScopeSession` (first sign-in on a fresh install creates the
owner), then resumes `redirectTo`. The param is validated to same-app
paths only — `/…` but not `//host`, `/\host`, or anything carrying
control characters.

## The 401 re-gate (session death mid-session)

A session revoked or expired after mount would otherwise degrade every
surface into error toasts forever. The scoped-fetch patch
(`installScopedApiFetch`) redirects to `/login` with the current
location when a response is 401 **and** the request carried our bearer
**and** the path is not a non-verdict path (`isAuthVerdictPath`) **and** we
are not already on `/login`. It fires once per token loss and rearms when
a new token is installed. The response must still match the bearer value
and credential revision that initiated the request; a delayed 401 from an
older page or session cannot clear a newly installed login bearer.

Non-verdict paths — a 401 there says nothing about the session:

- `/api/magician/v2/auth/` — auth endpoints answer with 401s by design (a
  probe's stale token is an answer, not a verdict).
- `/api/magician/v2/resource-authority/` — the resource-authority admin API
  is gated by its own `RESOURCE_AUTHORITY_API_KEY`, compared byte-for-byte
  against whatever `Authorization` carries. With that key set and no RA key
  in the browser, the scoped wrapper's session bearer is always "wrong"
  there, and the shell probes `/resource-authority/freeze` on every mount
  (`loadFreezeStatus`); without this exemption that 401 would wipe a valid
  session on every mount. The RA doc
  ([`resource-authority-api.md`](../magician/resource-authority-api.md)) is
  explicit that the key is not an authentication layer for the general API.

## Workspace switching

The TopBar account menu lists the session's owned workspaces (refetched
each open via `refreshScopeSession`), marks the current one, and
switches via `switchScopeWorkspace` — `POST /auth/session/scope`
rotates the bearer so the session's claim, not a request header, is the
only scope selector. Switching performs a deliberate full reload so
scope-keyed stores (tasks, chat, muij) re-resolve against the rotated
token; running work keeps executing under the workspace it started in —
a switch changes what this surface addresses next, nothing else. Sign
out is in the same menu (`logoutScopeSession` revokes server-side and
clears the local token).

Known boundary (deliberate): the web bearer lives in `sessionStorage`,
so each new browser tab signs in once. Native surfaces instead hydrate
from the Tauri credential store, which is what the hydration-aware gate
exists for.
