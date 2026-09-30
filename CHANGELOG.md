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

## [0.7.89] - 2026-09-30

First public release.

- **One runtime, not a stack of glued-together services.** Agents, skills,
  memory, channels, tool execution and the UI run on the same substrate, on
  your own machine: 89 skills, 31 agents, 6 chat channels (email, Telegram,
  WhatsApp), iOS and Android companions, and generative UI that renders
  charts, metric cards, feeds and forms instead of a transcript.
- **Every agent is a YAML file.** Persona, tools, delegation policy and typed
  memory tiers are data, so adding an agent is an edit rather than a recompile.
- **Zero-trust by construction.** Every tool call carries a capability token,
  every spend hits a double-entry ledger, and the runtime refuses an
  over-budget call instead of asking the agent to behave.
- **Models are replaceable per operation.** Ten providers, including a local
  Ollama; route research to a frontier API and keep anything sensitive
  on-device, either way with one line of config.
- **Nothing shares a process.** Each chat channel runs as its own scope-aware
  daemon under a supervisor that enforces restart policies, and browser
  automation runs out-of-process, so a wedged page cannot take the runtime
  down with it.
- **Decisions are cheap and local where they can be.** A separate decision
  engine answers typed questions (is this memory relevant, which action is
  next) with small models — on the Apple Silicon GPU where available — instead
  of a full LLM round trip.
- **Runs are replayable.** Analytics events are also written to Parquet per
  scope and day, so a run is replayed rather than reconstructed from logs.

Install with `make install`, which stands up the whole stack and ends with a
health sweep. The README has the mode and flow matrix; per-crate detail is in
each crate's own `CHANGELOG.md`.

### Also in this release

- Memory decisions run on the decision engine with reviewed confidence
  thresholds, queued per item, with editable local and cloud primary/backup
  models.
- Sarvam for Indic-language chat; the GPT-6 Sol profiles now run on GPT-6.1
  Sol.
- The desktop tray no longer grows without bound: screen lookups from its
  300 ms contextual-assist poll are now freed (Desktop `0.3.18`).

#### 2026-09-28 — App keys go only where you allow

- A key used by an app's tool reaches only the hosts the tool declares, or the hosts you pick for it at install; sending it to any site is a separate, explicit choice. "Any public host" works only when the app asks for it and every safety limit allows it. Skills running in place can no longer read tool settings in your home folder.
- Development versions: Magician `0.7.88`, Apps `0.2.7`, Unified UI `0.1.31`.

#### 2026-09-28 — Keys can be locked to several sites, or all sites

- MagicVault now scopes a released key to a set of sites, or to all sites (`*`, only when the key allows it); a set is allowed only when the key permits every site in it. MagicVault `5849709` (magicvault-core `0.1.6`, reviewed, merged); its own tests now run against the same MagicRun Magician ships.
- Development version: Magician `0.7.87`.

#### 2026-09-28 — Outside edits refresh the notes index

- A change to a Markdown file in the notes folder refreshes search after the folder has been still for about a second.
- Development version: Magician `0.7.86`.

#### 2026-09-28 — Notes stay in Magician

- Web, iOS, and Android open the Markdown library in the app. Settings call the location the notes folder. The old notebook site, web view, and notes server are no longer part of opening or installing notes.
- Creating, saving, and deleting a note still writes the Markdown file and refreshes the search index.

#### 2026-09-28 — Today reads like a morning newspaper on web, iPhone and Android

- The iPhone and Android Today screens now match web's Morning Edition:
  - A compact masthead and a live realtime wire.
  - The lead story or "Slate is Clear".
  - A swipeable operations carousel: spending, task state, and each agent's cost, tasks, success and reliability over the last 24 hours.
  - A Reading Room with a Bumble-style swipe deck or a paged broadsheet.
  - Briefings, deliverables and the digest.
- Web Today no longer reports a failed action as a success, and it no longer shows made-up placeholder activity.
- Development versions: Unified UI `0.1.29`, Magdroid `0.5.0` (code `25`), Magios `0.3.0` (build `202`).
