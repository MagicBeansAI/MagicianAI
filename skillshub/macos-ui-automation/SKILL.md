---
name: macos-ui-automation
version: 0.5.0
description: Native desktop computer use through CuaDriver on macOS, Windows, or Linux,
  locally or through the desktop host relay. Snapshot the target window, act on grounded
  elements, then re-snapshot. Requires a signed-in graphical desktop and platform
  accessibility permissions. Use this for multi-step GUI walks and for apps that are not
  scriptable (Electron, canvas surfaces, anything without an AppleScript dictionary); for
  a one-shot on a scriptable app — play something, set volume, add a reminder, read a
  Finder selection — the `macos_automation` tool is one call where this is a snapshot,
  a click and another snapshot. The legacy macos-ui-automation name is kept for
  compatibility.
metadata:
  magician:
    requires:
      cua: true
      bins:
      - macos-ui-controller
      - python3
    install_hint:
      docs: On the desktop host run `make setup-cua-driver ARGS=--start` (Linux/macOS),
        or `py -3 scripts/setup-cua-driver.py --start` (Windows). Read-only check is
        `make check-cua-driver`; headless backends use `make check-cua-driver ARGS=--relay`.
        Windows needs the signed-in desktop session; Linux needs X11/Wayland and AT-SPI.
        macOS alone needs CuaDriver.app Accessibility and Screen Recording grants.
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      exempt:
        reason: >-
          it drives the operator's GUI through accessibility permissions, so
          a probe would click real applications on a live desktop.
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - macos-ui-controller
        - python3
        entrypoint: macos-ui-controller
      runtime:
        protocol: cli
        command_prefix: []
        interaction: batch
        stdin:
          mode: required
          sensitivity: private
        working_directory:
          mode: denied
        limits:
          timeout_secs: 30
          stdin_bytes: 1048576
          stdout_bytes: 10485760
          stderr_bytes: 2097152
      auth:
        kind: native_permission
        requirement: required
        provider: native-desktop-computer-use
        profile_selection:
          mode: none
        storage:
          kind: operating_system
      policy_floor:
        approval: native_ui_control
        resource_scopes:
        - native_ui
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      input_delivery: canonical_json_stdin
      actions:
        call:
          description: 'Invoke a cua-driver action against the selected desktop host through the runtime''s

            resilient native controller. The controller starts the platform-specific driver as a

            daemon before every call, maps `serve`/`status`/`stop` to the top-level

            cua-driver lifecycle commands, and adds a shared `session` label to every

            tool that takes one so cursor and session state persist across calls. An

            element address is scoped to its snapshot: on `snapshot_id_required`,

            `stale_element_token` or `No cached AX state`, re-run `get_window_state` and

            resend with its new `snapshot_id` (or `element_token`). Captures

            (`get_window_state`, `get_desktop_state`, `zoom`) are written to a file named

            in the reply as `screenshot_file`. For Personal Tutor guided actions, keep

            the CUA visual agent cursor enabled and tune `set_agent_cursor_motion`

            before visible clicks so users can follow the demonstration.

            '
          parameters:
            action_name:
              type: string
              enum_values: [bring_to_front, browser_click, browser_dialog, browser_download, browser_navigate, browser_pointer, browser_prepare, browser_set_input_files, browser_type, check_for_update, check_permissions, click, clipboard_read, clipboard_write, double_click, drag, end_session, escalate_session, get_accessibility_tree, get_agent_cursor_state, get_browser_state, get_config, get_cursor_position, get_desktop_state, get_recording_state, get_screen_size, get_session, get_session_state, get_window_state, health_report, hotkey, install_ffmpeg, invoke_menu, kill_app, launch_app, list_apps, list_sessions, list_windows, move_cursor, page, press_key, replay_trajectory, right_click, scroll, serve, set_agent_cursor_enabled, set_agent_cursor_motion, set_agent_cursor_theme, set_config, set_value, set_window_frame, start_recording, start_session, status, stop, stop_recording, type_text, verify_state, zoom]
              description: The cua-driver action to run. `enum_values` above is the whole
                surface, generated from the installed driver by
                `scripts/sync_desktop_action_enum.py`; `bring_to_front` is the only tool that
                keeps an app or window in front (one action can use
                `"delivery_mode":"foreground"` instead).
              required: true
              max_length: 4096
            args_json:
              type: string
              description: JSON-encoded arguments for the subcommand (see top-level for examples).
              default: '{}'
              max_length: 4096
            screenshot_out_file:
              type: string
              description: Optional absolute path to write the captured screenshot PNG (only for capture
                actions).
              default: ''
              max_length: 4096
          timeout_secs: 30
    runtime_catalog:
      categories:
      - ui_automation
      - desktop_operations
      composition_category: desktop_operations
      expose_timeout_control: true
      timeout_default_secs: 30
---

# Desktop computer use (legacy skill ID: macos-ui-automation)

CuaDriver supports macOS, Windows and Linux. Choose the provider by the desktop
being controlled, not by the backend OS. Windows uses UI Automation/Win32;
Linux uses its graphical session and AT-SPI; macOS uses Accessibility and TCC.
The local controller runs only where a desktop session or running daemon exists.
Otherwise it uses the desktop host relay at `http://127.0.0.1:3017` (or
`MAGICIAN_HOST_GATEWAY_URL`). A headless Linux container does not need a local
CuaDriver. The private desktop relay works with the governed CLI's clean env.

## Setup by platform

- Windows: `py -3 scripts/setup-cua-driver.py --start` on the desktop host.
  Run as the signed-in user, not a Windows service or SSH Session 0.
- Linux: `make setup-cua-driver ARGS=--start` inside the target X11/Wayland
  desktop session. Keep its display and accessibility bus available; run
  `cua-driver doctor` for missing system dependencies and compositor limits.
- macOS: the same make target uses CuaDriver.app; grant it Accessibility and
  Screen Recording. Apple Events permission is independent of CUA.
- Read-only checks: `make check-cua-driver` locally, or
  `make check-cua-driver ARGS=--relay` from a headless backend.

The setup helper runs the [official platform installer](https://cua.ai/docs/how-to-guides/driver/install)
pinned to one CuaDriver release (`CUA_DRIVER_VERSION` in `scripts/setup-cua-driver.py`,
currently 0.28.2), with every installer script checked by hash. It replaces an
install of any other version and keeps a matching one unless `--upgrade` is
explicit; `--check` fails on version drift. Installing a binary
without a desktop is allowed for image preparation, but does not make CUA ready.
Headless backends normally use the relay. CUA availability does not enable iMessage,
AppleScript, or other Mac-only tools.

## Windows and Linux actions

Start with `list_apps` and `list_windows`, then inspect the installed driver's
platform tool schemas (`cua-driver list-tools`, `cua-driver describe <tool>`)
before constructing an action. Use
the returned window identifiers, accessibility elements, and screenshot geometry.
Do not apply Mac bundle IDs, osascript fallbacks, Retina scaling assumptions, or
Mac-only tools to Windows/Linux. Keep the observe → act → observe flow on all
platforms; compositor/toolkit support can limit Linux actions.

The detailed recipes below describe the existing **macOS** workflow only.
Use the [Windows](https://cua.ai/docs/reference/cua-driver/mcp-tools-windows) and
[Linux](https://cua.ai/docs/reference/cua-driver/mcp-tools-linux) tool references
for other hosts. The compatibility skill name does not select the platform.

## The daemon model (critical, read first)

Every `cua-driver call` goes through the daemon — reads included. The daemon
holds the snapshots (element addresses), recording state and the agent cursor.
The `macos-ui-automation__call` tool runs through a native controller that
starts the daemon before every call, and adds the shared `session` label
(`magician`, or `MAGICIAN_CUA_SESSION`) to each tool that takes one — without
it each call would run in a fresh session and cursor settings would not stick.
Pass your own `"session"` only to keep a run's state apart. You can also start
or inspect the daemon explicitly:

```bash
macos-ui-automation__call(action_name="serve", args_json="{}")
macos-ui-automation__call(action_name="status", args_json="{}")
```

This is idempotent and the daemon idles cheaply — leave it running for the
session. Do not call `macos-ui-automation__call(action_name="serve")` expecting
`cua-driver call serve`; the controller maps it to the top-level `cua-driver serve`
daemon lifecycle.

The daemon **must have its own TCC permissions** granted to `/Applications/CuaDriver.app` (Accessibility + Screen Recording). The CLI's permissions don't transfer. Verify with `cua-driver permissions status --json` (or `call health_report`); `doctor` warnings alone are not a readiness verdict.

**Recipes below use the shell form** `cua-driver call X '<json>' --screenshot-out-file F`.
From an agent, the same call is always
`macos-ui-automation__call(action_name="X", args_json='<json>', screenshot_out_file="F")` —
use the tool; its replies are compacted and name `screenshot_file` / `snapshot_file`.

## The combine recipe (memorize this — for any non-trivial UI task)

1. **Make sure the daemon is up.** The native controller auto-starts it for stateful
   actions. For an explicit preflight, call
   `macos-ui-automation__call(action_name="serve", args_json="{}")`. Do not use
   a shell bootstrap unless the controller itself reports that daemon startup failed.
   Without the daemon every call fails.

2. **Act in the background first.** cua-driver delivers clicks, text and keys to a
   backgrounded window. If a key or text does not land, repeat that ONE action with
   `"delivery_mode":"foreground","window_id":W` — it fronts the window for that action and
   restores focus. Use `bring_to_front` only when the app must stay in front, and
   `osascript` activation only as a last resort.

The controller reads each action's parameters from the driver's own
MCP `tools/list` and publishes them to
`~/.cua-driver/magician-driver-tools.json` (override `MAGICIAN_CUA_DRIVER_TOOLS`)
whenever the driver's version or the file's `format` changes. Each tool carries
`read_only`, the driver's `readOnlyHint`; the shared Decision Engine rail may
select observations and actions through the same authorized tool catalog.

3. **Snapshot the AX tree.** `cua-driver call get_window_state '{"pid":P,"window_id":W}'`. This populates the element_index cache. Pick the largest on-screen window from `list_windows` if multiple — or pass only `{"pid":P}` through this skill's tool and the controller picks the app's main window (on screen, largest) and names its `window_id` in the reply; it fails plainly when the app has no window on screen. `list_windows` also lists hidden helper windows (off-screen, 64×64); snapshotting or bringing one of those to front reads nothing.

   The tree you get back is the **compact view** (`tree_view: "labelled"`): the
   driver's tree in order, each element's state merged in. A line with `[N]` is an
   element you can act on; a line without one is text the pane shows — a heading, the
   label beside a control, the current value under a picker (`AXStaticText =
   "Multicolour"` after the colour buttons) — and is not a target. Unlabeled
   containers are dropped (a `*selected*` row stays, as the mark of the current
   sidebar entry), `*selected*` marks the chosen option and `(disabled)` what cannot
   be pressed, a role's usual action set is implied (a button presses; only a
   different set prints), an `id=` names a control that has no label, and the menu
   bar collapses to its menu-bar items (the first line says how many menu items are
   hidden; `invoke_menu` reaches them by path, or a `query` / `roles` view lists
   them). A 232-element Appearance pane is ~120 lines / ~1.5k tokens, which fits one
   tool-result page, so you will not need `read_result` to see a window. Element
   indices are the driver's own and unchanged. The structured `elements` (frames,
   tokens, selected/enabled) are **not inline** — they never fit a page — but in
   `snapshot_file` on disk, beside the screenshot. Narrow further with args on the
   same call — they are consumed before the driver sees them: `"query":"inbox"` (only
   nodes whose label/value/id contains the text, plus their descendants — menu items
   included), `"roles":["AXTextArea"]` (only those roles), `"max_lines":120`.
   `"filter":"full"` returns the driver's raw reply, elements and all, when the
   compact view hides what you need (rare: unlabeled groups you must click); it is
   paged. Never dump the state to a file and grep it with a script — ask for the view.

4. **Search the tree by label.** Walk the `tree_markdown` for the element you want. Labels like `AXButton (28)`, `AXLink (Apr)`, `AXTextArea (Compose message)`, `id=ChatBar_ComposerTextView` are gold — click them by index, with the snapshot they came from.

5. **Click by index, naming the snapshot.** `cua-driver call click '{"pid":P,"window_id":W,"snapshot_id":"s0000002e","element_index":N}'` — the reply's `snapshot_id` with the index — or `"element_token":"s0000002e:N"`. A bare `element_index` is **refused** (`snapshot_id_required`): the driver will not click an index from a snapshot it cannot tell is current. For rows that open on a double click (WhatsApp chat-list rows expose "Double tap to open chat" but no AXPress), use `double_click` with the same address — `click` has no double flag.

6. **Type.** `cua-driver call type_text '{"pid":P,"text":"...","window_id":W,"snapshot_id":"<snapshot_id>","element_index":N}'` focuses the element and types in one step (omit the address if the field is already focused). For Electron/web fields, which often ignore AX writes, target the field by pixel (`"x":X,"y":Y` in click space) or paste (step 6b). If the reply's `effect` is `unverifiable`, or a re-snapshot shows the field empty, the text did not land — resend with `"delivery_mode":"foreground"` before reaching for osascript.

   6b. **Paste path.** `clipboard_write {"text":"..."}` → click the field → `hotkey {"pid":P,"window_id":W,"keys":["cmd","v"]}`. Works where a field accepts only pasted or genuine keyboard input.

7. **Send/submit.** `cua-driver call press_key '{"pid":P,"key":"return"}'` for the final commit keystroke.

**A background window may not accept keystrokes.** Typing into a browser's address bar
can succeed while Return never submits — the query sits there and repeated `press_key`
does nothing. Resend that key with `"delivery_mode":"foreground","window_id":W`; use
`bring_to_front` only if focus must persist. If a key has had no effect twice, change the
delivery rather than pressing a third time.

**Menu commands:** `invoke_menu {"pid":P,"window_id":W,"path":["File","Export…"]}` runs a
menu-bar command by its exact path and fails closed — prefer it to hunting collapsed menu
items in the tree.

**Prove the effect:** `verify_state {"pid":P,"window_id":W,"expect":[…]}` checks predicates
on the window deterministically (see `cua-driver describe verify_state`) — cheaper and
more exact than re-reading a whole snapshot to confirm one change.

**Re-snapshot after any action that changes the UI** — element addresses are scoped to the snapshot that minted them. They go stale after any click that opens a chat, keystroke that filters results, picker that opens; a stale address fails closed (`stale_element_token`). Failing to re-snapshot is the #1 cause of wrong-target clicks.

## App Copilot smooth demonstration

For `@copilot`, `@app-copilot`, `@appcopilot`, `hey copilot`, or `hey app copilot` guided-action flows, make the automation visually followable before any visible click or pointing step. CUA has a visual agent cursor; enable it and tune it once per copilot session:

```bash
cua-driver call get_agent_cursor_state '{}'
cua-driver call set_agent_cursor_enabled '{"enabled":true}'
cua-driver call set_agent_cursor_motion '{"glide_duration_ms":750,"dwell_after_click_ms":400,"idle_hide_ms":8000,"start_handle":0.3,"end_handle":0.3,"arc_size":0.25,"arc_flow":0.0,"spring":0.72}'
```

Motion keys are **snake_case** — camelCase keys are silently ignored. Cursor state belongs
to a session: the controller supplies the shared label on every call, so these settings
hold for the clicks that follow. (From a raw shell, pass the same `"session":"<label>"` on
every call yourself.)

Use this as a visual affordance only. It does not replace the normal observe -> resolve -> draw/explain -> act -> observe -> verify loop:

1. Observe and resolve the target from the latest AX tree or screenshot.
2. Use `screen-draw` to point at or highlight the target.
3. If the user asked for action, click/type using the resolved `element_token` (or `snapshot_id` + `element_index`) or freshly refined coordinates.
4. Re-observe and verify the effect.

For pointing without clicking, use `move_cursor {x,y}` after the target is resolved: by default it moves only the agent-cursor overlay, in the window's click space; `"scope":"desktop"` moves the real pointer, in `get_desktop_state` pixel coordinates (not points). Do not treat a cursor move as proof that an app state changed.

## Finding the right pid + window_id

```
cua-driver call list_apps                          # find your bundle_id
cua-driver call launch_app '{"bundle_id":"net.whatsapp.WhatsApp"}'   # gets pid, launches hidden
cua-driver call list_windows '{"pid":<PID>}'       # pick the main window_id (usually largest, on-screen, layer 0)
```

## Coordinate spaces (memorize)

Four coordinate spaces exist; **using the wrong one is the #2 failure mode** after stale element_index. Each tool/output uses ONE of them:

1. **Screen logical points** — origin at top-left of the main display. Used by AppleScript `click at {x,y}` and general macOS APIs that take CGPoint. A window's bounds (`x,y,width,height` from `cua-driver call list_windows`) are in this space.

2. **Window-local logical points** — origin at top-left of the target window. Equals screen coords minus window's screen origin (`bounds.x`, `bounds.y`). Useful for reasoning about relative positions inside a window.

3. **cua-driver scaled-image space** — origin top-left of the PNG returned by `get_window_state`: window logical × backing scale, capped at `max_image_dimension` (1568 by default; see `get_config`). **This is what `cua-driver call click {x, y}` accepts** — the driver reverses the backing scale and downscale itself. Read `screenshot_width` / `screenshot_height`, `screenshot_scale` and `window_bounds` from the reply rather than assuming a ratio.

4. **Display pixels** — origin top-left of `get_desktop_state`'s full-display capture (true screen pixels, no downscale; 2× points on Retina). Used with `"scope":"desktop"` for targets that belong to no window (menu-bar extras, notifications). There is no separate `screenshot` tool.

When in doubt: take `get_window_state` first, read its `screenshot_width × screenshot_height`, and treat ALL subsequent `click {x,y}` coords in that space. To translate a screen-pts coord to cua-driver space:

```
cua_x = (screen_x − window_bounds.x) * (state.screenshot_width / window_bounds.width)
```

Use `zoom` to inspect specific regions at native resolution and `click {x, y, from_zoom: true}` to click within a zoomed image (daemon must be running — `from_zoom` reads the last zoom context).

## When to combine with osascript (last resort)

For React-based Electron compose inputs (Slack/WhatsApp/Discord message fields), an AX write can succeed without effect (it writes to a placeholder label). Try, in order: `type_text` with `"x","y"` on the field; the same with `"delivery_mode":"foreground","window_id":W`; the paste path (`clipboard_write` → click → `hotkey ["cmd","v"]`). Only if all of those fail:

1. `osascript -e 'tell application "<App>" to activate'` — gives the window real keyboard focus
2. `cua-driver call click '{"pid":P,"window_id":W,"snapshot_id":"S","element_index":N}'` on the compose field
3. `osascript -e 'tell application "System Events" to tell process "<App>" to keystroke "..."'` — sends genuine NSEvents that React inputs accept
4. `cua-driver call press_key '{"pid":P,"key":"return"}'` — sends/submits

## Tool quick reference

**Discovery / inspect:**
- `list_apps` — running + installed-but-not-running apps (with bundle_id)
- `launch_app` — start hidden, returns pid
- `list_windows` — top-level windows for a pid
- `get_window_state` — compact AX tree as Markdown with `[N]` element indexes, `snapshot_id`, screenshot metadata (`screenshot_file`, click space), and `snapshot_file` (the complete structured snapshot on disk)
- `get_window_state {"include_accessibility_tree":false}` — capture only: the window screenshot plus bounds and scale, no AX walk (there is no separate `screenshot` tool)
- `get_desktop_state` — the full display in true screen pixels, for targets outside any window
- `zoom` — padded (20%) JPEG crop of a click-space region, at most 500 px wide
- `verify_state {pid, window_id, expect}` — deterministic check that an effect happened
- `clipboard_read` — read the clipboard
- `cua-driver permissions status --json` / `cua-driver diagnose` — CLI commands (not `call` actions) for the TCC + bundle attribution check
- `get_screen_size` — main display logical size and backing scale
- `get_cursor_position` — current mouse position in screen points

**Input:**
Every element-addressed action takes `pid`, `window_id` and either `element_token` or `snapshot_id` + `element_index` (a bare index is refused), or `x`,`y` in click space; add `"delivery_mode":"foreground"` to one action that did not land in the background.
- `click` — left click (`button`, `count` on the pixel path, `action` on the AX path)
- `double_click`, `right_click` — explicit variants (use `double_click` for rows that open on double click)
- `type_text {pid, text, <address>?}` — focus + type; reply `effect` says whether it could verify the text landed
- `set_value {pid, value, <address>}` — direct AX value set (ignored by WebKit fields — use `type_text` there)
- `press_key {pid, key, modifiers?}` — keys: return, tab, escape, arrows, space, delete (= backspace), home, end, pageup, pagedown, f1–f12, letters, digits; `fn`+`delete` is forward delete
- `hotkey {pid, keys}` — modifier combos
- `scroll {pid, direction, amount?, by?, <address>?}` — direction up/down/left/right, amount in notches (1–50)
- `drag {pid, window_id, from_x, from_y, to_x, to_y}` — pixel-only
- `invoke_menu {pid, window_id, path}` — menu-bar command by exact path
- `clipboard_write {text}` — then `hotkey ["cmd","v"]` to paste
- `set_window_frame` — move/resize a window
- `move_cursor {x,y}` — agent-cursor overlay (window click space); `"scope":"desktop"` moves the real pointer in display pixels
- `bring_to_front {pid, window_id}` — raise the app AND focus that window; the reply
  verifies it (`code: bring_to_front_exact_window_verified`, `exact_window_effect.focused`).
  **This is the only tool that keeps a window in front** — there is no `activate_app`,
  `focus_window` or `raise_window`; for one action, pass `"delivery_mode":"foreground"`. `cua-driver list-tools` is the authoritative surface (56 tools in 0.28.2); a name
  that is not on it comes back as `Permission denied: tool 'x' has no reviewed risk
  classification`, which is what an unknown tool looks like, not a permission you can grant

**Session control:**
- `serve` — start daemon
- `stop` — gracefully stop daemon
- `status` — is the daemon running?
- `start_session` / `get_session` / `end_session` — the controller's shared label covers normal work; ending it is permanent for that name, so leave it unless a run must reset its state. The driver also ends an idle session by itself; the controller revives its own shared label once and retries the call, so idle expiry of the shared session is recovered automatically. A session you name yourself is yours to revive with `start_session`
- `desktop_unlocked` / `desktop_capture_authorized` in a `start_session` or `get_session_state` reply describe capture policy: whether whole-desktop capture has been unlocked beyond the window scope. They say nothing about the Mac's screen lock. `false` is the normal window-scoped state, so do not ask the user to unlock the Mac because of it; read the window you need with `get_window_state`
- `start_recording`, `stop_recording`, `get_recording_state` — trajectory capture for replay (`replay_trajectory` plays one back)

**Tutor visual cursor:**
- `get_agent_cursor_state` — inspect whether the visual cursor is enabled and what motion settings are active
- `set_agent_cursor_enabled {enabled}` — toggle the visual agent cursor overlay
- `set_agent_cursor_motion {glide_duration_ms?, dwell_after_click_ms?, idle_hide_ms?, start_handle?, end_handle?, arc_size?, arc_flow?, spring?, turn_radius?}` — tune the visible cursor sweep/dwell behavior for tutorials (snake_case; camelCase is ignored)
- `set_agent_cursor_theme` — select an installed cursor theme when a tutorial needs a different visual treatment

## Workflow patterns

### Pattern A — Messaging app (search → open chat → send)

```
cua-driver call get_window_state '{"pid":P,"window_id":W}'  # find the search field
cua-driver call hotkey '{"pid":P,"window_id":W,"keys":["cmd","f"]}'   # focus chat-list search
cua-driver call type_text '{"pid":P,"window_id":W,"text":"<query>"}'  # foreground delivery if it doesn't land
# wait ~1.5s for async search results to render
cua-driver call get_window_state '{"pid":P,"window_id":W}'  # fresh snapshot for result row
cua-driver call double_click '{"pid":P,"window_id":W,"snapshot_id":"<snapshot_id>","element_index":<row>}'   # WhatsApp rows open on double click
cua-driver call get_window_state '{"pid":P,"window_id":W}'  # fresh snapshot for compose field
cua-driver call click '{"pid":P,"window_id":W,"snapshot_id":"<snapshot_id>","element_index":<compose>}'
cua-driver call type_text '{"pid":P,"window_id":W,"text":"Hello"}'    # or the paste path
cua-driver call press_key '{"pid":P,"key":"return"}'      # send
```

### Pattern B — Canvas apps (Figma, Photoshop, video editors)

cua-driver alone is limited here — the canvas exposes no AX structure. Use the **grounding fallback** below: a vision model turns "the export button" into pixel coordinates, with mandatory zoom refinement before any click. (External app scripting APIs — Figma plugins, Photoshop scripting — remain better when they exist.)

## Grounding fallback (canvas / empty AX) — `POST /screen/ground`

Pixel-precise click targeting when the AX tree has nothing to click. Backed by a vision model via the `screen_grounding` operation (config-routed; see `docs/plans/2026-06-12-screen-grounding.md`). **A pixel click has no anchor — it clicks whatever is at those pixels and always "succeeds" — so every step below is mandatory.**

1. **AX first, always.** Only ground after `get_window_state` shows no usable element for the target.
2. **Capture the grounding frame** — a capture-only `get_window_state` (no AX walk) writes the click-space image to disk:
   ```bash
   cua-driver call get_window_state '{"pid":P,"window_id":W,"include_accessibility_tree":false}' --screenshot-out-file /tmp/win.png
   ```
   Its `screenshot_width × screenshot_height` (e.g. 1568x729) is the CLICK/ZOOM space, and it is also this image's size. Never ground on an old frame; if anything intervened, re-capture.
3. **Coarse grounding**:
   ```bash
   curl -s -X POST 'http://localhost:3002/api/magician/v2/screen/ground' \
     -H 'Content-Type: application/json' -H "Authorization: Bearer $MAGICIAN_BEARER_TOKEN" \
     -d "{\"target\": \"the play button\", \"image_b64\": \"$(base64 -i /tmp/win.png)\"}"
   # → {found, x, y, confidence, reasoning, image_width, image_height}  (coords in THE SENT IMAGE's space)
   ```
   `found: false` or `confidence < 0.5` → STOP and tell the user what you couldn't locate. Never click a guess.
   **Scale to click space**: `sx = x * screenshot_width / image_width`, `sy = y * screenshot_height / image_height`.
4. **Zoom refinement (mandatory)**: `cua-driver call zoom '{"pid":P,"window_id":W,"x1":sx-100,"y1":sy-55,"x2":sx+100,"y2":sy+55}' --screenshot-out-file /tmp/crop.jpg` (zoom takes a REGION box in click space; the crop comes back as JPEG — `/screen/ground` accepts it directly) → ground AGAIN on the crop → corrected (x,y) in CROP space.
5. **Act fresh**: `cua-driver call click '{"pid":P,"window_id":W,"x":X,"y":Y,"from_zoom":true}'` immediately after the refinement returns — from_zoom coords are the crop's own pixels; the daemon maps them back.
6. **Verify the EFFECT, not the click**: name the expected outcome BEFORE clicking (dialog opens, state toggles, panel appears), then check it with `verify_state`, a re-snapshot, or the app's scripting API. On failure: ONE retry from step 4, then report honestly.
7. **Audit**: state target, final coords, confidence, and the verified-effect verdict in your narration — a wrong click must be diagnosable afterwards.

Coordinate discipline (three spaces, one rule each): the GROUND answer is in whatever image you sent (window capture or crop — `image_width/height` is echoed so you can scale); CLICK/ZOOM-REGION coords are in `get_window_state`'s `screenshot_width × height` space; crop-refined coords pair ONLY with `from_zoom: true`. Screen logical points never appear in this recipe. A raw coarse-coordinate click without refinement is forbidden.

## Permissions setup (one-time)

1. Install: `make setup-cua-driver` (the pinned release; see "Setup by platform")
2. Grant: `cua-driver permissions grant` — launches CuaDriver through LaunchServices so the Accessibility, Screen Recording (and macOS direct-capture) requests belong to CuaDriver.app, then verifies live capture
3. If a pane still lacks it, in System Settings → Privacy & Security add `/Applications/CuaDriver.app` under **Screen Recording** and **Accessibility**
4. Restart the daemon: `cua-driver stop; open -n -g -a CuaDriver --args serve`
5. Verify: `cua-driver permissions status --json` — both true, attributed to `com.trycua.driver`

If `permissions status` reports `unknown` or false, or `cua-driver diagnose` shows the terminal as the owner, the grants went to the shell/terminal instead of CuaDriver.app — repeat step 2 (or step 3 with the `.app` path).

## Common gotchas (from real runs)

Pattern-match these symptoms and skip the failure:

- **`snapshot_id_required` / `stale_element_token` / "No cached AX state"**: the address is bare or from an older snapshot. Re-run `get_window_state` for this (pid, window_id) and resend with its new `snapshot_id` + `element_index` (or `element_token`).

- **TCC bundle attribution** — granting through a terminal's first prompt attributes to the terminal, not CuaDriver.app. Verify with `cua-driver permissions status --json`. If it reports the grants missing or `cua-driver diagnose` names the terminal: STOP, return the output, and ask the user to run `cua-driver permissions grant` (or grant `/Applications/CuaDriver.app` specifically).

- **`type_text` succeeds but the field still shows the placeholder** (reply `effect:"unverifiable"`, or a re-snapshot shows it empty): the AX write hit a label bound to nothing. Retarget by pixel, resend with `"delivery_mode":"foreground"`, or paste (`clipboard_write` → `hotkey ["cmd","v"]`); osascript keystroke is the last resort. Seen on WhatsApp compose and search.

- **Element-index goes stale** — any `get_window_state` invalidates prior indices for that window. The AX tree is rebuilt each call; indices are NOT stable across snapshots. Always pass the snapshot's `snapshot_id` and `element_index` to the very next `click` and re-snapshot before subsequent clicks. **This is the #1 cause of wrong-target clicks.**

- **Search results render async** — wait 1–2 seconds after typing a query before re-snapshotting. WhatsApp / Slack / Linear search dropdowns all need this. `sleep 1.5` then `get_window_state`.

- **WhatsApp chat-list rows need `double_click`** — a single click is a no-op; AX exposes "Double tap to open chat" but no AXPress action.

- **Cmd+F + typed text doesn't replace stale search content** — the search field keeps prior text. Always: Cmd+F → Cmd+A → Delete → type the new query. Or Escape twice to dismiss, then re-open.

- **App search / jump-to features may not index empty items** — in outline / hierarchical-content apps, search often returns only entries with existing content. If the target doesn't exist yet, search comes back "no matches" even though the navigation path is valid. Fall back to dedicated pickers (date pickers, file pickers) or to sidebar tree navigation — those create the target on navigation.

- **Decorative arrows ≠ navigation** — some calendar-strip / paginator widgets only scroll their visible ribbon; they don't change the underlying page. If clicking an arrow doesn't change the breadcrumb / page title in the next snapshot, you're paging UI chrome, not navigating. Find the real navigator (often a separate icon adjacent to the strip).

- **`from_zoom: true` in click fails with "no zoom context"** — zoom context is held by the daemon; if no daemon is running (or it was restarted), the context is gone. Either ensure daemon is running throughout, or compute the click coords manually using the zoom region offsets + padding (zoom adds 20% padding on each side).

- **WhatsApp / Slack first keystroke ignored** — after foregrounding (`delivery_mode:"foreground"`, `bring_to_front` or osascript activate), give the window ~0.5s to become key before sending keystrokes.

- **JSON args must be a single argument to `cua-driver call`** — always quote the JSON string. `cua-driver call click '{"pid":1,...}'` with single quotes is the safe form.

- **Daemon binds TCC to CuaDriver.app, not the calling shell** — restarting the daemon picks up newly-granted permissions. `cua-driver permissions status` answers through the daemon, so it reports CuaDriver's identity; `doctor` run from a shell may report the shell's. The actual proof is whether `get_window_state` returns a screenshot or fails with "Screen Recording not granted."

- **Date pickers in Electron apps** — per-day cells DO appear in the AX tree as `AXButton "<day>"` once the picker is open. Element-indexed clicks are reliable. The page's calendar-strip arrows however are often unlabeled — coord clicks only.

## When to NOT use cua-driver

- **Pure file / shell work** — use `files`, `shell`, `jq`.
- **Browser web automation** — `browser` (cua / Playwright) is richer for DOM-level work.
- **Background data extraction** — read the source (DB, API, file) rather than scraping a UI.
- **Pure-canvas tasks (Figma design surface, video timeline)** — vision-grounded tools or app plugin APIs are more reliable.
