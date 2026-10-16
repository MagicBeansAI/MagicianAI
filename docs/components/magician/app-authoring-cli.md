# App authoring CLI

`magician app` is the provider-free authoring loop for declarative
`skill_type: app` packages. Authoring commands run before any Tokio runtime,
live config, provider, procedure maintenance or HTTP service; lifecycle and
portability commands call the server owners over authenticated loopback HTTP,
never registry files. Exact arguments: `magician app --help`, `pack --help`,
`tools --help`, `approve --help`, `capability --help`.

Discovery memoizes classification and descriptor validation by exact source
contents (512 entries / 16 MiB per cache, serialized results included;
concurrent readers of one source share one validation; classification also binds
the skill folder name), but every read re-enumerates roots, rereads bounded
bytes, resolves symlinks and rebuilds owner, directory and collision bindings,
so edits (even same-size with unchanged timestamps), removals and invalid
sources show immediately (no TTL or filesystem stamp). The server prewarms
registered default workspaces before readiness. The cache grants no authority.
Tests: `make test-app-authoring-catalog` (also `-core`, `-api`).

## Machine-readable mode

`--json` right after `app` (`magician app --json check ./reading-list`) prints
exactly one compact, versioned JSON object on stdout.

- Success: `{schema_version, cli_protocol_version, command, ok: true, result}`.
  Failure after Clap parsing:
  `{schema_version, cli_protocol_version, command, ok: false, error: {code, message}}`,
  classified without paths, URLs, credentials, tokens or error chains.
- No tracing subscriber, so logs never interleave. Exit `0` success, `1` command
  failure envelope, `2` Clap usage/help.
- Live commands are a closed route client for the lifecycle kernels, not a
  generic HTTP/JSON-RPC, shell or argv bridge: literal loopback origin, no
  redirects or proxies, principal and workspace mandatory.

## Commands

Lifecycle and portability commands (authoring ones are covered below) all take
`--principal` and `--workspace`; `<id>` is `<installation_id>`, `<run>` is
`<migration_run_id>`:

| Command | Arguments |
| --- | --- |
| `list`; `detail`, `review`, `reenable-review`, `purge-preview` `<id>` | none |
| `approve <id>` | `--review-digest blake3:<hex>`, `--grant-*`, update flags |
| `disable`, `quarantine`, `uninstall-retain`, `grant-revoke`, `update-begin`, `update-abort` `<id>` | `--expected-generation`, `--request-id` |
| `update-plan <id>` | `--attempt-id`, `--expected-generation`, `--operations-file` |
| `update-backup <run>`, `rewind-preview <run>` | `--passphrase-file` |
| `rollback-code <id>` | `--migration-run-id`, `--expected-generation`, `--request-id` |
| `rewind-commit <run>` | `--preview-digest`, `--request-id`, `--passphrase-file`, `--confirm-data-rewind` |
| `reenable <id>` | `--expected-generation`, `--request-id`, `--review-digest` |
| `package-export <id>`; `package-import <zip>` | `--output` (export) |
| `candidate-publish <zip>` | `--request-id` |
| `data-export <id>` | `--kind data` or `combined`, `--request-id`, `--passphrase-file`, `--output` |
| `data-import-preview <id> <archive>` | `--request-id`, `--passphrase-file` |
| `data-import-approve <id>` | `--preview-digest`, `--request-id` |
| `data-import-commit <id>` | `--preview-digest`, `--approval-ref approval:data-import:<hex>`, `--request-id` |
| `purge-commit <id>`; `purge-status blake3:<hex>` | `--preview <file>`, `--idempotency-key blake3:<hex>` (commit) |

**Lifecycle rules.** Every generation-bearing mutation needs a caller-retained
`request-id` (e.g. `lifecycle:disable-1`) and resends exact bytes on retry.
`approve` takes `review`'s exact `workflow_material_digest`; `reenable` takes
`reenable-review`'s `review_digest`. Updates and reinstalls commit through the
same reviewed `approve` after `update-begin` and candidate publication.

**Updates and rollback.**

- `update-plan`
  (`POST /api/magician/v2/apps/installations/{installation_id}/update-plans`, no
  request-id) compiles and dry-runs one migration with the parked generation and
  attempt id from candidate publication; `--operations-file` is a JSON array of
  exact operation bodies (omitted = infer the unique safe V1 plan).
- Destructive receipts set `backup_required`: `update-backup`
  (`POST /api/magician/v2/apps/updates/{migration_run_id}/backup`), then
  `approve` with `--migration-run-id`, `--update-plan-digest`,
  `--confirm-destructive-migration` (when destructive).
- `rollback-code` (`POST .../installations/{installation_id}/rollbacks/code`)
  restores the previous package/surface after a switched code-only update;
  post-update writes stay, grants are not restored.
- Data rewind:
  `POST /api/magician/v2/apps/updates/{migration_run_id}/rewind-preview`, then
  `.../rewind-commit`.
- Passphrases come from a bounded regular no-follow file and are never written
  into a receipt.

**Package transfer.** Package archives carry no record data, credentials, scope
IDs, grants, schedules, memory or secrets. `package-import` stages a bounded
no-follow package-only archive inertly (local identity, conformance, permission
review and executable rebuild still required; no foreign grants).
`package-export` accepts only the package-only media type, caps the response,
and publishes by create-new hard link from a fsynced private file (never
overwrites, no partial path).

**Data archives.** Exports default to a versioned chunk-authenticated
XChaCha20-Poly1305 envelope keyed by Argon2id, sent only to the loopback archive
owner; `--explicit-plaintext` is warned and refused for Secret data. Data and
combined imports authenticate and validate locally, then run server
`preview -> approve -> commit` (conflicts skipped and reported, destination IDs
minted by the entity-store transaction, exact commit retry reopens the durable
receipt). A new scope needs candidate publication and approval first
(combined-archive package bytes are compatibility evidence, not install
permission), then a data preview into the enabled installation.

**Purge.** Save the `purge-preview` result (server generation, inventory digest,
time window; or the full CLI envelope) to a bounded JSON file; `purge-commit`
extracts those exact fields plus the idempotency key; `purge-status` recovers
the sealed receipt after response loss.

## Selection and catalog

`select --require <req>` (e.g. `durable-typed-records`) applies the server-owned
least-powerful rule: instruction-only → standalone procedure skill; typed
records, personal surfaces or background lifecycle → app; app-private reasoning
→ app with private procedures; new executable integration → separately
authorized capability. Order-independent; selection evidence only, no authority.

`tools list` / `tools show <name>`, `agents list`, `personalities list` and
`procedure list` print the catalog VibeDev uses
([app-primitive-catalog](app-primitive-catalog.md)): compiled packs from the
binary, skills and personalities from
`$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/skills`, then extra roots
(repo `skillshub/` is not searched). Scope: `--principal` / `--workspace`, else
`MAGICIAN_PRINCIPAL` / `MAGICIAN_WORKSPACE` (defaults `anonymous` / `default`);
`--skills-dir` overrides roots. Agents resolve scoped `agent_runtime`, extra
template roots, system templates, then a synthetic `personal-assistant` only
without a real default. `app pack` takes the same flags plus `--templates-dir`
and, with none, the same live scope.

- `--app-eligible` hides what the lock will refuse, showing only exact Ready
  owner leaves: a Task-callable agent's sole `agent_as_tool` action and the
  reviewed Browser, macOS and Android rosters as `<owner>__<action>` aliases,
  one leaf per snippet via `actions: [...]`. `--kind` filters by
  `compiled|skill|agent|interactive`; `--kind agent` / `--kind interactive` show
  only those leaves. Default or untyped agents and raw
  session/tab/selector/profile/CDP/AX/ADB/argv/device/transport IDs are never
  projected; there is no free-text add-any-tool path.
- The agent reader accepts the agent store's YAML forms (`!days` / `!fixed`,
  bounded block scalars, apostrophes, standalone cron wildcards); app manifests
  keep their smaller subset. A malformed non-default private agent is skipped as
  degraded; an invalid reserved default agent makes the catalog unavailable.
- Listed physical-owner descriptors whose private source is absent from the
  `check`/`pack` roots are omitted from the lock.
- Entries carry immutable `primitive_id`; friendly names work when
  `alias_available: true`. A private skill shadowing a platform name gets
  `primitive_ref: primitive:tool-skill:<digest>`; review marks it
  non-dispatchable and approval fails closed.

Authenticated HTTP twins (`kind=agent` / `kind=interactive` expose no generic
delegation or control classes):

```text
GET /api/magician/v2/apps/authoring/tools?app_eligible=true&kind=compiled
GET /api/magician/v2/apps/authoring/tools/{name}
GET /api/magician/v2/apps/authoring/agents
GET /api/magician/v2/apps/authoring/personalities
GET /api/magician/v2/apps/authoring/procedures
```

VibeDev App options on `/vibe` list the same catalog and insert YAML snippets;
the Tools tab shows "Ready in apps" first, then groups the rest under "Not yet
runnable in apps" by blocker (`partitionAuthoringToolsForApps`); blocked tools
stay selectable.

### Interactive dependencies

An interactive dependency keeps its singleton selector and declares the full
reviewed request; Browser `snapshot`/Observe shown (`navigate`, `scroll`,
`click` have their own aliases, classes, target policy, resources and
fresh-observation requirements):

```yaml
dependencies:
  tools:
    - name: browser
      version_requirement: "^1"
      actions: [snapshot]
      interactive:
        schema: magician.app-interactive-capability-request.v1
        owner: browser
        allowed_origins: [about:blank]
        target_profile_class: installation_ephemeral_headless
        target_selectors: { bundle_ids: [], package_ids: [], application_refs: [], current_reviewed_pairing: false }
        action_classes: [observe]
        background: direct_owner
        capture: structured_evidence_only
        transfer: denied
        resources:
          max_sessions: 1
          max_steps: 1
          max_duration_seconds: 300
          max_evidence_bytes: 524288
          max_evidence_nodes: 65536
          max_pixels: 0
          max_artifact_bytes: 0
          max_output_bytes: 524288
        expiry_session: { grant_lifetime_seconds: 2592000, max_session_seconds: 300, session: invocation_bound }
```

The result ceiling must equal the action's; review may narrow, never widen or
substitute. Only an exact single-snapshot dependency may omit `interactive`.
Contract: [app-interactive-capabilities](app-interactive-capabilities.md).

## Local loop

All provider-free, descriptor-pinned and bounded:

- `procedure check ./SKILL.md` parses a standalone procedure
  (`metadata.magician.skill_type: procedure`, canonical name, semver, non-empty
  description and instructions, optional valid `allowed-tools`); reports
  `skill:<name>`, version and content digest; publishes nothing.
- `init <name> --path <dir>` writes a strict manifest, fixtures and both
  generated artifacts.
- `check` admits the directory through the server's reader, validates
  manifest/contract compatibility and compares derived JSON and TypeScript;
  drift fails until `--write-generated`, which re-admits and compares again.
- `test` checks `fixtures/app-fixtures.json` (records, workflow I/O, view
  expectations) against the derived contract without a user database; unknown
  fields, unbounded JSON, duplicates and undeclared members fail.
- `preview [--view <view>]` renders views as the real MUIJ surface contract with
  fixture records (`authoritative: false`, `activation_capable: false`,
  deterministic for identical bytes).
- `pack [--output <file>]` (default `<name>-<version>.app.zip` beside the
  package) reruns compatibility, generated-output and fixture checks on one
  final snapshot, locks dependencies and writes a create-only archive outside
  the package. Archive v2 holds only `app-package.json` and `bundle/*`; its lock
  claim is untrusted and candidate publication reloads every registry byte.

### Event behaviors and owner notification ports

`app_event_behaviors_v1` adds `app.event_behaviors` and an exactly keyed
`app.resources.event_behaviors` map: one Event-triggered action per entry, the
closed
`{ source: installation_execution_terminal, outcomes: [succeeded|failed] }`
subscription, a minimum interval and bounded resources (no other events,
filters, workspace selectors, dedupe keys or cross-installation sources).
`app_owner_notifications_v1` adds workflow-local `notification_ports`
(`briefing|escalation`, `info|warning`, reviewed purpose, period volume, pending
ceiling, TTL). Omitting a feature and its fields keeps legacy identity and
denies all; fields without the feature, or the feature without resources/ports,
fail admission. `question`, `critical`, response schemas, owner answers and push
are not V1 ([threat model](app-events-owner-notifications-threat-model.md)).

## Publish an external candidate

`candidate-publish` (verified session, exact owner workspace) re-admits a packed
archive, verifies artifacts and fixtures, reproduces the built-in lock, and
atomically publishes an inert `ready_for_review` installation with the local
actor as publisher (the archive's publisher is advisory). HTTP requires package
ID, source publisher and content digest headers; exact replay returns the same
identities; the receipt reports `activation_authority_granted: false`.

Approve/enable is a separate owner surface on the same commit kernel
(`GET /api/magician/v2/apps/installations/{installation_id}/review`,
`POST /api/magician/v2/apps/installations/{installation_id}/approve`):

- `approve` reaches `commit_reviewed_installation` via the loopback owner (the
  CLI has no registry or staging handle). The commit is built before any
  approval row is published, so a failed check leaves no unconsumed approval; a
  retry on the enabled package returns `already_enabled`.
- Omitting `--grant-*` (e.g. `--grant-tool content_read`) grants every requested
  tool, agent and personality and each granted tool's displayed interactive
  request (the locked Observe request, never all actions); an empty interactive
  selection denies all.
- Grants store `capability:{tool}`, `agent:{name}`, `personality:{name}` (bare
  names canonicalized); per-run `max_tokens` prefers output, then input; only
  approved-destination packages get non-zero `max_browser_network_actions`.
- Review shows dispatch readiness ([app-tool-bind](app-tool-bind.md));
  unchecking a tool or the default runner makes its workflows inert without
  blocking enablement. The Apps directory **Needs attention** section offers the
  same **Approve and enable**.
- `/packages/import` only stages bytes, before conformance or review.

## Publish a standalone procedure revision

When `select` chooses `procedure_skill`, publish the exact `SKILL.md` (not a
copy in runtime `skillshub`):

```bash
curl --fail-with-body \
  -H 'Content-Type: application/vnd.magician.procedure-skill+markdown' \
  --data-binary @SKILL.md \
  http://127.0.0.1:3002/api/magician/v2/apps/procedure-revisions
```

Same verified session. Magician parses identity from the document, allocates the
next positive revision for scoped `skill:<name>` in one immediate transaction,
derives a content-bound immutable revision reference and stores the bytes
write-once; exact replay returns the same identity, a reused version with
different bytes conflicts. The receipt reports
`global_skill_catalog_published: false` and
`activation_authority_granted: false`. Copy its reference, version, immutable
reference and revision into the `pack` resolution file (`content_path` names the
local `SKILL.md`, whose digest must match); the candidate server reloads that
exact revision before review.

## Depend on an existing tool, or publish a new one

```yaml
dependencies:
  tools:
    - name: content_read
      version_requirement: "^1"
workflows:
  summarize:
    runner: auto
    agent: research-agent
    personality: brutal
    uses: [content_read, search_memory, create_task]
```

Apps lock the reviewed typed skills and compiled packs agents use (for
example `content_read`, `http`, `files`, `macos_automation`, `search_memory`,
`create_task`) as `capability:{name}`; the grant is the user-facing control.
A skill is app-eligible with a Universal Skill Runtime contract and typed
actions unless `expose.apps: false`. Procedures, personalities, shell binaries
and raw-argv passthrough are not tools. `dependencies.capabilities` is a
one-release read of the old field; never declare both. A workflow's `agent` and
`personality` are runner selections needing grants (`agent:research-agent`,
`personality:brutal`); if any workflow omits `agent`, the default
`personal-assistant` is requested so the owner can deny it.

For `executable_capability`, or a missing tool, run
`magician app capability check ./SKILL.md` and post it with
`Content-Type: application/vnd.magician.capability-skill+markdown` to
`http://127.0.0.1:3002/api/magician/v2/apps/capability-revisions`. It needs
`metadata.magician.skill_type: tool` and a Universal Skill Runtime contract with
typed actions; bytes are stored write-once under `capability:{name}` with the
same `false` flags, resolved by immutable revision only. VibeDev companions:
`{app-dir}.tool.md`, or `{stem}.procedure.md` for a tool-primary handoff.

## Verified VibeDev handoff

VibeDev never publishes a live directory or a model-asserted "verified" flag.
After a sealed green gate attestation (bound to scope, project, root
task/execution, candidate and gate generation), the server recaptures the
attested snapshot and reloads the only claim a verified build may write,
`.magician/app-artifact-handoffs/<root-task-id>.json`, and the app directory (a
contained relative path). Package admission is compared member-for-member with
the green snapshot, then captured again, before the candidate enters the same
`ready_for_review` publisher as an SDK archive:

```json
{ "schema_version": 1, "requirements": ["durable_typed_records"], "artifact_path": "reading-list" }
```

The claim has no artifact-kind, verification or activation field; the
`magician app select` selector picks an app candidate, standalone procedure or
executable capability (app-plus-executable also publishes sibling
`{dir}.tool.md`). Observe mode records evidence but never publishes; builds
without a handoff stay ordinary builds.

## Generated output

From the manifest the CLI derives `.magician/app-derived.json` (canonical
manifest, data-policy, resource and dependency requirements, workflow
input/result schema identities, TypeScript generator version and digest, and a
derivation digest) and `sdk/app.generated.ts` (contract/version constants,
entity/workflow/action types, schema descriptors and codecs, form metadata,
typed custom-surface bridge/run control, opaque handles, Recipe builders).
Neither grants authority; both are package members bound by the bundle digest
and lock and must stay byte-current (`check --write-generated` reaches a fixed
point).

## Immutable dependency evidence

The canonical `magician_contract` v1 bytes are compiled into the CLI and cannot
be overridden. Other dependencies come from a resolution file passed to `pack`:

```json
{
  "schema_version": 1,
  "dependencies": [
    {
      "kind": "procedure_skill",
      "dependency_ref": "skill:example",
      "semantic_version": "1.2.3",
      "immutable_revision_ref": "skill-revision:example-1.2.3",
      "revision": 7,
      "content_path": "content/example.skill"
    }
  ]
}
```

Content paths must be bounded relative paths under the file's directory, outside
the package; symlinks, traversal, absolute paths, replacement during reads and
exceeded counts/bytes fail closed. The lock is local reproducibility evidence,
not registry authority: candidate publication independently resolves and
revalidates everything, and review, qualification, approval and enablement stay
separate.

Tests: `make test-app-authoring`. See the
[app-platform contract kernel](app-platform-contract-kernel.md), the
[threat model](app-platform-threat-model.md), and the archived
implementation design.
