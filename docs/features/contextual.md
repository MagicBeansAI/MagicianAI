# Contextual Assistance

The everywhere-you-work surface: derive what you are doing, assemble the
context that explains it, act on it, and land the result back where you
are — without configuring anything.

This is the operator-facing guide. Mechanics live in
[contextual assist](../components/desktop/contextual-assist.md),
[the HUD](../components/desktop/hud-overlay-window.md),
[the contextual-writing contract](../components/magician/contextual-writing.md),
and [screen capture & ask](../components/magician/screen-capture-and-ask.md).

## The idea in one line

Invoke a gesture anywhere; Magician figures out what the target is
(selection, a field, a web page, files in Finder, or just the screen),
gathers what explains it (text, URL, app, window, a capture when visual
grounding helps), offers only actions that fit that target, and returns
the result in place — inserted back, replacing the selection, filed as a
real task, or opened in the HUD.

## The gesture map

| Gesture | What you get |
| --- | --- |
| **Single left-Option tap** | The contextual menu for whatever the target is (below). |
| **Double left-Option tap** | The HUD composer — the ask-anything surface, pre-warmed so the first summon is instant. |
| **Right-click in Chrome** | "Magician" on selections/fields, "Ask Magician about this page" anywhere, "Save selection to Notes". |
| **⇧⌥S / ⇧⌥A / ⇧⌥R / ⇧⌥W** | Screen capture / region / clip / watch, staged into the HUD as chips before you type. |
| **Drag files anywhere onto the HUD** | Staged as attachments. |
| **Camera button in the HUD dock** | One tap: attach a capture of what you were looking at. |

## What the single tap gives you, by situation

| Where you are | The menu offers | Result lands |
| --- | --- | --- |
| Text selected (Mail, Notes, anywhere) | Rewrite, Summarize, Reply, Task, Save-to-Notes | **Tab inserts in place — over the selection it replaces it**; R refines with one more sentence; Copy always |
| Empty reply box | Draft reply, Write from context | Tab inserts at the caret |
| Field with a draft | Continue, Improve, Shorten, Clarify | Tab inserts |
| Chrome, page open, nothing selected | Observe+draft, Task, HUD (also the right-click page item) | Preview with Copy; page URL is the context |
| Finder, files selected | Summarize (names/paths + window shot), Task from files | Preview with Copy — paths are context; file contents are never read in the desktop |
| Anything else (Photos, a game, a settings pane) | Observe+draft, Write, Task, HUD — grounded in a capture of the frontmost window | Preview with Copy |
| A password field | **Nothing** — suppressed | — |

The tap never dead-ends: if nothing text-like is found, the frontmost
window becomes the context. The only suppressions are password fields,
apps you excluded, and Magician's own windows.

## Where results go

Draft text returns in place (insertion is focus-verified — if focus moved,
nothing is pasted and the draft stays on screen with Copy). **Task** and
**Follow-up** create real tasks in the same thread, with the draft as the
description. **Save to Notes** files the text with its source URL/title.
The HUD handoff continues a per-app/per-site session, so the next invoke
in the same place remembers the conversation.

## Honesty rules

Nothing is captured silently — screen context arrives only through an
explicit chord, tap, or required by the action. The camera button and
chords show the capture as a chip you can simply not send. Insertion
restores your clipboard after pasting. Suppressed surfaces stay
suppressed.
