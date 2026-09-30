# App primitive catalog

The app primitive catalog is the internal identity and discovery source for
capabilities that apps may compose. It is not the supported-public Apps
operation inventory (owned by `magician-app-contract`), does not grant
authority, and does not create another executor. Public contracts may consume a
narrow immutable projection; they must not serialize this internal descriptor
type as their authority.

## Projection

`primitive_catalog.rs` builds one bounded immutable snapshot from the
compiled-pack registry and the current scoped skill and agent roots. It covers:

- compiled tools, including browser, macOS-host and Android-owner execution
  classifications;
- normal Universal Skill Runtime tool skills;
- procedure skills;
- agent definitions and the default workflow runner;
- interactive actions through the existing compiled physical owners;
- the experience admission primitives `overlay-draw` and `narration`
  (`experience_capability.rs`): EmbeddedPlatform interactive descriptors with
  one conditional leaf each. They are admission vocabulary only — `Conditional`,
  non-dispatchable, and refused by installation review until a reviewed
  physical-owner consumer exists. Voice-invocation admission is stopped (see
  [app-interactive-capabilities.md](app-interactive-capabilities.md)).

Each primitive has a content-addressed qualified identity, descriptor digest,
source identity and digest, kind, semantic version, execution and containment
classes, exposure flags, eligibility, dispatch status, invocation modes,
declared tool selectors and exact action descriptors. Each action has its own
identity and digest, bounded input/result schema state, effects, resources and
dispatch status.

Three decisions stay separate:

1. `app_eligible` — can participate in an app lock under the reviewed
   eligibility rule.
2. `lockable` — the descriptor eligibility decision; `discoverable_only` and
   `blocked` are explicit alternatives.
3. `dispatchable` — the current execution/containment owner can run at least
   one admitted action. A lockable OS-jail skill can be non-dispatchable without
   being reported absent. Reviewed MCP skills lock through the projected
   official-SDK actions and the governed-MCP contain profile, not CLI
   `runtime_actions`.

### Narrow Apps projections

- **HTTP.** Only the exact `get` action is Ready. The physical owner keeps the
  reviewed DNS answer in a move-only prepared dispatch, disables process
  proxies, connects only to those addresses while keeping the URL host for Host
  and SNI, and handles redirects itself: each hop must stay on the admitted
  origin and is re-resolved, public-address checked and pinned. Request bytes,
  pins, redirect policy and the 4 MiB result ceiling are in the physical target
  digest. The embedded provider bytes are registry-reserved, so a same-named
  scoped pack cannot inherit the witness. Other methods, request bodies,
  request-identity headers and response overflow are denied.
- **Host reads.** `internal_data` keeps its broad diagnostics vocabulary for
  agents, but Apps see exactly the two scoped learning review reads;
  `thinking_maps_data` projects exactly `list_maps`/`read_map` (map mutation
  stays on the first-party API). See [`app-tool-bind.md`](app-tool-bind.md) for
  binders, scope ownership, argument proof and ceilings.
- **Files/DuckDB.** Built-in `files.read`, `files.write`, `duckdb.preview` and
  `duckdb.describe` share one capability-directory owner: opens the reviewed
  root once, no-follow traversal, rejects links/special files/mount escapes,
  keeps target identity through I/O, bounds bytes, and writes via atomic fsynced
  create-new or compare-exchange. Table execution gets a descriptor path, not
  the ambient path. List/exists, append/delete/copy/move/mkdir, raw SQL,
  persistent databases and table globs are blocked.
- **Interactive.** Ready leaves: browser `{snapshot, navigate, scroll, click}`;
  macOS `{launch, focus, snapshot, click, type, key, scroll, drag}`; Android
  `android_snapshot.snapshot`, `android_screenshot.screenshot`,
  `android_act.{tap, type, key, scroll}`, `android_app.{launch, close}`. Raw CLI
  descriptors are not app dispatch contracts. Android is typed-unavailable
  without nonempty reviewed attestation/Play pins, a usable Play credential and
  code-verified desktop trust; device/package authority comes only from
  hardware-attested Apps enrollment and trusted Settings review. macOS Ready
  admission needs the fixed-loopback owner-code/Keychain Ed25519 identity proof
  ([app-macos-pairing.md](app-macos-pairing.md)).
- **Agents.** Blocked unless the exact non-default definition is enabled for
  the Task surface and declares the bounded `app_tool` contract; that one sealed
  `agent_as_tool` leaf is Ready only when schemas, byte ceilings,
  `AgentTask`/`TaskOwner` profile, single-action mode, source digest and
  Artifact V3 owner digest reproduce the lock. Procedure entries are
  discoverable-only.
- Result schemas are `undeclared` where the provider or skill has no typed
  result schema.

## Canonical eligibility rule

A skill in the reviewed scoped catalog is a tool when it has a valid Universal
Skill Runtime contract; it need not repeat `skill_type: tool`. CLI packages need
typed `runtime_actions`. MCP packages use official-SDK catalog projection
(`status`, `auth_start`, `list_tools`, `call_tool`, `clear_auth`) and must not
declare CLI actions. Standalone publication is an unreviewed boundary and needs
the explicit tool kind. `expose.apps: false` is always a deny. This is the
existing `tool_eligibility` rule, reused by the descriptor builder and
lock-evidence projection.

Malformed contracts, failed action-override compilation, missing input schema,
oversized material, resolver overflow and scan failure never become runnable
placeholders. A CLI tool without `runtime_actions` keeps one discoverable root
action, blocked along with its parent.

A tool that cannot dispatch is listed with a display-only `dispatch_note`
carrying the bind planner's reason (e.g. "skillshub/CLI tools need OS-jail
contain before they can run in apps") when the descriptor/action reasons name no
cause.

## Identity and collision rules

Platform names are reserved. A same-named private skill stays visible by its
qualified identity, while friendly-name resolution selects the platform
primitive. Two private primitives sharing an alias have no alias (callers use an
exact identity); two embedded platform primitives sharing an alias make the
whole snapshot unavailable rather than picking one by sort order.

Manifests may add `primitive_ref` to a `dependencies.tools` entry: the workflow
still uses the friendly `name`, while lock resolution selects the exact
`primitive:tool-skill:<digest>` source. A bare name is accepted only when the
snapshot says its alias is available; an exact selector also stops same-named
registry or platform evidence from satisfying the dependency. Aliases are
discovery conveniences only; locks bind exact source bytes, and action
identities include the parent identity and exact action contract.

Only exact manifest-selected action subsets are lockable, revalidated by action
identity/digest; unknown, duplicate or friendly-name-only substitutions fail
closed. Generic `primitive_ref` shapes, unsupported subsets and missing
implementation identity stay non-dispatchable.

## Bounds and deferred loading

Limits: 64 resolver roots / 64 KiB root path material, 512 scanned
entries/primitives, 128 actions per primitive, 64 KiB per inline schema, 16 MiB
retained lock-source material, 256 KiB per agent definition, 32 search results.
Schema size is counted through a bounded writer before hashing. Search is
schema-free except bounded per-action summaries (optional schema digests, exact
identities); full schemas load only from the selected snapshot.

Scanning is strict and streaming. Missing/unreadable configured roots, entry
failures, symlink roots, oversized files and overflow make the snapshot
unavailable. A syntactically invalid private document is quarantined without
erasing valid embedded primitives; authoring lists report `status: degraded`
(VibeDev: "Some tool sources could not be loaded") while unrelated exact locks
stay usable. The agent-definition preflight forbids anchors, aliases and tags
other than `!days` / `!fixed`, but reads a `*` inside a plain scalar as text
(serde_yaml writes crons unquoted, e.g. `schedule: 40 */2 * * *`); a `*` that
opens a node (`key: *a`, `- *a`, `[*a]`) is refused.

Ordered roots preserve runtime precedence: scoped skills and agents win over
configured fallbacks; configured skill roots follow scoped skills; configured
agent-template roots precede the primary system template. A malformed winner
never reactivates a lower source. Governed `SKILL.md` symlinks go through the
bounded skill reader.

Construction does all filesystem I/O before publishing the immutable `Arc`
snapshot; resolution, search and action loading do no I/O and take no global
lock. `AppPrimitiveSnapshotBinding` records the catalog owner reference and
snapshot digest — identity, not lifecycle authority; dispatch still rechecks
installation, grant, source lifecycle, physical owner, resources and
cancellation.

## Consumers

- CLI and HTTP authoring list/show consume the same snapshot and expose exact
  identities and digests in internal authoring DTOs. The public authoring tool
  projection admits ordinary compiled/skill tools, the sealed Ready
  `agent_as_tool` leaf and the Ready interactive rosters above. List YAML emits
  one explicit action selector per primitive (`snapshot` for browser / macOS /
  `android_snapshot`; `screenshot` / `tap` / `launch` for the other Android
  packs). Generic Agent, default runner, delegation and raw interactive CLI verbs
  are absent.
- `magician app pack` projects lockable, unambiguous normal-tool source bytes
  from the default scoped snapshot. Agent definitions use a distinct catalog
  entry (integer revision normalized to semver, e.g. `1.0.0`) and bind their
  source plus the selected `agent_as_tool` action. Explicit resolution files
  still provide registry/procedure evidence.
- SDK candidate publication and the VibeDev handoff build the snapshot for the
  authenticated workspace on a blocking worker before publication locks and
  reproduce the exact dependency lock. Portable replay is external-evidence-only
  and never treats a missing skill as an empty catalog.
- Embedded compiled tools resolve from process-owned embedded YAML; private
  collisions cannot replace them.

## First-party packages

All three wrap an existing owner rather than copying it, keep their colocated
`app/` directory invisible to the owner's scanner, use the minimum V1 surface
(entities and views may not be empty), do not check in generated artifacts
(`.magician/app-derived.json`, `sdk/app.generated.ts`; publish via
`magician app check --write-generated`, then `app test`/`app pack`), and change
nothing about the underlying owner when installed, disabled or uninstalled.

## First-party package: harness-SRE reliability (plan 2.2)

`magician_data_v3/system/agent_templates/agents/harness-sre/app/` wraps the
harness-SRE agent template one directory up.

- The single `run_audit` workflow declares `agent: harness-sre`. Review resolves
  it through the scoped `AgentDefinitionStore`, applies
  `agent_definition_permits_app_task`, and seals the definition digest; each run
  revalidates it and fails closed on drift. The definition has no `app_tool`
  contract, so it is a runner, not a callable tool.
- Template discovery reads only `agents/<id>/definition.agent.yaml`, so `app/`
  is invisible to it.
- Surface: one `reliability_audit` entity, one list view, one user-triggered
  action, no dependencies, contribution ports, LLM operations or interactive
  owners; `local_only` / `memory_promotion: denied`.
- The scheduled autonomous loop keeps launching the same definition either way.

## First-party package: youtube-search coverage (plan 2.4)

`skillshub/youtube-search/app/` publishes over the `youtube-search` skillshub
pack one directory up.

- `find_videos` declares `uses: [youtube-search]` and
  `dependencies.tools: [{name: youtube-search, version_requirement: "^0.3"}]`.
  The lock snapshots the pack's `SKILL.md` bytes into
  `capability:youtube-search` evidence (a `primitive-source:skill:` revision)
  with the `run` action's typed schema and implementation-plan digest; review
  revalidates through the existing ToolSkill lane (and OS-jail artifacts when
  present). No new review surface exists for skill packs.
- The pack carries only publication metadata:
  `metadata.magician.app_publication.display_name` (convention in
  [`skills-spec.md`](skills-spec.md)); name, description and version come from
  ordinary frontmatter.
- Skill discovery reads only `<skills-root>/<name>/SKILL.md` one level deep and
  skips `skill_type: app` documents.
- Surface: one `video_coverage` entity, one list view, one user-triggered
  action, one ToolSkill dependency.

## First-party package: memory & learning review console (plan 2.5)

`magician_data_v3/system/learning/app/` is a declarative review console over the
scoped learning substrate (manifest lives beside the learning system data; there
is no `firstparty-apps/` tree).

- **Population** is a host-read port: `sync_queue` declares
  `dependencies.tools: [{name: internal_data, version_requirement: "^1.13",
  actions: [list_learning_candidates, read_learning_candidate]}]`, locked to
  exactly those two Ready actions.
- **Review actions** `approve_candidate`, `reject_candidate`,
  `snooze_candidate` are tool-free workflows writing the package-owned
  `review_decision` ledger keyed by core candidate id. That record is the source
  head for the `magician.learning-decision` contribution port (proposal, owner
  review, ed25519-signed owner decision envelope, sealed
  ingress/invalidation/destination receipts, domain-separated under
  `magician.learning-*`). Its destination owner
  (`magician_v2/learning_decision_contribution.rs`) verifies the signed decision
  and calls `LearningStore::transition_candidate` — the same transition the
  first-party `/learning/candidates/{id}/transition` API uses, never a second
  authority. Approve → `approved`, reject → `rejected`, snooze logs a deferral
  without state change; `promoted` is unreachable (promotion bridges stay
  first-party); replays are idempotent via a marker in evidence refs; any
  decision on a terminal candidate fails closed. Memory-tier writes, skill/tool
  mutation and consolidation triggers stay core.
- Surface: two entities (`learning_candidate` mirrors, `review_decision`), two
  list views (`/`, `/decisions`), four user-triggered actions, one
  compiled-tool dependency narrowed to two actions. Agents keep the pack's full
  read vocabulary; uninstall is the rollback.
