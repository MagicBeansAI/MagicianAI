# `/ui/preferences` — scoped UI preferences

`GET` and `PUT /api/magician/v2/ui/preferences`, handled by
`ui_preferences_api.rs` over `UiPreferences` in `magician`. Scope comes from
`resolve_required_scope`, so every value is stored per **principal +
workspace**, never globally.

| Field | Meaning | Default |
| --- | --- | --- |
| `theme` | Theme id | `sorbet` (was `longhand` until 2026-10-01; a stored value is kept) |
| `composer_permission_mode` | `ask` or `accept_in_scope` — whether the composer's Do mode prompts before an in-scope file edit | `ask` |
| `appearance` | `system`, `day` or `night` — the mobile apps' day/night mode, so the theme follows the account across web/iOS/Android | none (absent) |

`GET` also returns `saved`, which is false when the scope has no stored file
yet, so a client can tell "never chosen" from "chose the default".

## Theme follows the account

The theme record is the account-level choice every surface converges on. The
web seeds it from its local pick when `saved` is false and adopts it on load
and focus; the mobile apps do the same on launch/resume, and push every
explicit pick (family chosen, Day/Night forced). A device following the system
appearance in `system` mode does NOT rewrite the record on its own day/night
flip — the record carries choices, not sunsets.

`appearance` is written only by the mobile apps; the web's theme id already
names a resolved variant, so it never sends one. A record without
`appearance` (a web-only write) leaves each surface on its own mode; the
mobile apps then take the family from `theme` and the day/night side from the
variant itself. Unknown values normalise to absent, so surfaces keep their own
mode rather than following something they cannot resolve.

## Partial writes leave the other fields alone

Both fields on `PutUiPreferencesRequest` are `Option`. An absent field is not
"reset to default", it is "do not touch". That is a correctness requirement,
not a convenience: the theme store and the composer store write this endpoint
independently, and a theme save that omitted `composer_permission_mode` must
not be able to move a *permission* by omission. The reverse holds too, and
`appearance` follows the same rule (the web omits it; only an explicit value
in the body can change it).

## Normalisation fails closed, and happens before the write

`UiPreferencesStore::save` normalises before it touches disk, so no unvalidated
value can be persisted even if a caller sends one.

For `composer_permission_mode` the failure direction is not arbitrary. It is a
permission, so anything unrecognised — `""`, `accept`, `true`,
`ACCEPT_IN_SCOPE`, a value from a future version — becomes `ask`, the posture
that still prompts. It never falls back to `accept_in_scope`. A preferences
file written before the field existed also reads as `ask`; both are pinned by
tests in `ui_preferences.rs`.

`theme` normalises the same way, to `sorbet`.

## Events

A successful `PUT` emits `UiPreferencesUpdated` on the realtime broadcaster,
scoped to the same principal and workspace, carrying the normalised
preferences — so other open surfaces converge without polling.

The composer UI that reads `composer_permission_mode` is described in
[composer permission mode](../unified-ui/composer-permission-mode.md).
