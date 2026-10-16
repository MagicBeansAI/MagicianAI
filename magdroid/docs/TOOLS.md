[← Back to README](../README.md)

---

# 🛠️ MCP Tools Reference (32 Tools)

Every tool is callable by your AI agent through MCP. Tools accept **selectors** (text, resource ID, content description) so your agent never needs to hardcode pixel coordinates.

## Apps physical-owner boundary

The broader paired-device MCP roster below is discovery, not Apps authority.
An installed App can reach only eight exact private owner mappings:

| Owner action | Wire tool | Closed arguments |
|---|---|---|
| `snapshot` | `android_get_ui_tree` | `filter=interactive`, `max_depth=50` |
| `screenshot` | `android_screenshot` | `quality=full` |
| `launch` | `android_launch_app` | reviewed `package_name`, `clear_task=false` |
| `close` | `android_close_app` | reviewed `package_name`, `force=false` |
| `tap` | `android_tap` | observation-derived `x`, `y` |
| `type` | `android_input_text` | bounded `text`, `append=false` |
| `key` | `android_press_key` | `back`, `home`, `enter`, `delete`, `tab`, `escape`, or `space` |
| `scroll` | `android_swipe` | observation-derived endpoints and 100–900 ms duration |

Each call must carry the exact private `_magician_apps` claim. Claims on other
tools, action/tool mismatches, unknown fields, generic selectors, raw device
operations, shell and ADB are rejected. Tap/type/key/scroll must revalidate the
prior foreground package, geometry, and structured snapshot SHA-256 immediately
before physical I/O. Successful calls return a uniform settled owner receipt;
this source completion is not a claim of live-device or release qualification.

---

### 👁️ Observe — See what's on screen

| Tool | Description | Typical Latency |
|---|---|---|
| `screenshot` | Capture screen as JPEG (full or thumbnail quality) | ~60ms |
| `get_ui_tree` | Full UI hierarchy with element IDs, text, bounds | 18-33ms |
| `find_elements` | Search for elements by text, ID, or content description | <10ms |
| `get_screen_context` | Screenshot + simplified UI tree in one call | ~70ms |
| `get_device_info` | Manufacturer, model, Android version, screen size | <5ms |
| `get_notifications` | Read notification titles, text, and actions | <10ms |
| `get_recent_toasts` | Capture toast messages shown on screen | <5ms |

---

### 👆 Act — Touch, type, and interact

| Tool | Description | Typical Latency |
|---|---|---|
| `tap` | Tap by selector or coordinates | ~2ms |
| `long_press` | Long press (default 1000ms hold) | ~1000ms |
| `double_tap` | Double tap gesture | ~150ms |
| `swipe` | Swipe between two points with duration control | ~2ms |
| `pinch` | Pinch zoom (scale >1.0 = in, <1.0 = out) | ~300ms |
| `drag` | Drag from point A to point B | ~500ms |
| `input_text` | Type text into a field | ~1.4ms |
| `press_key` | Press system keys (back, home, enter, etc.) | <5ms |
| `set_clipboard` | Set clipboard content | <10ms |

---

### 📱 Manage — Control apps and device

| Tool | Description | Typical Latency |
|---|---|---|
| `launch_app` | Launch app by package name | <100ms |
| `close_app` | Close app (graceful or force-stop via ADB) | ~200ms |
| `open_url` | Open URL in default browser | <100ms |
| `global_action` | System actions: back, home, recents, notifications | <10ms |
| `list_apps` | List installed apps (all or by filter) | ~200ms |

---

### ⏳ Wait — Synchronize with the UI

| Tool | Description | Typical Latency |
|---|---|---|
| `wait_for_element` | Wait until element appears (with timeout) | up to 5s |
| `wait_for_gone` | Wait until element disappears | up to 5s |
| `wait_for_idle` | Wait until UI stabilizes (no changes for 300ms) | up to 5s |
| `scroll_to_element` | Scroll through lists until element is found | up to 30s |

---

### 🧪 Test — Validate and debug

| Tool | Description | Typical Latency |
|---|---|---|
| `screenshot_diff` | Compare screenshots for visual regression | ~100ms |
| `accessibility_audit` | Audit screen for a11y issues (touch targets, labels) | <50ms |
| `enable_events` | Toggle real-time event streaming (UI changes, toasts) | <5ms |

---

### 📡 Device — Multi-device management

| Tool | Description |
|---|---|
| `list_devices` | List all connected Android devices with status |
| `select_device` | Switch active device for all subsequent commands |

---

### 🔍 Meta — Discover and explore tools

| Tool | Description | Typical Latency |
|---|---|---|
| `search_tools` | Search available tools by name, description, or category | <5ms |
| `describe_tools` | Get detailed description of one or all tools | <5ms |
