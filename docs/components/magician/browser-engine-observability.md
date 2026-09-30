# Browser Engine Observability

Magician records which browser engine actually attempted each browser command,
what scoped work owned it, which safe page it acted on, and whether it
succeeded. The dashboard lives at **Observe → Observation pipeline stats →
Browser engine activity**.

## Authority and coverage

The write boundary is `AgentBrowserSession::run_command_internal`, immediately
around the real `agent-browser` subprocess. This is intentionally below engine
selection and above no browser-specific LLM logic. A CloakBrowser concurrent
session failure and its bundled-Chrome retry therefore become two records; the
dashboard never infers an engine from configuration alone.

Scope-authoritative analytics contexts are attached to:

- ordinary browser capability work and retrieval handoffs;
- provider-neutral public/authenticated browser readers;
- VibeDev preview screenshots;
- automatic meeting joins; and
- API-mining authentication refresh sessions.

Each record includes session, task/execution lineage, an explicit work kind and
ID (`task`, `execution`, `content_read`, `preview_project`, `meeting_join`,
`api_auth_refresh`, or browser session), actual engine, fallback source,
connection mode, safe page, coarse operation, success, latency, and a bounded
failure class. Raw argv, stdout/stderr, page contents, and secrets are not
copied into this store.

## Privacy and storage

Only HTTP(S) origin and path are retained. URL username/password, query, and
fragment are stripped before persistence because they can contain tokens or
private search text. The database is scope-owned:

```text
<storage-root>/scopes/<principal>/<workspace>/analytics/browser_engine_usage.sqlite3
```

SQLite uses WAL mode and indexes newest-first plus engine/outcome reads. A
telemetry write failure is logged but never changes the authoritative browser
result. The ledger is bounded transactionally after each append: rows older
than 90 days expire and at most the newest 50,000 command attempts are retained
per scope. It is also included in the shared `/storage` governance inventory.

## API and pagination

`GET /api/magician/v2/browser/engine-usage` requires the normal explicit scope
headers and accepts:

- `limit` — 1–100, default 25;
- `offset` — zero-based server offset;
- `engine` — exact engine name; and
- `outcome` — `all`, `success`, or `failure`.

The response contains `items`, `total_count`, `limit`, `offset`, `has_more`,
and an engine summary for the active filters. Unified UI uses the shared
`ServerPager` and sends a new server request for every page; the page-size
selector offers 10, 25, 50, and 100 rows.

## Reproducible browser driver

The pinned upstream tag and
`skillshub/browser/_vendor/v0.38.1-Magician.0/patches/0001-magician-patches.patch`
are the source of truth. Native driver executables are local build artifacts
and are not committed. `make setup-agent-browser` automatically invokes the
source rebuild when the current artifact is missing, then installs the npm
launcher/skill data and mirrors the generated driver into the browser skill.
Container builds compile their Linux driver from the same patch and
`scripts/materialize-container-browser-skill.sh` installs that complete browser
skill into the default scope on first boot.
