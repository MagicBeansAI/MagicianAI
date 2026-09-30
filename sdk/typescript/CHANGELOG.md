# Changelog

## [Unreleased]

### 2026-09-11 — Live collections, round and transaction recipe nodes (0.1.0-dev.3)

- **Live collections:** `AppLiveCollection` gives 25-row snapshot pagination, durable live catch-up, canonical record merging, cancellation and bounded resets, over governed keyset pages under public contract 1.5.0 (no 10,000-row ceiling, no extra Load-more identity read).
- **Recipes:** `recipeNode.storeTransaction` and `recipeNode.contextualRound` builders, multi-source `ReconciliationSource` reconcile, and typed round expression/program types.
- **Contract:** public-contract mirrors carry wire 1.4.0's fourteen-feature inventory and 1.5.0's `pagination`; request bodies are sent as an `ArrayBuffer` copy.

_Current development version: `0.1.0-dev.3`._

### 2026-08-24 — App Platform SDK completion (0.1.0-dev.2)

- Add contract 1.3's eight-operation client, bounded value and Recipe
  authoring, generated per-app codecs, and exact reviewed interactive-capability
  request/grant builders.

### Changed - exact interactive action-class authoring

- Interactive request builders now admit one exact owner-supported Browser,
  macOS or Android action class with direct-owner, denied-transfer posture and
  invocation- or run-bound lifetime. Android pixel capture is admitted only for
  the exact reviewed capture class; multi-class, background, transfer and
  cross-owner class substitution remain rejected.

### Added - reviewed interactive capability authoring

- Generate deterministic Browser/macOS/Android request and grant-selection
  builders for the reviewed physical-owner contract, including exact
  logical targets, posture, resource and expiry/session fields.
- Add opaque session, observation, receipt, stop and status reference types;
  raw physical transport identifiers and control tokens remain absent.

### Added - bounded generated app value contracts

- Add the canonical generated workflow schema descriptor and exact codec
  factory for bounded records, arrays, closed unions/enums, optionals/nulls,
  scalars and logical resource handles. Runtime checks reject recursive or
  unreachable graphs, depth/item/byte overflow, unknown variants, input-side
  resource minting and internal task/execution identifier substitution.
- Add branded opaque run, session, observation, entity, artifact, receipt and
  resource handles plus the custom-bridge request/result/run-control types used
  by generated per-app modules. Recipe builders remain restricted to the exact
  Query/Get/Map/Validate/EmitValue/Sequence/Parallel/Switch set.

---

Older entries: `docs/archive/changelogs/sdk-typescript.md`
