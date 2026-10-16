# Changelog

## [Unreleased]

- 0.2.8: Boot enablement prepares an install review once and approves that review.
- 0.2.7: Widening a key's allowed hosts counts as an expanded secret use and needs review; re-submitting an older unscoped key leaves it ungranted instead of failing.
- 0.2.7: Install review offers "any public host" only when requested, shows reachable hosts per tool, and grants keys per host set or an explicit "any site" opt-in.
- 0.2.6: Install review shows how each tool runs, its hosts and where each key can go; "any public host" is off by default and gaining it needs review.
- 0.2.5: Review, approve and diff owner-selected secret uses for app tools; nothing is granted unless selected.
- **Security:** scripted-page asset credentials are bound to live installations; stale package, surface, grant or session revisions are refused.
- 0.2.4: Review, approve and diff owner memory grants for apps; purge removes them.

### 2026-09-11 — 0.2.3 — Keyset inventory, remote-processing migration, approval retry

- Purge, retention and forget cover keyset pagination cursors.
- New owner-reviewed migration to enable remote processing for existing records; retried approvals reproduce the original receipt.
- The `test-fixtures` feature no longer leaks into production builds.

### 2026-09-04 — 0.2.2 — Recipe digests on behavior grants

- **Security:** behavior grants bind a recipe steps digest, so an update cannot reorder or drop reviewed steps under an old grant; older grants stay valid.
- Boot-admitted system packages go through the ordinary installation review.

_Current development version: `0.2.1`._

### 2026-09-02 — Event and notification owner-review axes

- Installation review shows event subscriptions, schemas and notification ports; new approvals default to deny-all and can only narrow. Updates diff them as separate authority axes.
- Whole-installation purge covers event ledgers and notification outbox.

### 2026-09-02 — Background-behavior owner review and narrowing

- Installation review shows each background behavior with its purpose, operations, cadence and limits; approval can select a subset and only narrow it.
- Review binds the own-store selector so approval cannot substitute inputs.

### 2026-09-01 — canonical schema origin (0.2.1)

- App-contract schema identifiers use `https://magican.ai/contracts/apps/v1/`.

---

Older entries: `docs/archive/changelogs/magician-apps.md`
