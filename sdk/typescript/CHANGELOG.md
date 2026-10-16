# Changelog

## [Unreleased]

### 2026-09-11 — Live collections, round and transaction recipe nodes (0.1.0-dev.3)

- `AppLiveCollection`: paged snapshots with live catch-up, record merging and cancellation over keyset pages (contract 1.5.0), with no row ceiling.
- New store-transaction and contextual-round Recipe builders and multi-source reconciliation.
- Contract mirrors cover wire 1.4.0 features and 1.5.0 pagination.

_Current development version: `0.1.0-dev.3`._

### 2026-08-24 — App Platform SDK completion (0.1.0-dev.2)

- Contract 1.3's eight-operation client, bounded value and Recipe authoring, generated per-app codecs, and reviewed interactive-capability builders.

### Changed - exact interactive action-class authoring

- Interactive request builders accept exactly one supported Browser, macOS or Android action class; multi-class, background and transfer requests are rejected.

### Added - reviewed interactive capability authoring

- Generated Browser/macOS/Android request and grant builders, plus opaque session, observation, receipt and status references; raw transport identifiers stay hidden.

### Added - bounded generated app value contracts

- Generated workflow schema descriptors and codecs for bounded values, with runtime rejection of oversized, recursive or unknown input.
- Opaque branded handles and custom-bridge types for generated app modules; Recipe builders stay limited to the supported node set.

---

Older entries: `docs/archive/changelogs/sdk-typescript.md`
