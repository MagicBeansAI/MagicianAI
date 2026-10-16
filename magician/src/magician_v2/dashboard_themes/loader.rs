//! Theme loader — combines compile-time embedded themes with optional
//! per-scope YAML overlays loaded from disk.
//!
//! Embedded themes are listed explicitly here (one `include_str!` per file)
//! to match the existing `compiled_providers.rs::EMBEDDED_PACK_DEFS` pattern
//! — no new dep, deterministic ordering, and adding a theme is a one-line
//! addition to `EMBEDDED_THEMES`.

use std::path::Path;

use anyhow::{Context, Result};
use tracing::{debug, warn};

use super::DashboardTheme;

/// Compile-time embedded theme YAMLs. Each tuple is `(id-hint, yaml_source)`.
/// The id-hint is informational only — the real id comes from the parsed
/// theme's `id` field. Order here is the canonical "shipped" list order;
/// REST `list` returns themes in this order followed by scope overlays.
pub const EMBEDDED_THEMES: &[(&str, &str)] = &[
    ("editorial", include_str!("embedded/editorial.yaml")),
    ("brutalist", include_str!("embedded/brutalist.yaml")),
    ("refined", include_str!("embedded/refined.yaml")),
    ("terminal", include_str!("embedded/terminal.yaml")),
    ("studio", include_str!("embedded/studio.yaml")),
];

/// Load embedded themes first, then overlay any scope-extensible themes
/// from `scope_overrides_dir` (typically `<scope>/dashboard_themes/`). A
/// scope theme with the same `id` as an embedded one shadows the embedded
/// version. Invalid YAMLs are logged and skipped (load is best-effort —
/// one broken theme file shouldn't take the whole registry down).
pub fn load_all(scope_overrides_dir: Option<&Path>) -> Result<Vec<DashboardTheme>> {
    let mut themes: Vec<DashboardTheme> = Vec::new();
    let mut ids_seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (id_hint, yaml) in EMBEDDED_THEMES {
        match parse_and_validate(yaml) {
            Ok(theme) => {
                if !ids_seen.insert(theme.id.clone()) {
                    warn!(
                        target: "dashboard_themes",
                        duplicate_id = %theme.id,
                        "duplicate embedded theme id; keeping the first occurrence"
                    );
                    continue;
                }
                themes.push(theme);
            },
            Err(error) => {
                warn!(
                    target: "dashboard_themes",
                    id_hint = id_hint,
                    error = %error,
                    "skipping invalid embedded theme"
                );
            },
        }
    }

    if let Some(dir) = scope_overrides_dir {
        overlay_scope_themes(dir, &mut themes, &mut ids_seen);
    }

    Ok(themes)
}

fn parse_and_validate(yaml: &str) -> Result<DashboardTheme> {
    let theme = DashboardTheme::from_yaml_str(yaml).context("parsing theme YAML")?;
    let errors = theme.validate();
    if !errors.is_empty() {
        anyhow::bail!(
            "theme '{}' failed validation: {}",
            theme.id,
            errors.join("; ")
        );
    }
    Ok(theme)
}

fn overlay_scope_themes(
    dir: &Path,
    themes: &mut Vec<DashboardTheme>,
    ids_seen: &mut std::collections::HashSet<String>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => {
            // Missing scope dir is normal — most scopes won't author custom
            // themes. Quiet at debug, not warn.
            debug!(
                target: "dashboard_themes",
                dir = %dir.display(),
                "no scope theme overrides directory; using embedded only"
            );
            return;
        },
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        if !matches!(ext, "yaml" | "yml") {
            continue;
        }
        let yaml = match std::fs::read_to_string(&path) {
            Ok(y) => y,
            Err(error) => {
                warn!(
                    target: "dashboard_themes",
                    path = %path.display(),
                    error = %error,
                    "failed to read scope theme file"
                );
                continue;
            },
        };
        let theme = match parse_and_validate(&yaml) {
            Ok(t) => t,
            Err(error) => {
                warn!(
                    target: "dashboard_themes",
                    path = %path.display(),
                    error = %error,
                    "skipping invalid scope theme"
                );
                continue;
            },
        };

        // Shadow embedded id when there's a collision.
        if let Some(existing) = themes.iter_mut().find(|t| t.id == theme.id) {
            debug!(
                target: "dashboard_themes",
                id = %theme.id,
                source = %path.display(),
                "scope theme shadows embedded theme of same id"
            );
            *existing = theme;
        } else {
            ids_seen.insert(theme.id.clone());
            themes.push(theme);
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn loads_all_embedded_themes() {
        let themes = load_all(None).expect("load");
        // We ship 5 embedded themes.
        assert_eq!(themes.len(), 5);
        let ids: Vec<&str> = themes.iter().map(|t| t.id.as_str()).collect();
        for expected in ["editorial", "brutalist", "refined", "terminal", "studio"] {
            assert!(
                ids.contains(&expected),
                "missing embedded theme '{}'; got {:?}",
                expected,
                ids
            );
        }
    }

    #[test]
    fn scope_theme_shadows_embedded() {
        let tmp = TempDir::new().unwrap();
        let custom_yaml = r##"
id: editorial
name: Custom Editorial
description: Shadowed by scope
best_for: Test override
fonts:
  display:
    family: "'TestDisplay', serif"
    weights: [400]
  body:
    family: "'TestBody', sans-serif"
    weights: [400]
  mono:
    family: "ui-monospace, monospace"
palette:
  background: "#FFFFFF"
  surface: "#FFFFFF"
  foreground: "#000000"
  foreground_muted: "#888888"
  accent: "#FF0000"
  chart_colors: ["#FF0000"]
  border: "rgba(0,0,0,0.1)"
  shadow: "rgba(0,0,0,0.1)"
spacing:
  container_max_width_px: 800
  container_padding_x_px: 16
  container_padding_y_px: 16
  block_gap_px: 16
  prose_line_height: 1.5
  heading_scale: 1.2
atmosphere:
  kind: flat
  intensity: 0.0
motion:
  load_stagger_ms: 0
  load_duration_ms: 0
  load_easing: linear
  hover_lift_px: 0
  scroll_reveal: false
"##;
        fs::write(tmp.path().join("editorial.yaml"), custom_yaml).unwrap();

        let themes = load_all(Some(tmp.path())).expect("load");
        let editorial = themes
            .iter()
            .find(|t| t.id == "editorial")
            .expect("editorial present");
        assert_eq!(editorial.name, "Custom Editorial");
        // Still ship the other 4 embedded.
        assert_eq!(themes.len(), 5);
    }

    #[test]
    fn invalid_scope_theme_is_skipped_not_fatal() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("broken.yaml"), "not: valid: yaml: at all").unwrap();
        let themes = load_all(Some(tmp.path())).expect("load still succeeds");
        assert_eq!(themes.len(), 5);
    }

    #[test]
    fn non_yaml_files_in_scope_dir_are_ignored() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("README.md"), "# readme").unwrap();
        let themes = load_all(Some(tmp.path())).expect("load");
        assert_eq!(themes.len(), 5);
    }

    #[test]
    fn missing_scope_dir_is_not_fatal() {
        let themes = load_all(Some(Path::new("/nonexistent/path"))).expect("load");
        assert_eq!(themes.len(), 5);
    }
}
