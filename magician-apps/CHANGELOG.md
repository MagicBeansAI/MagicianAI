# Changelog

## [Unreleased]

- 0.2.7: The update diff compares each key's scope: adding a picked host or ticking "any site" is `secret_uses: expanded` and needs review. Re-submitting an older unscoped key of a host-less tool leaves it ungranted instead of failing the review.

- 0.2.7: Install review offers "any public host" only when the app asks for it; per-tool reachable hosts; per-key host sets or an explicit "any site" opt-in (`AppSecretUseGrant::hosts`/`any_site`), validated against the app's named hosts.

- 0.2.6: Install review shows how each tool runs (in place from its skill), its hosts and where each key can go; approval takes the explicit "any public host" grant (off by default; gaining it needs review).

- 0.2.5: Review, approve and diff owner-ticked secret uses for app tools (`app_secret_use_v1`); nothing is granted unless ticked.

- Bind scripted-page asset credentials to live installations and refuse stale package, surface, grant, or host-session revisions.

- 0.2.4: Review, approve and diff owner memory grants for apps; purge removes an app's memory grant.

### 2026-09-11 — 0.2.3 — Keyset inventory, remote-processing migration, approval retry

- **Retention:** purge, retention and forget inventory the `app_keyset_cursors` table that keyset pagination (contract 1.5.0) introduces.
- **Updates:** an explicit `EnableRemoteProcessingForExistingRecords` migration operation flips existing records' handling policy under owner review; a retried installation approval reproduces the prior immutable receipt.
- **Build:** the `test-fixtures` feature forwards to `magician/test-fixtures` instead of pulling it into production dependency graphs.

### 2026-09-04 — 0.2.2 — Recipe digests on behavior grants

- `AppBehaviorGrant` and `AppEventBehaviorGrant` carry `steps_digest`, folded
  into `app_behavior_request_digest`. Without it a package update could reorder
  a reviewed recipe's steps or drop a guard while the selector/action/operation
  tuple stayed byte-identical, and the scheduler would keep dispatching the old
  grant. `None` for a behavior with no steps, so grants that predate the recipe
  contract keep their digest and stay valid.
- Boot-admitted system packages are reviewed and approved through the ordinary
  `AppInstallationReviewService`; no separate path was added.

_Current development version: `0.2.1`._

### 2026-09-02 — Event and notification owner-review axes

- Show exact event subscriptions, host projection schemas, output schemas and
  workflow-local notification ports during installation review. New approval
  fields default to deny-all; selected entries can only slow/lower an exact
  reviewed digest. Update comparison treats both as independent authority
  axes while preserving legacy empty-axis identities.
- Include event receipts/fires/rate ledgers and notification outbox debt in
  whole-installation purge inventory and ordered deletion.

### 2026-09-02 — Background-behavior owner review and narrowing

- Hydrate exact behavior requests into installation review and bind each to its
  purpose, action, operations, output-schema digest, cadence, and resource ceilings.
  Approval may select a subset and only slow/lower it; durable grants and
  update permission diffs treat behavior authority as an independent axis.
- Show the full own-store singleton selector in review and bind its digest into
  every requested/granted entry so approval cannot substitute input records or
  projections.

### 2026-09-01 — canonical schema origin (0.2.1)

- Generated app-contract schema identifiers now use
  `https://magican.ai/contracts/apps/v1/`.

---

Older entries: `docs/archive/changelogs/magician-apps.md`
