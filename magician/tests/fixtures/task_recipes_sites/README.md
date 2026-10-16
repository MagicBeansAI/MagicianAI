# Task Recipes fixture sites

Four small single-page sites served in-process by
`magician/examples/task_recipes_fixture_eval` (one port each, so each is its
own origin). They emulate the patterns real sites use so the Task Recipes rail
can be proven browserless without network access or real credentials.

| Site | Emulates | Knobs |
| --- | --- | --- |
| `catalog` | search by query param, list→detail dependency chain, a server-rendered page whose answer is only in the HTML Document | — |
| `portal` | login form → HttpOnly cookie session + CSRF meta → authenticated XHR | `session_ttl_secs` (401 after expiry) |
| `notes` | CSRF-checked JSON write, verify-by-read, a denylisted path that is never called | — |
| `board` | GraphQL read (POST with the query in the body) | `schema_version` (v2 renames `score` → `points`) |

## The browserless oracle

Every page load mints a fresh `X-Page-Nonce` (sent on every API call the page
makes) and fires a beacon (`POST /t/collect`, `GET /px.gif`). A replay resends
captured requests, so during a browserless run the site must see **no new
nonce** and **no beacon**. `User-Agent` and `Sec-Fetch-*` are logged but are
not gates: header templates replay verbatim.

## Control plane (`/__eval/*`, never called by page JS)

- `GET /__eval/requests?since=<mark>` — request log (method, path, kind,
  nonce, cookie/CSRF presence, JSON body keys, status; never values).
- `POST /__eval/reset` — clear the log (sequence counter stays monotonic).
- `POST /__eval/knobs` — merge `{session_ttl_secs, schema_version}`.
- `GET /__eval/sessions` — fixture session ids/tokens, so the driver can prove
  a recipe never stores them.

Credentials are fixture literals: `eval` / `eval-pass`. Nothing here is real.
