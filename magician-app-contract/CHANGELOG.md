# Changelog

## [Unreleased]

- 0.3.2: Add the `app_memory_read_v1` manifest feature: apps may request owner-granted memory reads per app and run mode.
- Add the Android automation trust mode to native enrollment.
- Formatting-only change to the macOS host module; no behaviour change.

### 2026-09-25 — 0.3.1 — Fence the target, not the window; menus are chrome

- macOS host actions are fenced on the target element (or window, for key presses) rather than the whole window content.
- Fixed: menu commands such as "Passwords…" no longer trip the secure-content filter; the menu bar is stripped and cannot be targeted. Secure fields still fail closed.

### 2026-09-25 — 0.3.0 — macOS host wire v2 for CuaDriver 0.28

- **Breaking (private server-to-desktop wire):** macOS host wire v1 is replaced by v2 and v1 permits no longer verify. Element actions carry CuaDriver element tokens; scroll takes direction and amount; drag carries two tokens and the screenshot scale.

### 2026-09-20 — Stable Android owner bootstrap socket location

- Desktop and Magician share a stable, short socket location that survives bundle-identifier changes.

### 2026-09-11 — 0.2.1 — Supported-public contract 1.5.0: keyset pagination

- Opt-in keyset pagination for indexed app queries; app packages and approvals need no revision.

### 2026-09-05 — Rustfmt normalization (no contract change)

- Formatting only; no contract change.

---

Older entries: `docs/archive/changelogs/magician-app-contract.md`
