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
wrapped in `{#if}`, so a conditionally filled slot still registers and the row
would paint around nothing; `:empty` is unreliable with Svelte's comment anchors.
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
| transcript | not rendered | flat, unmasked |
| `.chat-main` | centred, hugs the composer | `top: 32px` → `bottom: 96px` |
| entered by | default | toolbar toggle, or a send that streams here |

Expansion is never remembered — `handleWindowBlur` resets it on dismiss. A send
routed to `@brainstorm` / `@tutor` / `@copilot` dismisses instead of expanding
(each renders on its own surface); everything else fires `inline-send`, which the
HUD turns into an expand. See
`docs/plans/2026-07-27-hud-collapsed-command-bar-design.md`. There is no 3D
tilt/perspective treatment, so `.hud` creates no containing block for fixed
descendants.

**Backdrop dismiss** compares the click point against a list of bounding rects;
dock-row popups (the fixed theme panel, the absolute more-menu) need their own
entries since they fall outside `.composer-wrap`'s rect.

### Popups render IN FLOW, and the window grows

The HUD window is sized to its panel
(`docs/plans/2026-07-27-hud-windowed-panel-design.md`) and cannot paint outside
itself; an out-of-flow popup contributes no height, so the panel would never
grow. `/hud` forces `.theme-dropdown` and `.more-menu` to `position: static`:
opening one grows the panel and therefore the window (the launcher idiom) —
correct by construction, not by viewport maths.

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
  `.pp-model`, `.pp-chevron` hidden; `.pp-icon` restored to 16px (it is alone in
  the chip).

`/t/[name]`'s thread-bar remains left-aligned; the floating pill that forced this
no longer exists, so centring the tabs is a free visual decision.
