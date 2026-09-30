# Changelog

All notable changes to the Magician Desktop project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

_Current development version: `0.3.18`._

---
## [Unreleased]

### 2026-09-29 — the host no longer leaks screen lookups (0.3.18)

- The 300 ms contextual-assist poll and the Orb's 1 s tick drain an autorelease pool each tick; unbounded `NSScreen` dictionaries had grown the tray to 7 GB in two days.

### 2026-09-27 — the Orb rests beside the notch and listens on a hold (0.3.17)

- The compact Orb is a tab on the right screen edge, under the menu bar. Hover or an active turn slides the status line out; it still grows to the top center and the center-screen spotlight.
- Wake listening is off by default. Hold Left Option to talk; a double-tap of the same key still opens Quick Automate.
- A Live conversation keeps its connection after the key is released. The microphone is open only while the key is held.

### 2026-09-25 — typed macOS actions run; older engines' catalogs read (0.3.16)

- **Typed Apps macOS mutations run.** The owner had refused every launch,
  focus, click, type, key, scroll and drag since the app platform shipped
  ("typed macOS v1 exposes only direct observation I/O"). They now execute
  under their one-action permits with the observation fences, and
  `live_textedit_owner_round_trip` (`--ignored`) drives each one through the
  real CuaDriver 0.28.2 owner against a scratch TextEdit document.
- **Fixed — a retitled document refused unrelated actions.** The pre-action
  fence covered the whole window, and macOS renames an untitled document a few
  seconds after an edit. It now covers only the action's target element and
  its ancestors' indexes and roles (both drag endpoints), or the window row and
  its child roles for a key press.
- **Fixed — an older engine's catalog failed every setup flow.** Desktop reads
  the engine's whole components catalog at once, and the pinned `cua_driver`
  shape refused a pre-pin engine's bare installer URLs; it now reads and
  ignores them (Desktop installs its own compiled pin).
- **Fixed — a debug Desktop took ~27 s per typed action.** The owner hashes
  the staged CuaDriver bundle (~73 MB) several times per action, and blake3
  compiled at `opt-level = 0` loses its SIMD paths. `blake3` now builds at
  `opt-level = 3` in dev; the live TextEdit run fell from 325 s to 44 s.
- A failed observation fence now says which check failed — the driver call
  (with its exit code and stderr), the byte ceiling, or the secure filter —
  instead of one message for all three.

### 2026-09-25 — onboarding installs the pinned CuaDriver 0.28.2 (0.3.15)

- **Fixed — Control desktop apps installed whatever `cua.ai` served, and kept
  any installed driver.** Desktop now installs the release pinned in the
  components setup catalog compiled into it: tag-pinned GitHub installer
  scripts, each SHA-256-checked before it runs, with `CUA_DRIVER_RS_VERSION`
  set. A driver of another version is replaced, and `cua-driver --version`
  must equal the pin afterwards.
- **Changed — macOS Grant & Verify runs `cua-driver permissions grant`**, so
  grants attribute to CuaDriver.app and Tahoe's direct-capture consent is
  requested; the Privacy panes are the fallback. The row shows
  `direct_capture_status` and is not ready when a live capture failed. The
  setup-guide link points at Magician's CUA setup doc; the Windows/Linux
  `list_windows` probe drops flags 0.28 does not have.

### 2026-09-23 — host gateway logs the AX actions that cross it

`/host/ax/<action>` is plain HTTP to loopback, and the `macos-ui-automation`
controller is not its only client: an agent holding the plane's generic `http`
tool can drive the Mac through this route, and one did — a whole desktop walk
whose only counter lived in the controller, so the run read as zero tool calls.
Each action now logs its name, the window and element it addressed, and the arg
size. `text` and `value` are excluded from the summary by construction, so the
text an action types cannot reach a log line.

---

Older entries: `docs/archive/changelogs/desktop.md`
