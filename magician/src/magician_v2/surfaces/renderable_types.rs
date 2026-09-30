//! Registry of dashboardable artifact types for auto-surface publication.
//!
//! Maps well-known artifact types (e.g., `custom:metric_set`) to their
//! corresponding surface section kinds, projection paths, and default titles.
//! The auto-publisher uses this registry to decide which artifacts should
//! be rendered and how.

/// A dashboardable artifact type and its rendering contract.
#[derive(Debug, Clone)]
pub struct DashboardableType {
    /// Artifact type string (e.g., "custom:metric_set").
    pub artifact_type: &'static str,
    /// Section kind for the surface compiler (e.g., "metric_grid").
    pub section_kind: &'static str,
    /// Default JSON Pointer path to extract section data from the artifact.
    pub default_projection_path: &'static str,
    /// Default section title when render_hints.section_title is not set.
    pub default_section_title: &'static str,
}

pub const DASHBOARDABLE_TYPES: &[DashboardableType] = &[
    DashboardableType {
        artifact_type: "custom:metric_set",
        section_kind: "metric_grid",
        default_projection_path: "/items",
        default_section_title: "Metrics",
    },
    DashboardableType {
        artifact_type: "custom:record_table",
        section_kind: "table",
        default_projection_path: "",
        default_section_title: "Records",
    },
    DashboardableType {
        artifact_type: "custom:activity_feed",
        section_kind: "activity_feed",
        default_projection_path: "/items",
        default_section_title: "Activity",
    },
    DashboardableType {
        artifact_type: "custom:summary_note",
        section_kind: "markdown",
        default_projection_path: "/content",
        default_section_title: "Summary",
    },
];

/// Look up a dashboardable type by its artifact_type string.
pub fn lookup(artifact_type: &str) -> Option<&'static DashboardableType> {
    DASHBOARDABLE_TYPES
        .iter()
        .find(|t| t.artifact_type == artifact_type)
}

/// Check if an artifact type is dashboardable.
pub fn is_dashboardable(artifact_type: &str) -> bool {
    lookup(artifact_type).is_some()
}
