# Changelog

## [Unreleased]

- 0.3.2: Add the `app_memory_read_v1` manifest feature: apps may request reads of the owner's memory, granted per app and per run mode by the owner.

- Add the closed Android automation trust-mode enum and bind it into the
  native begin-enrollment request and response.

### 2026-09-25 — 0.3.1 — Fence the target, not the window; menus are chrome

- `observation_content_digest` is now the action's fence, not the whole window
  content: `app_macos_host_element_fence` (the target line plus its ancestors'
  `[N] AXRole` identities), joined for a drag's two elements by
  `app_macos_host_element_fence_digest_input`, and `app_macos_host_window_fence`
  (the `[0] AXWindow` row plus its child roles) for a key press;
  `app_macos_host_observation_fence_digest` is the shared `blake3:` form.
- The secure-content filter no longer matches menu commands: CuaDriver 0.28
  appends the menu bar to a window's tree, and every text app's Edit menu has
  AutoFill's "Passwords…", so every TextEdit observation was refused. A secure
  field, a `secure: true` flag, or "password" anywhere in window content still
  fails closed. `app_macos_host_observation_content_tree` strips the menu-bar
  subtree; `app_macos_host_is_menu_chrome_role` marks roles an element action
  may not target.

### 2026-09-25 — 0.3.0 — macOS host wire v2 for CuaDriver 0.28

- **Breaking (private server-to-desktop wire):** `APP_MACOS_HOST_WIRE_V1` is
  replaced by `APP_MACOS_HOST_WIRE_V2` (`magician.app-macos-host-wire.v2`); v1
  permits no longer verify. `ClickElement`, `TypeText` and `ScrollElement`
  carry the observed CuaDriver `element_token` (`s<8 hex>:<index>`, parsed by
  `app_macos_host_parse_element_token`) instead of a bare index.
  `ScrollElement` is `direction` (`AppMacosHostScrollDirection`) plus `amount`
  (1–`APP_MACOS_HOST_MAX_SCROLL_AMOUNT`). `DragElements` carries two tokens
  from one snapshot and the observation's `screenshot_scale_millis`, because
  0.28 drag is pixel-only. `Backspace` lowers to `delete` and `DeleteForward`
  to `delete` plus the implied `fn` modifier (`cua_implied_modifier`).


### 2026-09-20 — Stable Android owner bootstrap socket location

- Share the short Application Support directory and private owner-directory
  names used by Magican Desktop and Magician so their reciprocal macOS Unix
  socket remains identical across desktop bundle-identifier changes and fits
  within the platform `sun_path` limit.

### 2026-09-11 — 0.2.1 — Supported-public contract 1.5.0: keyset pagination

- **Queries:** opt-in `pagination: "keyset"` for governed indexed app queries; the component contract stays 1.0.0, so app packages and approvals need no revision.

### 2026-09-05 — Rustfmt normalization (no contract change)

Normalize the contribution-contract and crate-root formatting with the current
workspace rustfmt configuration. Wire schemas, validation rules, public types,
and generated contract projections are unchanged.

---

Older entries: `docs/archive/changelogs/magician-app-contract.md`
