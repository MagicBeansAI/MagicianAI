//! Yutori Navigator N1.5 action vocabulary → `agent-browser` CLI argv
//! translator.
//!
//! N1.5 is the active runtime model. N1 legacy compatibility is **not
//! supported** in this module — every code path here targets N1.5
//! exclusively. If you ever need to support older versions, fork.
//!
//! Yutori N1.5 is trained on its own native browser-action catalog
//! (`left_click`, `double_click`, `drag`, `type`, `scroll`,
//! `goto_url`, …) and emits all interactions through that catalog
//! regardless of what `tools` we pass. Earlier we tried to disable
//! N1.5's builtins and force it to emit our `agent-browser` tool
//! names directly; the model routinely produced wrong tool calls.
//! This module flips the responsibility: we let N1.5 emit its native
//! vocabulary and translate the resulting tool calls to one or more
//! `agent-browser` CLI invocations on this side. When a single Yutori
//! action expands to multiple agent-browser commands (a click is
//! `mouse move` → `mouse down` → `mouse up`, a drag is four steps),
//! we collapse them into a single `agent-browser batch --bail`
//! invocation so the entire sequence runs in one subprocess with the
//! configured inter-command spacing.
//!
//! ## N1.5 schema (canonical)
//!
//! Sources: `docs.yutori.com/reference/n1-5` and the reference SDK at
//! `yutori-ai/yutori-sdk-python/examples/navigator_n1_5.py`.
//!
//! - Coordinates: `coordinates: [x, y]` (integer array, normalised
//!   1000×1000 — magicllm denormalises to CSS pixels before this
//!   module sees the values).
//! - `drag`: `start_coordinates` (press) + `coordinates` (release).
//! - `scroll`: `coordinates`, `direction` (`up/down/left/right`), and
//!   `amount` (integer; reference SDK = `amount × 100` flat CSS
//!   pixels, NOT viewport-relative — the runtime authority over the
//!   docs' "≈10 %" wording). Emitted as `mouse move` → `mouse wheel
//!   <dy> [dx]` (cursor-anchored, mirroring the SDK's `mouse.wheel`),
//!   NOT agent-browser's page-level `scroll` command, so the
//!   `coordinates` anchor is honored.
//! - `wait`: `duration` in **seconds** (float), SDK clamps ≤ 100,
//!   default 5.
//! - `key_press`: `key` only — lowercase tokens like `ctrl+c`,
//!   `enter`, `left`; whitespace-separated tokens are a sequence
//!   ("down down enter") that we expand to one `press` per token.
//!   Each token is mapped from Yutori's lowercase vocabulary to
//!   agent-browser's Playwright key names (`enter`→`Enter`,
//!   `ctrl+c`→`Control+c`, `left`→`ArrowLeft`) via `map_key_token`.
//! - `type`: `text` only. Plain text input — no auto-clear, no
//!   auto-submit. The model chains a follow-up `key_press` to press
//!   Enter when it wants to submit, and a separate selection +
//!   delete sequence to clear.
//! - `mouse_move` / `mouse_down` / `mouse_up`: `coordinates`
//!   required.
//! - `hold_key`: `key` (combo) and optional `duration` in seconds.
//!   agent-browser has no raw `keyboard down/up` primitive, so we
//!   fall back to a single `press <key>` and lose the hold semantics
//!   — a future agent-browser feature.
//! - `modifier` (click + scroll, values:
//!   `ctrl/shift/alt/meta/command/super`) is currently dropped
//!   for the same reason as `hold_key`.
//! - `goto_url`: `url`. SDK auto-prefixes `https://` for schemeless
//!   URLs (`navigator_n1_5.py:636-637`); we mirror that.
//! - `event.isTrusted` on synthesised `dblclick` / triple-click
//!   sequences is `false` because CDP `Input.dispatchMouseEvent` has
//!   no `clickCount` exposure through agent-browser. Apps gating on
//!   `isTrusted` won't fire — irreducible without a new CLI
//!   primitive.

use anyhow::{anyhow, bail, Result};
use serde_json::Value;

use super::dispatch::{
    batch_command_spacing_ms, batch_mutation_settle_ms, space_batch_commands, BrowserCliInvocation,
};

/// Returns true when the Yutori action's expansion presses a mouse
/// button (and therefore needs a defensive `mouse up` cleanup if the
/// batch aborts mid-sequence). Used by the dispatcher to release a
/// stuck button on failure paths. `double_click` / `triple_click` are
/// excluded because they synthesise events via JS instead of real
/// `mouse down`/`mouse up` CDP events — there's no real button state
/// to clean up.
pub fn yutori_action_might_press_mouse(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "left_click" | "right_click" | "middle_click" | "drag" | "mouse_down"
    )
}

/// Returns true when `tool_name` is one of the Yutori N1.5 native
/// action names this translator handles or knows how to surface a
/// clean error for. The `BrowserDispatcher` calls this to decide
/// whether to route a tool call through the Yutori translator or
/// through the standard agent-browser argv mapping.
///
/// Includes both `browser_tools_core-20260403` (the set we actually
/// translate) AND the four `browser_tools_expanded-20260403` actions
/// (`extract_elements`, `find`, `set_element_value`, `execute_js`)
/// — those would otherwise fall through to the native dispatcher
/// path where `find` exists with a totally different signature and
/// the others don't exist at all, producing confusing CLI errors.
/// We recognise them so the translator can emit a clear "not yet
/// supported" message instead.
pub fn is_yutori_action(tool_name: &str) -> bool {
    matches!(
        tool_name,
        // browser_tools_core-20260403
        // click variants
        "left_click"
            | "right_click"
            | "middle_click"
            | "double_click"
            | "triple_click"
            // pointer / drag
            | "mouse_move"
            | "mouse_down"
            | "mouse_up"
            | "drag"
            // scroll / input
            | "scroll"
            | "type"
            | "key_press"
            | "hold_key"
            | "wait"
            // navigation
            | "goto_url"
            | "go_back"
            | "go_forward"
            | "refresh"
            // browser_tools_expanded-20260403 — recognised so we can
            // surface a clean "not supported" error rather than
            // routing through the native dispatcher.
            | "extract_elements"
            | "find"
            | "set_element_value"
            | "execute_js"
    )
}

/// Translate one Yutori action into an `agent-browser` CLI invocation.
///
/// Single-step actions return a plain argv invocation. Multi-step
/// expansions return a `batch` invocation with a JSON command matrix
/// on stdin so all the steps run in one subprocess with the
/// configured inter-command spacing.
pub fn translate_yutori_action(tool_name: &str, arguments: &Value) -> Result<BrowserCliInvocation> {
    let obj = arguments
        .as_object()
        .ok_or_else(|| anyhow!("yutori action `{tool_name}` arguments must be an object"))?;

    match tool_name {
        "left_click" => {
            let (x, y) = extract_coordinates(obj, "coordinates")?;
            Ok(batch(vec![
                mouse_move_argv(x, y),
                vec!["mouse".into(), "down".into()],
                vec!["mouse".into(), "up".into()],
            ]))
        },
        "right_click" => {
            let (x, y) = extract_coordinates(obj, "coordinates")?;
            Ok(batch(vec![
                mouse_move_argv(x, y),
                vec!["mouse".into(), "down".into(), "right".into()],
                vec!["mouse".into(), "up".into(), "right".into()],
            ]))
        },
        "middle_click" => {
            let (x, y) = extract_coordinates(obj, "coordinates")?;
            Ok(batch(vec![
                mouse_move_argv(x, y),
                vec!["mouse".into(), "down".into(), "middle".into()],
                vec!["mouse".into(), "up".into(), "middle".into()],
            ]))
        },
        "double_click" => {
            // agent-browser doesn't expose a coords-based dblclick,
            // but it DOES expose `dblclick <selector>` which
            // internally uses CDP `Input.dispatchMouseEvent` with
            // `clickCount: 2` — a real dblclick with
            // `event.isTrusted === true`. We bridge coords →
            // selector by marking the element at (x, y) with a
            // unique data attribute, calling agent-browser's
            // high-level `dblclick`, then cleaning up the marker.
            // Three steps in a batch: mark → dblclick → cleanup.
            // `--bail` would skip the cleanup on failure, so step 1
            // pre-cleans any stale marker from a prior aborted call
            // before setting a fresh one.
            let (x, y) = extract_coordinates(obj, "coordinates")?;
            Ok(batch(vec![
                vec!["eval".into(), dblclick_mark_js(x, y)],
                vec!["dblclick".into(), "[data-yt-dbl]".into()],
                vec!["eval".into(), dblclick_unmark_js()],
            ]))
        },
        "triple_click" => {
            // Same constraint as `double_click`: CDP needs
            // `clickCount: 3` at dispatch time. Synthesised via
            // `eval`: three click sequences with detail 1, 2, 3
            // respectively, plus a `dblclick` after the second click
            // (browsers don't have a triple-click event; apps key off
            // `click` with `detail === 3`).
            let (x, y) = extract_coordinates(obj, "coordinates")?;
            Ok(single(vec!["eval".into(), tripleclick_eval_js(x, y)]))
        },
        "mouse_move" => {
            let (x, y) = extract_coordinates(obj, "coordinates")?;
            Ok(single(mouse_move_argv(x, y)))
        },
        "mouse_down" => {
            // N1.5 spec: `coordinates` is required. Anchor the
            // pointer with a leading `mouse move` step so the press
            // lands at the model's intended location, not wherever
            // the cursor happened to be.
            let (x, y) = extract_coordinates(obj, "coordinates")?;
            Ok(batch(vec![
                mouse_move_argv(x, y),
                vec!["mouse".into(), "down".into()],
            ]))
        },
        "mouse_up" => {
            let (x, y) = extract_coordinates(obj, "coordinates")?;
            Ok(batch(vec![
                mouse_move_argv(x, y),
                vec!["mouse".into(), "up".into()],
            ]))
        },
        "drag" => {
            // N1.5 schema: `start_coordinates` (press point) +
            // `coordinates` (release point). No `end_coordinates`
            // field exists.
            let (sx, sy) = extract_coordinates(obj, "start_coordinates")?;
            let (ex, ey) = extract_coordinates(obj, "coordinates")?;
            Ok(batch(vec![
                mouse_move_argv(sx, sy),
                vec!["mouse".into(), "down".into()],
                mouse_move_argv(ex, ey),
                vec!["mouse".into(), "up".into()],
            ]))
        },
        "scroll" => {
            // N1.5 spec: `coordinates`, `direction`, `amount`. The reference
            // SDK (`navigator_n1_5.py:571-574`) scrolls at the cursor via
            // `playwright.mouse.wheel(...)`, defaulting missing `direction` to
            // `"down"` and `amount` to `3` (→ `amount × 100` px).
            //
            // We mirror that with agent-browser's `mouse wheel <dy> [dx]`, which
            // — like Playwright's `mouse.wheel` — dispatches the wheel event at
            // the CURRENT cursor position, so the scrollable element under
            // (x, y) is what moves. We deliberately do NOT use agent-browser's
            // high-level `scroll <dir> <px>`: that scrolls the page (or a
            // `--selector` container) and ignores the cursor, which would
            // silently drop Yutori's `coordinates` anchor — the exact failure
            // the leading `mouse move` is meant to prevent. `coordinates` stays
            // required so the anchor is always set before the wheel.
            let (x, y) = extract_coordinates(obj, "coordinates")?;
            let direction = obj
                .get("direction")
                .and_then(Value::as_str)
                .unwrap_or("down")
                .to_lowercase();
            let units = obj
                .get("amount")
                .and_then(Value::as_i64)
                .unwrap_or(3)
                .max(1);
            let pixels = units * 100;
            // agent-browser `mouse wheel <dy> [dx]`: +dy scrolls down, +dx right
            // (Playwright delta convention). Horizontal needs an explicit dy=0.
            let wheel = match direction.as_str() {
                "up" => vec!["mouse".into(), "wheel".into(), (-pixels).to_string()],
                "left" => {
                    vec![
                        "mouse".into(),
                        "wheel".into(),
                        "0".into(),
                        (-pixels).to_string(),
                    ]
                },
                "right" => vec![
                    "mouse".into(),
                    "wheel".into(),
                    "0".into(),
                    pixels.to_string(),
                ],
                // "down" and any unexpected direction default to scrolling down.
                _ => vec!["mouse".into(), "wheel".into(), pixels.to_string()],
            };
            Ok(batch(vec![mouse_move_argv(x, y), wheel]))
        },
        "type" => {
            // N1.5 `type` is plain text input — `text` is the only
            // field. No auto-clear, no auto-submit. The model chains
            // a follow-up `key_press {"key": "enter"}` to submit and
            // a separate `triple_click` + `key_press {"key": "delete"}`
            // to clear.
            let text = obj
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("`type` requires `text`"))?;
            Ok(single(vec![
                "keyboard".into(),
                "type".into(),
                text.to_string(),
            ]))
        },
        "key_press" => {
            // N1.5 schema: `key` field, lowercase tokens like
            // `ctrl+c`, `enter`, `left`. Whitespace-separated tokens
            // are a sequence ("down down enter") that we expand to
            // one `press` per token, since agent-browser `press`
            // handles one combo at a time.
            let key = obj
                .get("key")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("`key_press` requires `key`"))?;
            let tokens: Vec<&str> = key.split_whitespace().collect();
            if tokens.len() <= 1 {
                Ok(single(vec!["press".into(), map_key_token(key.trim())]))
            } else {
                let steps = tokens
                    .into_iter()
                    .map(|t| vec!["press".into(), map_key_token(t)])
                    .collect::<Vec<_>>();
                Ok(batch(steps))
            }
        },
        "hold_key" => {
            // N1.5-only. Schema: `key` (combo, e.g. "shift") and
            // optional `duration` in seconds (SDK clamps ≤ 100).
            // agent-browser exposes no raw key down/up CLI primitive,
            // so we cannot synthesise a real hold via CDP. Best
            // available approximation: emit a single `press <key>` —
            // this loses the hold semantics but at least delivers the
            // keystroke. Models that rely on the hold (e.g. drag-with-
            // shift to extend selection) will need a future
            // agent-browser primitive (`keyboard down/up <key>`).
            let key = obj
                .get("key")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("`hold_key` requires `key`"))?;
            Ok(single(vec!["press".into(), map_key_token(key)]))
        },
        "wait" => {
            // Yutori spec: `duration` is **seconds** (float). SDK
            // clamps ≤ 100. Convert to ms for agent-browser `wait
            // <ms>`. Default 5 seconds when omitted (N1 + N1.5 SDK
            // default).
            let seconds = obj
                .get("duration")
                .and_then(Value::as_f64)
                .unwrap_or(5.0)
                .clamp(0.0, 100.0);
            let ms = (seconds * 1000.0).round() as i64;
            Ok(single(vec!["wait".into(), ms.to_string()]))
        },
        "goto_url" => {
            // SDK reference (`navigator_n1_5.py` line 636-637)
            // auto-prefixes `https://` for schemeless URLs — N1.5
            // sometimes emits bare hostnames like `"yutori.com"`. We
            // mirror the same behaviour so a schemeless URL doesn't
            // hit `agent-browser open` as a relative path. Detection
            // is `"://"` substring (matches the SDK exactly).
            let raw = obj
                .get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("`goto_url` requires `url`"))?;
            let url = if raw.contains("://") {
                raw.to_string()
            } else {
                format!("https://{raw}")
            };
            Ok(single(vec!["open".into(), url]))
        },
        "go_back" => Ok(single(vec!["back".into()])),
        "go_forward" => Ok(single(vec!["forward".into()])),
        "refresh" => Ok(single(vec!["reload".into()])),
        // browser_tools_expanded-20260403 — recognised but not
        // implemented. These actions use ref-based addressing
        // (`extract_elements` / `find` populate refs that
        // `set_element_value` / clicks then consume), which the
        // magician runtime doesn't currently inject into the DOM.
        // Surface a clear error so the model retries with a
        // coords-based core action rather than blocking on a
        // confusing "unknown command" from the native dispatcher.
        "extract_elements" | "find" | "set_element_value" | "execute_js" => {
            bail!(
                "yutori action `{tool_name}` is part of the N1.5 expanded toolset \
                (`browser_tools_expanded-20260403`) and is not currently supported \
                by the backend runtime. Use coords-based core actions \
                (`left_click`, `type`, `scroll`, …) instead."
            )
        },
        other => bail!("yutori action `{other}` is not supported by the translator"),
    }
}

/// Map a Yutori key token to agent-browser's Playwright-style key name.
///
/// Yutori N1.5 emits lowercase tokens (`enter`, `esc`, `ctrl+c`, `left`),
/// while agent-browser's `press` expects Playwright names (`Enter`, `Escape`,
/// `Control+c`, `ArrowLeft`). A combo (`ctrl+c`) maps each `+`-separated part
/// independently. Tokens that are already correct, printable single characters,
/// or otherwise unrecognized pass through unchanged so we never *lose* a key.
fn map_key_token(token: &str) -> String {
    token
        .split('+')
        .map(|part| map_key_part(part.trim()))
        .collect::<Vec<_>>()
        .join("+")
}

/// Map one `+`-separated part of a key token. See [`map_key_token`].
fn map_key_part(part: &str) -> String {
    let lower = part.to_lowercase();
    let mapped = match lower.as_str() {
        "ctrl" | "control" => "Control",
        "cmd" | "command" | "meta" | "super" | "win" => "Meta",
        "alt" | "option" => "Alt",
        "shift" => "Shift",
        "enter" | "return" => "Enter",
        "tab" => "Tab",
        "esc" | "escape" => "Escape",
        "space" | "spacebar" => "Space",
        "backspace" => "Backspace",
        "delete" | "del" => "Delete",
        "insert" | "ins" => "Insert",
        "up" | "arrowup" => "ArrowUp",
        "down" | "arrowdown" => "ArrowDown",
        "left" | "arrowleft" => "ArrowLeft",
        "right" | "arrowright" => "ArrowRight",
        "home" => "Home",
        "end" => "End",
        "pageup" | "pgup" => "PageUp",
        "pagedown" | "pgdn" => "PageDown",
        "capslock" => "CapsLock",
        _ => {
            // Function keys f1..f12 → F1..F12.
            if let Some(num) = lower.strip_prefix('f') {
                if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
                    return format!("F{num}");
                }
            }
            // Single printable char or already-correct token: pass through
            // unchanged (Playwright `press` accepts `a`, `A`, `1`, `/`, …).
            return part.to_string();
        },
    };
    mapped.to_string()
}

fn extract_coordinates(obj: &serde_json::Map<String, Value>, key: &str) -> Result<(i64, i64)> {
    let raw = obj.get(key).ok_or_else(|| anyhow!("missing `{key}`"))?;
    parse_xy_value(raw).ok_or_else(|| anyhow!("`{key}` must be [x, y]"))
}

fn parse_xy_value(raw: &Value) -> Option<(i64, i64)> {
    let arr = raw.as_array()?;
    if arr.len() < 2 {
        return None;
    }
    let x = arr[0].as_i64()?;
    let y = arr[1].as_i64()?;
    Some((x, y))
}

fn mouse_move_argv(x: i64, y: i64) -> Vec<String> {
    vec!["mouse".into(), "move".into(), x.to_string(), y.to_string()]
}

/// Mark the element at `(x, y)` with `data-yt-dbl="1"` so agent-
/// browser's high-level `dblclick "[data-yt-dbl]"` (which uses real
/// CDP `Input.dispatchMouseEvent` with `clickCount: 2`) can target
/// it. Pre-cleans any stale marker from a prior aborted call before
/// setting the fresh one — `batch --bail` skips the cleanup step on
/// failure, so this defensive pre-clean keeps marker drift bounded.
///
/// When `elementFromPoint(x, y)` returns null (coords off-screen,
/// over a `pointer-events:none` element, or page hadn't laid out) we
/// `throw` — `agent-browser eval` exits non-zero with the error
/// message in stderr, the dispatcher surfaces that as a tool failure,
/// and the model gets a clear signal to retry.
fn dblclick_mark_js(x: i64, y: i64) -> String {
    format!(
        "(function(){{\
document.querySelectorAll('[data-yt-dbl]').forEach(function(e){{e.removeAttribute('data-yt-dbl')}});\
var el=document.elementFromPoint({x},{y});\
if(!el){{throw new Error('elementFromPoint({x},{y}) returned null — no element at coords');}}\
el.setAttribute('data-yt-dbl','1');\
return true;\
}})()"
    )
}

/// Remove the `data-yt-dbl` marker from every matching element. Runs
/// after the dblclick step in a batch.
fn dblclick_unmark_js() -> String {
    "(function(){document.querySelectorAll('[data-yt-dbl]').forEach(function(e){e.removeAttribute('data-yt-dbl')});return true;})()"
        .to_string()
}

/// Synthesise a triple-click at `(x, y)`. Browsers don't have a
/// dedicated triple-click event — apps key off `click` with `detail
/// === 3` (text-selection on triple-click selects the paragraph). We
/// dispatch the same per-click sequence as `dblclick_eval_js` for
/// `detail: 1`, `detail: 2` (plus a `dblclick`), and `detail: 3`.
/// Same null-element `throw` semantics so the model sees a real tool
/// failure rather than silent success.
fn tripleclick_eval_js(x: i64, y: i64) -> String {
    format!(
        "(function(){{\
var el=document.elementFromPoint({x},{y});\
if(!el){{throw new Error('elementFromPoint({x},{y}) returned null — no element at coords');}}\
var P=window.PointerEvent||MouseEvent;\
var pe=function(t,d){{el.dispatchEvent(new P(t,{{bubbles:true,cancelable:true,clientX:{x},clientY:{y},pointerId:1,pointerType:'mouse',isPrimary:true,detail:d,button:0,buttons:t==='pointerdown'?1:0}}))}};\
var me=function(t,d,b){{el.dispatchEvent(new MouseEvent(t,{{bubbles:true,cancelable:true,view:window,clientX:{x},clientY:{y},detail:d,button:0,buttons:b}}))}};\
pe('pointerdown',1);me('mousedown',1,1);\
pe('pointerup',1);me('mouseup',1,0);\
me('click',1,0);\
pe('pointerdown',2);me('mousedown',2,1);\
pe('pointerup',2);me('mouseup',2,0);\
me('click',2,0);\
me('dblclick',2,0);\
pe('pointerdown',3);me('mousedown',3,1);\
pe('pointerup',3);me('mouseup',3,0);\
me('click',3,0);\
return true;\
}})()"
    )
}

fn single(argv: Vec<String>) -> BrowserCliInvocation {
    BrowserCliInvocation {
        argv: argv.clone(),
        stdin: None,
        artifact_commands: vec![argv],
    }
}

fn batch(commands: Vec<Vec<String>>) -> BrowserCliInvocation {
    // Match the native batch path's spacing semantics: insert
    // `["wait", "<ms>"]` between consecutive subcommands so rapid
    // pointer-event sequences (drag, scroll-with-anchor) don't drop
    // intermediate events. The native `batch` invocation builder in
    // dispatch.rs has done this since the helper landed; the Yutori
    // translator now reuses the same `space_batch_commands` to keep
    // behaviour consistent. The base gap is configurable via
    // MAGICIAN_AGENT_BROWSER_BATCH_SPACING_MS (default 50 ms); a longer
    // settle gap (MAGICIAN_AGENT_BROWSER_MUTATION_SETTLE_MS, default 150 ms)
    // is used after a high-level mutation so the page reaction lands first.
    // Artifact detection inspects the LLM-authored commands, not the
    // spaced version, so synthetic waits never produce artifacts.
    let argv = vec!["batch".into(), "--bail".into()];
    let mut artifact_commands = vec![argv.clone()];
    artifact_commands.extend(commands.clone());
    let spaced = space_batch_commands(
        commands,
        batch_command_spacing_ms(),
        batch_mutation_settle_ms(),
    );
    let stdin = serde_json::to_string(&spaced).expect("commands serializable");
    BrowserCliInvocation {
        argv,
        stdin: Some(stdin),
        artifact_commands,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::json;

    fn batch_commands(inv: &BrowserCliInvocation) -> Vec<Vec<String>> {
        let stdin = inv.stdin.as_deref().expect("batch invocation has stdin");
        serde_json::from_str(stdin).expect("stdin is valid command matrix JSON")
    }

    /// Batches go through `space_batch_commands` so consecutive
    /// non-wait commands get a `["wait","<ms>"]` step between them.
    /// Tests that care about the user-authored steps (not the
    /// spacing) filter out the synthetic waits to keep assertions
    /// readable. The dispatch path tests cover spacing directly.
    fn user_commands(inv: &BrowserCliInvocation) -> Vec<Vec<String>> {
        batch_commands(inv)
            .into_iter()
            .filter(|c| c.first().map(String::as_str) != Some("wait"))
            .collect()
    }

    #[test]
    fn left_click_expands_to_batch_of_three_mouse_commands() {
        let inv =
            translate_yutori_action("left_click", &json!({"coordinates": [640, 400]})).unwrap();
        assert_eq!(inv.argv, vec!["batch", "--bail"]);
        assert_eq!(
            user_commands(&inv),
            vec![
                vec!["mouse", "move", "640", "400"],
                vec!["mouse", "down"],
                vec!["mouse", "up"],
            ]
        );
    }

    #[test]
    fn batch_inserts_wait_between_user_commands() {
        // Spacing matches the native batch path's behaviour. Default
        // is 50 ms via MAGICIAN_AGENT_BROWSER_BATCH_SPACING_MS.
        let inv = translate_yutori_action("left_click", &json!({"coordinates": [10, 20]})).unwrap();
        let raw = batch_commands(&inv);
        assert_eq!(raw.len(), 5, "expected move + wait + down + wait + up");
        assert_eq!(raw[0][0], "mouse");
        assert_eq!(raw[1][0], "wait");
        assert_eq!(raw[2][0], "mouse");
        assert_eq!(raw[3][0], "wait");
        assert_eq!(raw[4][0], "mouse");
    }

    #[test]
    fn drag_reads_end_from_canonical_coordinates_field() {
        let inv = translate_yutori_action(
            "drag",
            &json!({
                "start_coordinates": [10, 20],
                "coordinates": [100, 200],
            }),
        )
        .unwrap();
        let cmds = user_commands(&inv);
        assert_eq!(cmds.len(), 4);
        assert_eq!(cmds[0], vec!["mouse", "move", "10", "20"]);
        assert_eq!(cmds[1], vec!["mouse", "down"]);
        assert_eq!(cmds[2], vec!["mouse", "move", "100", "200"]);
        assert_eq!(cmds[3], vec!["mouse", "up"]);
    }

    #[test]
    fn type_is_plain_text_input() {
        let inv = translate_yutori_action("type", &json!({"text": "hello"})).unwrap();
        assert_eq!(inv.argv, vec!["keyboard", "type", "hello"]);
        assert!(inv.stdin.is_none());
    }

    #[test]
    fn goto_url_passes_through_when_url_has_scheme() {
        let inv =
            translate_yutori_action("goto_url", &json!({"url": "https://example.com"})).unwrap();
        assert_eq!(inv.argv, vec!["open", "https://example.com"]);
    }

    #[test]
    fn goto_url_auto_prefixes_https_for_schemeless_url() {
        let inv = translate_yutori_action("goto_url", &json!({"url": "yutori.com"})).unwrap();
        assert_eq!(inv.argv, vec!["open", "https://yutori.com"]);
    }

    #[test]
    fn goto_url_does_not_double_prefix_http() {
        let inv =
            translate_yutori_action("goto_url", &json!({"url": "http://example.com"})).unwrap();
        assert_eq!(inv.argv, vec!["open", "http://example.com"]);
    }

    #[test]
    fn scroll_uses_cursor_anchored_wheel_at_one_hundred_pixels_per_unit() {
        // SDK reference (navigator_n1_5.py:574): playwright.mouse.wheel at the
        // cursor, amount * 100 px. We anchor with `mouse move` then `mouse wheel`
        // (cursor-anchored) — NOT the page-level `scroll` command.
        let inv = translate_yutori_action(
            "scroll",
            &json!({"coordinates": [100, 200], "direction": "down", "amount": 3}),
        )
        .unwrap();
        let cmds = user_commands(&inv);
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0], vec!["mouse", "move", "100", "200"]);
        assert_eq!(cmds[1], vec!["mouse", "wheel", "300"]);
    }

    #[test]
    fn scroll_directions_map_to_signed_wheel_deltas() {
        let cases = [
            ("up", vec!["mouse", "wheel", "-300"]),
            ("down", vec!["mouse", "wheel", "300"]),
            ("left", vec!["mouse", "wheel", "0", "-300"]),
            ("right", vec!["mouse", "wheel", "0", "300"]),
        ];
        for (dir, expected) in cases {
            let inv = translate_yutori_action(
                "scroll",
                &json!({"coordinates": [10, 10], "direction": dir, "amount": 3}),
            )
            .unwrap();
            let cmds = user_commands(&inv);
            assert_eq!(
                cmds[1],
                expected.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                "direction {dir}"
            );
        }
    }

    #[test]
    fn scroll_defaults_direction_and_amount_per_sdk() {
        // SDK (navigator_n1_5.py:571-572) defaults `direction = "down"`
        // and `amount = 3` if missing. Only `coordinates` is hard-
        // required (a cursor-anchored wheel without an anchor scrolls
        // from wherever the cursor happens to be).
        let inv = translate_yutori_action("scroll", &json!({"coordinates": [50, 60]})).unwrap();
        let cmds = user_commands(&inv);
        assert_eq!(cmds[0], vec!["mouse", "move", "50", "60"]);
        assert_eq!(cmds[1], vec!["mouse", "wheel", "300"]);
    }

    #[test]
    fn scroll_requires_coordinates() {
        let err = translate_yutori_action("scroll", &json!({"direction": "down", "amount": 1}))
            .unwrap_err();
        assert!(err.to_string().contains("coordinates"));
    }

    #[test]
    fn key_press_maps_lowercase_combo_to_playwright_names() {
        // Yutori emits lowercase `ctrl+c`; agent-browser `press` wants
        // Playwright `Control+c`. The single-char part stays as-is.
        let inv = translate_yutori_action("key_press", &json!({"key": "ctrl+c"})).unwrap();
        assert_eq!(inv.argv, vec!["press", "Control+c"]);
    }

    #[test]
    fn key_press_maps_named_keys() {
        for (yutori, expected) in [
            ("enter", "Enter"),
            ("esc", "Escape"),
            ("cmd+a", "Meta+a"),
            ("f5", "F5"),
        ] {
            let inv = translate_yutori_action("key_press", &json!({ "key": yutori })).unwrap();
            assert_eq!(inv.argv, vec!["press".to_string(), expected.to_string()]);
        }
    }

    #[test]
    fn key_press_space_separated_sequence_expands_to_batch() {
        let inv =
            translate_yutori_action("key_press", &json!({"key": "down down down enter"})).unwrap();
        assert_eq!(
            user_commands(&inv),
            vec![
                vec!["press", "ArrowDown"],
                vec!["press", "ArrowDown"],
                vec!["press", "ArrowDown"],
                vec!["press", "Enter"],
            ]
        );
    }

    #[test]
    fn wait_treats_duration_as_seconds() {
        let inv = translate_yutori_action("wait", &json!({"duration": 5})).unwrap();
        assert_eq!(inv.argv, vec!["wait", "5000"]);
    }

    #[test]
    fn wait_default_is_five_seconds() {
        let inv = translate_yutori_action("wait", &json!({})).unwrap();
        assert_eq!(inv.argv, vec!["wait", "5000"]);
    }

    #[test]
    fn wait_clamps_at_one_hundred_seconds() {
        let inv = translate_yutori_action("wait", &json!({"duration": 1000})).unwrap();
        assert_eq!(inv.argv, vec!["wait", "100000"]);
    }

    #[test]
    fn mouse_down_anchors_pointer_first_with_coordinates() {
        let inv = translate_yutori_action("mouse_down", &json!({"coordinates": [50, 60]})).unwrap();
        assert_eq!(
            user_commands(&inv),
            vec![vec!["mouse", "move", "50", "60"], vec!["mouse", "down"]]
        );
    }

    #[test]
    fn mouse_down_requires_coordinates_per_n1_5_spec() {
        let err = translate_yutori_action("mouse_down", &json!({})).unwrap_err();
        assert!(err.to_string().contains("coordinates"));
    }

    #[test]
    fn mouse_up_requires_coordinates_per_n1_5_spec() {
        let err = translate_yutori_action("mouse_up", &json!({})).unwrap_err();
        assert!(err.to_string().contains("coordinates"));
    }

    #[test]
    fn double_click_marks_element_then_uses_real_cdp_dblclick() {
        // Critical upgrade: previously synthesised dblclick via JS
        // event dispatch (isTrusted=false). New approach marks the
        // element at (x, y) with a unique data attribute, calls
        // agent-browser's high-level `dblclick` primitive (which
        // dispatches CDP `Input.dispatchMouseEvent` with
        // `clickCount: 2` for a real, isTrusted=true dblclick),
        // then cleans up the marker.
        let inv =
            translate_yutori_action("double_click", &json!({"coordinates": [400, 300]})).unwrap();
        let cmds = user_commands(&inv);
        assert_eq!(cmds.len(), 3);
        // Step 1: eval that marks element at coords.
        assert_eq!(cmds[0][0], "eval");
        assert!(cmds[0][1].contains("elementFromPoint(400,300)"));
        assert!(cmds[0][1].contains("setAttribute('data-yt-dbl','1')"));
        // Pre-cleanup of stale markers from prior aborted calls.
        assert!(cmds[0][1].contains("querySelectorAll('[data-yt-dbl]')"));
        // Throws on null elementFromPoint so the model sees the
        // failure as a tool error, not silent success.
        assert!(cmds[0][1].contains("throw new Error"));
        // Step 2: real CDP dblclick via the marker selector.
        assert_eq!(cmds[1], vec!["dblclick", "[data-yt-dbl]"]);
        // Step 3: eval that removes the marker.
        assert_eq!(cmds[2][0], "eval");
        assert!(cmds[2][1].contains("removeAttribute('data-yt-dbl')"));
    }

    #[test]
    fn tripleclick_eval_throws_when_no_element_at_point() {
        // Triple-click stays eval-based: agent-browser has no
        // `tripleclick` primitive and CDP needs `clickCount: 3` for
        // a real triple-click (text-selection-on-triple-click).
        // Until upstream exposes one, eval is the best we can do.
        let inv =
            translate_yutori_action("triple_click", &json!({"coordinates": [400, 300]})).unwrap();
        let js = &inv.argv[1];
        assert!(js.contains("throw new Error"));
    }

    #[test]
    fn expanded_toolset_actions_bail_with_clear_message() {
        // Round-4 audit M2: extract_elements / find / set_element_value
        // / execute_js are part of the N1.5 expanded toolset which
        // the magician runtime doesn't currently support. Without
        // recognition they'd fall through to the native dispatcher
        // path where `find` exists with a different signature and
        // the others don't exist at all. We recognise them here so
        // the model gets a clean "use coords-based core actions"
        // message rather than a confusing CLI error.
        for name in [
            "extract_elements",
            "find",
            "set_element_value",
            "execute_js",
        ] {
            assert!(
                is_yutori_action(name),
                "{name} must be recognised by is_yutori_action so it routes through this translator"
            );
            let err = translate_yutori_action(name, &json!({})).unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("expanded toolset") && msg.contains("core actions"),
                "expected expanded-toolset bail message for `{name}`, got: {msg}"
            );
        }
    }

    #[test]
    fn yutori_action_might_press_mouse_only_for_button_pressing_actions() {
        for name in [
            "left_click",
            "right_click",
            "middle_click",
            "drag",
            "mouse_down",
        ] {
            assert!(
                yutori_action_might_press_mouse(name),
                "{name} expands to a real mouse-down and needs cleanup on failure"
            );
        }
        // double_click / triple_click are eval-based; mouse_up
        // doesn't press anything; the rest don't touch buttons.
        for name in [
            "double_click",
            "triple_click",
            "mouse_up",
            "mouse_move",
            "scroll",
            "type",
            "key_press",
            "wait",
            "goto_url",
            "go_back",
            "go_forward",
            "refresh",
            "hold_key",
        ] {
            assert!(
                !yutori_action_might_press_mouse(name),
                "{name} does not press a real mouse button"
            );
        }
    }

    #[test]
    fn triple_click_synthesises_via_eval_with_three_click_phases() {
        let inv =
            translate_yutori_action("triple_click", &json!({"coordinates": [400, 300]})).unwrap();
        assert_eq!(inv.argv[0], "eval");
        let js = &inv.argv[1];
        assert!(js.contains("pointerdown"));
        assert!(js.contains("'click',1"));
        assert!(js.contains("'click',2"));
        assert!(js.contains("'click',3"));
        // `dblclick` fires once per the spec (after the second click,
        // not the third).
        assert!(js.contains("dblclick"));
    }

    #[test]
    fn hold_key_falls_back_to_press_due_to_no_cli_primitive() {
        let inv = translate_yutori_action("hold_key", &json!({"key": "Shift"})).unwrap();
        assert_eq!(inv.argv, vec!["press", "Shift"]);
    }

    #[test]
    fn truly_unknown_action_returns_error() {
        // Names we actively recognise (core + expanded) take other
        // match arms with their own messages. This guards the
        // catch-all `_ => bail!` at the bottom of the match.
        let err = translate_yutori_action("completely_made_up_action", &json!({})).unwrap_err();
        assert!(err.to_string().contains("not supported"));
    }

    #[test]
    fn is_yutori_action_recognises_n1_5_core_set_and_excludes_n1_legacy() {
        for name in [
            "left_click",
            "right_click",
            "middle_click",
            "double_click",
            "triple_click",
            "mouse_move",
            "mouse_down",
            "mouse_up",
            "drag",
            "scroll",
            "type",
            "key_press",
            "hold_key",
            "wait",
            "goto_url",
            "go_back",
            "go_forward",
            "refresh",
        ] {
            assert!(is_yutori_action(name), "should recognise {name}");
        }
        // N1 legacy / never-emitted names — kept out of the routing
        // predicate now that N1.5 is the active runtime.
        for name in [
            "hover",
            "left_click_drag",
            "screenshot",
            "cursor_position",
            "key",
            "click",
        ] {
            assert!(!is_yutori_action(name), "should NOT recognise {name}");
        }
        // Native agent-browser primitives must keep falling through to
        // the standard translator path.
        assert!(!is_yutori_action("fill"));
        assert!(!is_yutori_action("press"));
    }
}
