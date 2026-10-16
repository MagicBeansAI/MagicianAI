# Composer dock row

The chat composer carries a **dock row** above its input: a thin strip holding
the session `ContextPill` and, optionally, host-supplied controls pinned to its
right. Applies to every `ChatPanel` surface — `/chat`, `/t/[name]/chat`, and the
Tauri HUD (`/hud`).

## Anatomy

```
composer-shell
├── composer-dock          ← ContextPill  +  host actions (right)
├── banner slot            voice stage · tray-live PTT · autosend countdown
├── plan-banner            "Plan mode — I'll draft a plan first…"
├── attachments slot       staged attachment chips
├── composer-row           textarea · send/stop · mic · call
├── mention-dock           @-picker
├── composer-tools         Do|Plan · attach · engine/profile · thinking chip
└── warnings slot          profile warnings
```

The dock row is the composer's title bar (identity you read); `composer-tools` is
its control bar (controls you scan), deliberately at different densities.

The outer `.composer-wrap` is an accessible `group` named `Message composer`
owning the drag/drop listeners; the visible attach button is the keyboard path
to the same action. The explicit group keeps Svelte a11y diagnostics enabled
instead of suppressing the drag-surface warning.

`composer-tools` is split by a `flex: 1` spacer: **mode + attach left**, **the
profile chip and thinking status right**. The profile dropdown is
`position: fixed` and viewport-centred, not anchored to its trigger. The trigger
opens the chat engine picker: installed Plane harnesses first, then Magician API
profiles (for Magician/Pi) or the harness model list. On narrow screens it is one
icon-sized target. The engine/model choice is shared with chat Settings in
browser storage across sessions (per device/browser); each message sends that
route with the selected API profile.

## Turning it on

`FloatingComposer` renders the row only when the host passes `dock={true}`:

```svelte
<FloatingComposer dock={composerDisplayMode !== 'dev'}>
  <svelte:fragment slot="dock">
    <ContextPill inline … />
  </svelte:fragment>
  <svelte:fragment slot="dock-actions">
    <!-- optional; empty on /chat -->
  </svelte:fragment>
</FloatingComposer>
```

**An explicit prop, not slot detection:** `<svelte:fragment slot>` cannot be
wrapped in `{#if}`, so a conditionally filled slot still registers, and `:empty`
is unreliable with Svelte's comment anchors.
The dev/workbench variant passes `dock={false}` (a terminal has no chat session
identity).

## `ContextPill` inline variant

`<ContextPill inline />` drops the floating treatment (`position: fixed`,
anchoring, `z-index`, and the `pointer-events: none` guard). That guard exists
only for the floating pill, which shares a y-band with the `/t/[name]`
thread-bar; a docked pill takes pointer events normally. Nothing is trimmed:
thread chip, title, relative timestamp, archived badge, `+`, history and the `⋯`
more-menu all remain (`ContextPill.component.test.ts`).

The more-menu owns conversation-wide actions: "Build in VibeDev" appears there
only when the conversation has seedable text (it serializes the conversation, so
it is not repeated per message). Per-message playback sits beside the
author/timestamp metadata (no extra footer row) and inherits the surface's
resolved foreground in every state, including on accent-coloured bubbles.

### Archived sessions and "Return to active"

Opening an archived session is a read-only overlay (`viewingArchivedId`); the
selected active session is kept as the "Return to active" destination. When
none was selected — the archived session was opened directly from a
`?session=` link on a fresh page — `chatStore.returnToActive(threadId)` loads
the thread's active session (creating one if needed) instead of returning
silently, and ChatPanel rewrites `?session=` to that destination
(`chatStore.sessionSelection.component.test.ts`).

### Quiet at rest, exposed on hover

Title and thread chip are always readable; action buttons sit at `opacity: 0`
and fade in on hover/focus:

| Where | Rule |
| --- | --- |
| `ContextPill` | `.ctx-pill--inline:hover` / `:focus-within` reveals |
| `FloatingComposer` | `.composer-shell:hover` / `:focus-within` reveals via `:global()` |
| `ContextPill` | `.more-wrap:has(.more-menu)` stays revealed |

Hovering anywhere on the composer reveals them; an open more-menu stays visible.
Opacity (not `display`) keeps layout stable.

### Clicking the session name

In the docked variant, thread chip + session name is a `.ctx-identity` button
dispatching `open-history` (same as the clock button), with no chrome at rest.
The floating pill keeps an inert `<span>` — a wide clickable label would swallow
thread-bar clicks.

### Width

The pill uses `flex: 1 1 auto`, **not** `width: 100%`: the HUD sets
`flex-wrap: wrap`, and a 100%-wide item would claim the first line and push
`dock-actions` to a second row. `dock-actions` children are `flex-shrink: 0`.
`.title` is `flex: 1 1 auto; min-width: 0` (ellipsizes; long
`meeting-<label>-<date>` ids are the stress case); `.thread` keeps a 180px cap.

## HUD specifics

`/hud` fills `dock-actions` with its theme switcher and expand toggle via
`ChatPanel`'s `composer-dock` slot.

### Two states

| | collapsed (every summon) | expanded |
| --- | --- | --- |
| transcript | not rendered | flat, unmasked; `.chat-main` capped at 520px and scrolls |
| entered by | default | toolbar toggle, or a send that streams here |

Expansion is never remembered: `dismiss()`, `handleWindowBlur` and
`overlay-focus-input` reset it (an attach-screen re-summon keeps it). A send
routed to `@brainstorm` / `@tutor` / `@copilot` dismisses instead of expanding
(each renders on its own surface); everything else fires `inline-send`, which the
HUD turns into an expand. See
`docs/archive/plans/2026-07-27-hud-collapsed-command-bar-design.md`. There is no
3D tilt/perspective treatment, so `.hud` creates no containing block for fixed
descendants.

### A transparent stage around the panel

The HUD window is a fixed transparent stage, 1000×760 logical px clamped to 90%
of the cursor's monitor (`OVERLAY_HUD_WIDTH`/`HEIGHT`, `center_hud_window` in
`desktop/src-tauri/src/overlay.rs`); the themed `.hud` panel (820px wide) is
centred in it by CSS. Popups (`.theme-dropdown`, `.more-menu`, capped at 320px
and scrolling) overlay normally and spill into the stage, since a window cannot
paint outside itself. A click on the stage outside `.hud` (and outside the
drawer, confirmation modal and answer modal) dismisses; popups are children of
the panel, so that ancestor test needs no per-popup list. A file dropped on the
stage outside the panel is attached to the composer instead of navigating the
webview. This replaced the earlier window sized to the panel
(`docs/archive/plans/2026-07-27-hud-windowed-panel-design.md`).

## Density

The dock row does not make the composer taller because `composer-tools` is
dense: symmetric `2px 12px` padding, `.seg button` `2px 7px` at 9.5px, `.profile`
`2px 6px` gap 4 (model 9.5px, badge/tier 8.5px, icon 10px, chevron 8px),
`.icon-btn` and `.auto-speak-toggle` 20px boxes with 11px glyphs, `.thinking-chip`
`2px 8px 2px 6px`. `.pp-tier` colours instant / normal / advanced / frontier as
distinct chips (frontier is the GPT-6 Astra adaptive composite).

- **Chrome rows use symmetric vertical padding** — asymmetric padding makes an
  `align-items: center` row look top-aligned.
- **Every child of `.profile` is sized explicitly** — each sets its own
  `font-size`, and the inline SVGs size from attributes, so shrinking `.profile`
  alone leaves them large.
- `.icon-btn` and `.auto-speak-toggle` are kept in lockstep by hand.
- `@media (max-width: 767px)`: 40px touch targets; `.seg`, `.pp-badge`,
  `.pp-model`, `.pp-engine`, `.pp-chevron` hidden; `.pp-icon` restored to 16px (it is alone in
  the chip).

## God mode orb

On chat-next, the dock row's right end carries the `PresenceOrb`. It is also
the chat's god-mode switch: a button (`data-testid="god-mode-switch"`) that
toggles `execution.god_mode` for this chat through
`PUT /api/magician/v2/chat/sessions/{id}/god-mode` (`lib/settings/godMode.ts`).
Turning it on asks for confirmation; while on, the orb wears a red halo and its
tooltip says "God mode on". It is disabled until the server answers the chat's
`GET`. The default lives in the Settings god-mode panel
(`lib/settings/GodModePanel.svelte`, top of Settings). See
[god-mode.md](../magician/god-mode.md).

## Super god mode toggle and banner

Super god mode (debug, `execution.super_god_mode`) is global. Settings and the
VibeDev composer both set it, and one shared poll (`superGodModePoll` in
`lib/settings/godMode.ts`, 30 s idle) feeds every surface.

- **VibeDev composer** (`SuperGodModeToggle.svelte`, next to the coding
  profile): a ⚡ button. Off, it opens "Super god mode for 1 hour" and "Super
  god mode until turned off", each behind the same confirmation as Settings.
  On, it is red, shows the time left ("Super god · 52 min"), and one click
  turns it off.
- **Banner** (`SuperGodModeBanner.svelte`): mounted in the app layout under
  the top bar on `/chat`, `/tasks`, `/t/…`, `/vibe` and `/settings`. It is
  red, says how long is left and that approved harnesses start with no
  restrictions, and has Turn off.
- **Settings** (god mode panel): "On for 1 hour", "On until turned off", or
  Turn off.

What it does is server-side: each harness launch asks first (once, this
session or always), and only an approved launch starts unrestricted. See
`docs/components/magician/god-mode.md`.

