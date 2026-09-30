# HUD — the collapsed command bar

The Tauri quick summon (double-tap Left Option → `/hud`) is a two-state
command bar in a small centred window with the theme's own chrome.

Design: 2026-07-27 HUD collapsed command bar.

## What it is

|            | collapsed (every summon)     | expanded                    |
| ---------- | ---------------------------- | --------------------------- |
| transcript | not rendered                 | normal, flat, unmasked      |
| composer   | the entire UI                | pinned at bottom            |
| controls   | docked row, hover/focus-only | always visible              |

Expansion is never remembered: blur, dismiss and `overlay-focus-input` all
force `expanded = false`, so every summon starts collapsed.

**Every summon also focuses the composer**, via `ChatPanel.focusComposer()`.
This has to be explicit for two reasons that compound: Tauri keeps the HUD
WebView mounted across hide/show, so `onMount` runs exactly once ever; and
`handleWindowBlur` deliberately blurs the focused element on hide so the HUD
"starts fresh". Together those meant the command bar reopened with nothing
focused and the first keystroke went nowhere. `overlay-focus-input` — emitted by
`show_overlay_window` and named for precisely this — was already being listened
for, but the handler only resets expansion and re-glides the mascot. The first
open is focused from `onMount` instead of the event, because the listener is
registered asynchronously and can miss the emission that opened that very
window; that call sits ahead of the `isTauri` guard so a browser `/hud` behaves
the same.

## Window

Code depends on these window invariants:

- **Screen-coordinate math must add the window origin.** The mascot glide
  (`glide_mascot_to`) derives its target from `win.outerPosition()` +
  viewport coordinates. Any future feature translating viewport→screen must
  do the same.
- Popups (theme panel, history drawer) render into a transparent stage
  within the window rather than resizing it, wrap inside the panel, and
  must never assume monitor-sized room. Do not reintroduce window resizing
  for a popup.
- The mobile/phone composer variant must never render in the HUD window,
  whatever the window width reports.

## Send routing

One decision at send time:

```
@brainstorm | @tutor family | @copilot family  →  dispatch, then dismiss
everything else                                →  expand, reply streams in place
```

- Each dismissing invoke owns a surface elsewhere (thinking-map page, tutor
  and copilot overlay rails), so the HUD leaving strands nothing.
- A plain reply has nowhere else to go — the notify overlay deliberately
  excludes chat replies (`lib/notify/infoModel.ts`) — so `ChatPanel`
  dispatches **`inline-send`** (before awaiting the turn, so the expansion
  animates while the reply is in flight) and the HUD expands.
- **Dismiss fires on successful dispatch, never on Enter.** Two paths fail
  after the user commits and keep their error in a still-open HUD: the
  app-copilot screenshot guard, and a failed `createThinkingMap`, which
  hands the typed text back.

Invoke detectors and routing live inline in `ChatPanel.svelte`.

### `@brainstorm` in HUD context

The HUD webview persists across hide/show, so a `goto('/thinking-maps/…')`
would render the map inside the overlay AND leave the webview parked off
`/hud` for the next summon. In HUD mode `startBrainstormFromComposer` calls
the Tauri `open_app_at` command (main window) and dismisses; `/chat` keeps
its `goto`.

## Docked chrome

The composer shell carries a dock row gated on `FloatingComposer`'s `dock`
boolean prop (slot fragments cannot be conditionally registered, so presence
is not the gate). Non-HUD hosts leave `dock` false and render without the
row. The row contains:

- the **real `ContextPill`** in its `inline` variant — thread chip, session
  title, timestamp, `+`, history, `⋯`, nothing trimmed. The variant drops the
  floating treatment (fixed positioning and the `pointer-events: none`
  container guard, which exists only for the floating pill's overlap with
  the `/t/[name]` thread-bar). The pill must not claim the whole row —
  theme and expand keep their space.
- theme switcher and the expand/collapse toggle.

The row is quiet at rest and appears on hover or `focus-within`, scoped to
the composer element. `/chat` keeps the floating pill.

## Text queue controls

The shared ChatPanel/FloatingComposer exposes normal Send-to-queue, Stop & send,
and Run in parallel in the HUD too. Queue inspection lives inside the composer so
it remains reachable while the transcript is collapsed. Send options render in
flow and wrap within the window. Hiding the HUD does not cancel accepted queued
or parallel work. See [concurrent requests](concurrent-voice.md).
