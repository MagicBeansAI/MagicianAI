# Apps Supported-Public Contract · V1

This directory is generated compatibility evidence for the supported-public
subset of the live Apps platform. It is not an inventory of every internal,
review, custom-surface bridge, or authority-shaped route.

The pure source contract lives in `magician-app-contract`. It contains no
Actix, SQLite, filesystem, registry, provider, or execution dependency. The
runtime wire models remain with their current owners while extraction proceeds
incrementally. `magician-api` consumes the canonical operation paths directly
when registering supported-public routes and asserts the registered HTTP
method; the generator consumes those same descriptors.

Generated artifacts are:

- `contract.json`, pinning independent contract/protocol versions plus schema,
  fixture, and operation-inventory digests;
- `operations.json`, the canonical supported-public operation inventory with
  stable version-independent operation IDs, authentication posture,
  idempotency, success statuses, schemas, examples, and concrete route-specific
  reason codes retained inside canonical error diagnostics;
- `schemas/*.schema.json`, standalone JSON Schema Draft 2020-12 roots;
- `openapi.json`, OpenAPI 3.1 paths for only the supported-public operations;
  and
- `fixtures/*.json`, cross-client query, mutation, action, run, error,
  artifact, uncertainty, and contract-capabilities goldens.

The same generator embeds fixture values in
`ui/unified-ui/src/lib/app-platform/AppContractFixtures.generated.ts` and
`magios/Shared/AppContractFixtures.generated.swift`, and projects the exact
supported-public operation/deprecation inventory into
`sdk/typescript/src/generated/public-contract.ts`. Generated files must not be
edited independently.

## Compatibility and authority

Contract, data-plane protocol, manifest schema, authoring CLI, and language SDK
versions are independent. `metadata.magician.app_sdk_version` is a deprecated
informational legacy marker. New manifests declare `required_features` and may
record a semantic `generated_by` identity; neither field is accepted as runtime
authority. Unknown required-feature enum values fail manifest admission because
the package is explicitly asking for behavior this server cannot promise.
Missing or empty `required_features` means only the legacy V1 manifest baseline;
it never opts into a future feature implicitly. Additive fields in capability
responses are structurally ignorable. Supported-public error envelopes are
closed at the top level; only their bounded, route-reviewed `details` map is
extensible. Unknown enum values remain a contract-version compatibility
decision, not an implicit fallback.
The pre-release TypeScript SDK deliberately closes every capabilities object
and deprecation row under its `current_contract_only` policy; an additive
capability therefore requires regeneration and review before that client
accepts it.
The advertised SDK window is current-contract-only; no N-1 promise
is claimed until that policy is deliberately widened and its goldens pass.

The current live supported-public additive bundle is contract `1.5.0`. The
preceding checked component-only catalog already identified itself as `1.0.0`,
so live operations, capabilities, and additional schemas are not relabeled
under that older version. Manifest compatibility requirement `"1"` continues
to match the component/data-plane contract `1.0.0`, while supported-public
`1.5.0` is not a package-manifest compatibility input. Scaffold manifests
therefore need no breaking major edit.

`GET /api/magician/v2/apps/contract-capabilities` requires a fresh
middleware-verified session but no workspace selection. It returns only public
compatibility, fixed limits, deprecations, and the supported-public operation
projection. It never exposes principal/workspace values, grants, primitive
eligibility, provider evidence, internal task IDs, or credentials.

The supported-public action launch uses
`POST .../actions/{action_id}/runs` and returns only the opaque logical run
handle, optional current execution identity, and typed result. The existing
`.../launch` route remains a legacy compatibility adapter and is deliberately
absent from the supported-public inventory.
Supported-public polling accepts only the canonical
`run:app-action:<opaque-run-suffix>` locator; the historical bare
`task_app_*` locator remains confined to legacy/internal compatibility seams.
Malformed, missing, or unknown entity-change query fields return
`AppErrorEnvelope` with `invalid_request` + `terminal`; the reviewed legacy
reason `invalid_app_contract` is retained in `details.reason`, and Actix
extractor text is never part of the public contract.

All eight supported-public operations now return the closed canonical
`AppErrorEnvelope` on non-success. `code` identifies the stable error family;
only `disposition` controls retry, refresh, reauthorization, user-action, or
uncertainty behavior. `details.operation_id` correlates the rejecting
operation, while an optional inventoried `details.reason` preserves a bounded
route-specific diagnostic. Internal and legacy Apps routes continue to use
their existing compatibility responses. Run polling truthfully declares both
`202` for a nonterminal snapshot and `200` for a terminal snapshot.

`compose_action_run` also owns bounded multi-hop composition and result-state
subscriptions; neither adds a ninth public operation. A request declares at
most three destination hops (linear fanout `1`), with a distinct idempotency
key and server-compiled mapping at every hop. The owner binds the full request
digest into every durable transfer receipt, caps one admission attempt at 30
seconds, rejects repeated run/installation identities, and reopens each
source/destination installation generation, package, schema, grant, action,
result, policy, provenance, labels, deadline, and resource ceiling immediately
before the next launch. A receipt is recovery evidence, never app-to-app bearer
authority; the executable chain is move-only and server-minted. An uncertain
or withheld hop stops the chain and is never reported as partial success.

The optional `subscription` member polls the same compose route with the exact
same request. Its opaque cursor is bound to the authenticated scope and
authentication revision, origin run, and complete chain digest. Pages contain
at most eight payload-free updates, use monotonic sequence numbers, expire
after ten minutes, and carry only canonical run/installation/action
correlation—never task or execution IDs. Because this owner does not invent
missed history, an expired cursor or sequence gap returns `reset_required` and
the current canonical state.

## Generation and qualification gate

```bash
make app-contract-codegen
make app-contract-check
```

The generator source, runtime capability operation, and route inventory are
implemented. On 2026-08-23 the canonical generator completed and refreshed the
checked `contract.json`, `openapi.json`, schemas, fixtures, TypeScript inventory,
Unified UI fixture mirror, and Swift fixture mirror for the exact eight-operation
supported-public `1.5.0` bundle. The codegen unit suite passes 3/3 and the
focused private SDK suite passes 33/33. These checks establish generated-byte
and consumer-fixture coherence; they do not publish the private SDK or qualify
the physical workflow owners. On 2026-08-28 the generator refreshed those same
artifacts again for `custom_surfaces_v1`. On 2026-09-02 the checked capability,
schema, OpenAPI, TypeScript, Unified UI, and Swift inventories were aligned
with the source's complete fourteen-feature set, including `app_widgets_v1`,
`app_behaviors_v1`, `app_event_behaviors_v1`, and
`app_owner_notifications_v1`. This source checkpoint does not claim that the
generation or qualification commands above were executed in that session.

The component/data-plane contract is an independent compatibility axis and
remains `1.0.0` (registry revision 1). Regenerated client mirrors expose both
explicitly named versions while retaining `APP_CONTRACT_VERSION` only as a
component-axis compatibility alias. The immutable component manifest lives at
`docs/contracts/app-platform/components/v1/contract.json`; authoring and
publication embed only that path. Public generation owns this directory's
`contract.json` and validates, but never repairs or overwrites, the component
manifest. Runtime authoring and publication also pin its exact byte identity to
SHA-256 `4c6f6f4edfeaafcc56610fea425cf3df24913ed18605b75c46805e18584e8b2d`;
even a same-version whitespace edit must mint a new immutable revision. The
generated mirrors and consumer assertions expose component `1.0.0`,
supported-public `1.5.0`, and alias equality explicitly in the same change.
The checked `@magician/apps` source is likewise private and guarded by a failing
`prepack`. Its 33 focused tests and a fresh packed external-consumer HTTP-double
canary pass. Publication remains blocked on the full `make app-contract-check`
lane and a real authenticated `AppPlatformApi` consumer canary with the actual
Artifact/workflow owner. At this checkpoint the umbrella Make target still
stops in unrelated Rust test-only compile failures before completing every
workspace golden; that residual is qualification debt, not generated-artifact
staleness.

`make test-app-typescript-real-server-canary` owns the maximum provider-free
real-server subset. It starts the production Apps route configuration and real
`AppPlatformApi` over a temporary authenticated scope, then has a freshly
packed external TypeScript consumer exercise capabilities and the real
registry/entity query, mutation, re-query, and entity-change owners. It cannot
yet qualify action launch/run polling: those handlers require the canonical
Artifact V2 task owner, and there is no provider-free constructor for the
`ArtifactV2Service` plus workflow runtime fixture. Publication remains blocked
until that owner seam exists and the full real-server eight-operation lane passes.
