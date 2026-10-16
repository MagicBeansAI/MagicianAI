# Which scopes the boot sweeps touch

`magician-bin` is the composition root: it wires the background sweeps that run
once at startup and then on a timer. Several resolve per-scope state, so which
scopes they enumerate is a contract, not a detail.

## The rule

A sweep that initialises a tenant's subsystem enumerates with
`list_tenant_scopes()` (or the `*_scope_segments` counterparts) rather than
`list_scopes()`. The difference is the reserved sinks — `system`/`system` and
`_quarantine`/`_quarantine` — which exist to catch records belonging to no
tenant and have no owner, no surface and no inbox.

The chat-memory boot sweep here is one of them: it resolves an
`AgentMemoryService` per scope, and resolving one in a sink materialises a
memory index for an agent that cannot exist. It calls
`AgentMemoryResolver::list_tenant_scopes()`.

Sweeps whose state genuinely belongs in a sink — the transport-log event stream
and the analytics derived from it — keep `list_scopes()` deliberately.

## Why this is a contract

Scopes are created implicitly: any write runs `create_dir_all` on its parent, so
a sweep that merely enumerates and initialises materialises its subsystem
everywhere it looked. That is how a reserved sink came to hold an app store, an
inbox DuckDB, a UI feed and a memory index. Each per-scope DuckDB also spawns a
scheduler pool sized to the core count, so the cost is not only disk.

Full reasoning and the list of gated sweeps:
[what a scope is for](../magician/storage-abstraction.md#what-a-scope-is-for)
and every scope pays for every subsystem.
