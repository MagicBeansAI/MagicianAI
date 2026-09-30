# Reviewed App runtime v1 compatibility fixtures

These five package archives are the immutable seed packages exported from the
2026-09-09 deployed build before the runtime repair. They contain package code,
manifest, dependency-lock and publisher evidence; no App-store rows, credentials
or runtime sessions. Do not regenerate them from the current source: they prove
that the current runtime still admits the exact earlier package/action bindings.

Recipe runtime baseline: `blake3:b341b9bd7a597cb9a5bdc3c869eda92c603a930603a33148f0b62dba136feccd`.
Compiled-owner baseline: `blake3:db0575b27747c5ce58b43de307f8a6aecc3a1b5b65f110d44b3f2dfc2ba4640f`.

The compatibility contract is described in
`docs/components/magician/app-runtime-compatibility.md`.
