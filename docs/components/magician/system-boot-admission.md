# System-package boot admission

**Owner:** `magician/src/magician_v2/apps/system_boot_admission.rs`
**Wired at:** `magician-bin/src/main.rs` (awaited during startup)
**API:** `AppPlatformApi::admit_system_packages_at_boot`

## What it is

The host-controlled, digest-pinned owner that makes `distribution: system`
admissible, and the only way a package acquires system class.
`package_staging.rs` and `candidate_publication.rs` refuse a system manifest
with *"system-distribution packages require host-controlled digest-pinned boot
admission"* — this module is the owner they name.

## The trust argument

Manifest distribution is a **claim**; it mints nothing. System class is earned
by **provenance** — the bytes came from the deployment's read-only seed root
(`ArtifactV2Workspace::system_seed_root`), which no HTTP request writes and no
package nominates.

| Property | How it is enforced |
| --- | --- |
| Only this module can vouch for provenance | `SystemSeedProvenance` has private fields; nothing else can construct one. |
| The witness is the authority, not a label | `publish_candidate` branches on the *witness*, not the `AppCandidateProducer` variant (any caller can name a variant). |
| Provenance is bound to bytes | The witness carries the bundle digest it was minted for; `stage_trusted_system_candidate` re-checks it. |
| The gate has an inverse | The trusted path admits system class **and nothing else**; installable packages never get seed provenance. |
| Class is not power | Admission publishes inert `ready_for_review` installations. Only `enable_at_boot` lets the report mint an installation-bound host grantor. |
| Symlink and traversal safety | Symlinked seed entries are skipped (`DirEntry::file_type` does not follow). Package dirs go to `admit_package_directory`, which walks every component with `O_NOFOLLOW`. |

### Why the seed *root* is canonicalized and its packages are not

`admit_package_directory` refuses any path with a `.`/`..` component, so a
relative or symlinked seed root would fail every package; the root is
canonicalized. Package directories are *not*: they are joined onto the resolved
root and handed to the symlink-refusing walker, so a package pointed somewhere
writable is still refused.

## `inventory_digest`

Digest over sorted `(package_dir, seed_digest)` pairs — stable across boots,
independent of read order. Computed **before any publication**, so a package
that fails to publish still counts (a digest that shrank on partial failure
would mint authority for a different-looking fleet). This is the value
`CompiledAppInstallationProvenance::trusted_system` takes.

## Reaching the widget runtime

The report carries a `TrustedSystemInventoryPin` (private fields): the
inventory digest plus the bundle digests that earned system class this boot.
`admit_system_packages_at_boot` hands it to
`AppWidgetRuntime::adopt_trusted_system_inventory`; when the runtime compiles
an installation's declarations it asks the pin about the package's reconciled
content digest and grants trusted-system provenance only on a hit.

| Question | Answer |
| --- | --- |
| What if the pin was never adopted? | Every `distribution: system` manifest refuses to compile. Fail-closed — why admission is awaited, not spawned. |
| Can an installable package acquire system provenance? | No. Membership is by content digest, the resolver refuses a seed package not declaring `distribution: system`, and `compile_native_manifest_widgets` refuses the pairing independently. |
| Why key on digest, not name? | Provenance is bound to bytes; a renamed/re-uploaded package gets what its content deserves. |
| What if a system package leaves the seed root? | Its installation survives, but the next boot's pin lacks its digest, so its widgets stop compiling. |

Adopting the pin clears the runtime's negative-admission cache (keyed by
installation generation and package revision, neither of which changes when
the pin lands). Re-adopting an unchanged pin is a no-op.

## Behaviour

- **Per scope.** Every scope from `list_scopes`, plus the default scope (zero
  discovered scopes on first boot must not mean no apps).
- **Idempotent.** Publication is content-addressed; identical bytes yield
  `AlreadyPresent` whether the attempt is `ready_for_review` or `committed`.
- **Purge remains terminal.** Purge keeps the immutable revision and a `Purged`
  tombstone; seed replay recognizes only that shape and leaves it inactive;
  other partial publications are refused.
- **Failures are per-package** — logged and recorded in the report, not
  propagated.
- **Non-package neighbours are skipped.** The seed root also holds agent
  templates, db templates and trust policies; a dir with no `app/` or no
  `app/SKILL.md` is a neighbour. A manifest that does not declare system class
  is an error.

## Preconditions the path depends on

- **`compiled-revision:` refs are not registry lookups.** Every system package
  depends on a host binder (`agent_roster_data`, `meetings_data`,
  `evidence_data`, `internal_data`, `thinking_maps_data`) whose pack YAML is
  compiled into the binary. `trusted_registry_dependencies` skips those refs
  (like `primitive-source:skill:`); `complete_declared_tool_evidence`
  re-derives evidence from the in-binary bytes, and the lock-digest comparison
  is unchanged.
- **Generated artifacts must exist.** Each package needs
  `.magician/app-derived.json` (`magician app check --write-generated`).
- **Agent definitions use `for_bounded_agent_definition`**, not the app
  manifest's `max_string_bytes: 4096` (shipped personas exceed it). Only the
  per-string ceiling is raised; depth, node count and total bytes are
  unchanged. Failure here cascades silently: an invalid default agent breaks
  the agent catalog → primitive catalog → every capability, surfacing as
  *"capability X no longer matches its locked primitive descriptor"*.
- **Preflight YAML lexer** tracks single-quote state: a quote opens a scalar
  only at a value boundary, so possessive plurals (`contacts' preferred`) are
  prose; `''` escapes handled; an unterminated boundary quote is refused.
  `every_shipped_agent_definition_passes_preflight` pins shipped agents.
- **Classification floors** must be valid `AppDataClassification` variants
  (`public|ordinary|personal|sensitive|secret`).

End-to-end regression:
`an_owner_can_enable_every_boot_admitted_system_package` (seed → admitted →
reviewed → approved → `Enabled` for every shipped package).

## Activation

`admit_system_packages` grants nothing by itself: every publication lands as
`ready_for_review`. Its unforgeable report can mint a
`TrustedSystemPackageHost` scope only for an installation that published from
the current pinned inventory; that scope is bound to the exact installation,
not transport-deserializable, cannot execute apps, and cannot approve a
neighbouring package.

`AppPlatformApi` consumes it only when `system_packages.enable_at_boot` is on.
Schema default `false`; the shipped dev/package config opts in so built-ins
are available on a fresh deployment. With `false`, admission stops at
`ready_for_review` and the owner approves via the ordinary route; with the
opt-in, the host grantor performs the same review/digest/approval transition,
then registration and slot-default maintenance run in the same boot pass.
Owner revoke/disable/quarantine remain authoritative;
`background_behaviors.enabled` is an independent switch.

Slot-default maintenance runs after widget registration. An enabled or missing
installation absent from the widget inventory refuses the wholesale update; an
installation awaiting review, disabled, quarantined, updating, uninstalled or
purged contributes no default (an empty set is legitimate).

## Why the schema default remains OFF

A deployment that omits system-package policy stays fail-closed, and an
ordinary `SystemWorker` still cannot approve an installation. The shipped
opt-in is acceptable only because the host grantor derives from the private
boot-admission proof and is bound to one installation.

## Immutable dependency-lock migrations

A system package version identifies both its bundle bytes and its dependency
lock. Changing an embedded primitive descriptor, action digest or
implementation-plan digest requires a package **version bump** (with
regenerated `.magician/app-derived.json` and `sdk/app.generated.ts`), even if
authored workflows did not change; restarting an existing version cannot
re-lock it. Boot publication reproduces the new lock from the process-embedded
catalog only (scoped skills and artifact reviews cannot enter the comparison).
The prior version and lock stay readable; the new version gets a distinct
revision and installation identity.

A migration that omits the bump is refused immediately with package, version,
published lock digest and newly admitted lock digest.

## Version

Shipped in `magician` 0.7.14 / `magician-api` 0.3.13 / `magician-bin` 0.2.8.
