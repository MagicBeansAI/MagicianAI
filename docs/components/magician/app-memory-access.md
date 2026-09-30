# App memory access (`app_memory_read_v1`)

Apps can read the owner's memory, but only what the owner allows, per app, and
only for the kind of run it is. This page is the contract for that access.

## The model

- **The app requests; the owner grants.** A manifest declares what it would
  like to read:

  ```yaml
  metadata:
    magician:
      required_features: [app_memory_read_v1]
  app:
    memory:
      read:
        user_tiers: [preferences, contacts]   # facts about the owner
        agents: [research-agent]              # what these agents learned
        purpose: Personalise suggestions
  ```

  The block is review material, never authority. It is refused without the
  feature, with an unknown tier, a root tier (`knowledge`, `user`), an invalid
  agent id, duplicates, more than 16 of either, or a blank purpose.
- **Split by run mode.** The grant has two selections:
  `interactive` (runs the owner started — a user-triggered, foreground
  workflow) and `background` (schedules, events, recurrences, brokered
  transfers). Anything that is not clearly interactive is background.
- **Sensitive tiers need a tick.** Tiers are classified in
  `apps/memory_access.rs`, fail-closed: an explicit list is `ordinary`
  (preferences, skills, contacts, channels, routines, research findings,
  workflows, organization); root tiers are never readable; **every other tier
  — identity, accounts, screen observations, the email/calendar/chat evidence
  lanes, and any tier added later — is `sensitive`**.
- **Defaults.** Approving without choosing grants every requested ordinary
  tier and every requested agent for interactive runs, and nothing for
  background runs. Sensitive tiers are granted only by an explicit tick.
- **Owner-only engagement binding.** Every grant today is `owner_only`: the
  app reads the owner's own memory and never engagement- or meeting-labelled
  memory, whatever the grant says. A future client-serving app would be bound
  to exactly one engagement at install.

## Where it lives

| Piece | Location |
|---|---|
| Request/grant types, tier classification, subset checks | `magician/src/magician_v2/apps/memory_access.rs` |
| Manifest field + feature-gated validation | `apps/manifest.rs` (`AppManifestBody::memory`, `validate_memory_access`) |
| Review (`requested_memory_read`) and approval (`granted_memory_read`) | `magician-apps/src/apps/installation_review.rs` |
| Reviewed grant | `AppGrantRevision::{requested,granted}_memory_read` (folded into the authority digest only when present) |
| Owner edits after install | registry table `app_memory_read_grant_heads` (schema v38), `apps/memory_access_store.rs` |
| Permission diff axis | `AppPermissionDiff::memory_read` (widening requires review) |
| API | `GET` / `POST /api/magician/v2/apps/installations/{id}/memory-access` |
| Runtime binder | `memory_data` — see [app-tool-bind](app-tool-bind.md) |
| UI | install review "Memory access" fieldset; `AppMemoryAccessPanel` on installed apps |

## Editing after install

`POST .../memory-access` takes `{expected_edit_revision, interactive,
background}`, compare-and-swapped on `edit_revision`. The selection must stay
within the app's request. Edits live in `app_memory_read_grant_heads` and
**do not** bump the grant revision, so they never fence runs in flight; a
narrowed or revoked grant applies on the app's next memory read.

An edit applies only while it pins the installation's current grant revision:
an update or reinstall is re-reviewed and its reviewed grant takes over again.
The effective grant is `None` — nothing readable — when the installation is
not enabled, the grant is revoked, or the head row fails validation against
the request (a widened row is ignored, never trusted). Stored grants are also
checked against their request in `AppGrantRevision` contract validation.

## Compatibility

Every addition is skipped when absent, so packages and installations that do
not use memory keep byte-identical manifest, derived-artifact, review-material,
authority and permission-diff digests. The registry moves to schema v38 (one
new table), created by the first writable open of a v37 (or older) store; as
with every registry bump, older binaries cannot open a migrated store.
