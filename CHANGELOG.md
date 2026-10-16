# Changelog

Product-level release log. One entry per published release, newest first,
written for someone reading the repository rather than for the team that
wrote the code.

Per-crate detail lives in each crate's own `CHANGELOG.md` (`magician/`,
`magicutor/`, `magicllm/`, `ui/unified-ui/`, and the rest), which keep their
five most recent entries; anything older is in `docs/archive/changelogs/`.
This file keeps its five most recent entries too; older ones are in
`docs/archive/changelogs/product.md`.

Entries accumulate under `[Unreleased]` between releases and are promoted to a
dated, version-stamped heading when a release is published.

## [Unreleased]

## [0.7.109] - 2026-10-16

- **Work and coding runs:** Follow live work from the desktop notch and phones; runs and paused approvals survive restarts. Cost, time and attempt limits pause for an owner-approved allowance; tracked sends, calendar effects and replies feed follow-through.
- **Safer execution:** OS-enforced shell and network policies, scoped approvals and sandboxed coding engines. Fully specified tool packs run directly, and stalled turns stop with a reason.
- **Less resource growth:** Idle workspace databases close after 60 seconds, Attention expires old items and stores rows compactly, and deleted workspaces stay deleted. Task deletion recovers from invalidated Feed databases; the live-work indicator excludes idle ready tasks.
- **Updated interface:** Browse archived chats, tasks and coding runs; use the Talk / Code composer, notes folder and command palette. HTML outputs render in a sandboxed frame, and media editing supports silent video.
- **Setup and builds:** macOS source installer, optional private Cloudflare and Clarity configuration, portable mr-boxington build caching and leaner DuckDB C++ builds.
- **Usage accounting:** Faster lifetime cost totals, file-based model prices and metered speech calls.
- **Storage compatibility:** New Attention rows require Magician 0.7.98 or later. Do not downgrade below that version after upgrading.

## [0.7.95] - 2026-10-04

- Bounded Relentless task improvement with review, sealed references, paging, capture policies and guidance evaluation; fixes to verification, media probing, accounting, task state and supervisor shutdown; refreshed thinking maps and branding. (Development notes.)

## [0.7.90] - 2026-09-30

- **Sarvam speech.** Sarvam Bulbul v3 voices and Saaras v4 dictation for Hindi, Tamil, Bengali and other Indian languages, using `SARVAM_API_KEY`.
- **The autonomous harness says why it did not run.** Dropped cycles record a reason, a missing program document is reported, and the global pause can no longer be bypassed at startup.
- **Delegation survives restarts.** A delegated agent that cannot resume fails with a reason so its parent carries on.
- **No phantom workspaces.** Stray writes are quarantined instead of creating new workspaces.
- Development versions: Magician `0.7.90`, Magician API `0.3.32`, Magdroid `0.6.0` (code `26`), Magios `0.4.0` (build `203`).

## [0.7.89] - 2026-09-30

First public release.

- **One runtime, not a stack of glued-together services.** Agents, skills, memory, channels, tool execution and UI run together on your own machine: 89 skills, 31 agents, 6 chat channels, iOS and Android companions, and generative UI.
- **Every agent is a YAML file.** Persona, tools, delegation and memory tiers are data, not code.
- **Zero-trust by construction.** Every tool call carries a capability token, every spend hits a double-entry ledger, and over-budget calls are refused.
- **Models are replaceable per operation.** Ten providers, including local Ollama, chosen per operation in one line of config.
- **Nothing shares a process.** Chat channels and browser automation run as supervised separate processes.
- **Decisions are cheap and local where they can be.** A separate decision engine answers typed questions with small models, on the Apple Silicon GPU where available.
- **Runs are replayable.** Analytics events are also written to Parquet per scope and day.

Install with `make install`, which stands up the whole stack and ends with a
health sweep. The README has the mode and flow matrix; per-crate detail is in
each crate's own `CHANGELOG.md`.

### Also in this release

- Memory decisions run on the decision engine with reviewed thresholds and editable local and cloud models.
- Sarvam for Indic-language chat; GPT-6 Sol profiles run on GPT-6.1 Sol.
- Fixed a desktop tray memory leak (Desktop `0.3.18`).

#### 2026-09-28 — App keys go only where you allow

- **Security:** an app tool's key reaches only the hosts the tool declares or you pick at install; "any site" is a separate explicit choice. Skills can no longer read tool settings in your home folder.
- Development versions: Magician `0.7.88`, Apps `0.2.7`, Unified UI `0.1.31`.

#### 2026-09-28 — Keys can be locked to several sites, or all sites

- MagicVault scopes a released key to a set of sites, or all sites when the key allows it (magicvault-core `0.1.6`).
- Development version: Magician `0.7.87`.

#### 2026-09-28 — Outside edits refresh the notes index

- Editing a Markdown file in the notes folder refreshes search once the folder has been still for about a second.
- Development version: Magician `0.7.86`.

#### 2026-09-28 — Notes stay in Magician

- **Removed:** web, iOS and Android open notes in the app; the old notebook site, web view and notes server are gone. Notes are still Markdown files.

#### 2026-09-28 — Today reads like a morning newspaper on web, iPhone and Android

- iPhone and Android Today match web's Morning Edition: masthead, realtime wire, lead story, operations carousel, Reading Room and briefings.
- Fixed: web Today no longer reports failed actions as successes or shows placeholder activity.
- Development versions: Unified UI `0.1.29`, Magdroid `0.5.0` (code `25`), Magios `0.3.0` (build `202`).
