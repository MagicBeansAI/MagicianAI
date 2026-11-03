# Changelog

All notable changes to the runtime-core project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---
## Unreleased

_Current development version: `0.1.9`._

- On Windows `process::resolve_program` finds a bare name's `.exe`, `.cmd` or other `PATHEXT` file, as `cmd.exe` does. It joined the name as given, so `git`, `rg`, `node` and every other bare name resolved to a path with no extension and was never found. macOS and Linux are unchanged. CI's Windows desktop check compiles it; its Windows-only tests have not run yet.
- `process::resolve_ripgrep` picks the ripgrep the `grep` and `glob` tools run: `MAGICIAN_RG`, the packaged `rg.bin` beside the binary or in its folder, the `rg` skill's pinned copy under skillshub, then the host's `rg`, with its source.
### Added — `process::git_program` (0.1.9)

- One git for Magician's own git work: `MAGICIAN_GIT`, else a working host git, else the `dugite` skill's bundled git.

### Added — `FileSandboxConfig.read_only_roots` (unversioned)

- Roots where file actions may read, check existence, list and copy from, but every mutation is refused. Defaults to empty, so configs without the key are unchanged.
- `FileSandboxConfig.extra_writable_roots` (not read from or written to config): writable roots added at run time, which never outrank a read-only root.

### Added — `ShellSandboxConfig.os_enforce` (0.1.8)

- Optional OS-level sandbox for shell commands; off by default, enabled in Magician's seed config.

### Added — delegation-parent settlement transaction (0.1.7)

- Conversation stores can settle a waiting delegation parent atomically, so recovery never sees a runnable parent still pointing at a finished child round.

### Added — deterministic execution-turn insertion

- Idempotent insertion of a turn with a server-derived ID, so launch recovery cannot duplicate a planning message.

---

Older entries: `docs/archive/changelogs/runtime-core.md`
