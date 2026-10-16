# App runtime compatibility

An App review fixes package bytes, dependencies, action schemas, effect classes,
transport bounds, policy and resource ceilings. A compatible host optimization
must not force the owner to approve those same App permissions again. Runtime
source evidence and the supported semantic contract therefore have separate
identities.

The recipe and compiled App effect owners anchor v1 to the exact previously
reviewed deployment. The existing wire field `implementation_digest` continues
to carry the following identity so immutable locks and retained plans need no
rewrite:

| Owner | v1 compatibility identity |
| --- | --- |
| Recipe | `blake3:b341b9bd7a597cb9a5bdc3c869eda92c603a930603a33148f0b62dba136feccd` |
| Compiled App effect | `blake3:db0575b27747c5ce58b43de307f8a6aecc3a1b5b65f110d44b3f2dfc2ba4640f` |

These are the source digests of the previously reviewed deployed build, pinned as
the explicit supported semantic baselines in `apps/runtime_contract.rs`. This is
one exact compatibility decision, not acceptance of arbitrary older hashes.
Source identity is still computed from framed implementation bytes, including
the contract declaration, and reported separately in `magician app check`'s
`runtime_evidence`. It does not change the generated App artifacts.

Existing recipe nodes keep their schema, node owner, input, plan, authority,
resource, effect and receipt semantics. Additive contextual rounds use the same
existing effect/resource owners but must be explicitly authored in the package.
Pre-round recipes retain their original complete v1 node-set binding. Locks
admit that exact set and the explicit contextual-round/store-transaction
extensions; missing,
unknown or arbitrary partial sets remain refused. Package bytes and compiled
plan checks prevent an older package from silently acquiring a new node.

Compiled actions still bind exact provider identity, tool source digest,
action plan, schemas, effects and transport limits. Normal physical-owner
attestation and live grant checks remain mandatory. This change does not relax
browser, MCP, Android or process-sandbox owner identities.

A change to existing input/output interpretation, authority, effect routing,
receipt semantics or recovery is a breaking contract change. It requires a new
identity and explicit migration, or a separately preserved implementation of
the old semantics. Do not keep these constants merely to silence re-review.
Compatible pooling, query batching, cancellation-aware admission, diagnostics,
and equivalent deterministic lowering can preserve the contract.

`make test-app-reconciliation` pins the baseline: it checks the archived
packages' immutable lock digests and resolves their locked physical actions
against current descriptors.
