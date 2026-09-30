//! Single source of truth for the active app theme, shared across every Tauri
//! window (the unified-ui main surface + the desktop Settings/setup/logs
//! frontend, which are separate webviews and may not share `localStorage`).
//!
//! Flow: the unified-ui owns theme selection (its `themeStore` writes
//! `data-theme` + `localStorage['magican-theme']`). On init and on every switch it
//! reads its OWN resolved token values (`getComputedStyle(:root)` for the
//! sync-contract variables) and calls [`set_app_theme`]. We cache that and emit
//! `app-theme-changed` to all windows. The desktop frontend (which has none of
//! the theme CSS) calls [`get_app_theme`] on mount + listens for the event, and
//! applies the tokens as inline CSS variables — so Settings tracks whatever
//! theme is active, live, without bundling the unified-ui's 6k-line `app.css`.
//! The private frontend bundles the font files named by those typography tokens.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

/// The resolved theme snapshot the unified-ui publishes and the desktop applies.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AppThemeState {
    /// The `data-theme` name (e.g. `longhand`, `mario-8bit`). Applied verbatim
    /// to the desktop frontend's `<html>` so any theme-name-keyed CSS matches.
    pub name: String,
    /// Resolved CSS variable values for the sync contract (e.g.
    /// `{"--bg-base": "#fdfcf8", ...}`), applied inline on the desktop `:root`.
    pub tokens: HashMap<String, String>,
}

/// Process-global cache of the last-published theme. In-memory: the unified-ui
/// re-publishes on every page load, so a running app always converges; a value
/// is present as soon as the main surface has rendered once.
fn theme_state() -> &'static Mutex<AppThemeState> {
    static THEME: OnceLock<Mutex<AppThemeState>> = OnceLock::new();
    THEME.get_or_init(|| Mutex::new(AppThemeState::default()))
}

/// Event name broadcast to every window when the active theme changes.
pub const APP_THEME_CHANGED_EVENT: &str = "app-theme-changed";

/// Publish the active theme (called by the unified-ui on init + every switch).
/// Caches it and broadcasts `app-theme-changed` so other windows (Settings)
/// re-apply immediately.
#[tauri::command]
pub fn set_app_theme(
    app: AppHandle,
    name: String,
    tokens: HashMap<String, String>,
) -> Result<(), String> {
    let snapshot = AppThemeState { name, tokens };
    {
        let mut guard = theme_state().lock().map_err(|e| e.to_string())?;
        *guard = snapshot.clone();
    }
    app.emit(APP_THEME_CHANGED_EVENT, &snapshot)
        .map_err(|e| format!("emit {APP_THEME_CHANGED_EVENT}: {e}"))?;
    Ok(())
}

/// Read the current theme snapshot (called by the desktop frontend on mount,
/// before the first `app-theme-changed` event arrives). Empty until the
/// unified-ui has published once.
#[tauri::command]
pub fn get_app_theme() -> AppThemeState {
    theme_state().lock().map(|g| g.clone()).unwrap_or_default()
}
