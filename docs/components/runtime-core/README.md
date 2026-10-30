# Runtime Core Docs

Landing page for `runtime-core` documentation.

`runtime_core::cua` shares CuaDriver executable discovery and desktop-session
detection between the backend and desktop gateway, including Windows `.exe`
installer paths and headless Linux detection. See [CUA setup](../scripts/cua-setup.md).

`fair_queue::FairLane<T>` provides bounded owner-round-robin scheduling shared
by the LLM dispatcher and Decision Model dispatcher, including cancellation removal.

`process::git_program` is the git Magician runs for its own git work: `MAGICIAN_GIT`, else a
working host git (on macOS only with the developer tools installed), else the `dugite` skill's
bundled git.

## Canonical References

- [Runtime Core](runtime-core.md) — traits, prompt categories, tool catalog
  extensions, and the V2 conversation store contract
- [Workspace Architecture](../../ARCHITECTURE_V2.md)
- [Magician V2 API Guide](../magician/v2-api-guide.md)
