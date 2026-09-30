# Presto Route Contract

This folder defines compile-time route contracts for route hardening.

## Files

- `schemas.ts`: canonical schema version per route.
- `routeContracts.ts`: typed contract map for every app route.
- `id.ts`: deterministic component ID rules and helpers.

## Contract fields

- `route`: canonical route key (supports dynamic path canonicalization).
- `schemaVersion`: expected schema tag for the route surface.
- `requiredComponents`: component types that must exist in rendered GAUI tree.
- `requiredLifecycleEvents`: route lifecycle events that must be handled.
- `operationalReadinessRequired`: whether route is part of operational cutover set.

## Invariants

- Every app route in scope must exist in `PRESTO_ROUTE_CONTRACTS`.
- Dynamic routes are canonicalized before lookup.
- Contract coverage is enforced by TypeScript map typing (`Record<PrestoRoutePath, ...>`).
