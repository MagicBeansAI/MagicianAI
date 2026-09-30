# Changelog

All notable changes to the runtime-core project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---
## Unreleased

_Current development version: `0.1.8`._

### Added — delegation-parent settlement transaction (0.1.7)

- `V2ConversationStore::settle_delegation_parent` gives production stores one
  operation for changing a waiting parent status and clearing its active child
  group. The canonical file store performs both mutations under the same
  execution-document lock and status revision; recovery therefore cannot expose
  a runnable parent that still names the completed child round. The default
  implementation remains a fail-propagating compatibility path for embedded
  stores that do not provide a transactional document update.

### Added — deterministic execution-turn insertion

- `V2ConversationStore::add_turn_with_id` lets a caller idempotently bind one
  server-derived turn id to exact execution, direction, text, and reply-slot
  content. The safe default returns unsupported without mutation; durable
  accepted-launch recovery can therefore fail closed instead of calling the
  random-id append path and duplicating a planning message. The file store
  returns the identical existing turn on exact replay and rejects an id already
  bound to different content.

### Added — exact execution-status compare-and-swap

- `V2ConversationStore` now requires status CAS by expected enum, status CAS by
  expected enum plus status-only revision, and a status-revision read. Durable
  multi-execution pause/resume protocols can therefore reject stale writers and
  Paused-state ABA without treating unrelated execution metadata updates as
  status changes; stores that cannot provide real atomicity must fail closed.

### Added — `PromptCategory::Social`

- Added the category used by the checked-in fleet social gate and composition
  prompts so strict prompt loading and category telemetry remain aligned.

### Added — `PromptCategory::Conversational`

New variant on `PromptCategory` (in `prompts.rs`) for
realtime voice session prompts — modality addendum, speech-tag
instructions, task-completion announcements. Voice prompts use
this instead of `Chat` so category-level telemetry can
distinguish voice rails from text-chat templates without
guessing from filename. See
`docs/components/magician/realtime-media-rails.md` for the
upstream voice substrate.

---

---

Older entries: `docs/archive/changelogs/runtime-core.md`
