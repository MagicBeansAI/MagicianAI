//! Private server-to-host wire contract for typed macOS computer use.
//!
//! These DTOs are intentionally not part of the package/app input surface.
//! They carry physical identifiers only after the Apps server has resolved
//! them from an owner-held observation. The desktop host accepts them only
//! under a short-lived, one-shot keyed signature covering every claim and the
//! exact closed action. Possessing a serialized DTO is therefore not authority.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// v2 (CuaDriver 0.28): element-addressed actions carry the observed CUA
/// `element_token` instead of a bare index, scroll is direction+notches and
/// drag carries the observation's screenshot scale. v1 permits cannot verify.
pub const APP_MACOS_HOST_WIRE_V2: &str = "magician.app-macos-host-wire.v2";
pub const APP_MACOS_HOST_PAIRING_V1: &str = "magician.app-macos-host-pairing.v1";
pub const APP_MACOS_DESKTOP_IDENTITY_V1: &str = "magician.app-macos-desktop-identity.v1";
pub const APP_MACOS_HOST_MAX_PERMIT_LIFETIME_MS: i64 = 30_000;
pub const APP_MACOS_HOST_MAX_CLOCK_SKEW_MS: i64 = 5_000;
pub const APP_MACOS_HOST_MAX_RESULT_BYTES: u64 = 16 * 1024 * 1024;
pub const APP_MACOS_HOST_MAX_EVIDENCE_BYTES: u64 = 16 * 1024 * 1024;
pub const APP_MACOS_HOST_RESPONSE_ENVELOPE_BYTES: u64 = 4 * 1024;
pub const APP_MACOS_HOST_MAX_TEXT_BYTES: usize = 64 * 1024;
pub const APP_MACOS_HOST_MAX_MODIFIERS: usize = 4;
pub const APP_MACOS_HOST_PROTECTED_POLICY_V1: &str =
    "magician.app-macos-host.protected-applications.v1";

/// Deny-default protected application set shared by the runtime owner and the
/// native verifier. Keeping it in the wire crate prevents the two enforcement
/// points from silently drifting.
pub fn app_macos_host_protected_bundle_id(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    matches!(
        value.as_str(),
        "com.apple.systempreferences"
            | "com.apple.systemsettings"
            | "com.apple.keychainaccess"
            | "com.apple.securityagent"
            | "com.apple.terminal"
            | "com.googlecode.iterm2"
            | "com.apple.scripteditor2"
            | "com.apple.automator"
            | "com.apple.shortcuts"
            | "com.apple.activitymonitor"
            | "com.1password.1password"
            | "com.agilebits.onepassword7"
    ) || value.contains("passwordmanager")
        || value.contains("authenticator")
}

/// Decode the CUA JSON envelope before checking roles/attributes so escaped
/// secure-field markers cannot bypass either the runtime or desktop fence.
/// Malformed evidence is unsafe by default.
pub fn app_macos_host_observation_contains_secure_content(value: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(value) else {
        return true;
    };
    let mut pending = vec![&value];
    while let Some(value) = pending.pop() {
        match value {
            serde_json::Value::Array(values) => pending.extend(values),
            serde_json::Value::Object(values) => {
                let menu_chrome = values
                    .get("role")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(is_menu_chrome_role);
                for (key, value) in values {
                    let normalized_key = normalize_security_marker(key);
                    if matches!(
                        normalized_key.as_str(),
                        "issecure" | "secure" | "password" | "passwordfield"
                    ) && value.as_bool() == Some(true)
                    {
                        return true;
                    }
                    if secure_marker_text(key) {
                        return true;
                    }
                    // A menu command's label is the app's chrome, not window
                    // content: every text app's Edit menu carries AutoFill's
                    // "Passwords…", and matching it refused every observation.
                    if menu_chrome && value.is_string() {
                        continue;
                    }
                    pending.push(value);
                }
            },
            serde_json::Value::String(value) => {
                if value
                    .lines()
                    .filter(|line| !is_menu_chrome_tree_line(line))
                    .any(secure_marker_text)
                {
                    return true;
                }
            },
            serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            },
        }
    }
    false
}

/// The window-content part of a CuaDriver `tree_markdown`: every line except
/// the menu bar and everything nested under it. The runtime digests it as the
/// observation's content identity; the desktop's pre-action fence is narrower
/// (see [`app_macos_host_element_fence`]).
///
/// CuaDriver 0.28 appends the application's menu bar to the window's tree, and
/// menus update on their own shortly after an edit (an Undo item appearing).
/// Menu items are therefore not valid element-action targets (use a menu path).
pub fn app_macos_host_observation_content_tree(tree: &str) -> String {
    let mut kept = Vec::new();
    let mut menu_bar_indent: Option<usize> = None;
    for line in tree.lines() {
        let indent = line.len() - line.trim_start().len();
        if let Some(menu_indent) = menu_bar_indent {
            if indent > menu_indent {
                continue;
            }
            menu_bar_indent = None;
        }
        if is_menu_chrome_tree_line(line) {
            if tree_line_role(line) == Some("AXMenuBar") {
                menu_bar_indent = Some(indent);
            }
            continue;
        }
        kept.push(line);
    }
    kept.join("\n")
}

/// Separator between the fences of a multi-element action (drag). A NUL line
/// never occurs in a CuaDriver `tree_markdown`, so two fences cannot be
/// re-split into a different pair that joins to the same bytes.
const APP_MACOS_HOST_FENCE_SEPARATOR: &str = "\n\u{0}\n";

/// The fence text for one element of a CuaDriver 0.28 `tree_markdown`: its
/// ancestor chain top-down, each ancestor reduced to its identity (`[N] AXRole`,
/// or the bare role for an unindexed row), then the element's own line in full.
///
/// A whole-window digest failed unrelated actions whenever macOS changed the
/// window on its own: a document is retitled a few seconds after its first
/// edit, and the window title is an ancestor of every element. Ancestor labels
/// are therefore left out; their indexes and roles stay, so the element still
/// sits at the same place in the same structure. Its own label, role and index
/// are fenced exactly.
///
/// `None` when the index is absent or appears twice, or when the element is
/// menu chrome or inside the menu bar (menu items update themselves after every
/// edit; address them with a menu path).
pub fn app_macos_host_element_fence(tree: &str, element_index: u32) -> Option<String> {
    let index_text = element_index.to_string();
    let lines = tree.lines().collect::<Vec<_>>();
    let mut matches = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| {
            tree_node(line).is_some_and(|node| node.index == Some(index_text.as_str()))
        })
        .map(|(position, _)| position);
    let position = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    let target = lines[position];
    let target_node = tree_node(target)?;
    if is_menu_chrome_role(target_node.role) {
        return None;
    }
    let mut threshold = target_node.indent;
    let mut ancestors = Vec::new();
    for line in lines[..position].iter().rev() {
        if threshold == 0 {
            break;
        }
        let Some(node) = tree_node(line) else {
            continue;
        };
        if node.indent >= threshold {
            continue;
        }
        if is_menu_chrome_role(node.role) {
            return None;
        }
        ancestors.push(node.identity());
        threshold = node.indent;
    }
    ancestors.reverse();
    ancestors.push(target.trim_start().to_owned());
    Some(ancestors.join("\n"))
}

/// The fence text of one or more elements (a drag has two), in order, joined
/// by a separator no tree contains. `None` if there are no elements or any
/// element has no fence.
pub fn app_macos_host_element_fence_digest_input(tree: &str, indexes: &[u32]) -> Option<String> {
    if indexes.is_empty() {
        return None;
    }
    let fences = indexes
        .iter()
        .map(|index| app_macos_host_element_fence(tree, *index))
        .collect::<Option<Vec<_>>>()?;
    Some(fences.join(APP_MACOS_HOST_FENCE_SEPARATOR))
}

/// The fence of a window-addressed action (a key press): the unique `[0]
/// AXWindow` row reduced to its identity, then the roles of its direct
/// children, in order. A key lands on whatever the window has focused, so no
/// element can be named; what can steal that focus is a sheet or popover
/// appearing as a new child of the window, which this fence catches. The
/// title, every label and every child index (which shifts whenever an earlier
/// sibling's subtree grows) are left out. The process and window-id fences
/// already prove it is the same window.
pub fn app_macos_host_window_fence(tree: &str) -> Option<String> {
    let lines = tree.lines().collect::<Vec<_>>();
    let mut windows = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| tree_node(line).is_some_and(|node| node.index == Some("0")))
        .map(|(position, _)| position);
    let position = windows.next()?;
    if windows.next().is_some() {
        return None;
    }
    let window = tree_node(lines[position])?;
    if window.role != "AXWindow" {
        return None;
    }
    let mut fence = vec![window.identity()];
    let mut child_indent = None;
    for line in &lines[position + 1..] {
        let Some(node) = tree_node(line) else {
            continue;
        };
        if node.indent <= window.indent {
            break;
        }
        let child_indent = *child_indent.get_or_insert(node.indent);
        if node.indent == child_indent && !is_menu_chrome_role(node.role) {
            fence.push(node.role.to_owned());
        }
    }
    Some(fence.join("\n"))
}

/// The fence text a desktop re-snapshot must reproduce before an observed
/// action: the element fences of `element_indexes`, or the window fence for a
/// window-addressed action (no element indexes).
pub fn app_macos_host_observation_fence_input(
    tree: &str,
    element_indexes: &[u32],
) -> Option<String> {
    if element_indexes.is_empty() {
        app_macos_host_window_fence(tree)
    } else {
        app_macos_host_element_fence_digest_input(tree, element_indexes)
    }
}

/// `blake3:<hex>` of [`app_macos_host_observation_fence_input`], the value an
/// observed action's permit carries as `observation_content_digest`.
pub fn app_macos_host_observation_fence_digest(
    tree: &str,
    element_indexes: &[u32],
) -> Option<String> {
    app_macos_host_observation_fence_input(tree, element_indexes)
        .map(|input| format!("blake3:{}", blake3::hash(input.as_bytes()).to_hex()))
}

/// One node row of a CuaDriver 0.28 `tree_markdown`: `- [N] AXRole …`, or an
/// unindexed `- AXRole …` (non-actionable text). Other lines are not nodes.
struct TreeNode<'a> {
    indent: usize,
    index: Option<&'a str>,
    role: &'a str,
}

impl TreeNode<'_> {
    fn identity(&self) -> String {
        match self.index {
            Some(index) => format!("[{index}] {}", self.role),
            None => self.role.to_owned(),
        }
    }
}

fn tree_node(line: &str) -> Option<TreeNode<'_>> {
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();
    let rest = trimmed.strip_prefix("- ")?;
    let (index, rest) = match rest.strip_prefix('[') {
        Some(bracketed) => {
            let (index, rest) = bracketed.split_once("] ")?;
            if index.is_empty() || !index.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            (Some(index), rest)
        },
        None => (None, rest),
    };
    let role = rest.split_whitespace().next()?;
    Some(TreeNode {
        indent,
        index,
        role,
    })
}

/// Whether an element role is menu chrome, which element actions may not target.
pub fn app_macos_host_is_menu_chrome_role(role: &str) -> bool {
    is_menu_chrome_role(role)
}

fn tree_line_role(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("- [")?;
    let (_, rest) = rest.split_once("] ")?;
    rest.split_whitespace().next()
}

/// Menu bar, menu and menu-item roles: app commands, never secret content.
fn is_menu_chrome_role(role: &str) -> bool {
    matches!(role, "AXMenuBar" | "AXMenuBarItem" | "AXMenu" | "AXMenuItem")
}

/// A CuaDriver `tree_markdown` line (`- [N] AXRole "label" …`) for menu chrome.
fn is_menu_chrome_tree_line(line: &str) -> bool {
    let Some(rest) = line.trim_start().strip_prefix("- [") else {
        return false;
    };
    let Some((index, rest)) = rest.split_once("] ") else {
        return false;
    };
    !index.is_empty()
        && index.bytes().all(|byte| byte.is_ascii_digit())
        && rest.split_whitespace().next().is_some_and(is_menu_chrome_role)
}

fn secure_marker_text(value: &str) -> bool {
    let value = normalize_security_marker(value);
    [
        "axsecuretextfield",
        "securetextfield",
        "password",
        "passwordfield",
        "passwordmanager",
    ]
    .iter()
    .any(|marker| value.contains(marker))
}

fn normalize_security_marker(value: &str) -> String {
    value
        .bytes()
        .filter(|byte| byte.is_ascii_alphanumeric())
        .map(|byte| byte.to_ascii_lowercase() as char)
        .collect()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppMacosHostActionClass {
    Observe,
    CapturePixels,
    NavigateOrLaunch,
    Interact,
    OutwardCommit,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppMacosHostKey {
    Return,
    Tab,
    Escape,
    Space,
    Backspace,
    DeleteForward,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
}

impl AppMacosHostKey {
    /// CuaDriver 0.28 key names. The Mac "delete" key is backspace; there is
    /// no forward-delete name, so `DeleteForward` is "delete" held with the
    /// `fn` modifier (see [`Self::cua_implied_modifier`]).
    pub const fn as_cua_name(self) -> &'static str {
        match self {
            Self::Return => "return",
            Self::Tab => "tab",
            Self::Escape => "escape",
            Self::Space => "space",
            Self::Backspace => "delete",
            Self::DeleteForward => "delete",
            Self::ArrowUp => "up",
            Self::ArrowDown => "down",
            Self::ArrowLeft => "left",
            Self::ArrowRight => "right",
            Self::Home => "home",
            Self::End => "end",
            Self::PageUp => "pageup",
            Self::PageDown => "pagedown",
        }
    }

    /// Modifier the key itself implies on macOS. `fn`+delete is forward
    /// delete; it is never an app-selectable modifier.
    pub const fn cua_implied_modifier(self) -> Option<&'static str> {
        match self {
            Self::DeleteForward => Some("fn"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppMacosHostScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

impl AppMacosHostScrollDirection {
    pub const fn as_cua_name(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

/// CuaDriver 0.28 `scroll.amount` bounds (wheel notches / key repeats).
pub const APP_MACOS_HOST_MAX_SCROLL_AMOUNT: u8 = 50;
/// Screenshot backing-scale bounds a drag may carry, in thousandths.
pub const APP_MACOS_HOST_MIN_SCREENSHOT_SCALE_MILLIS: u16 = 500;
pub const APP_MACOS_HOST_MAX_SCREENSHOT_SCALE_MILLIS: u16 = 4_000;

/// Parse a CuaDriver 0.28 element token (`s` + 8 lowercase hex snapshot
/// digits, `:`, decimal element index) into `(snapshot_id, element_index)`.
/// Tokens are opaque to Apps; only the physical owner and desktop read them.
pub fn app_macos_host_parse_element_token(token: &str) -> Option<(&str, u32)> {
    let (snapshot_id, index) = token.split_once(':')?;
    if !app_macos_host_valid_snapshot_id(snapshot_id)
        || index.is_empty()
        || index.len() > 10
        || !index.bytes().all(|byte| byte.is_ascii_digit())
        || (index.len() > 1 && index.starts_with('0'))
    {
        return None;
    }
    Some((snapshot_id, index.parse().ok()?))
}

/// `^s[0-9a-f]{8}$`, the CuaDriver 0.28 `snapshot_id` shape.
pub fn app_macos_host_valid_snapshot_id(value: &str) -> bool {
    value.len() == 9
        && value.starts_with('s')
        && value[1..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppMacosHostModifier {
    Command,
    Option,
    Control,
    Shift,
}

impl AppMacosHostModifier {
    pub const fn as_cua_name(self) -> &'static str {
        match self {
            Self::Command => "cmd",
            Self::Option => "alt",
            Self::Control => "ctrl",
            Self::Shift => "shift",
        }
    }
}

/// Closed host action. There is deliberately no raw action-name, selector,
/// script, argv, environment, path or generic JSON variant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppMacosHostAction {
    Launch {
        bundle_id: String,
    },
    Focus {
        bundle_id: String,
        process_id: u32,
    },
    Observe {
        bundle_id: String,
        process_id: u32,
        window_id: u32,
    },
    /// Observe the sole current window of the exact reviewed application.
    /// PID/window selection remains inside the desktop owner so a durable
    /// pairing never persists stale process identifiers.
    ObserveApplication {
        bundle_id: String,
    },
    CapturePixels {
        bundle_id: String,
        process_id: u32,
        window_id: u32,
    },
    /// Element-addressed variants carry the `element_token` minted by the
    /// owner-held observation. The desktop re-snapshots the window, checks the
    /// element fence (`app_macos_host_element_fence`: the target's ancestor
    /// identities and its own line) and rebinds the token's index to that fresh
    /// snapshot, because every `get_window_state` supersedes older tokens.
    ClickElement {
        bundle_id: String,
        process_id: u32,
        window_id: u32,
        observation_ref: String,
        element_token: String,
        click_count: u8,
    },
    TypeText {
        bundle_id: String,
        process_id: u32,
        window_id: u32,
        observation_ref: String,
        element_token: String,
        text: String,
    },
    PressKey {
        bundle_id: String,
        process_id: u32,
        window_id: u32,
        observation_ref: String,
        key: AppMacosHostKey,
        modifiers: Vec<AppMacosHostModifier>,
    },
    ScrollElement {
        bundle_id: String,
        process_id: u32,
        window_id: u32,
        observation_ref: String,
        element_token: String,
        direction: AppMacosHostScrollDirection,
        amount: u8,
    },
    /// CuaDriver 0.28 drag is pixel-only (window-local screenshot pixels).
    /// The desktop derives both centres from the fresh, digest-fenced
    /// snapshot's element frames and the window frame; the observation's
    /// screenshot scale converts those points to screenshot pixels.
    DragElements {
        bundle_id: String,
        process_id: u32,
        window_id: u32,
        observation_ref: String,
        source_element_token: String,
        destination_element_token: String,
        screenshot_scale_millis: u16,
    },
}

impl AppMacosHostAction {
    pub const fn class(&self) -> AppMacosHostActionClass {
        match self {
            Self::Launch { .. } | Self::Focus { .. } => AppMacosHostActionClass::NavigateOrLaunch,
            Self::Observe { .. } | Self::ObserveApplication { .. } => {
                AppMacosHostActionClass::Observe
            },
            Self::CapturePixels { .. } => AppMacosHostActionClass::CapturePixels,
            Self::TypeText { .. } | Self::ScrollElement { .. } => AppMacosHostActionClass::Interact,
            Self::ClickElement { .. } | Self::PressKey { .. } | Self::DragElements { .. } => {
                AppMacosHostActionClass::OutwardCommit
            },
        }
    }

    pub fn bundle_id(&self) -> &str {
        match self {
            Self::Launch { bundle_id }
            | Self::Focus { bundle_id, .. }
            | Self::Observe { bundle_id, .. }
            | Self::ObserveApplication { bundle_id }
            | Self::CapturePixels { bundle_id, .. }
            | Self::ClickElement { bundle_id, .. }
            | Self::TypeText { bundle_id, .. }
            | Self::PressKey { bundle_id, .. }
            | Self::ScrollElement { bundle_id, .. }
            | Self::DragElements { bundle_id, .. } => bundle_id,
        }
    }

    pub fn observation_ref(&self) -> Option<&str> {
        match self {
            Self::ClickElement {
                observation_ref, ..
            }
            | Self::TypeText {
                observation_ref, ..
            }
            | Self::PressKey {
                observation_ref, ..
            }
            | Self::ScrollElement {
                observation_ref, ..
            }
            | Self::DragElements {
                observation_ref, ..
            } => Some(observation_ref),
            Self::Launch { .. }
            | Self::Focus { .. }
            | Self::Observe { .. }
            | Self::ObserveApplication { .. }
            | Self::CapturePixels { .. } => None,
        }
    }

    pub fn requires_screen_recording(&self) -> bool {
        matches!(self, Self::CapturePixels { .. })
    }

    pub const fn process_id(&self) -> Option<u32> {
        match self {
            Self::Launch { .. } | Self::ObserveApplication { .. } => None,
            Self::Focus { process_id, .. }
            | Self::Observe { process_id, .. }
            | Self::CapturePixels { process_id, .. }
            | Self::ClickElement { process_id, .. }
            | Self::TypeText { process_id, .. }
            | Self::PressKey { process_id, .. }
            | Self::ScrollElement { process_id, .. }
            | Self::DragElements { process_id, .. } => Some(*process_id),
        }
    }

    pub const fn window_id(&self) -> Option<u32> {
        match self {
            Self::Observe { window_id, .. }
            | Self::CapturePixels { window_id, .. }
            | Self::ClickElement { window_id, .. }
            | Self::TypeText { window_id, .. }
            | Self::PressKey { window_id, .. }
            | Self::ScrollElement { window_id, .. }
            | Self::DragElements { window_id, .. } => Some(*window_id),
            Self::Launch { .. } | Self::Focus { .. } | Self::ObserveApplication { .. } => None,
        }
    }

    pub fn validate(&self) -> Result<(), AppMacosHostWireError> {
        validate_bundle_id(self.bundle_id())?;
        match self {
            Self::Launch { .. } | Self::ObserveApplication { .. } => {},
            Self::Focus { process_id, .. } => validate_process_id(*process_id)?,
            Self::Observe {
                process_id,
                window_id,
                ..
            }
            | Self::CapturePixels {
                process_id,
                window_id,
                ..
            } => validate_physical_window(*process_id, *window_id)?,
            Self::ClickElement {
                process_id,
                window_id,
                observation_ref,
                element_token,
                click_count,
                ..
            } => {
                validate_physical_element(*process_id, *window_id, observation_ref, element_token)?;
                if !matches!(click_count, 1 | 2) {
                    return Err(AppMacosHostWireError::InvalidAction);
                }
            },
            Self::TypeText {
                process_id,
                window_id,
                observation_ref,
                element_token,
                text,
                ..
            } => {
                validate_physical_element(*process_id, *window_id, observation_ref, element_token)?;
                if text.is_empty()
                    || text.len() > APP_MACOS_HOST_MAX_TEXT_BYTES
                    || text.chars().any(char::is_control)
                {
                    return Err(AppMacosHostWireError::InvalidAction);
                }
            },
            Self::PressKey {
                process_id,
                window_id,
                observation_ref,
                modifiers,
                ..
            } => {
                validate_physical_window(*process_id, *window_id)?;
                validate_token("observation_ref", observation_ref, 192)?;
                let distinct = modifiers.iter().copied().collect::<HashSet<_>>();
                if modifiers.len() > APP_MACOS_HOST_MAX_MODIFIERS
                    || distinct.len() != modifiers.len()
                {
                    return Err(AppMacosHostWireError::InvalidAction);
                }
            },
            Self::ScrollElement {
                process_id,
                window_id,
                observation_ref,
                element_token,
                amount,
                ..
            } => {
                validate_physical_element(*process_id, *window_id, observation_ref, element_token)?;
                if !(1..=APP_MACOS_HOST_MAX_SCROLL_AMOUNT).contains(amount) {
                    return Err(AppMacosHostWireError::InvalidAction);
                }
            },
            Self::DragElements {
                process_id,
                window_id,
                observation_ref,
                source_element_token,
                destination_element_token,
                screenshot_scale_millis,
                ..
            } => {
                validate_physical_element(
                    *process_id,
                    *window_id,
                    observation_ref,
                    source_element_token,
                )?;
                validate_physical_element(
                    *process_id,
                    *window_id,
                    observation_ref,
                    destination_element_token,
                )?;
                // Both ends must come from one observation snapshot and name
                // two different elements of it.
                let (source_snapshot, source_index) =
                    app_macos_host_parse_element_token(source_element_token)
                        .ok_or(AppMacosHostWireError::InvalidAction)?;
                let (destination_snapshot, destination_index) =
                    app_macos_host_parse_element_token(destination_element_token)
                        .ok_or(AppMacosHostWireError::InvalidAction)?;
                if source_snapshot != destination_snapshot
                    || source_index == destination_index
                    || !(APP_MACOS_HOST_MIN_SCREENSHOT_SCALE_MILLIS
                        ..=APP_MACOS_HOST_MAX_SCREENSHOT_SCALE_MILLIS)
                        .contains(screenshot_scale_millis)
                {
                    return Err(AppMacosHostWireError::InvalidAction);
                }
            },
        }
        Ok(())
    }
}

pub const APP_MACOS_HOST_PAIRING_MAX_LIFETIME_MS: i64 = 10 * 60 * 1_000;
pub const APP_MACOS_HOST_MAX_PAIRED_TARGETS: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMacosHostPairingTargetRequest {
    pub target_ref: String,
    pub bundle_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMacosHostPairingTargetIdentity {
    pub target_ref: String,
    pub bundle_id: String,
    pub application_identity_digest: String,
}

/// Owner-supplied, short-lived challenge used before the runtime discloses a
/// pairing HMAC key to the fixed desktop endpoint. The approval-code digest is
/// consumed by the native owner; the code itself never crosses host-gateway
/// HTTP.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMacosDesktopIdentityChallenge {
    pub schema: String,
    pub desktop_identity_key_id: String,
    pub desktop_identity_public_key_hex: String,
    pub challenge_nonce: String,
    pub owner_approval_code_digest: String,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
}

impl AppMacosDesktopIdentityChallenge {
    pub fn mint(
        desktop_identity_key_id: String,
        desktop_identity_public_key_hex: String,
        challenge_nonce: String,
        owner_approval_code_digest: String,
        issued_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<Self, AppMacosHostWireError> {
        let value = Self {
            schema: APP_MACOS_DESKTOP_IDENTITY_V1.to_owned(),
            desktop_identity_key_id,
            desktop_identity_public_key_hex,
            challenge_nonce,
            owner_approval_code_digest,
            issued_at_ms,
            expires_at_ms,
        };
        value.validate(issued_at_ms)?;
        Ok(value)
    }

    pub fn validate(&self, now_ms: i64) -> Result<(), AppMacosHostWireError> {
        if self.schema != APP_MACOS_DESKTOP_IDENTITY_V1
            || self.issued_at_ms < 0
            || self.expires_at_ms <= now_ms
            || self.expires_at_ms <= self.issued_at_ms
            || self.expires_at_ms.saturating_sub(self.issued_at_ms) > 30_000
        {
            return Err(AppMacosHostWireError::InvalidClaims);
        }
        validate_token("desktop_identity_key_id", &self.desktop_identity_key_id, 96)?;
        validate_token("challenge_nonce", &self.challenge_nonce, 192)?;
        decode_desktop_identity_public_key(&self.desktop_identity_public_key_hex)?;
        validate_digest(&self.owner_approval_code_digest)?;
        Ok(())
    }

    pub fn digest(&self) -> Result<String, AppMacosHostWireError> {
        let bytes = serde_json::to_vec(self).map_err(|_| AppMacosHostWireError::Encoding)?;
        Ok(format!("blake3:{}", blake3::hash(&bytes).to_hex()))
    }
}

/// Ed25519 proof from the Keychain-backed native desktop identity. Runtime
/// verifies this before creating/sending a proposal containing its HMAC key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMacosDesktopIdentityAttestation {
    pub schema: String,
    pub desktop_identity_key_id: String,
    pub desktop_identity_public_key_hex: String,
    pub challenge_digest: String,
    pub host_identity_digest: String,
    pub attested_at_ms: i64,
    pub signature_hex: String,
}

impl AppMacosDesktopIdentityAttestation {
    pub fn unsigned(
        challenge: &AppMacosDesktopIdentityChallenge,
        host_identity_digest: String,
        attested_at_ms: i64,
    ) -> Result<Self, AppMacosHostWireError> {
        challenge.validate(attested_at_ms)?;
        validate_digest(&host_identity_digest)?;
        Ok(Self {
            schema: APP_MACOS_DESKTOP_IDENTITY_V1.to_owned(),
            desktop_identity_key_id: challenge.desktop_identity_key_id.clone(),
            desktop_identity_public_key_hex: challenge.desktop_identity_public_key_hex.clone(),
            challenge_digest: challenge.digest()?,
            host_identity_digest,
            attested_at_ms,
            signature_hex: String::new(),
        })
    }

    pub fn signing_bytes(&self) -> Result<Vec<u8>, AppMacosHostWireError> {
        serde_json::to_vec(&(
            "magician.app-macos-desktop-identity.attestation.v1",
            AppMacosDesktopIdentityAttestationMaterial {
                schema: &self.schema,
                desktop_identity_key_id: &self.desktop_identity_key_id,
                desktop_identity_public_key_hex: &self.desktop_identity_public_key_hex,
                challenge_digest: &self.challenge_digest,
                host_identity_digest: &self.host_identity_digest,
                attested_at_ms: self.attested_at_ms,
            },
        ))
        .map_err(|_| AppMacosHostWireError::Encoding)
    }

    pub fn verify(
        &self,
        challenge: &AppMacosDesktopIdentityChallenge,
        now_ms: i64,
    ) -> Result<(), AppMacosHostWireError> {
        challenge.validate(now_ms)?;
        if self.schema != APP_MACOS_DESKTOP_IDENTITY_V1
            || self.desktop_identity_key_id != challenge.desktop_identity_key_id
            || self.desktop_identity_public_key_hex != challenge.desktop_identity_public_key_hex
            || self.challenge_digest != challenge.digest()?
            || self.attested_at_ms < challenge.issued_at_ms
            || self.attested_at_ms > now_ms.saturating_add(APP_MACOS_HOST_MAX_CLOCK_SKEW_MS)
        {
            return Err(AppMacosHostWireError::InvalidClaims);
        }
        validate_digest(&self.host_identity_digest)?;
        verify_desktop_identity_signature(
            &self.desktop_identity_public_key_hex,
            &self.signing_bytes()?,
            &self.signature_hex,
        )
    }

    pub fn digest(&self) -> Result<String, AppMacosHostWireError> {
        let bytes = serde_json::to_vec(self).map_err(|_| AppMacosHostWireError::Encoding)?;
        Ok(format!("blake3:{}", blake3::hash(&bytes).to_hex()))
    }
}

#[derive(Serialize)]
struct AppMacosDesktopIdentityAttestationMaterial<'a> {
    schema: &'a str,
    desktop_identity_key_id: &'a str,
    desktop_identity_public_key_hex: &'a str,
    challenge_digest: &'a str,
    host_identity_digest: &'a str,
    attested_at_ms: i64,
}

pub fn app_macos_desktop_identity_digest(
    key_id: &str,
    public_key_hex: &str,
) -> Result<String, AppMacosHostWireError> {
    validate_token("desktop_identity_key_id", key_id, 96)?;
    decode_desktop_identity_public_key(public_key_hex)?;
    let bytes = serde_json::to_vec(&(APP_MACOS_DESKTOP_IDENTITY_V1, key_id, public_key_hex))
        .map_err(|_| AppMacosHostWireError::Encoding)?;
    Ok(format!("blake3:{}", blake3::hash(&bytes).to_hex()))
}

pub fn app_macos_desktop_owner_approval_code_digest(
    code: &str,
) -> Result<String, AppMacosHostWireError> {
    if code.len() < 24
        || code.len() > 128
        || !code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(AppMacosHostWireError::InvalidClaims);
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"magician.app-macos-desktop-owner-approval.v1\0");
    hasher.update(code.as_bytes());
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

/// Runtime-created setup capability. It is accepted by the desktop only as a
/// pending request and cannot install action authority without explicit native
/// approval followed by a separately signed finalization.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMacosHostPairingProposal {
    pub schema: String,
    pub setup_id: String,
    pub generation: u64,
    pub previous_generation: Option<u64>,
    pub key_id: String,
    pub signing_key_hex: String,
    pub scope_binding_ref: String,
    pub gateway_action_url: String,
    pub gateway_endpoint_digest: String,
    pub desktop_identity_attestation_digest: String,
    pub requested_targets: Vec<AppMacosHostPairingTargetRequest>,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    pub signature: String,
    pub rotation_signature: Option<String>,
}

impl std::fmt::Debug for AppMacosHostPairingProposal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppMacosHostPairingProposal")
            .field("setup_id", &self.setup_id)
            .field("generation", &self.generation)
            .field("previous_generation", &self.previous_generation)
            .field("key_id", &self.key_id)
            .field("scope_binding_ref", &self.scope_binding_ref)
            .field("gateway_action_url", &self.gateway_action_url)
            .field("requested_targets", &self.requested_targets)
            .field("issued_at_ms", &self.issued_at_ms)
            .field("expires_at_ms", &self.expires_at_ms)
            .finish_non_exhaustive()
    }
}

impl AppMacosHostPairingProposal {
    pub fn mint(
        setup_id: String,
        generation: u64,
        key_id: String,
        signing_key: &[u8; 32],
        scope_binding_ref: String,
        gateway_action_url: String,
        gateway_endpoint_digest: String,
        desktop_identity_attestation_digest: String,
        requested_targets: Vec<AppMacosHostPairingTargetRequest>,
        issued_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<Self, AppMacosHostWireError> {
        Self::mint_inner(
            setup_id,
            generation,
            None,
            key_id,
            signing_key,
            scope_binding_ref,
            gateway_action_url,
            gateway_endpoint_digest,
            desktop_identity_attestation_digest,
            requested_targets,
            issued_at_ms,
            expires_at_ms,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn mint_rotation(
        setup_id: String,
        generation: u64,
        previous_generation: u64,
        key_id: String,
        signing_key: &[u8; 32],
        previous_signing_key: &[u8; 32],
        scope_binding_ref: String,
        gateway_action_url: String,
        gateway_endpoint_digest: String,
        desktop_identity_attestation_digest: String,
        requested_targets: Vec<AppMacosHostPairingTargetRequest>,
        issued_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<Self, AppMacosHostWireError> {
        let mut value = Self::mint_inner(
            setup_id,
            generation,
            Some(previous_generation),
            key_id,
            signing_key,
            scope_binding_ref,
            gateway_action_url,
            gateway_endpoint_digest,
            desktop_identity_attestation_digest,
            requested_targets,
            issued_at_ms,
            expires_at_ms,
        )?;
        value.rotation_signature = Some(pairing_signature(
            "magician.app-macos-host-pairing.rotation.v1",
            &value.signing_material(),
            previous_signing_key,
        )?);
        Ok(value)
    }

    #[allow(clippy::too_many_arguments)]
    fn mint_inner(
        setup_id: String,
        generation: u64,
        previous_generation: Option<u64>,
        key_id: String,
        signing_key: &[u8; 32],
        scope_binding_ref: String,
        gateway_action_url: String,
        gateway_endpoint_digest: String,
        desktop_identity_attestation_digest: String,
        requested_targets: Vec<AppMacosHostPairingTargetRequest>,
        issued_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<Self, AppMacosHostWireError> {
        let mut proposal = Self {
            schema: APP_MACOS_HOST_PAIRING_V1.to_owned(),
            setup_id,
            generation,
            previous_generation,
            key_id,
            signing_key_hex: encode_pairing_key(signing_key),
            scope_binding_ref,
            gateway_action_url,
            gateway_endpoint_digest,
            desktop_identity_attestation_digest,
            requested_targets,
            issued_at_ms,
            expires_at_ms,
            signature: String::new(),
            rotation_signature: None,
        };
        proposal.validate_shape(issued_at_ms)?;
        proposal.signature = pairing_signature(
            "magician.app-macos-host-pairing.proposal.v1",
            &proposal.signing_material(),
            signing_key,
        )?;
        Ok(proposal)
    }

    pub fn verify_rotation(
        &self,
        expected_previous_generation: u64,
        previous_signing_key: &[u8; 32],
    ) -> Result<(), AppMacosHostWireError> {
        if self.previous_generation != Some(expected_previous_generation) {
            return Err(AppMacosHostWireError::InvalidClaims);
        }
        let expected = pairing_signature(
            "magician.app-macos-host-pairing.rotation.v1",
            &self.signing_material(),
            previous_signing_key,
        )?;
        if !self
            .rotation_signature
            .as_deref()
            .is_some_and(|signature| constant_time_eq(expected.as_bytes(), signature.as_bytes()))
        {
            return Err(AppMacosHostWireError::InvalidSignature);
        }
        Ok(())
    }

    pub fn verify(&self, now_ms: i64) -> Result<[u8; 32], AppMacosHostWireError> {
        if self.previous_generation.is_some() != self.rotation_signature.is_some() {
            return Err(AppMacosHostWireError::InvalidClaims);
        }
        self.validate_shape(now_ms)?;
        let key = decode_pairing_key(&self.signing_key_hex)?;
        let expected = pairing_signature(
            "magician.app-macos-host-pairing.proposal.v1",
            &self.signing_material(),
            &key,
        )?;
        if !constant_time_eq(expected.as_bytes(), self.signature.as_bytes()) {
            return Err(AppMacosHostWireError::InvalidSignature);
        }
        Ok(key)
    }

    pub fn digest(&self) -> Result<String, AppMacosHostWireError> {
        let bytes = serde_json::to_vec(self).map_err(|_| AppMacosHostWireError::Encoding)?;
        Ok(format!("blake3:{}", blake3::hash(&bytes).to_hex()))
    }

    fn signing_material(&self) -> AppMacosHostPairingProposalMaterial<'_> {
        AppMacosHostPairingProposalMaterial {
            schema: &self.schema,
            setup_id: &self.setup_id,
            generation: self.generation,
            previous_generation: self.previous_generation,
            key_id: &self.key_id,
            signing_key_digest: format!(
                "blake3:{}",
                blake3::hash(self.signing_key_hex.as_bytes()).to_hex()
            ),
            scope_binding_ref: &self.scope_binding_ref,
            gateway_action_url: &self.gateway_action_url,
            gateway_endpoint_digest: &self.gateway_endpoint_digest,
            desktop_identity_attestation_digest: &self.desktop_identity_attestation_digest,
            requested_targets: &self.requested_targets,
            issued_at_ms: self.issued_at_ms,
            expires_at_ms: self.expires_at_ms,
        }
    }

    fn validate_shape(&self, now_ms: i64) -> Result<(), AppMacosHostWireError> {
        if self.schema != APP_MACOS_HOST_PAIRING_V1
            || self.generation == 0
            || self
                .previous_generation
                .is_some_and(|previous| previous == 0 || previous >= self.generation)
            || self.issued_at_ms < 0
            || self.expires_at_ms <= now_ms
            || self.expires_at_ms <= self.issued_at_ms
            || self.expires_at_ms.saturating_sub(self.issued_at_ms)
                > APP_MACOS_HOST_PAIRING_MAX_LIFETIME_MS
            || self.requested_targets.is_empty()
            || self.requested_targets.len() > APP_MACOS_HOST_MAX_PAIRED_TARGETS
            || self.gateway_action_url.len() > 2_048
        {
            return Err(AppMacosHostWireError::InvalidClaims);
        }
        for (value, max) in [
            (self.setup_id.as_str(), 192),
            (self.key_id.as_str(), 64),
            (self.scope_binding_ref.as_str(), 192),
        ] {
            validate_token("pairing", value, max)?;
        }
        validate_digest(&self.gateway_endpoint_digest)?;
        validate_digest(&self.desktop_identity_attestation_digest)?;
        let mut target_refs = HashSet::new();
        let mut bundle_ids = HashSet::new();
        for target in &self.requested_targets {
            validate_token("target_ref", &target.target_ref, 192)?;
            validate_bundle_id(&target.bundle_id)?;
            if app_macos_host_protected_bundle_id(&target.bundle_id)
                || !target_refs.insert(target.target_ref.as_str())
                || !bundle_ids.insert(target.bundle_id.to_ascii_lowercase())
            {
                return Err(AppMacosHostWireError::InvalidClaims);
            }
        }
        let url = url::Url::parse(&self.gateway_action_url)
            .map_err(|_| AppMacosHostWireError::InvalidClaims)?;
        if url.scheme() != "http"
            // Pairing sends a clear one-time owner key. A hostname here would
            // make DNS or hosts-file rebinding part of the trust boundary, so
            // V1 binds the wire to the literal IPv4 loopback owner only.
            || url.host_str() != Some("127.0.0.1")
            || url.port() != Some(3017)
            || url.path() != "/host/apps/macos/action"
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(AppMacosHostWireError::InvalidClaims);
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct AppMacosHostPairingProposalMaterial<'a> {
    schema: &'a str,
    setup_id: &'a str,
    generation: u64,
    previous_generation: Option<u64>,
    key_id: &'a str,
    signing_key_digest: String,
    scope_binding_ref: &'a str,
    gateway_action_url: &'a str,
    gateway_endpoint_digest: &'a str,
    desktop_identity_attestation_digest: &'a str,
    requested_targets: &'a [AppMacosHostPairingTargetRequest],
    issued_at_ms: i64,
    expires_at_ms: i64,
}

/// Desktop-observed identities issued only after explicit native approval.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMacosHostPairingApproval {
    pub schema: String,
    pub setup_id: String,
    pub generation: u64,
    pub key_id: String,
    pub scope_binding_ref: String,
    pub proposal_digest: String,
    pub gateway_action_url: String,
    pub gateway_endpoint_digest: String,
    pub host_identity_digest: String,
    pub cua_driver_binary_digest: String,
    pub tcc_policy_digest: String,
    pub tcc_epoch: u64,
    pub reviewed_targets: Vec<AppMacosHostPairingTargetIdentity>,
    pub approved_at_ms: i64,
    pub expires_at_ms: i64,
    pub signature: String,
    pub desktop_identity_key_id: String,
    pub desktop_identity_signature_hex: String,
}

impl AppMacosHostPairingApproval {
    pub fn sign(
        mut self,
        proposal: &AppMacosHostPairingProposal,
        signing_key: &[u8; 32],
    ) -> Result<Self, AppMacosHostWireError> {
        self.signature.clear();
        self.desktop_identity_signature_hex.clear();
        self.validate_against(proposal, self.approved_at_ms)?;
        self.signature = pairing_signature(
            "magician.app-macos-host-pairing.approval.v1",
            &self.unsigned(),
            signing_key,
        )?;
        Ok(self)
    }

    pub fn verify(
        &self,
        proposal: &AppMacosHostPairingProposal,
        now_ms: i64,
    ) -> Result<(), AppMacosHostWireError> {
        let key = proposal.verify(now_ms)?;
        self.validate_against(proposal, now_ms)?;
        let expected = pairing_signature(
            "magician.app-macos-host-pairing.approval.v1",
            &self.unsigned(),
            &key,
        )?;
        if !constant_time_eq(expected.as_bytes(), self.signature.as_bytes()) {
            return Err(AppMacosHostWireError::InvalidSignature);
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String, AppMacosHostWireError> {
        let bytes = serde_json::to_vec(self).map_err(|_| AppMacosHostWireError::Encoding)?;
        Ok(format!("blake3:{}", blake3::hash(&bytes).to_hex()))
    }

    pub fn desktop_identity_signing_bytes(&self) -> Result<Vec<u8>, AppMacosHostWireError> {
        if self.signature.is_empty() {
            return Err(AppMacosHostWireError::InvalidClaims);
        }
        serde_json::to_vec(&(
            "magician.app-macos-desktop-identity.approval.v1",
            &self.unsigned(),
            self.signature.as_str(),
        ))
        .map_err(|_| AppMacosHostWireError::Encoding)
    }

    pub fn verify_desktop_identity(
        &self,
        expected_key_id: &str,
        expected_public_key_hex: &str,
    ) -> Result<(), AppMacosHostWireError> {
        if self.desktop_identity_key_id != expected_key_id {
            return Err(AppMacosHostWireError::InvalidClaims);
        }
        verify_desktop_identity_signature(
            expected_public_key_hex,
            &self.desktop_identity_signing_bytes()?,
            &self.desktop_identity_signature_hex,
        )
    }

    fn validate_against(
        &self,
        proposal: &AppMacosHostPairingProposal,
        now_ms: i64,
    ) -> Result<(), AppMacosHostWireError> {
        if self.schema != APP_MACOS_HOST_PAIRING_V1
            || self.setup_id != proposal.setup_id
            || self.generation != proposal.generation
            || self.key_id != proposal.key_id
            || self.scope_binding_ref != proposal.scope_binding_ref
            || self.proposal_digest != proposal.digest()?
            || self.gateway_action_url != proposal.gateway_action_url
            || self.gateway_endpoint_digest != proposal.gateway_endpoint_digest
            || self.tcc_epoch == 0
            || self.approved_at_ms < proposal.issued_at_ms
            || self.approved_at_ms > now_ms.saturating_add(APP_MACOS_HOST_MAX_CLOCK_SKEW_MS)
            || self.expires_at_ms <= now_ms
            || self.expires_at_ms > proposal.expires_at_ms
            || self.reviewed_targets.len() != proposal.requested_targets.len()
        {
            return Err(AppMacosHostWireError::InvalidClaims);
        }
        validate_token("desktop_identity_key_id", &self.desktop_identity_key_id, 96)?;
        for digest in [
            &self.host_identity_digest,
            &self.cua_driver_binary_digest,
            &self.tcc_policy_digest,
        ] {
            validate_digest(digest)?;
        }
        for (requested, reviewed) in proposal
            .requested_targets
            .iter()
            .zip(&self.reviewed_targets)
        {
            if requested.target_ref != reviewed.target_ref
                || requested.bundle_id != reviewed.bundle_id
            {
                return Err(AppMacosHostWireError::InvalidClaims);
            }
            validate_digest(&reviewed.application_identity_digest)?;
        }
        Ok(())
    }

    fn unsigned(&self) -> AppMacosHostPairingApprovalMaterial<'_> {
        AppMacosHostPairingApprovalMaterial {
            schema: &self.schema,
            setup_id: &self.setup_id,
            generation: self.generation,
            key_id: &self.key_id,
            scope_binding_ref: &self.scope_binding_ref,
            proposal_digest: &self.proposal_digest,
            gateway_action_url: &self.gateway_action_url,
            gateway_endpoint_digest: &self.gateway_endpoint_digest,
            host_identity_digest: &self.host_identity_digest,
            cua_driver_binary_digest: &self.cua_driver_binary_digest,
            tcc_policy_digest: &self.tcc_policy_digest,
            tcc_epoch: self.tcc_epoch,
            reviewed_targets: &self.reviewed_targets,
            approved_at_ms: self.approved_at_ms,
            expires_at_ms: self.expires_at_ms,
            desktop_identity_key_id: &self.desktop_identity_key_id,
        }
    }
}

#[derive(Serialize)]
struct AppMacosHostPairingApprovalMaterial<'a> {
    schema: &'a str,
    setup_id: &'a str,
    generation: u64,
    key_id: &'a str,
    scope_binding_ref: &'a str,
    proposal_digest: &'a str,
    gateway_action_url: &'a str,
    gateway_endpoint_digest: &'a str,
    host_identity_digest: &'a str,
    cua_driver_binary_digest: &'a str,
    tcc_policy_digest: &'a str,
    tcc_epoch: u64,
    reviewed_targets: &'a [AppMacosHostPairingTargetIdentity],
    approved_at_ms: i64,
    expires_at_ms: i64,
    desktop_identity_key_id: &'a str,
}

/// Runtime confirmation binding the approved native snapshot to the exact
/// reviewed runtime implementation. Only this second message may activate the
/// desktop verifier.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMacosHostPairingFinalization {
    pub schema: String,
    pub setup_id: String,
    pub generation: u64,
    pub key_id: String,
    pub scope_binding_ref: String,
    pub proposal_digest: String,
    pub approval_digest: String,
    pub owner_profile_digest: String,
    pub owner_implementation_digest: String,
    pub finalized_at_ms: i64,
    pub signature: String,
}

impl AppMacosHostPairingFinalization {
    pub fn mint(
        proposal: &AppMacosHostPairingProposal,
        approval: &AppMacosHostPairingApproval,
        owner_profile_digest: String,
        owner_implementation_digest: String,
        signing_key: &[u8; 32],
        finalized_at_ms: i64,
    ) -> Result<Self, AppMacosHostWireError> {
        approval.verify(proposal, finalized_at_ms)?;
        validate_digest(&owner_profile_digest)?;
        validate_digest(&owner_implementation_digest)?;
        let mut value = Self {
            schema: APP_MACOS_HOST_PAIRING_V1.to_owned(),
            setup_id: proposal.setup_id.clone(),
            generation: proposal.generation,
            key_id: proposal.key_id.clone(),
            scope_binding_ref: proposal.scope_binding_ref.clone(),
            proposal_digest: proposal.digest()?,
            approval_digest: approval.digest()?,
            owner_profile_digest,
            owner_implementation_digest,
            finalized_at_ms,
            signature: String::new(),
        };
        value.signature = pairing_signature(
            "magician.app-macos-host-pairing.finalization.v1",
            &value.unsigned(),
            signing_key,
        )?;
        Ok(value)
    }

    pub fn verify(
        &self,
        proposal: &AppMacosHostPairingProposal,
        approval: &AppMacosHostPairingApproval,
        now_ms: i64,
    ) -> Result<(), AppMacosHostWireError> {
        // Finalization authority is committed at `finalized_at_ms`. Delivery
        // may occur after proposal/approval expiry during crash recovery, so
        // verify the immutable signed chain at that historical instant while
        // retaining a current-clock future-skew fence.
        let key = proposal.verify(self.finalized_at_ms)?;
        approval.verify(proposal, self.finalized_at_ms)?;
        if self.schema != APP_MACOS_HOST_PAIRING_V1
            || self.setup_id != proposal.setup_id
            || self.generation != proposal.generation
            || self.key_id != proposal.key_id
            || self.scope_binding_ref != proposal.scope_binding_ref
            || self.proposal_digest != proposal.digest()?
            || self.approval_digest != approval.digest()?
            || self.finalized_at_ms < approval.approved_at_ms
            || self.finalized_at_ms >= approval.expires_at_ms
            || self.finalized_at_ms > now_ms.saturating_add(APP_MACOS_HOST_MAX_CLOCK_SKEW_MS)
        {
            return Err(AppMacosHostWireError::InvalidClaims);
        }
        validate_digest(&self.owner_profile_digest)?;
        validate_digest(&self.owner_implementation_digest)?;
        let expected = pairing_signature(
            "magician.app-macos-host-pairing.finalization.v1",
            &self.unsigned(),
            &key,
        )?;
        if !constant_time_eq(expected.as_bytes(), self.signature.as_bytes()) {
            return Err(AppMacosHostWireError::InvalidSignature);
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String, AppMacosHostWireError> {
        let bytes = serde_json::to_vec(self).map_err(|_| AppMacosHostWireError::Encoding)?;
        Ok(format!("blake3:{}", blake3::hash(&bytes).to_hex()))
    }

    fn unsigned(&self) -> AppMacosHostPairingFinalizationMaterial<'_> {
        AppMacosHostPairingFinalizationMaterial {
            schema: &self.schema,
            setup_id: &self.setup_id,
            generation: self.generation,
            key_id: &self.key_id,
            scope_binding_ref: &self.scope_binding_ref,
            proposal_digest: &self.proposal_digest,
            approval_digest: &self.approval_digest,
            owner_profile_digest: &self.owner_profile_digest,
            owner_implementation_digest: &self.owner_implementation_digest,
            finalized_at_ms: self.finalized_at_ms,
        }
    }
}

#[derive(Serialize)]
struct AppMacosHostPairingFinalizationMaterial<'a> {
    schema: &'a str,
    setup_id: &'a str,
    generation: u64,
    key_id: &'a str,
    scope_binding_ref: &'a str,
    proposal_digest: &'a str,
    approval_digest: &'a str,
    owner_profile_digest: &'a str,
    owner_implementation_digest: &'a str,
    finalized_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMacosHostPairingFinalized {
    pub schema: String,
    pub setup_id: String,
    pub generation: u64,
    pub key_id: String,
    pub finalization_digest: String,
    pub activated_at_ms: i64,
    pub signature: String,
    pub desktop_identity_key_id: String,
    pub desktop_identity_signature_hex: String,
}

impl AppMacosHostPairingFinalized {
    pub fn sign(
        finalization: &AppMacosHostPairingFinalization,
        signing_key: &[u8; 32],
        activated_at_ms: i64,
    ) -> Result<Self, AppMacosHostWireError> {
        if activated_at_ms < finalization.finalized_at_ms {
            return Err(AppMacosHostWireError::InvalidClaims);
        }
        let mut value = Self {
            schema: APP_MACOS_HOST_PAIRING_V1.to_owned(),
            setup_id: finalization.setup_id.clone(),
            generation: finalization.generation,
            key_id: finalization.key_id.clone(),
            finalization_digest: finalization.digest()?,
            activated_at_ms,
            signature: String::new(),
            desktop_identity_key_id: String::new(),
            desktop_identity_signature_hex: String::new(),
        };
        value.signature = pairing_signature(
            "magician.app-macos-host-pairing.finalized.v1",
            &value.unsigned(),
            signing_key,
        )?;
        Ok(value)
    }

    pub fn verify(
        &self,
        finalization: &AppMacosHostPairingFinalization,
        signing_key: &[u8; 32],
    ) -> Result<(), AppMacosHostWireError> {
        if self.schema != APP_MACOS_HOST_PAIRING_V1
            || self.setup_id != finalization.setup_id
            || self.generation != finalization.generation
            || self.key_id != finalization.key_id
            || self.finalization_digest != finalization.digest()?
            || self.activated_at_ms < finalization.finalized_at_ms
        {
            return Err(AppMacosHostWireError::InvalidClaims);
        }
        let expected = pairing_signature(
            "magician.app-macos-host-pairing.finalized.v1",
            &self.unsigned(),
            signing_key,
        )?;
        if !constant_time_eq(expected.as_bytes(), self.signature.as_bytes()) {
            return Err(AppMacosHostWireError::InvalidSignature);
        }
        Ok(())
    }

    pub fn desktop_identity_signing_bytes(&self) -> Result<Vec<u8>, AppMacosHostWireError> {
        validate_token("desktop_identity_key_id", &self.desktop_identity_key_id, 96)?;
        if self.signature.is_empty() {
            return Err(AppMacosHostWireError::InvalidClaims);
        }
        serde_json::to_vec(&(
            "magician.app-macos-desktop-identity.finalized.v1",
            &self.unsigned(),
            self.signature.as_str(),
            self.desktop_identity_key_id.as_str(),
        ))
        .map_err(|_| AppMacosHostWireError::Encoding)
    }

    pub fn verify_desktop_identity(
        &self,
        expected_key_id: &str,
        expected_public_key_hex: &str,
    ) -> Result<(), AppMacosHostWireError> {
        if self.desktop_identity_key_id != expected_key_id {
            return Err(AppMacosHostWireError::InvalidClaims);
        }
        verify_desktop_identity_signature(
            expected_public_key_hex,
            &self.desktop_identity_signing_bytes()?,
            &self.desktop_identity_signature_hex,
        )
    }

    fn unsigned(&self) -> AppMacosHostPairingFinalizedMaterial<'_> {
        AppMacosHostPairingFinalizedMaterial {
            schema: &self.schema,
            setup_id: &self.setup_id,
            generation: self.generation,
            key_id: &self.key_id,
            finalization_digest: &self.finalization_digest,
            activated_at_ms: self.activated_at_ms,
        }
    }
}

#[derive(Serialize)]
struct AppMacosHostPairingFinalizedMaterial<'a> {
    schema: &'a str,
    setup_id: &'a str,
    generation: u64,
    key_id: &'a str,
    finalization_digest: &'a str,
    activated_at_ms: i64,
}

/// Keyless durable proof that the physical desktop owner revoked one exact
/// pairing generation. The desktop signs this before deleting the proposal
/// key; the runtime can therefore recover a lost revoke response without the
/// desktop retaining revoked authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMacosHostPairingRevoked {
    pub schema: String,
    pub setup_id: String,
    pub generation: u64,
    pub key_id: String,
    pub proposal_digest: String,
    pub revoked_at_ms: i64,
    pub signature: String,
    pub desktop_identity_key_id: String,
    pub desktop_identity_signature_hex: String,
}

impl AppMacosHostPairingRevoked {
    pub fn sign(
        proposal: &AppMacosHostPairingProposal,
        signing_key: &[u8; 32],
        revoked_at_ms: i64,
    ) -> Result<Self, AppMacosHostWireError> {
        let proposal_key = decode_pairing_key(&proposal.signing_key_hex)?;
        if revoked_at_ms < proposal.issued_at_ms || !constant_time_eq(&proposal_key, signing_key) {
            return Err(AppMacosHostWireError::InvalidClaims);
        }
        let mut value = Self {
            schema: APP_MACOS_HOST_PAIRING_V1.to_owned(),
            setup_id: proposal.setup_id.clone(),
            generation: proposal.generation,
            key_id: proposal.key_id.clone(),
            proposal_digest: proposal.digest()?,
            revoked_at_ms,
            signature: String::new(),
            desktop_identity_key_id: String::new(),
            desktop_identity_signature_hex: String::new(),
        };
        value.signature = pairing_signature(
            "magician.app-macos-host-pairing.revoked.v1",
            &value.unsigned(),
            signing_key,
        )?;
        Ok(value)
    }

    pub fn verify(
        &self,
        proposal: &AppMacosHostPairingProposal,
        signing_key: &[u8; 32],
        now_ms: i64,
    ) -> Result<(), AppMacosHostWireError> {
        let proposal_key = decode_pairing_key(&proposal.signing_key_hex)?;
        if !constant_time_eq(&proposal_key, signing_key)
            || self.schema != APP_MACOS_HOST_PAIRING_V1
            || self.setup_id != proposal.setup_id
            || self.generation != proposal.generation
            || self.key_id != proposal.key_id
            || self.proposal_digest != proposal.digest()?
            || self.revoked_at_ms < proposal.issued_at_ms
            || self.revoked_at_ms > now_ms.saturating_add(APP_MACOS_HOST_MAX_CLOCK_SKEW_MS)
        {
            return Err(AppMacosHostWireError::InvalidClaims);
        }
        let expected = pairing_signature(
            "magician.app-macos-host-pairing.revoked.v1",
            &self.unsigned(),
            signing_key,
        )?;
        if !constant_time_eq(expected.as_bytes(), self.signature.as_bytes()) {
            return Err(AppMacosHostWireError::InvalidSignature);
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String, AppMacosHostWireError> {
        let bytes = serde_json::to_vec(self).map_err(|_| AppMacosHostWireError::Encoding)?;
        Ok(format!("blake3:{}", blake3::hash(&bytes).to_hex()))
    }

    pub fn desktop_identity_signing_bytes(&self) -> Result<Vec<u8>, AppMacosHostWireError> {
        validate_token("desktop_identity_key_id", &self.desktop_identity_key_id, 96)?;
        if self.signature.is_empty() {
            return Err(AppMacosHostWireError::InvalidClaims);
        }
        serde_json::to_vec(&(
            "magician.app-macos-desktop-identity.revoked.v1",
            &self.unsigned(),
            self.signature.as_str(),
            self.desktop_identity_key_id.as_str(),
        ))
        .map_err(|_| AppMacosHostWireError::Encoding)
    }

    pub fn verify_desktop_identity(
        &self,
        expected_key_id: &str,
        expected_public_key_hex: &str,
    ) -> Result<(), AppMacosHostWireError> {
        if self.desktop_identity_key_id != expected_key_id {
            return Err(AppMacosHostWireError::InvalidClaims);
        }
        verify_desktop_identity_signature(
            expected_public_key_hex,
            &self.desktop_identity_signing_bytes()?,
            &self.desktop_identity_signature_hex,
        )
    }

    fn unsigned(&self) -> AppMacosHostPairingRevokedMaterial<'_> {
        AppMacosHostPairingRevokedMaterial {
            schema: &self.schema,
            setup_id: &self.setup_id,
            generation: self.generation,
            key_id: &self.key_id,
            proposal_digest: &self.proposal_digest,
            revoked_at_ms: self.revoked_at_ms,
        }
    }
}

#[derive(Serialize)]
struct AppMacosHostPairingRevokedMaterial<'a> {
    schema: &'a str,
    setup_id: &'a str,
    generation: u64,
    key_id: &'a str,
    proposal_digest: &'a str,
    revoked_at_ms: i64,
}

/// Runtime-minted, one-shot request for the exceptional case where the
/// runtime pairing store was lost but the desktop still retains a monotonic
/// anti-rollback floor. It carries no authority until the native owner signs
/// an acknowledgment after displaying the exact floor and desktop identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMacosHostPairingResetChallenge {
    pub schema: String,
    pub scope_binding_ref: String,
    pub reset_nonce: String,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
}

impl AppMacosHostPairingResetChallenge {
    pub fn mint(
        scope_binding_ref: String,
        reset_nonce: String,
        issued_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<Self, AppMacosHostWireError> {
        let value = Self {
            schema: APP_MACOS_HOST_PAIRING_V1.to_owned(),
            scope_binding_ref,
            reset_nonce,
            issued_at_ms,
            expires_at_ms,
        };
        value.validate(issued_at_ms)?;
        Ok(value)
    }

    pub fn validate(&self, now_ms: i64) -> Result<(), AppMacosHostWireError> {
        if self.schema != APP_MACOS_HOST_PAIRING_V1
            || self.issued_at_ms < 0
            || self.issued_at_ms > now_ms.saturating_add(APP_MACOS_HOST_MAX_CLOCK_SKEW_MS)
            || self.expires_at_ms <= now_ms
            || self.expires_at_ms.saturating_sub(self.issued_at_ms) > 120_000
        {
            return Err(AppMacosHostWireError::InvalidClaims);
        }
        validate_token("scope_binding_ref", &self.scope_binding_ref, 192)?;
        validate_token("reset_nonce", &self.reset_nonce, 192)
    }

    pub fn digest(&self) -> Result<String, AppMacosHostWireError> {
        let bytes = serde_json::to_vec(self).map_err(|_| AppMacosHostWireError::Encoding)?;
        Ok(format!("blake3:{}", blake3::hash(&bytes).to_hex()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMacosHostPairingResetAck {
    pub schema: String,
    pub challenge_digest: String,
    pub scope_binding_ref: String,
    pub host_identity_digest: String,
    pub desktop_identity_key_id: String,
    pub desktop_identity_public_key_hex: String,
    pub desktop_identity_digest: String,
    pub prior_generation_floor: u64,
    pub approved_at_ms: i64,
    pub desktop_identity_signature_hex: String,
}

impl AppMacosHostPairingResetAck {
    pub fn unsigned(
        challenge: &AppMacosHostPairingResetChallenge,
        host_identity_digest: String,
        desktop_identity_key_id: String,
        desktop_identity_public_key_hex: String,
        prior_generation_floor: u64,
        approved_at_ms: i64,
    ) -> Result<Self, AppMacosHostWireError> {
        let desktop_identity_digest = app_macos_desktop_identity_digest(
            &desktop_identity_key_id,
            &desktop_identity_public_key_hex,
        )?;
        let value = Self {
            schema: APP_MACOS_HOST_PAIRING_V1.to_owned(),
            challenge_digest: challenge.digest()?,
            scope_binding_ref: challenge.scope_binding_ref.clone(),
            host_identity_digest,
            desktop_identity_key_id,
            desktop_identity_public_key_hex,
            desktop_identity_digest,
            prior_generation_floor,
            approved_at_ms,
            desktop_identity_signature_hex: String::new(),
        };
        value.validate_against(challenge, approved_at_ms)?;
        Ok(value)
    }

    pub fn signing_bytes(&self) -> Result<Vec<u8>, AppMacosHostWireError> {
        serde_json::to_vec(&(
            "magician.app-macos-desktop-identity.reset-ack.v1",
            &self.unsigned_material(),
        ))
        .map_err(|_| AppMacosHostWireError::Encoding)
    }

    pub fn verify(
        &self,
        challenge: &AppMacosHostPairingResetChallenge,
        now_ms: i64,
    ) -> Result<(), AppMacosHostWireError> {
        self.validate_against(challenge, now_ms)?;
        verify_desktop_identity_signature(
            &self.desktop_identity_public_key_hex,
            &self.signing_bytes()?,
            &self.desktop_identity_signature_hex,
        )
    }

    pub fn digest(&self) -> Result<String, AppMacosHostWireError> {
        let bytes = serde_json::to_vec(self).map_err(|_| AppMacosHostWireError::Encoding)?;
        Ok(format!("blake3:{}", blake3::hash(&bytes).to_hex()))
    }

    fn validate_against(
        &self,
        challenge: &AppMacosHostPairingResetChallenge,
        now_ms: i64,
    ) -> Result<(), AppMacosHostWireError> {
        // The desktop may durably commit the exact acknowledgment before the
        // challenge expires and deliver it after a crash/restart. Validate the
        // approval at its signed historical time, while retaining a current
        // future-skew fence on that signed timestamp.
        challenge.validate(self.approved_at_ms)?;
        if self.schema != APP_MACOS_HOST_PAIRING_V1
            || self.challenge_digest != challenge.digest()?
            || self.scope_binding_ref != challenge.scope_binding_ref
            || self.prior_generation_floor == 0
            || self.approved_at_ms < challenge.issued_at_ms
            || self.approved_at_ms > now_ms.saturating_add(APP_MACOS_HOST_MAX_CLOCK_SKEW_MS)
            || self.desktop_identity_digest
                != app_macos_desktop_identity_digest(
                    &self.desktop_identity_key_id,
                    &self.desktop_identity_public_key_hex,
                )?
        {
            return Err(AppMacosHostWireError::InvalidClaims);
        }
        validate_digest(&self.host_identity_digest)?;
        validate_digest(&self.desktop_identity_digest)?;
        validate_token("desktop_identity_key_id", &self.desktop_identity_key_id, 96)?;
        decode_desktop_identity_public_key(&self.desktop_identity_public_key_hex)?;
        Ok(())
    }

    fn unsigned_material(&self) -> AppMacosHostPairingResetAckMaterial<'_> {
        AppMacosHostPairingResetAckMaterial {
            schema: &self.schema,
            challenge_digest: &self.challenge_digest,
            scope_binding_ref: &self.scope_binding_ref,
            host_identity_digest: &self.host_identity_digest,
            desktop_identity_key_id: &self.desktop_identity_key_id,
            desktop_identity_public_key_hex: &self.desktop_identity_public_key_hex,
            desktop_identity_digest: &self.desktop_identity_digest,
            prior_generation_floor: self.prior_generation_floor,
            approved_at_ms: self.approved_at_ms,
        }
    }
}

#[derive(Serialize)]
struct AppMacosHostPairingResetAckMaterial<'a> {
    schema: &'a str,
    challenge_digest: &'a str,
    scope_binding_ref: &'a str,
    host_identity_digest: &'a str,
    desktop_identity_key_id: &'a str,
    desktop_identity_public_key_hex: &'a str,
    desktop_identity_digest: &'a str,
    prior_generation_floor: u64,
    approved_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMacosHostPairingStatusRequest {
    pub schema: String,
    pub setup_id: String,
    pub generation: u64,
    pub key_id: String,
    pub proposal_digest: String,
    pub approval_digest: Option<String>,
    pub finalization_digest: Option<String>,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    pub signature: String,
}

impl AppMacosHostPairingStatusRequest {
    pub fn mint(
        proposal: &AppMacosHostPairingProposal,
        signing_key: &[u8; 32],
        issued_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<Self, AppMacosHostWireError> {
        if expires_at_ms <= issued_at_ms || expires_at_ms.saturating_sub(issued_at_ms) > 30_000 {
            return Err(AppMacosHostWireError::InvalidClaims);
        }
        let mut value = Self {
            schema: APP_MACOS_HOST_PAIRING_V1.to_owned(),
            setup_id: proposal.setup_id.clone(),
            generation: proposal.generation,
            key_id: proposal.key_id.clone(),
            proposal_digest: proposal.digest()?,
            approval_digest: None,
            finalization_digest: None,
            issued_at_ms,
            expires_at_ms,
            signature: String::new(),
        };
        value.signature = pairing_signature(
            "magician.app-macos-host-pairing.status.v1",
            &value.unsigned(),
            signing_key,
        )?;
        Ok(value)
    }

    pub fn mint_recovery(
        proposal: &AppMacosHostPairingProposal,
        approval: &AppMacosHostPairingApproval,
        finalization: &AppMacosHostPairingFinalization,
        signing_key: &[u8; 32],
        issued_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<Self, AppMacosHostWireError> {
        if expires_at_ms <= issued_at_ms || expires_at_ms.saturating_sub(issued_at_ms) > 30_000 {
            return Err(AppMacosHostWireError::InvalidClaims);
        }
        let mut value = Self {
            schema: APP_MACOS_HOST_PAIRING_V1.to_owned(),
            setup_id: proposal.setup_id.clone(),
            generation: proposal.generation,
            key_id: proposal.key_id.clone(),
            proposal_digest: proposal.digest()?,
            approval_digest: Some(approval.digest()?),
            finalization_digest: Some(finalization.digest()?),
            issued_at_ms,
            expires_at_ms,
            signature: String::new(),
        };
        value.signature = pairing_signature(
            "magician.app-macos-host-pairing.status.v1",
            &value.unsigned(),
            signing_key,
        )?;
        Ok(value)
    }

    pub fn verify(
        &self,
        proposal: &AppMacosHostPairingProposal,
        signing_key: &[u8; 32],
        now_ms: i64,
    ) -> Result<(), AppMacosHostWireError> {
        if self.schema != APP_MACOS_HOST_PAIRING_V1
            || self.setup_id != proposal.setup_id
            || self.generation != proposal.generation
            || self.key_id != proposal.key_id
            || self.proposal_digest != proposal.digest()?
            || self.issued_at_ms > now_ms.saturating_add(APP_MACOS_HOST_MAX_CLOCK_SKEW_MS)
            || self.expires_at_ms <= now_ms
            || self.expires_at_ms.saturating_sub(self.issued_at_ms) > 30_000
            || self.approval_digest.is_some() != self.finalization_digest.is_some()
        {
            return Err(AppMacosHostWireError::InvalidClaims);
        }
        let expected = pairing_signature(
            "magician.app-macos-host-pairing.status.v1",
            &self.unsigned(),
            signing_key,
        )?;
        if !constant_time_eq(expected.as_bytes(), self.signature.as_bytes()) {
            return Err(AppMacosHostWireError::InvalidSignature);
        }
        Ok(())
    }

    fn unsigned(&self) -> AppMacosHostPairingStatusMaterial<'_> {
        AppMacosHostPairingStatusMaterial {
            schema: &self.schema,
            setup_id: &self.setup_id,
            generation: self.generation,
            key_id: &self.key_id,
            proposal_digest: &self.proposal_digest,
            approval_digest: self.approval_digest.as_deref(),
            finalization_digest: self.finalization_digest.as_deref(),
            issued_at_ms: self.issued_at_ms,
            expires_at_ms: self.expires_at_ms,
        }
    }
}

#[derive(Serialize)]
struct AppMacosHostPairingStatusMaterial<'a> {
    schema: &'a str,
    setup_id: &'a str,
    generation: u64,
    key_id: &'a str,
    proposal_digest: &'a str,
    approval_digest: Option<&'a str>,
    finalization_digest: Option<&'a str>,
    issued_at_ms: i64,
    expires_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppMacosHostPairingStatusResponse {
    Pending,
    Approved {
        approval: AppMacosHostPairingApproval,
    },
    Active {
        finalized: AppMacosHostPairingFinalized,
    },
    Revoked {
        revoked: AppMacosHostPairingRevoked,
    },
}

fn pairing_signature<T: Serialize>(
    domain: &'static str,
    value: &T,
    signing_key: &[u8; 32],
) -> Result<String, AppMacosHostWireError> {
    let bytes =
        serde_json::to_vec(&(domain, value)).map_err(|_| AppMacosHostWireError::Encoding)?;
    Ok(blake3::keyed_hash(signing_key, &bytes).to_hex().to_string())
}

pub fn encode_pairing_key(signing_key: &[u8; 32]) -> String {
    signing_key
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn decode_pairing_key(value: &str) -> Result<[u8; 32], AppMacosHostWireError> {
    let key = decode_lower_hex::<32>(value)?;
    if key == [0_u8; 32] {
        return Err(AppMacosHostWireError::InvalidClaims);
    }
    Ok(key)
}

fn decode_desktop_identity_public_key(value: &str) -> Result<[u8; 32], AppMacosHostWireError> {
    let key = decode_lower_hex::<32>(value)?;
    if key == [0_u8; 32] {
        return Err(AppMacosHostWireError::InvalidClaims);
    }
    Ok(key)
}

fn verify_desktop_identity_signature(
    public_key_hex: &str,
    message: &[u8],
    signature_hex: &str,
) -> Result<(), AppMacosHostWireError> {
    let public_key = decode_desktop_identity_public_key(public_key_hex)?;
    let signature = decode_lower_hex::<64>(signature_hex)?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public_key)
        .verify(message, &signature)
        .map_err(|_| AppMacosHostWireError::InvalidSignature)
}

fn decode_lower_hex<const N: usize>(value: &str) -> Result<[u8; N], AppMacosHostWireError> {
    if value.len() != N.saturating_mul(2)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(AppMacosHostWireError::InvalidClaims);
    }
    let mut bytes = [0_u8; N];
    for (index, slot) in bytes.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| AppMacosHostWireError::InvalidClaims)?;
    }
    Ok(bytes)
}

/// Exact common-effect and interactive-session identity covered by the host
/// signature. The desktop independently verifies host/TCC/application identity
/// immediately before lowering the closed action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMacosHostPermitClaims {
    pub schema: String,
    pub key_id: String,
    pub nonce: String,
    pub host_identity_digest: String,
    pub installation_id: String,
    pub installation_generation: u64,
    pub run_ref: String,
    pub grant_revision: u64,
    pub grant_digest: String,
    pub policy_digest: String,
    pub grant_descriptor_digest: String,
    pub target_policy_digest: String,
    pub owner_profile_digest: String,
    pub owner_implementation_digest: String,
    pub cua_driver_binary_digest: String,
    pub owner_target_ref: String,
    pub owner_target_digest: String,
    pub session_binding_digest: String,
    pub resource_lease_ref: String,
    pub effect_binding_digest: String,
    pub interactive_permit_digest: String,
    pub action_ref: String,
    pub action_class: AppMacosHostActionClass,
    pub input_digest: String,
    pub input_bytes: u64,
    pub observation_digest: Option<String>,
    pub observation_content_digest: Option<String>,
    pub observation_revalidation_byte_ceiling: Option<u64>,
    pub result_byte_ceiling: u64,
    pub evidence_byte_ceiling: u64,
    pub bundle_id: String,
    pub application_identity_digest: String,
    pub tcc_policy_digest: String,
    pub tcc_epoch: u64,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
}

impl AppMacosHostPermitClaims {
    fn validate(
        &self,
        action: &AppMacosHostAction,
        now_ms: i64,
        expected_key_id: &str,
        expected_host_identity_digest: &str,
    ) -> Result<(), AppMacosHostWireError> {
        if self.schema != APP_MACOS_HOST_WIRE_V2
            || self.key_id != expected_key_id
            || self.host_identity_digest != expected_host_identity_digest
            || self.installation_generation == 0
            || self.grant_revision == 0
            || self.tcc_epoch == 0
            || self.input_bytes == 0
            || self.result_byte_ceiling == 0
            || self.result_byte_ceiling > APP_MACOS_HOST_MAX_RESULT_BYTES
            || self.evidence_byte_ceiling > APP_MACOS_HOST_MAX_EVIDENCE_BYTES
            || (matches!(
                self.action_class,
                AppMacosHostActionClass::Observe | AppMacosHostActionClass::CapturePixels
            )) != (self.evidence_byte_ceiling > 0)
            || self.action_class != action.class()
            || self.bundle_id != action.bundle_id()
            || self.observation_digest.is_some() != action.observation_ref().is_some()
            || self.observation_content_digest.is_some() != action.observation_ref().is_some()
            || self.observation_revalidation_byte_ceiling.is_some()
                != action.observation_ref().is_some()
            || self
                .observation_revalidation_byte_ceiling
                .is_some_and(|ceiling| ceiling == 0 || ceiling > APP_MACOS_HOST_MAX_EVIDENCE_BYTES)
            || match (&self.observation_digest, action.observation_ref()) {
                (Some(digest), Some(reference)) => {
                    !observation_reference_matches_digest(reference, digest)
                },
                (None, None) => false,
                _ => true,
            }
            || now_ms < 0
            || self.issued_at_ms < 0
            || self.issued_at_ms > now_ms.saturating_add(APP_MACOS_HOST_MAX_CLOCK_SKEW_MS)
            || self.expires_at_ms <= now_ms
            || self.expires_at_ms <= self.issued_at_ms
            || self.expires_at_ms.saturating_sub(self.issued_at_ms)
                > APP_MACOS_HOST_MAX_PERMIT_LIFETIME_MS
        {
            return Err(AppMacosHostWireError::InvalidClaims);
        }
        for (name, value, max) in [
            ("key_id", self.key_id.as_str(), 64),
            ("nonce", self.nonce.as_str(), 192),
            ("installation_id", self.installation_id.as_str(), 128),
            ("run_ref", self.run_ref.as_str(), 192),
            ("owner_target_ref", self.owner_target_ref.as_str(), 192),
            ("resource_lease_ref", self.resource_lease_ref.as_str(), 192),
            ("action_ref", self.action_ref.as_str(), 192),
        ] {
            validate_token(name, value, max)?;
        }
        for value in [
            self.host_identity_digest.as_str(),
            self.grant_digest.as_str(),
            self.policy_digest.as_str(),
            self.grant_descriptor_digest.as_str(),
            self.target_policy_digest.as_str(),
            self.owner_profile_digest.as_str(),
            self.owner_implementation_digest.as_str(),
            self.cua_driver_binary_digest.as_str(),
            self.owner_target_digest.as_str(),
            self.session_binding_digest.as_str(),
            self.effect_binding_digest.as_str(),
            self.interactive_permit_digest.as_str(),
            self.input_digest.as_str(),
            self.application_identity_digest.as_str(),
            self.tcc_policy_digest.as_str(),
        ] {
            validate_digest(value)?;
        }
        if let Some(value) = &self.observation_digest {
            validate_digest(value)?;
        }
        if let Some(value) = &self.observation_content_digest {
            validate_digest(value)?;
        }
        validate_bundle_id(&self.bundle_id)?;
        action.validate()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SignedAppMacosHostRequest {
    pub claims: AppMacosHostPermitClaims,
    pub action: AppMacosHostAction,
    pub signature: String,
}

impl SignedAppMacosHostRequest {
    pub fn mint(
        claims: AppMacosHostPermitClaims,
        action: AppMacosHostAction,
        signing_key: &[u8; 32],
    ) -> Result<Self, AppMacosHostWireError> {
        // Validate against its own key/host identity here. The desktop repeats
        // the same validation against independently configured expectations.
        claims.validate(
            &action,
            claims.issued_at_ms,
            &claims.key_id,
            &claims.host_identity_digest,
        )?;
        let signature = signature_for(&claims, &action, signing_key)?;
        Ok(Self {
            claims,
            action,
            signature,
        })
    }

    pub fn verify(
        &self,
        signing_key: &[u8; 32],
        now_ms: i64,
        expected_key_id: &str,
        expected_host_identity_digest: &str,
    ) -> Result<(), AppMacosHostWireError> {
        self.claims.validate(
            &self.action,
            now_ms,
            expected_key_id,
            expected_host_identity_digest,
        )?;
        let expected = signature_for(&self.claims, &self.action, signing_key)?;
        if !constant_time_eq(expected.as_bytes(), self.signature.as_bytes()) {
            return Err(AppMacosHostWireError::InvalidSignature);
        }
        Ok(())
    }
}

fn signature_for(
    claims: &AppMacosHostPermitClaims,
    action: &AppMacosHostAction,
    signing_key: &[u8; 32],
) -> Result<String, AppMacosHostWireError> {
    #[derive(Serialize)]
    struct Material<'a> {
        domain: &'static str,
        claims: &'a AppMacosHostPermitClaims,
        action: &'a AppMacosHostAction,
    }
    let bytes = serde_json::to_vec(&Material {
        domain: APP_MACOS_HOST_WIRE_V2,
        claims,
        action,
    })
    .map_err(|_| AppMacosHostWireError::Encoding)?;
    Ok(blake3::keyed_hash(signing_key, &bytes).to_hex().to_string())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn validate_physical_element(
    process_id: u32,
    window_id: u32,
    observation_ref: &str,
    element_token: &str,
) -> Result<(), AppMacosHostWireError> {
    validate_physical_window(process_id, window_id)?;
    validate_token("observation_ref", observation_ref, 192)?;
    if app_macos_host_parse_element_token(element_token).is_none() {
        return Err(AppMacosHostWireError::InvalidAction);
    }
    Ok(())
}

fn validate_physical_window(process_id: u32, window_id: u32) -> Result<(), AppMacosHostWireError> {
    validate_process_id(process_id)?;
    if window_id == 0 {
        return Err(AppMacosHostWireError::InvalidAction);
    }
    Ok(())
}

fn validate_process_id(process_id: u32) -> Result<(), AppMacosHostWireError> {
    if process_id == 0 {
        return Err(AppMacosHostWireError::InvalidAction);
    }
    Ok(())
}

fn validate_bundle_id(value: &str) -> Result<(), AppMacosHostWireError> {
    if value.is_empty()
        || value.len() > 255
        || value.starts_with('.')
        || value.ends_with('.')
        || !value.contains('.')
        || value.split('.').any(str::is_empty)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
    {
        return Err(AppMacosHostWireError::InvalidAction);
    }
    Ok(())
}

fn validate_token(
    _name: &'static str,
    value: &str,
    max_bytes: usize,
) -> Result<(), AppMacosHostWireError> {
    if value.is_empty()
        || value.len() > max_bytes
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'-' | b'.' | b':' | b'/' | b'@' | b'#')
        })
    {
        return Err(AppMacosHostWireError::InvalidClaims);
    }
    Ok(())
}

fn validate_digest(value: &str) -> Result<(), AppMacosHostWireError> {
    let Some(hex) = value.strip_prefix("blake3:") else {
        return Err(AppMacosHostWireError::InvalidClaims);
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(AppMacosHostWireError::InvalidClaims);
    }
    Ok(())
}

fn observation_reference_matches_digest(reference: &str, digest: &str) -> bool {
    reference
        .strip_prefix("interactive-observation:")
        .zip(digest.strip_prefix("blake3:"))
        .is_some_and(|(reference_hex, digest_hex)| reference_hex == digest_hex)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMacosHostWireError {
    InvalidClaims,
    InvalidAction,
    InvalidSignature,
    Encoding,
}

impl core::fmt::Display for AppMacosHostWireError {
    /// Callers stringify this into their own error payloads, so it needs a
    /// stable, non-`Debug` rendering. Deliberately terse: these travel to a
    /// caller that must not learn which part of a signature check failed.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let message = match self {
            Self::InvalidClaims => "invalid claims",
            Self::InvalidAction => "invalid action",
            Self::InvalidSignature => "invalid signature",
            Self::Encoding => "encoding error",
        };
        formatter.write_str(message)
    }
}

impl core::error::Error for AppMacosHostWireError {}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::KeyPair as _;

    fn digest(byte: u8) -> String {
        format!("blake3:{}", format!("{byte:02x}").repeat(32))
    }

    fn observation_reference(byte: u8) -> String {
        format!(
            "interactive-observation:{}",
            format!("{byte:02x}").repeat(32)
        )
    }

    fn desktop_key() -> ring::signature::Ed25519KeyPair {
        let pkcs8 =
            ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
                .expect("desktop key");
        ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("desktop keypair")
    }

    fn lower_hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn pairing_proposal(now_ms: i64, key: &[u8; 32]) -> AppMacosHostPairingProposal {
        AppMacosHostPairingProposal::mint(
            "setup:desktop-identity".to_owned(),
            1,
            "key:desktop-identity".to_owned(),
            key,
            "scope:desktop-identity".to_owned(),
            "http://127.0.0.1:3017/host/apps/macos/action".to_owned(),
            digest(22),
            digest(21),
            vec![AppMacosHostPairingTargetRequest {
                target_ref: "runtime:desktop-identity-target".to_owned(),
                bundle_id: "com.example.Editor".to_owned(),
            }],
            now_ms,
            now_ms + 60_000,
        )
        .expect("proposal")
    }

    fn claims(action: &AppMacosHostAction) -> AppMacosHostPermitClaims {
        AppMacosHostPermitClaims {
            schema: APP_MACOS_HOST_WIRE_V2.to_owned(),
            key_id: "key:desktop:1".to_owned(),
            nonce: "nonce:1".to_owned(),
            host_identity_digest: digest(1),
            installation_id: "install_1".to_owned(),
            installation_generation: 1,
            run_ref: "run:app-action:1".to_owned(),
            grant_revision: 1,
            grant_digest: digest(2),
            policy_digest: digest(3),
            grant_descriptor_digest: digest(15),
            target_policy_digest: digest(4),
            owner_profile_digest: digest(5),
            owner_implementation_digest: digest(6),
            cua_driver_binary_digest: digest(16),
            owner_target_ref: "runtime:macos:1".to_owned(),
            owner_target_digest: digest(7),
            session_binding_digest: digest(8),
            resource_lease_ref: "lease:1".to_owned(),
            effect_binding_digest: digest(9),
            interactive_permit_digest: digest(10),
            action_ref: "action:click".to_owned(),
            action_class: action.class(),
            input_digest: digest(11),
            input_bytes: 16,
            observation_digest: action.observation_ref().map(|_| digest(12)),
            observation_content_digest: action.observation_ref().map(|_| digest(17)),
            observation_revalidation_byte_ceiling: action.observation_ref().map(|_| 8_192),
            result_byte_ceiling: 4_096,
            evidence_byte_ceiling: if matches!(
                action.class(),
                AppMacosHostActionClass::Observe | AppMacosHostActionClass::CapturePixels
            ) {
                8_192
            } else {
                0
            },
            bundle_id: action.bundle_id().to_owned(),
            application_identity_digest: digest(13),
            tcc_policy_digest: digest(14),
            tcc_epoch: 1,
            issued_at_ms: 1_000,
            expires_at_ms: 10_000,
        }
    }

    #[test]
    fn signed_request_binds_every_action_byte_and_is_short_lived() {
        let key = [7_u8; 32];
        let action = AppMacosHostAction::ClickElement {
            bundle_id: "com.example.Editor".to_owned(),
            process_id: 42,
            window_id: 9,
            observation_ref: observation_reference(12),
            element_token: "s0000000a:3".to_owned(),
            click_count: 1,
        };
        let mut request =
            SignedAppMacosHostRequest::mint(claims(&action), action, &key).expect("mint");
        request
            .verify(&key, 2_000, "key:desktop:1", &digest(1))
            .expect("verify");

        if let AppMacosHostAction::ClickElement { element_token, .. } = &mut request.action {
            *element_token = "s0000000a:4".to_owned();
        }
        assert_eq!(
            request.verify(&key, 2_000, "key:desktop:1", &digest(1)),
            Err(AppMacosHostWireError::InvalidSignature)
        );
    }

    #[test]
    fn wire_has_no_raw_script_selector_path_or_action_name_escape_hatch() {
        let value = serde_json::to_value(AppMacosHostAction::PressKey {
            bundle_id: "com.example.Editor".to_owned(),
            process_id: 42,
            window_id: 9,
            observation_ref: observation_reference(12),
            key: AppMacosHostKey::Return,
            modifiers: vec![AppMacosHostModifier::Command],
        })
        .expect("serialize");
        let object = value.as_object().expect("object");
        for forbidden in [
            "action_name",
            "args_json",
            "script",
            "selector",
            "argv",
            "path",
            "environment",
        ] {
            assert!(!object.contains_key(forbidden));
        }
    }

    #[test]
    fn malformed_bundle_segments_and_pre_epoch_claims_fail_closed() {
        let key = [7_u8; 32];
        let malformed_action = AppMacosHostAction::Launch {
            bundle_id: "com..example".to_owned(),
        };
        assert_eq!(
            SignedAppMacosHostRequest::mint(claims(&malformed_action), malformed_action, &key,),
            Err(AppMacosHostWireError::InvalidAction)
        );

        let action = AppMacosHostAction::Launch {
            bundle_id: "com.example.Editor".to_owned(),
        };
        let mut invalid_claims = claims(&action);
        invalid_claims.issued_at_ms = -1;
        assert_eq!(
            SignedAppMacosHostRequest::mint(invalid_claims, action, &key),
            Err(AppMacosHostWireError::InvalidClaims)
        );
    }

    #[test]
    fn physical_element_reference_must_be_the_permitted_observation_identity() {
        let key = [7_u8; 32];
        let action = AppMacosHostAction::ClickElement {
            bundle_id: "com.example.Editor".to_owned(),
            process_id: 42,
            window_id: 9,
            observation_ref: "interactive-observation:foreign".to_owned(),
            element_token: "s0000000a:3".to_owned(),
            click_count: 1,
        };
        assert_eq!(
            SignedAppMacosHostRequest::mint(claims(&action), action, &key),
            Err(AppMacosHostWireError::InvalidClaims)
        );
    }

    #[test]
    fn element_tokens_follow_the_cua_0_28_shape() {
        assert_eq!(
            app_macos_host_parse_element_token("s00000006:4"),
            Some(("s00000006", 4))
        );
        for malformed in [
            "",
            "s00000006",
            "s00000006:",
            "s0000006:4",
            "S00000006:4",
            "s0000000G:4",
            "s00000006:04",
            "s00000006:-4",
            "s00000006:4:5",
            "s00000006:99999999999",
        ] {
            assert_eq!(app_macos_host_parse_element_token(malformed), None, "{malformed}");
        }
    }

    #[test]
    fn scroll_and_drag_carry_only_cua_0_28_bounded_shapes() {
        let scroll = |amount| AppMacosHostAction::ScrollElement {
            bundle_id: "com.example.Editor".to_owned(),
            process_id: 42,
            window_id: 9,
            observation_ref: observation_reference(12),
            element_token: "s0000000a:3".to_owned(),
            direction: AppMacosHostScrollDirection::Down,
            amount,
        };
        assert!(scroll(1).validate().is_ok());
        assert!(scroll(APP_MACOS_HOST_MAX_SCROLL_AMOUNT).validate().is_ok());
        assert_eq!(scroll(0).validate(), Err(AppMacosHostWireError::InvalidAction));
        assert_eq!(
            scroll(APP_MACOS_HOST_MAX_SCROLL_AMOUNT + 1).validate(),
            Err(AppMacosHostWireError::InvalidAction)
        );

        let drag = |source: &str, destination: &str, scale| AppMacosHostAction::DragElements {
            bundle_id: "com.example.Editor".to_owned(),
            process_id: 42,
            window_id: 9,
            observation_ref: observation_reference(12),
            source_element_token: source.to_owned(),
            destination_element_token: destination.to_owned(),
            screenshot_scale_millis: scale,
        };
        assert!(drag("s0000000a:3", "s0000000a:7", 2_000).validate().is_ok());
        for invalid in [
            drag("s0000000a:3", "s0000000a:3", 2_000),
            drag("s0000000a:3", "s0000000b:7", 2_000),
            drag("s0000000a:3", "s0000000a:7", 0),
            drag("s0000000a:3", "7", 2_000),
        ] {
            assert_eq!(invalid.validate(), Err(AppMacosHostWireError::InvalidAction));
        }
    }

    #[test]
    fn mac_delete_keys_lower_to_cua_names() {
        assert_eq!(AppMacosHostKey::Backspace.as_cua_name(), "delete");
        assert_eq!(AppMacosHostKey::Backspace.cua_implied_modifier(), None);
        assert_eq!(AppMacosHostKey::DeleteForward.as_cua_name(), "delete");
        assert_eq!(AppMacosHostKey::DeleteForward.cua_implied_modifier(), Some("fn"));
    }

    #[test]
    fn escaped_secure_markers_and_malformed_observations_fail_closed() {
        assert!(app_macos_host_observation_contains_secure_content(
            r#"{"tree_markdown":"AXSecure\u0054extField"}"#,
        ));
        assert!(app_macos_host_observation_contains_secure_content(
            r#"{"attributes":{"is_secure":true}}"#,
        ));
        assert!(app_macos_host_observation_contains_secure_content(
            "not-json"
        ));
        assert!(!app_macos_host_observation_contains_secure_content(
            r#"{"tree_markdown":"AXButton Save"}"#,
        ));
    }

    #[test]
    fn the_content_digest_ignores_the_menu_bar_but_not_the_window() {
        let tree = "- [0] AXWindow \"Untitled\"\n  - [1] AXTextArea \"seed\"\n\
                    - [37] AXMenuBar\n  - [38] AXMenuBarItem \"Edit\"\n    - [39] AXMenu\n      \
                    - [40] AXMenuItem \"Undo\"\n      - [41] AXStaticText \"⌘Z\"";
        let content = app_macos_host_observation_content_tree(tree);
        assert_eq!(content, "- [0] AXWindow \"Untitled\"\n  - [1] AXTextArea \"seed\"");
        // The menu changing (Undo Typing appearing) leaves the content alone…
        let menu_changed = tree.replace("\"Undo\"", "\"Undo Typing\"");
        assert_eq!(app_macos_host_observation_content_tree(&menu_changed), content);
        // …but any change to the window content, index included, does not.
        let content_changed = tree.replace("[1] AXTextArea \"seed\"", "[1] AXTextArea \"seedx\"");
        assert_ne!(app_macos_host_observation_content_tree(&content_changed), content);
        assert!(app_macos_host_is_menu_chrome_role("AXMenuItem"));
        assert!(!app_macos_host_is_menu_chrome_role("AXTextArea"));
    }

    const FENCE_TREE: &str = "- [0] AXWindow \"Untitled 3\" [id=_NS:31 actions=[raise]]\n  \
        - [1] AXScrollArea\n    - [2] AXTextArea \"seed\" [actions=[confirm]]\n  \
        - AXGroup\n    - [3] AXButton \"Edited\" [actions=[press]]\n  \
        - AXStaticText = \"Untitled 3\"\n  - [4] AXButton \"close\" [actions=[press]]\n\
        - [37] AXMenuBar\n  - [38] AXMenuBarItem \"Edit\"\n    - [39] AXMenu\n      \
        - [40] AXMenuItem \"Undo\"\n      - [41] AXStaticText \"⌘Z\"";

    #[test]
    fn the_element_fence_is_the_ancestor_identities_and_the_target_line() {
        assert_eq!(
            app_macos_host_element_fence(FENCE_TREE, 2).as_deref(),
            Some("[0] AXWindow\n[1] AXScrollArea\n- [2] AXTextArea \"seed\" [actions=[confirm]]"),
        );
        // An unindexed ancestor is reduced to its role alone.
        assert_eq!(
            app_macos_host_element_fence(FENCE_TREE, 3).as_deref(),
            Some("[0] AXWindow\nAXGroup\n- [3] AXButton \"Edited\" [actions=[press]]"),
        );
        assert_eq!(
            app_macos_host_element_fence(FENCE_TREE, 0).as_deref(),
            Some("- [0] AXWindow \"Untitled 3\" [id=_NS:31 actions=[raise]]"),
        );
    }

    #[test]
    fn a_retitled_window_keeps_its_element_fences() {
        let fence = app_macos_host_element_fence(FENCE_TREE, 2).expect("fence");
        // macOS renames an untitled document on its own after an edit: the
        // window title, the title text and the title button all change.
        let retitled = FENCE_TREE
            .replace("\"Untitled 3\"", "\"Live Magician Live Seed 3\"")
            .replace("\"Edited\"", "\"Suggested\"");
        assert_eq!(app_macos_host_element_fence(&retitled, 2).as_deref(), Some(fence.as_str()));
        // A sibling or the menu bar changing leaves it alone too.
        let sibling = FENCE_TREE.replace("[4] AXButton \"close\"", "[4] AXButton \"closed\"");
        assert_eq!(app_macos_host_element_fence(&sibling, 2).as_deref(), Some(fence.as_str()));
        let menu = FENCE_TREE.replace("\"Undo\"", "\"Undo Typing\"");
        assert_eq!(app_macos_host_element_fence(&menu, 2).as_deref(), Some(fence.as_str()));
        assert_eq!(
            app_macos_host_window_fence(&retitled),
            app_macos_host_window_fence(FENCE_TREE),
        );
    }

    #[test]
    fn the_target_and_its_ancestor_identities_are_fenced_exactly() {
        let fence = app_macos_host_element_fence(FENCE_TREE, 2).expect("fence");
        for changed in [
            // The target's label, role and index.
            FENCE_TREE.replace("AXTextArea \"seed\"", "AXTextArea \"seedx\""),
            FENCE_TREE.replace("[2] AXTextArea", "[2] AXTextField"),
            FENCE_TREE.replace("[2] AXTextArea", "[5] AXTextArea"),
            // An ancestor's role or index.
            FENCE_TREE.replace("[1] AXScrollArea", "[1] AXGroup"),
            FENCE_TREE.replace("[1] AXScrollArea", "[6] AXScrollArea"),
            FENCE_TREE.replace("- [0] AXWindow", "- [0] AXSheet"),
        ] {
            assert_ne!(
                app_macos_host_element_fence(&changed, 2).as_deref(),
                Some(fence.as_str()),
                "{changed}",
            );
        }
        // An unindexed ancestor's role is fenced as well.
        let group = app_macos_host_element_fence(FENCE_TREE, 3).expect("fence");
        let regrouped = FENCE_TREE.replace("- AXGroup", "- AXSplitGroup");
        assert_ne!(app_macos_host_element_fence(&regrouped, 3), Some(group));
    }

    #[test]
    fn menu_absent_and_duplicate_targets_have_no_fence() {
        assert_eq!(app_macos_host_element_fence(FENCE_TREE, 40), None);
        assert_eq!(app_macos_host_element_fence(FENCE_TREE, 37), None);
        // A non-menu row inside the menu bar is still menu chrome.
        assert_eq!(app_macos_host_element_fence(FENCE_TREE, 41), None);
        assert_eq!(app_macos_host_element_fence(FENCE_TREE, 99), None);
        let duplicated = FENCE_TREE.replace("[4] AXButton", "[2] AXButton");
        assert_eq!(app_macos_host_element_fence(&duplicated, 2), None);
        assert_eq!(app_macos_host_element_fence_digest_input(FENCE_TREE, &[2, 40]), None);
        assert_eq!(app_macos_host_element_fence_digest_input(FENCE_TREE, &[]), None);
    }

    #[test]
    fn a_drag_fences_both_elements_in_order() {
        let source = app_macos_host_element_fence(FENCE_TREE, 2).expect("source");
        let destination = app_macos_host_element_fence(FENCE_TREE, 4).expect("destination");
        assert_eq!(
            app_macos_host_element_fence_digest_input(FENCE_TREE, &[2, 4]),
            Some(format!("{source}\n\u{0}\n{destination}")),
        );
        assert_ne!(
            app_macos_host_observation_fence_digest(FENCE_TREE, &[2, 4]),
            app_macos_host_observation_fence_digest(FENCE_TREE, &[4, 2]),
        );
        assert_eq!(
            app_macos_host_observation_fence_digest(FENCE_TREE, &[2]),
            Some(format!("blake3:{}", blake3::hash(source.as_bytes()).to_hex())),
        );
    }

    #[test]
    fn the_window_fence_is_its_identity_and_child_roles() {
        assert_eq!(
            app_macos_host_window_fence(FENCE_TREE).as_deref(),
            Some("[0] AXWindow\nAXScrollArea\nAXGroup\nAXStaticText\nAXButton"),
        );
        assert_eq!(
            app_macos_host_observation_fence_input(FENCE_TREE, &[]),
            app_macos_host_window_fence(FENCE_TREE),
        );
        // A sheet appearing (which would take the key) changes it; a child's
        // index shifting does not.
        let sheet = FENCE_TREE.replace(
            "  - [4] AXButton \"close\"",
            "  - [4] AXButton \"close\"\n  - [5] AXSheet \"Save?\"",
        );
        assert_ne!(app_macos_host_window_fence(&sheet), app_macos_host_window_fence(FENCE_TREE));
        let shifted = FENCE_TREE.replace("[4] AXButton", "[9] AXButton");
        assert_eq!(app_macos_host_window_fence(&shifted), app_macos_host_window_fence(FENCE_TREE));
        assert_eq!(app_macos_host_window_fence("- [0] AXSheet \"x\""), None);
        assert_eq!(app_macos_host_window_fence("- [1] AXButton \"x\""), None);
    }

    #[test]
    fn menu_commands_named_for_passwords_are_chrome_not_secure_content() {
        // CuaDriver 0.28 includes the menu bar; every text app's Edit menu has
        // AutoFill's "Passwords…", which refused every TextEdit observation.
        assert!(!app_macos_host_observation_contains_secure_content(
            r#"{"elements":[{"element_index":202,"role":"AXMenuItem","label":"Passwords…"}],
                "tree_markdown":"- [0] AXWindow \"Untitled\"\n  - [202] AXMenuItem \"Passwords…\" [id=_handleInsertFromPasswordsCommand: actions=[press]]"}"#,
        ));
        // Window content still fails closed: a password label, a secure field,
        // or a menu item flagged secure.
        assert!(app_macos_host_observation_contains_secure_content(
            r#"{"elements":[{"element_index":3,"role":"AXStaticText","label":"Password"}]}"#,
        ));
        assert!(app_macos_host_observation_contains_secure_content(
            r#"{"tree_markdown":"- [4] AXSecureTextField \"Account\""}"#,
        ));
        assert!(app_macos_host_observation_contains_secure_content(
            r#"{"tree_markdown":"- [5] AXStaticText \"Your password\""}"#,
        ));
        assert!(app_macos_host_observation_contains_secure_content(
            r#"{"elements":[{"role":"AXMenuItem","label":"x","secure":true}]}"#,
        ));
    }

    #[test]
    fn desktop_identity_attestation_precedes_key_disclosure_and_revoke_is_recoverable() {
        let now_ms = 10_000;
        let desktop = desktop_key();
        let public_key_hex = lower_hex(desktop.public_key().as_ref());
        let key_id = format!(
            "desktop-identity:{}",
            blake3::hash(&public_key_hex.as_bytes()).to_hex()
        );
        let owner_code = "0123456789abcdef0123456789abcdef0123456789abcdef";
        let challenge = AppMacosDesktopIdentityChallenge::mint(
            key_id.clone(),
            public_key_hex.clone(),
            "nonce:desktop-identity:1".to_owned(),
            app_macos_desktop_owner_approval_code_digest(owner_code).expect("code digest"),
            now_ms,
            now_ms + 30_000,
        )
        .expect("challenge");
        let mut attestation =
            AppMacosDesktopIdentityAttestation::unsigned(&challenge, digest(23), now_ms)
                .expect("attestation");
        attestation.signature_hex = lower_hex(
            desktop
                .sign(&attestation.signing_bytes().expect("bytes"))
                .as_ref(),
        );
        attestation.verify(&challenge, now_ms).expect("verify");

        let pairing_key = [8_u8; 32];
        let proposal = pairing_proposal(now_ms, &pairing_key);
        let mut revoked =
            AppMacosHostPairingRevoked::sign(&proposal, &pairing_key, proposal.expires_at_ms + 1)
                .expect("revoked after setup expiry");
        revoked.desktop_identity_key_id = key_id.clone();
        revoked.desktop_identity_signature_hex = lower_hex(
            desktop
                .sign(
                    &revoked
                        .desktop_identity_signing_bytes()
                        .expect("revoke bytes"),
                )
                .as_ref(),
        );
        revoked
            .verify(&proposal, &pairing_key, proposal.expires_at_ms + 1)
            .expect("pairing revoke signature");
        revoked
            .verify_desktop_identity(&key_id, &public_key_hex)
            .expect("desktop revoke signature");

        revoked.proposal_digest = digest(24);
        assert!(revoked
            .verify_desktop_identity(&key_id, &public_key_hex)
            .is_err());
    }

    #[test]
    fn signed_reset_preserves_floor_and_exact_scope_across_late_delivery() {
        let desktop = desktop_key();
        let public_key_hex = lower_hex(desktop.public_key().as_ref());
        let key_id = format!(
            "desktop-identity:{}",
            blake3::hash(public_key_hex.as_bytes()).to_hex()
        );
        let challenge = AppMacosHostPairingResetChallenge::mint(
            "scope:workspace-a".to_owned(),
            "reset:nonce:1".to_owned(),
            10_000,
            20_000,
        )
        .expect("challenge");
        let mut acknowledgment = AppMacosHostPairingResetAck::unsigned(
            &challenge,
            digest(91),
            key_id,
            public_key_hex,
            17,
            15_000,
        )
        .expect("acknowledgment");
        acknowledgment.desktop_identity_signature_hex = lower_hex(
            desktop
                .sign(&acknowledgment.signing_bytes().expect("signing bytes"))
                .as_ref(),
        );
        acknowledgment
            .verify(&challenge, 60_000)
            .expect("exact pre-expiry approval remains deliverable after expiry");

        let mut wrong_scope = challenge.clone();
        wrong_scope.scope_binding_ref = "scope:workspace-b".to_owned();
        assert!(acknowledgment.verify(&wrong_scope, 60_000).is_err());
        let mut lowered_floor = acknowledgment.clone();
        lowered_floor.prior_generation_floor = 1;
        assert!(lowered_floor.verify(&challenge, 60_000).is_err());
    }

    #[test]
    fn retained_pre_expiry_finalization_verifies_after_delivery_expiry() {
        let issued_at_ms = 10_000;
        let key = [5_u8; 32];
        let proposal = pairing_proposal(issued_at_ms, &key);
        let approval = AppMacosHostPairingApproval {
            schema: APP_MACOS_HOST_PAIRING_V1.to_owned(),
            setup_id: proposal.setup_id.clone(),
            generation: proposal.generation,
            key_id: proposal.key_id.clone(),
            scope_binding_ref: proposal.scope_binding_ref.clone(),
            proposal_digest: proposal.digest().expect("proposal digest"),
            gateway_action_url: proposal.gateway_action_url.clone(),
            gateway_endpoint_digest: proposal.gateway_endpoint_digest.clone(),
            host_identity_digest: digest(31),
            cua_driver_binary_digest: digest(32),
            tcc_policy_digest: digest(33),
            tcc_epoch: 1,
            reviewed_targets: vec![AppMacosHostPairingTargetIdentity {
                target_ref: proposal.requested_targets[0].target_ref.clone(),
                bundle_id: proposal.requested_targets[0].bundle_id.clone(),
                application_identity_digest: digest(34),
            }],
            approved_at_ms: issued_at_ms + 1_000,
            expires_at_ms: proposal.expires_at_ms - 1_000,
            signature: String::new(),
            desktop_identity_key_id: "desktop:key:retained-finalization".to_owned(),
            desktop_identity_signature_hex: String::new(),
        }
        .sign(&proposal, &key)
        .expect("approval");
        let finalization = AppMacosHostPairingFinalization::mint(
            &proposal,
            &approval,
            digest(35),
            digest(36),
            &key,
            issued_at_ms + 2_000,
        )
        .expect("finalization");
        finalization
            .verify(&proposal, &approval, proposal.expires_at_ms + 60_000)
            .expect("late byte-identical delivery");
        let finalized = AppMacosHostPairingFinalized::sign(
            &finalization,
            &key,
            proposal.expires_at_ms + 60_000,
        )
        .expect("late activation ack");
        finalized
            .verify(&finalization, &key)
            .expect("exact late ack chain");
    }
}
