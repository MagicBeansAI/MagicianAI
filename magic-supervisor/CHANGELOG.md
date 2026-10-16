# Changelog

All notable changes to the Magic Supervisor project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---
## [Unreleased]

## [0.1.9] - 2026-10-06

- Native Windows executable defaults; managed services drain on Ctrl+C, SIGTERM and startup failure.

### Fixed - process-tree cleanup and crash visibility (v0.1.8)

- Magician runs in its own process group, so stop, restart, shutdown and unexpected exit reap the whole tree, including the audio sidecar.
- Unexpected exit status is logged; a stale audio-engine listener is cleaned up without touching unrelated listeners.
- A child's startup error or panic is now logged at ERROR instead of INFO; unlabelled stderr logs at WARN.

### Changed - compatible macOS system-proxy discovery (v0.1.7)

- Upgraded the HTTP client with explicit macOS system-proxy support.

### Fixed — health-check warmup no longer logs false "unresponsive" (v0.1.6)

- Health-check failures before the first success log at DEBUG; only a healthy-to-unhealthy transition warns.

### Added
- Host gateway proxy commands for host status and macOS presence-host start/stop/restart.

### Changed
- Restart limiting uses a rolling 5-minute window (still 5 attempts) instead of a lifetime counter.

---

Older entries: `docs/archive/changelogs/magic-supervisor.md`
