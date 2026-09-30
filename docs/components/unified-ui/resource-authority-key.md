# Resource Authority API Key (Budget Dashboard)

The budget dashboard (`/budget`) talks to the resource-authority admin API,
which is gated by the server's `RESOURCE_AUTHORITY_API_KEY` (see
`docs/components/magician/resource-authority-api.md`). When that key is set on
the server, the dashboard must present it on every request.

## Where the key lives

- **Store:** `ui/unified-ui/src/lib/stores/resourceAuthorityStore.ts`
  - `getResourceAuthorityApiKey()` / `setResourceAuthorityApiKey(key)` read and
    persist the operator-entered key in `localStorage` under
    `resourceAuthorityApiKey`.
  - `apiFetch` attaches `Authorization: Bearer <key>` **only** to
    `/api/magician/v2/resource-authority` requests.
- **Entry field:** `/budget` (`src/routes/(app)/budget/+page.svelte`) has an
  "API Key" card — a password input plus Save — that writes the key via
  `setResourceAuthorityApiKey` and reloads the dashboard.

## Why it is scoped, not global

The key is deliberately **not** injected by a global `fetch` interceptor.
A global interceptor would attach the secret to every outbound request —
including any third-party/cross-origin call — which leaks it. Scoping it to the
resource-authority store keeps the secret on same-origin admin traffic only.

The key is a **server secret**. Storing it in the browser is acceptable only
because this is a same-origin, operator-only admin dashboard. It must not be
publicly exposed. The key is always sent as an `Authorization` header, never in
a URL/query string, so it cannot leak into access logs, proxy logs, browser
history, or `Referer` headers.

## Local dev auto-seed

For local development the store seeds the key automatically so it does not have
to be pasted each time:

- Set `VITE_RESOURCE_AUTHORITY_API_KEY=<key>` in `ui/unified-ui/.env.development`
  (gitignored, and loaded only in dev mode).
- On module load, `resourceAuthorityStore` seeds `localStorage` from it, but
  **only** under `import.meta.env.DEV` and only when no key is already stored.

This is double-guarded so the secret can never reach a production bundle or the
public origin: the `.env.development` file is not loaded for production builds,
and the seeding code is behind `import.meta.env.DEV` (dead-code-eliminated in
production). In a production build the operator enters the key once via the
`/budget` "API Key" card.
