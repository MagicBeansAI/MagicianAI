# Runtime Core Docs

Landing page for `runtime-core` documentation.

`runtime_core::cua` shares CuaDriver executable discovery and desktop-session
detection between the backend and desktop gateway, including Windows `.exe`
installer paths and headless Linux detection. See [CUA setup](../scripts/cua-setup.md).

`fair_queue::FairLane<T>` provides bounded owner-round-robin scheduling shared
by the LLM dispatcher and Decision Model dispatcher, including cancellation removal.

`process::resolve_program` turns a bare program name into the absolute path the child will run,
searched on the PATH that child receives, so a spawn that overrides `PATH` stays on
`posix_spawn` (see `AGENTS.md`). On Windows it searches like `cmd.exe`: the name as given when it
has an extension, then each `PATHEXT` extension (`.COM;.EXE;.BAT;.CMD` when unset), so `git`
finds `git.exe` and `npx` finds `npx.cmd`; macOS and Linux join the name unchanged.

`process::git_program` is the git Magician runs for its own git work: `MAGICIAN_GIT`, else a
working host git (on macOS only with the developer tools installed), else the `dugite` skill's
bundled git.

`process::resolve_ripgrep` is the ripgrep the `grep` and `glob` tools run, pinned copies first so
every install searches with the same version: `MAGICIAN_RG` (`Missing` when it names no
executable, never a silent fall-through), else the packaged `rg.bin` (`rg.exe`) beside the running
binary or in its working folder, else the `rg` skill's copy (`skillshub/rg/bin/rg` or
`node_modules/@vscode/ripgrep-<os>-<arch>/bin/rg`, pinned in `skillshub/package-lock.json`), else
the host's `rg` on PATH (on macOS, then Homebrew's `/opt/homebrew/bin/rg` or `/usr/local/bin/rg`, for a service whose PATH lacks Homebrew). On Windows the pinned names are `rg.exe` and the PATH lookup finds `rg.exe` through `resolve_program`. It returns the source (`override`, `packaged`, `skillshub`, `system`,
`missing`) for the startup log and `/health`.

## Canonical References

- [Runtime Core](runtime-core.md) — traits, prompt categories, tool catalog
  extensions, and the V2 conversation store contract
- [Workspace Architecture](../../ARCHITECTURE_V2.md)
- [Magician V2 API Guide](../magician/v2-api-guide.md)
