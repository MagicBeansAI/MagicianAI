# `/ui/preferences` — scoped UI preferences

`GET` and `PUT /api/magician/v2/ui/preferences`, handled by
`ui_preferences_api.rs` over `UiPreferences` in `magician`. Scope comes from
`resolve_required_scope`, so every value is stored per **principal +
workspace**, never globally.

| Field | Meaning | Default |
| --- | --- | --- |
| `theme` | Theme id | `longhand` |
| `composer_permission_mode` | `ask` or `accept_in_scope` — whether the composer's Do mode prompts before an in-scope file edit | `ask` |

`GET` also returns `saved`, which is false when the scope has no stored file
yet, so a client can tell "never chosen" from "chose the default".

## Partial writes leave the other fields alone

Both fields on `PutUiPreferencesRequest` are `Option`. An absent field is not
"reset to default", it is "do not touch". That is a correctness requirement,
not a convenience: the theme store and the composer store write this endpoint
independently, and a theme save that omitted `composer_permission_mode` must
not be able to move a *permission* by omission. The reverse holds too.

## Normalisation fails closed, and happens before the write

`UiPreferencesStore::save` normalises before it touches disk, so no unvalidated
value can be persisted even if a caller sends one.

For `composer_permission_mode` the failure direction is not arbitrary. It is a
permission, so anything unrecognised — `""`, `accept`, `true`,
`ACCEPT_IN_SCOPE`, a value from a future version — becomes `ask`, the posture
that still prompts. It never falls back to `accept_in_scope`. A preferences
file written before the field existed also reads as `ask`; both are pinned by
tests in `ui_preferences.rs`.

`theme` normalises the same way, to `longhand`.

## Events

A successful `PUT` emits `UiPreferencesUpdated` on the realtime broadcaster,
scoped to the same principal and workspace, carrying the normalised
preferences — so other open surfaces converge without polling.

The composer UI that reads `composer_permission_mode` is described in
[composer permission mode](../unified-ui/composer-permission-mode.md).
