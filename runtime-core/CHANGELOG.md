# Changelog

All notable changes to the runtime-core project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---
## Unreleased

_Current development version: `0.1.8`._

### Added — `ShellSandboxConfig.os_enforce` (0.1.8)

- Optional OS-level sandbox for shell commands; off by default, enabled in Magician's seed config.

### Added — delegation-parent settlement transaction (0.1.7)

- Conversation stores can settle a waiting delegation parent atomically, so recovery never sees a runnable parent still pointing at a finished child round.

### Added — deterministic execution-turn insertion

- Idempotent insertion of a turn with a server-derived ID, so launch recovery cannot duplicate a planning message.

### Added — exact execution-status compare-and-swap

- **Breaking:** conversation stores must provide status compare-and-swap (with revision) so pause/resume rejects stale writers; stores without atomicity must fail closed.

### Added — `PromptCategory::Social`

- New prompt category for social gate and composition prompts.

---

Older entries: `docs/archive/changelogs/runtime-core.md`
