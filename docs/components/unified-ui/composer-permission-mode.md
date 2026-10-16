# Composer permission mode (Do · Ask / Do · Accept)

The composer carries two independent things:

- **What the turn is for.** Do something, or produce a plan. A view preference.
- **How much it asks first.** Prompt before each in-scope file edit (`Ask`), or
  perform them without a prompt (`Accept`). A *permission* posture.

`Accept` is not a peer of `Do`; it is a property *of* Do. So Do is a
split control — the face selects Do and states its posture, the caret opens the
choice — and `Plan` stays a separate button.

```
┌─────────────────┐ ┌──────┐        ┌─────────────────────┐
│  Do · Ask     ▾ │ │ Plan │   →    │ ✓ Ask               │ Prompt before each file edit
└─────────────────┘ └──────┘        │   Accept            │ In-scope edits, no prompt…
                                    └─────────────────────┘
```

Selecting Plan **parks** the permission rather than discarding it, so leaving
Plan returns to the posture that was in force. The posture is part of the Do
button's accessible name, so a screen reader hears `Do · Accept` without
opening the menu.

## What Accept actually waives

Narrower than the name suggests. In `executor.rs`, a prompt is skipped only
when **both** hold:

1. the action is a filesystem one — `Write`, `Append`, `Delete`, `CreateDir`,
   `Copy`, `Move`; and
2. no path escapes the workspace tree.

Shell, network, and any absolute path outside the workspace still prompt. It
also fails closed: a pack action with no extractable path cannot *prove* it is
in-tree, so it prompts rather than assuming. `accept_in_scope_does_not_skip_out_of_tree_pack_paths`
pins that.

## Where the setting lives, and why not in the browser

The mode already rides along with every message as `ChatMessageMode`, so the
backend never needs to remember it to behave correctly. What needed a home was
the *preference*.

It lives on `UiPreferences` as `composer_permission_mode`, which is stored per
**principal + workspace**. Two consequences worth stating:

- Relaxing the gate is a decision about **one workspace**, not a global switch
  that follows the operator onto every machine they sign into.
- It is deliberately **not** cached in `localStorage`. A cached
  `accept_in_scope` would survive a workspace switch and show a relaxed gate for
  a scope that never enabled it. The composer paints `Ask` until the backend
  answers, which is the direction that cannot be wrong.

Planning stays in `localStorage` (`chat_planning`) because it is a harmless
view preference. The old single key `chat_plan_mode` held both, which meant
"stop asking before you write files" was remembered exactly like a selected
tab; `planModeStore` carries it over once and then removes it, so a permission
never lingers in local storage.

Normalisation fails closed — see
[the endpoint contract](../magician-api/ui-preferences-endpoint.md).

## Store shape

`planModeStore` exposes the effective tri-state as `mode` so existing callers
are unchanged, plus `permission` for the menu's checkmark and
`permissionLoaded` for surfaces that should not claim a posture they have not
confirmed.

- `select(mode)` — the existing tri-state entry point. `plan` parks the
  permission; `ask`/`accept_in_scope` set it and persist.
- `selectPermission(p)` — choose the posture without disturbing Plan.
- `hydrate()` — ask the backend for this scope's posture. Called from
  `ChatPanel`'s `onMount`; single-flight, safe to call repeatedly.
