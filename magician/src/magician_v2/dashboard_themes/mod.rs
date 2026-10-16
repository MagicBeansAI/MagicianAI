//! Dashboard themes — YAML-defined design token bundles selected per-dashboard.
//!
//! A theme is a complete bundle of fonts, palette, spacing, atmosphere, and
//! motion tokens. The LLM picks `dashboard_theme: <id>` when synthesizing
//! user-output; the frontend reads the theme + injects CSS variables. Adding
//! a new theme means dropping a YAML file into `embedded/` (compile-time) or
//! `<scope>/dashboard_themes/` (runtime, per-scope override).
//!
//! See [`docs/plans/2026-05-12-multi-format-dashboard-rendering.md`](../../../../../docs/plans/2026-05-12-multi-format-dashboard-rendering.md)
//! for the full design.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub mod loader;

/// A complete dashboard theme — design tokens used by the chrome + every
/// renderer. Loaded from YAML; serialized as JSON over REST.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DashboardTheme {
    /// Stable identifier (e.g. `"editorial"`, `"brutalist"`). The LLM emits
    /// this id when synthesizing user-output; the frontend dispatches on it.
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// One-line description shown to the LLM in synthesis-prompt guidance.
    pub description: String,
    /// Free-form guidance for when to pick this theme — surfaced in the
    /// `list_dashboard_themes` response and in synthesis prompt examples.
    pub best_for: String,
    pub fonts: ThemeFonts,
    pub palette: ThemePalette,
    pub spacing: ThemeSpacing,
    pub atmosphere: ThemeAtmosphere,
    pub motion: ThemeMotion,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThemeFonts {
    pub display: ThemeFontFace,
    pub body: ThemeFontFace,
    pub mono: ThemeFontFace,
}

/// A single font face — the loadable stylesheet URL (or a `local:` sentinel
/// when the family is shipped via the app's static assets) plus the CSS
/// `font-family` value and the weights to preload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThemeFontFace {
    /// CSS `font-family` value (e.g. `"'Fraunces', serif"`).
    pub family: String,
    /// Stylesheet URL to inject (Google Fonts, Fontshare, etc.). `None` when
    /// the family is already available system-wide (e.g. mono fallbacks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Weights to preload — comma-joined into the URL where applicable.
    #[serde(default)]
    pub weights: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThemePalette {
    pub background: String,
    pub surface: String,
    pub foreground: String,
    pub foreground_muted: String,
    pub accent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent_alt: Option<String>,
    /// Ordered palette used to color chart series — first color for the first
    /// series, second for the second, etc. Wraps for series count > palette.
    pub chart_colors: Vec<String>,
    pub border: String,
    pub shadow: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThemeSpacing {
    pub container_max_width_px: u32,
    pub container_padding_x_px: u32,
    pub container_padding_y_px: u32,
    pub block_gap_px: u32,
    /// Prose `line-height` value (e.g. `1.65`).
    pub prose_line_height: f32,
    /// Heading scale multiplier (e.g. `1.25` → modular scale).
    pub heading_scale: f32,
}

/// Background atmosphere kind. The frontend's `AtmosphereLayer` switches on
/// this to render the corresponding effect (or nothing for `flat`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AtmosphereKind {
    Flat,
    PaperTexture,
    GradientMesh,
    Noise,
    Scanlines,
    Watercolor,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThemeAtmosphere {
    pub kind: AtmosphereKind,
    /// 0.0 (invisible) → 1.0 (opaque). Used as the CSS `opacity` on the
    /// atmosphere layer; subtler values (0.02–0.08) usually feel right for
    /// texture overlays.
    pub intensity: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThemeMotion {
    pub load_stagger_ms: u32,
    pub load_duration_ms: u32,
    /// CSS easing function. Free-form so themes can pin `linear`, a
    /// cubic-bezier, or `steps(...)` for terminal aesthetics.
    pub load_easing: String,
    #[serde(default)]
    pub hover_lift_px: u32,
    #[serde(default)]
    pub scroll_reveal: bool,
}

impl DashboardTheme {
    /// Parse a single theme YAML.
    pub fn from_yaml_str(source: &str) -> Result<Self> {
        serde_yaml::from_str(source).context("parsing dashboard theme YAML")
    }

    /// Validation pass — semantic checks that serde alone can't catch.
    /// Returns a `Vec<String>` of error messages (empty when valid).
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.id.trim().is_empty() {
            errors.push("theme id must be non-empty".into());
        }
        if !self
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            errors.push(format!(
                "theme id '{}' must contain only ascii alphanumeric / `-` / `_`",
                self.id
            ));
        }
        if self.name.trim().is_empty() {
            errors.push("theme name must be non-empty".into());
        }
        if self.palette.chart_colors.is_empty() {
            errors.push("palette.chart_colors must list at least one color".into());
        }
        if !(0.0..=1.0).contains(&self.atmosphere.intensity) {
            errors.push(format!(
                "atmosphere.intensity must be within [0.0, 1.0]; got {}",
                self.atmosphere.intensity
            ));
        }
        if self.spacing.heading_scale <= 1.0 {
            errors.push(format!(
                "spacing.heading_scale should be > 1.0 (modular scale); got {}",
                self.spacing.heading_scale
            ));
        }
        if self.spacing.container_max_width_px == 0 {
            errors.push("spacing.container_max_width_px must be > 0".into());
        }
        errors
    }
}

/// In-memory registry of all loaded themes. Constructed at startup via the
/// loader; held behind `Arc<RwLock>` so administrative endpoints can trigger
/// a live reload without restarting the process.
#[derive(Debug, Default)]
pub struct DashboardThemeRegistryInner {
    themes: Vec<DashboardTheme>,
    by_id: HashMap<String, usize>,
}

impl DashboardThemeRegistryInner {
    fn rebuild_index(&mut self) {
        self.by_id.clear();
        for (idx, theme) in self.themes.iter().enumerate() {
            self.by_id.insert(theme.id.clone(), idx);
        }
    }
}

/// Cloneable handle around the inner registry. All public methods are
/// non-blocking reads except `replace`, which is the live-reload write path.
#[derive(Debug, Clone, Default)]
pub struct DashboardThemeRegistry {
    inner: Arc<RwLock<DashboardThemeRegistryInner>>,
}

impl DashboardThemeRegistry {
    pub fn new(themes: Vec<DashboardTheme>) -> Self {
        let mut inner = DashboardThemeRegistryInner {
            themes,
            by_id: HashMap::new(),
        };
        inner.rebuild_index();
        Self {
            inner: Arc::new(RwLock::new(inner)),
        }
    }

    /// Load embedded + scope-extensible themes from disk. Scope themes
    /// shadow embedded entries with the same id.
    pub fn load(scope_overrides_dir: Option<&Path>) -> Result<Self> {
        let themes = loader::load_all(scope_overrides_dir)?;
        Ok(Self::new(themes))
    }

    pub fn list(&self) -> Vec<DashboardTheme> {
        self.inner
            .read()
            .expect("DashboardThemeRegistry poisoned")
            .themes
            .clone()
    }

    pub fn get(&self, id: &str) -> Option<DashboardTheme> {
        let guard = self.inner.read().expect("DashboardThemeRegistry poisoned");
        let idx = *guard.by_id.get(id)?;
        guard.themes.get(idx).cloned()
    }

    pub fn replace(&self, themes: Vec<DashboardTheme>) {
        let mut guard = self.inner.write().expect("DashboardThemeRegistry poisoned");
        guard.themes = themes;
        guard.rebuild_index();
    }

    pub fn len(&self) -> usize {
        self.inner
            .read()
            .expect("DashboardThemeRegistry poisoned")
            .themes
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn sample_yaml() -> &'static str {
        r##"
id: test-theme
name: Test Theme
description: A theme used in unit tests
best_for: Verifying serde roundtrip
fonts:
  display:
    family: "'Fraunces', serif"
    url: "https://example.com/fraunces"
    weights: [400, 700]
  body:
    family: "'Inter Tight', sans-serif"
    weights: [400]
  mono:
    family: "ui-monospace, monospace"
palette:
  background: "#FAF8F4"
  surface: "#FFFFFF"
  foreground: "#1A1814"
  foreground_muted: "#5B5650"
  accent: "#C24E1B"
  chart_colors: ["#C24E1B", "#1F4F4A"]
  border: "rgba(0,0,0,0.08)"
  shadow: "rgba(0,0,0,0.06)"
spacing:
  container_max_width_px: 960
  container_padding_x_px: 32
  container_padding_y_px: 48
  block_gap_px: 32
  prose_line_height: 1.65
  heading_scale: 1.25
atmosphere:
  kind: paper_texture
  intensity: 0.04
motion:
  load_stagger_ms: 60
  load_duration_ms: 480
  load_easing: cubic-bezier(0.2, 0.65, 0.3, 1)
  hover_lift_px: 1
  scroll_reveal: true
"##
    }

    #[test]
    fn deserializes_yaml_and_serializes_to_json() {
        let theme = DashboardTheme::from_yaml_str(sample_yaml()).expect("parse");
        assert_eq!(theme.id, "test-theme");
        assert_eq!(theme.palette.chart_colors.len(), 2);
        assert_eq!(theme.atmosphere.kind, AtmosphereKind::PaperTexture);
        let json = serde_json::to_string(&theme).expect("to_json");
        let roundtripped: DashboardTheme = serde_json::from_str(&json).expect("from_json");
        assert_eq!(theme, roundtripped);
    }

    #[test]
    fn validate_flags_empty_id() {
        let mut theme = DashboardTheme::from_yaml_str(sample_yaml()).expect("parse");
        theme.id = String::new();
        let errors = theme.validate();
        assert!(errors.iter().any(|e| e.contains("id must be non-empty")));
    }

    #[test]
    fn validate_flags_invalid_id_chars() {
        let mut theme = DashboardTheme::from_yaml_str(sample_yaml()).expect("parse");
        theme.id = "has spaces".into();
        let errors = theme.validate();
        assert!(errors.iter().any(|e| e.contains("ascii alphanumeric")));
    }

    #[test]
    fn validate_flags_intensity_out_of_range() {
        let mut theme = DashboardTheme::from_yaml_str(sample_yaml()).expect("parse");
        theme.atmosphere.intensity = 1.5;
        let errors = theme.validate();
        assert!(errors.iter().any(|e| e.contains("intensity")));
    }

    #[test]
    fn validate_flags_empty_chart_colors() {
        let mut theme = DashboardTheme::from_yaml_str(sample_yaml()).expect("parse");
        theme.palette.chart_colors.clear();
        let errors = theme.validate();
        assert!(errors.iter().any(|e| e.contains("chart_colors")));
    }

    #[test]
    fn registry_get_and_list() {
        let theme = DashboardTheme::from_yaml_str(sample_yaml()).expect("parse");
        let registry = DashboardThemeRegistry::new(vec![theme.clone()]);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.get("test-theme").map(|t| t.id), Some(theme.id));
        assert!(registry.get("missing").is_none());
        let listed = registry.list();
        assert_eq!(listed.len(), 1);
    }

    #[test]
    fn registry_replace_rebuilds_index() {
        let theme = DashboardTheme::from_yaml_str(sample_yaml()).expect("parse");
        let registry = DashboardThemeRegistry::new(vec![theme.clone()]);
        let mut second = theme.clone();
        second.id = "second".into();
        registry.replace(vec![second.clone()]);
        assert!(registry.get("test-theme").is_none());
        assert_eq!(registry.get("second").map(|t| t.id), Some(second.id));
    }
}
