# App authoring CLI

`magician app` is the provider-free authoring loop for declarative
`skill_type: app` packages. Catalog, init, check, test, preview and pack run
before either Tokio runtime, live configuration, providers, procedure
maintenance or the HTTP service starts. Lifecycle and portability commands are
the exception: a small authenticated loopback HTTP client calls the mounted
server owners and never touches registry files behind the running service.
`magician app --help`, `pack --help`, `tools --help`, `approve --help` and
`capability --help` describe the exact arguments.

## Machine-readable mode

Put `--json` immediately after `app` to get exactly one compact, versioned JSON
object on stdout:

```bash
magician app --json check ./reading-list
magician app --json review <installation_id> --principal owner --workspace default
magician app --json approve <installation_id> --review-digest blake3:<hex> \
  --principal owner --workspace default
```

Success: `{schema_version, cli_protocol_version, command, ok: true, result}`.
Failures after successful Clap parsing:
`{schema_version, cli_protocol_version, command, ok: false, error: {code, message}}`,
classified without paths, URLs, credentials, tokens or error chains. JSON mode
installs no tracing subscriber, so logs cannot interleave. Exit `0` success,
`1` command failure envelope, `2` Clap usage/help error. Live commands are a
closed route client for the lifecycle kernels below — not a generic
HTTP/JSON-RPC, shell or argv bridge; the API origin must be literal loopback,
redirects and proxies are disabled, and principal and workspace are mandatory.

## Commands

```bash
magician app select --require durable-typed-records --require interactive-personal-surface
magician app tools list --app-eligible
magician app tools list --kind compiled
magician app tools show content_read
magician app agents list
magician app personalities list
magician app procedure list
magician app procedure check ./SKILL.md
magician app capability check ./SKILL.md
magician app init reading-list --path ./reading-list
magician app check ./reading-list
magician app check ./reading-list --write-generated
magician app test ./reading-list
magician app preview ./reading-list
magician app preview ./reading-list --view items
magician app pack ./reading-list --output ./reading-list-0.1.0.app.zip
magician app list --principal owner --workspace default
magician app detail <installation_id> --principal owner --workspace default
magician app review <installation_id> --principal owner --workspace default
magician app approve <installation_id> --review-digest blake3:<hex> \
  --principal owner --workspace default
magician app disable <installation_id> --expected-generation 4 \
  --request-id lifecycle:disable-1 --principal owner --workspace default
magician app quarantine <installation_id> --expected-generation 4 \
  --request-id lifecycle:quarantine-1 --principal owner --workspace default
magician app uninstall-retain <installation_id> --expected-generation 4 \
  --request-id lifecycle:uninstall-1 --principal owner --workspace default
magician app grant-revoke <installation_id> --expected-generation 4 \
  --request-id lifecycle:revoke-1 --principal owner --workspace default
magician app update-begin <installation_id> --expected-generation 4 \
  --request-id lifecycle:update-1 --principal owner --workspace default
magician app update-abort <installation_id> --expected-generation 5 \
  --request-id lifecycle:abort-1 --principal owner --workspace default
magician app update-plan <installation_id> --attempt-id attempt:update-1 \
  --expected-generation 5 --operations-file ./migrations.json \
  --principal owner --workspace default
magician app update-backup <migration_run_id> --passphrase-file ./archive.passphrase \
  --principal owner --workspace default
magician app rollback-code <installation_id> --migration-run-id <migration_run_id> \
  --expected-generation 6 --request-id lifecycle:rollback-1 \
  --principal owner --workspace default
magician app rewind-preview <migration_run_id> --passphrase-file ./archive.passphrase \
  --principal owner --workspace default
magician app rewind-commit <migration_run_id> --preview-digest blake3:<hex> \
  --request-id lifecycle:rewind-1 --passphrase-file ./archive.passphrase \
  --confirm-data-rewind --principal owner --workspace default
magician app reenable-review <installation_id> --principal owner --workspace default
magician app reenable <installation_id> --expected-generation 5 \
  --request-id lifecycle:reenable-1 --review-digest blake3:<hex> \
  --principal owner --workspace default
magician app package-export <installation_id> --output ./app-export.zip \
  --principal owner --workspace default
magician app package-import ./app-export.zip --principal owner --workspace default
magician app candidate-publish ./app-export.zip --request-id candidate:publish-1 \
  --principal owner --workspace default
magician app data-export <installation_id> --kind data \
  --request-id data-export:1 --passphrase-file ./archive.passphrase \
  --output ./app-data.appdata --principal owner --workspace default
magician app data-export <installation_id> --kind combined \
  --request-id combined-export:1 --passphrase-file ./archive.passphrase \
  --output ./app-migration.appdata --principal owner --workspace default
magician app data-import-preview <installation_id> ./app-data.appdata \
  --request-id data-preview:1 --passphrase-file ./archive.passphrase \
  --principal owner --workspace default
magician app data-import-approve <installation_id> \
  --preview-digest blake3:<hex> --request-id data-approval:1 \
  --principal owner --workspace default
magician app data-import-commit <installation_id> \
  --preview-digest blake3:<hex> --approval-ref approval:data-import:<hex> \
  --request-id data-commit:1 --principal owner --workspace default
magician app purge-preview <installation_id> --principal owner --workspace default
magician app purge-commit <installation_id> --preview ./purge-preview.json \
  --idempotency-key blake3:<hex> --principal owner --workspace default
magician app purge-status blake3:<hex> --principal owner --workspace default
```

**Lifecycle rules.** Every generation-bearing mutation needs a caller-retained
`request-id` and reuses exact request bytes on retry. `approve` needs the exact
`workflow_material_digest` from `review`; `reenable` needs the `review_digest`
from `reenable-review`. Update and reinstall commit through the same reviewed
`approve` after `update-begin` and candidate publication.

**Updates and rollback.**

- `update-plan` (`POST /api/magician/v2/apps/installations/{installation_id}/update-plans`)
  compiles and dry-runs one migration; it needs the parked
  `--expected-generation` and `--attempt-id` from candidate publication and an
  optional `--operations-file` (JSON array of exact operation bodies; omitted =
  infer the unique safe V1 plan); no request-id.
- Destructive receipts set `backup_required`; `update-backup`
  (`POST /api/magician/v2/apps/updates/{migration_run_id}/backup`) takes
  `--passphrase-file` only. `approve` then consumes `--migration-run-id`,
  `--update-plan-digest`, and `--confirm-destructive-migration` when
  destructive.
- `rollback-code` (`POST .../installations/{installation_id}/rollbacks/code`)
  restores the previous package/surface after a switched code-only update,
  keeps post-update writes, does not restore grants.
- Data rewind: `rewind-preview` then `rewind-commit`
  (`POST /api/magician/v2/apps/updates/{migration_run_id}/rewind-preview` and
  `.../rewind-commit`), the latter needing `--preview-digest`, request-id, the
  same passphrase file and `--confirm-data-rewind`.
- Passphrases are read from a bounded regular no-follow file and never written
  into a receipt.

**Package transfer.** `package-import` admits a bounded no-follow package-only
archive and posts it to inert staging (local identity, conformance, permission
review and executable rebuild still required; no foreign grants).
`package-export` accepts only the package-only media type, caps the response,
writes a private sibling staging file, fsyncs, and publishes by create-new hard
link (never overwrites; no partial final path). Package archives carry no record
data, credentials, scope IDs, grants, schedules, memory or secrets.
`candidate-publish` sends the same exact bytes plus package ID, source publisher,
content digest and request identity; the server reruns conformance and creates
only an inert `ready_for_review` candidate (exact retry returns the same
identities).

**Data archives.** Data and combined exports default to a versioned
chunk-authenticated XChaCha20-Poly1305 envelope with an Argon2id passphrase key,
sent only to the loopback archive owner. `--explicit-plaintext` is a separate
warned action refused for Secret data. Imports authenticate and validate locally,
then follow server `preview -> approve -> commit`; conflicts are reported and
skipped; destination IDs are minted by the entity-store transaction. For a new
scope, candidate publication and approval come first (combined-archive package
bytes are compatibility evidence, not install permission); then preview the data
into the enabled installation. An exact commit retry reopens the durable import
receipt.

**Purge.** `purge-preview` emits server generation, inventory digest and time
window; save its result (or the full CLI envelope) to a bounded JSON file.
`purge-commit` extracts those exact fields plus the idempotency key.
`purge-status` recovers the sealed receipt after response loss.

## Selection and catalog

`select` applies the server-owned least-powerful rule to explicit requirements:
instruction-only → standalone procedure skill; typed records, personal surfaces
or background lifecycle → app; app-private reasoning → app with private
procedures; new executable integration → a separately authorized capability.
Order-independent; selection evidence only, no authority.

`tools list`, `tools show`, `agents list`, `personalities list` and
`procedure list` print the catalog VibeDev App options consume (see
[app-primitive-catalog](app-primitive-catalog.md)). Compiled packs come from the
binary; skills and personalities from
`$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/skills` then configured extra
roots (repo `skillshub/` is the install source, not a search root). Scope comes
from `--principal` / `--workspace` or `MAGICIAN_PRINCIPAL` / `MAGICIAN_WORKSPACE`
(defaults `anonymous` / `default`); `--skills-dir` overrides roots. Agents follow
runtime precedence: scoped `agent_runtime`, extra template roots, primary system
templates, then synthetic `personal-assistant` only when no real default exists.

- `--app-eligible` hides tools the lock will refuse. The projection includes
  only exact Ready owner leaves: a Task-callable agent's sole `agent_as_tool`
  action and the reviewed Browser, macOS and Android rosters, exposed as
  `<owner>__<action>` aliases; emitted YAML always selects exactly one leaf with
  `actions: [...]`. `--kind agent` / `--kind interactive` show only those.
  Default or untyped agents and raw session/tab/selector/profile/CDP/AX/ADB/argv/
  device/transport IDs are never projected; there is no free-text add-any-tool
  path.
- The agent-definition reader accepts the agent store's YAML forms (bounded
  block scalars, `!days` / `!fixed`, apostrophes, standalone cron wildcards);
  app manifests keep their smaller subset. A malformed non-default private agent
  is skipped as a degraded source; an invalid reserved default agent makes the
  catalog unavailable.
- Discovery may list reviewed physical-owner descriptors whose private source is
  absent from a given `check`/`pack` roots; the lock resolver omits them.
- Entries include immutable `primitive_id`. Friendly names suffice when
  `alias_available: true`; for a private skill colliding with a reserved platform
  name, `tools list/show` emits a snippet with
  `primitive_ref: primitive:tool-skill:<digest>`. Review currently marks such a
  dependency non-dispatchable and approval fails closed.
- `app pack` accepts the same `--principal`, `--workspace`, `--skills-dir`,
  `--templates-dir` arguments and, with none, the same live scope as
  `tools list/show`.

Live HTTP twins (authenticated):

```text
GET /api/magician/v2/apps/authoring/tools?app_eligible=true&kind=compiled
GET /api/magician/v2/apps/authoring/tools/{name}
GET /api/magician/v2/apps/authoring/agents
GET /api/magician/v2/apps/authoring/personalities
GET /api/magician/v2/apps/authoring/procedures
```

`kind=agent` and `kind=interactive` expose no generic delegation or control
classes.

VibeDev App options on `/vibe` list the same catalog and insert YAML snippets.
The Tools tab shows "Ready in apps" first and groups the rest under "Not yet
runnable in apps" by blocker (`partitionAuthoringToolsForApps`); blocked tools
stay selectable.

### Interactive dependencies

An interactive dependency keeps its exact singleton selector and declares the
full reviewed request. The Browser `snapshot`/Observe compatibility form
(`navigate`, `scroll`, `click` use their own aliases, classes, target policy,
resources and fresh-observation requirements):

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
        target_selectors:
          bundle_ids: []
          package_ids: []
          application_refs: []
          current_reviewed_pairing: false
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
        expiry_session:
          grant_lifetime_seconds: 2592000
          max_session_seconds: 300
          session: invocation_bound
```

The result ceiling must equal the selected action's. Review may narrow any
dimension but never widen or substitute. Omitting `interactive` works only for an
already exact single-snapshot dependency. See
[app-interactive-capabilities](app-interactive-capabilities.md).

## Local loop

- `procedure check` reads one descriptor-pinned bounded `SKILL.md` with the exact
  standalone-procedure parser: `metadata.magician.skill_type: procedure`,
  canonical name, semver, non-empty description and instructions, optional valid
  `allowed-tools`. Reports `skill:<name>`, version and content digest; publishes
  nothing.
- `init` creates a strict manifest, a bounded provider-free fixture suite and the
  two generated artifacts.
- `check` admits the directory through the server's descriptor-pinned bounded
  reader, validates manifest and contract compatibility, and compares derived
  JSON and TypeScript; drift fails until `--write-generated`, which re-admits and
  compares again.
- `test` validates bounded entity records, workflow inputs/results and view
  minimum-record expectations from `fixtures/app-fixtures.json` against the
  derived contract (unknown fields, unbounded JSON, duplicate fixtures and
  undeclared members rejected). No provider, no user database.
- `preview` compiles every view (or `--view`) into the real MUIJ surface
  contract with fixture records; it is `authoritative: false`,
  `activation_capable: false`, and deterministic for identical bytes.
- `pack` admits one final snapshot, reruns compatibility, generated-output and
  fixture checks, locks dependencies and writes a create-only archive outside the
  package directory. Portable archive v2 holds only `app-package.json` and
  `bundle/*`; the manifest carries the full dependency-lock claim (treated as
  untrusted by candidate publication, which reloads every registry-backed byte).

### Event behaviors and owner notification ports

`app_event_behaviors_v1` adds a top-level `app.event_behaviors` list plus an
exactly keyed `app.resources.event_behaviors` map. Each entry names one
Event-triggered action, the closed
`{ source: installation_execution_terminal, outcomes: [succeeded|failed] }`
subscription, a minimum interval and bounded resources — no arbitrary event
names, filters, workspace selectors, dedupe keys or cross-installation sources.

`app_owner_notifications_v1` adds a workflow-local `notification_ports` map:
`briefing|escalation`, `info|warning`, reviewed purpose, period volume, pending
ceiling and TTL. Omitting a feature and its fields keeps legacy identity and
means deny-all; fields without the feature, or the feature without its
resources/ports, fail admission. `question`, `critical`, response schemas, owner
answers and push are not V1 surfaces. See the
[threat model](app-events-owner-notifications-threat-model.md).

## Publish an external candidate

Claude, Codex, another SDK client or a human can submit a packed archive to the
same reviewed-candidate lifecycle VibeDev uses:

```bash
magician app --json candidate-publish reading-list-0.1.0.app.zip \
  --request-id candidate:reading-list-1 \
  --principal owner --workspace default
```

This needs a verified session and exact owner workspace. The server rebuilds the
archive through bounded admission, verifies generated SDK artifacts and fixtures,
reproduces the trusted built-in dependency lock, and atomically publishes an
inert `ready_for_review` installation. The local actor becomes the candidate
publisher (the archive publisher is advisory). The HTTP boundary requires the
package ID, source publisher and content digest headers; exact replay returns the
same identities. The receipt always reports
`activation_authority_granted: false`.

Owner approve/enable is a separate surface on the same reviewed commit kernel:

```bash
magician app review <installation_id> --principal anonymous --workspace default
magician app approve <installation_id> --review-digest <workflow_material_digest> \
  --grant-tool content_read --principal anonymous --workspace default
```

```text
GET  /api/magician/v2/apps/installations/{installation_id}/review
POST /api/magician/v2/apps/installations/{installation_id}/approve
```

- `approve` calls the loopback HTTP owner, which calls
  `commit_reviewed_installation`; the CLI has no registry or staging handle. The
  consume-side commit is built before any approval row is published, so a failed
  check cannot leave an unconsumed approval. A retry against the same enabled
  package returns `already_enabled`.
- Omitting `--grant-*` grants every requested tool, agent and personality and
  the displayed interactive request per granted tool (the locked Observe request
  only, never all actions); an explicit empty interactive selection denies all.
- Grants store `capability:{tool}`, `agent:{name}`, `personality:{name}` (bare
  names are canonicalized). Per-run `max_tokens` prefers output, then input.
  Approved-destination packages get a non-zero
  `max_browser_network_actions`; denied egress stays zero.
- Review lists each tool's dispatch readiness (classes and wired binders:
  [app-tool-bind](app-tool-bind.md)). Unchecking a tool or the default runner
  does not block enablement; review names the workflows that become inert.
- The Apps directory **Needs attention** section offers the same review →
  **Approve and enable** → **Installed**.
- The lower-level `/packages/import` endpoint only stages bytes and stops before
  conformance or review.

## Publish a standalone procedure revision

When `select` chooses `procedure_skill`, publish the exact `SKILL.md` without
copying it into runtime `skillshub`:

```bash
magician app procedure check ./SKILL.md

curl --fail-with-body \
  -H 'Content-Type: application/vnd.magician.procedure-skill+markdown' \
  --data-binary @SKILL.md \
  http://127.0.0.1:3002/api/magician/v2/apps/procedure-revisions
```

Same verified session as candidate publication. Magician parses identity from
the document, allocates the next positive revision for scoped `skill:<name>` in
one immediate transaction, derives a content-bound immutable revision reference
and stores the bytes write-once. Exact replay returns the same identity; reusing
a version with different bytes conflicts. The receipt reports
`global_skill_catalog_published: false` and `activation_authority_granted: false`.
Copy its dependency reference, version, immutable revision reference and numeric
revision into the external resolution file for `pack` (`content_path` names the
local `SKILL.md`, whose digest must match); the candidate server reloads that
exact revision before review.

## Depend on an existing tool, or publish a new one

```yaml
dependencies:
  tools:
    - name: content_search
      version_requirement: "^1"
```

The engine snapshots an existing reviewed typed skill or typed compiled pack
(`content_read`, `http`, `files`, `macos_automation`, `search_memory`,
`create_task`, …) into the lock as `capability:{name}`; apps lock the same tools
agents use, and the grant is the user-facing control. A skill is app-eligible
with a Universal Skill Runtime contract and typed actions; omitted `expose.apps`
is eligible, explicit `false` denies. Procedures, personalities, shell binaries
and raw-argv passthrough are not tools. `dependencies.capabilities` is a
one-release read of the old field; do not declare both.

A workflow may hire an agent and optionally a personality (runner selections,
not tools). The default runner is `personal-assistant`; if any workflow omits
`agent`, that default is requested on the grant so the owner can deny it.

```yaml
workflows:
  summarize:
    runner: auto
    agent: research-agent
    personality: brutal
    uses: [content_read, search_memory, create_task]
```

The grant must include `agent:research-agent` and `personality:brutal`.

When `select` chooses `executable_capability`, or the tool does not exist,
publish one `SKILL.md` (ordinary tool publication):

```bash
magician app capability check ./SKILL.md

curl --fail-with-body \
  -H 'Content-Type: application/vnd.magician.capability-skill+markdown' \
  --data-binary @SKILL.md \
  http://127.0.0.1:3002/api/magician/v2/apps/capability-revisions
```

It must declare `metadata.magician.skill_type: tool` and a Universal Skill
Runtime contract with typed actions. Bytes are stored write-once under
`capability:{name}`; the receipt reports `global_skill_catalog_published: false`
and `activation_authority_granted: false`. Resolver lookup is by immutable
revision only. For a VibeDev app needing a new integration, the companion file
is `{app-dir}.tool.md`; a tool-primary handoff may carry `{stem}.procedure.md`.

## Verified VibeDev handoff

VibeDev never publishes a live directory or a model-asserted "verified" flag.
Its server-owned handoff loads the verification gate and sealed green
attestation, binds scope, project, root task/execution, candidate and gate
generation, and recaptures the attested repository snapshot. The app directory
must be a contained relative path; its package admission is compared
member-for-member with the green snapshot, followed by a second capture, before
the candidate enters the same `ready_for_review` publisher as an SDK archive.

A verified build may write only the task-bound claim
`.magician/app-artifact-handoffs/<root-task-id>.json`:

```json
{
  "schema_version": 1,
  "requirements": ["durable_typed_records"],
  "artifact_path": "reading-list"
}
```

The claim has no artifact-kind, verification or activation field. After the gate
is green, Magician reloads claim and artifact from the attested snapshot, runs
the same selector as `magician app select`, and publishes an app candidate,
standalone procedure or executable capability (app-plus-executable publishes both
with the sibling `{dir}.tool.md`). Observe mode records evidence but never
publishes. Builds without a handoff stay ordinary builds.

## Generated output

The manifest is the source of truth. The CLI derives:

- `.magician/app-derived.json` — canonical manifest/data-policy/resource and
  dependency requirements, workflow input/result schema identities, TypeScript
  generator version and generated-TypeScript digest, plus a derivation digest;
- `sdk/app.generated.ts` — contract/version constants, named
  entity/workflow/action types, input/result schema descriptors and codecs,
  form metadata, typed custom-surface bridge/run control, opaque handles, and
  the supported Recipe builders.

Neither grants authority. Both are package members bound by the bundle digest
and lock, and must stay byte-current; `check --write-generated` verifies a
deterministic fixed point.

## Immutable dependency evidence

The canonical `magician_contract` v1 bytes are compiled into the CLI and cannot
be overridden. Other dependencies come from an external resolution file passed
to `pack`:

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

Content paths must be bounded normal relative paths under the resolution file's
directory, which must be outside the package. Symlinks, traversal, absolute
paths, replacement during reads and exceeded counts/bytes fail closed. The lock
is local reproducibility evidence, not registry authority: candidate
publication independently resolves and revalidates everything, and review,
qualification, approval and enablement remain separate.

Tests: `make test-app-authoring`.

See the [app-platform contract kernel](app-platform-contract-kernel.md), the
[threat model](app-platform-threat-model.md), and the archived
implementation design.
