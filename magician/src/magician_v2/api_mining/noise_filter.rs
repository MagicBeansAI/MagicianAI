//! Store-managed telemetry/noise filtering for mined traffic.

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoiseFilter {
    pub version: String,
    pub hosts: Vec<String>,
    pub host_suffixes: Vec<String>,
    pub path_substrings: Vec<String>,
    #[serde(default)]
    pub vendor_path_substrings: Vec<String>,
    #[serde(default)]
    pub whitelist: Vec<String>,
    /// Operator-supplied host+path substring blocks.
    #[serde(default)]
    pub blacklist: Vec<String>,
}

pub const SEED_HOSTS: &[&str] = &[
    "cloudflareinsights.com",
    "ab.chatgpt.com",
    "px.ads.linkedin.com",
    "ad.doubleclick.net",
    "analytics.google.com",
    "www.google-analytics.com",
    "ssl.google-analytics.com",
    "stats.g.doubleclick.net",
    "api-eu1.hubapi.com",
    "forms-eu1.hscollectedforms.net",
    "js.hs-scripts.com",
    "cdn.segment.com",
    "api.segment.io",
    "k6-e4e4123be8e54363900a90353991ca93.ecs.us-west-1.on.aws",
];

pub const SEED_HOST_SUFFIXES: &[&str] = &[
    ".posthog.com",
    ".doubleclick.net",
    ".google-analytics.com",
    ".clarity.ms",
    ".pendo.io",
    ".gstatic.com",
    ".applicationinsights.azure.com",
];

pub const SEED_PATHS: &[&str] = &[
    "/beacon",
    "/collect",
    "/pixel",
    "/tracking",
    "/telemetry",
    "/log",
    "/rum",
    "/ccm/",
    "/rmkt/",
];

pub const SEED_VENDOR_PATHS: &[&str] = &[
    "/cdn-cgi/",
    "/_vercel/insights",
    "/ces/v1/",
    "/v1/rgstr",
    "/v1/initialize",
    "/statsig/",
    "/segment/",
    "/posthog/",
];

pub fn has_meaningful_json_body(
    trace: &crate::magician_v2::api_mining::types::NetworkTraceEvent,
) -> bool {
    let Some(body) = trace.response_body.as_deref() else {
        return false;
    };
    match serde_json::from_str::<serde_json::Value>(body.trim()) {
        Ok(serde_json::Value::Object(map)) => !map.is_empty(),
        Ok(serde_json::Value::Array(items)) => !items.is_empty(),
        _ => false,
    }
}

impl NoiseFilter {
    pub fn seeded() -> Self {
        let mut hosts: Vec<String> = super::miner::NOISE_HOSTS
            .iter()
            .chain(SEED_HOSTS)
            .map(|value| value.to_ascii_lowercase())
            .collect();
        hosts.sort_unstable();
        hosts.dedup();

        let mut path_substrings: Vec<String> = super::miner::NOISE_PATHS
            .iter()
            .chain(SEED_PATHS)
            .map(|value| value.to_ascii_lowercase())
            .collect();
        path_substrings.sort_unstable();
        path_substrings.dedup();

        Self {
            version: "1.0.0".into(),
            hosts,
            host_suffixes: SEED_HOST_SUFFIXES
                .iter()
                .map(|value| value.to_string())
                .collect(),
            path_substrings,
            vendor_path_substrings: SEED_VENDOR_PATHS
                .iter()
                .map(|value| value.to_string())
                .collect(),
            whitelist: Vec::new(),
            blacklist: Vec::new(),
        }
    }

    pub fn load_or_seed(base: &Path) -> Self {
        let layout = ArtifactV2Workspace::with_local_file_provider(base);
        let path = base.join("noise_filter.json");
        if let Ok(json) = layout.read_to_string_path_sync(&path) {
            if let Ok(filter) = serde_json::from_str(&json) {
                return filter;
            }
        }

        let seeded = Self::seeded();
        if let Err(error) = layout.create_dir_all_path_sync(base) {
            tracing::warn!("[API_MINING] failed to create noise-filter directory: {error}");
            return seeded;
        }
        match serde_json::to_string_pretty(&seeded) {
            Ok(json) => {
                if let Err(error) = layout.write_atomic_path_sync(path, json.as_bytes()) {
                    tracing::warn!("[API_MINING] failed to persist seeded noise filter: {error}");
                }
            },
            Err(error) => tracing::warn!("[API_MINING] failed to serialize noise filter: {error}"),
        }
        seeded
    }

    /// Read the scoped filter without creating files. Dashboard GETs and
    /// evaluators use this path so observation never mutates runtime state.
    pub fn load_read_only(base: &Path) -> Self {
        let layout = ArtifactV2Workspace::with_local_file_provider(base);
        layout
            .read_to_string_path_sync(&base.join("noise_filter.json"))
            .ok()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_else(Self::seeded)
    }

    fn parsed_parts(url: &str) -> Option<(String, String)> {
        let parsed = url::Url::parse(url).ok()?;
        Some((
            parsed.host_str().unwrap_or_default().to_ascii_lowercase(),
            parsed.path().to_ascii_lowercase(),
        ))
    }

    fn whitelisted(&self, host: &str, path: &str) -> bool {
        let host_path = format!("{host}{path}");
        self.whitelist
            .iter()
            .any(|entry| host_path.contains(&entry.to_ascii_lowercase()))
    }

    pub fn is_noise_host(&self, url: &str) -> bool {
        let Some((host, path)) = Self::parsed_parts(url) else {
            return false;
        };
        !self.whitelisted(&host, &path)
            && (self.hosts.iter().any(|entry| entry == &host)
                || self
                    .host_suffixes
                    .iter()
                    .any(|suffix| host.ends_with(&suffix.to_ascii_lowercase())))
    }

    pub fn is_noise_path(&self, url: &str) -> bool {
        let Some((host, path)) = Self::parsed_parts(url) else {
            return false;
        };
        !self.whitelisted(&host, &path)
            && self
                .path_substrings
                .iter()
                .any(|entry| path.contains(&entry.to_ascii_lowercase()))
    }

    pub fn is_noise_vendor_path(&self, url: &str) -> bool {
        let Some((host, path)) = Self::parsed_parts(url) else {
            return false;
        };
        !self.whitelisted(&host, &path)
            && self
                .vendor_path_substrings
                .iter()
                .any(|entry| path.contains(&entry.to_ascii_lowercase()))
    }

    pub fn is_noise_for(&self, url: &str, has_meaningful_json_body: bool) -> bool {
        let custom_blacklisted = Self::parsed_parts(url).is_some_and(|(host, path)| {
            !self.whitelisted(&host, &path)
                && self
                    .blacklist
                    .iter()
                    .any(|entry| format!("{host}{path}").contains(&entry.to_ascii_lowercase()))
        });
        custom_blacklisted
            || self.is_noise_host(url)
            || self.is_noise_vendor_path(url)
            || (!has_meaningful_json_body && self.is_noise_path(url))
    }
}

impl Default for NoiseFilter {
    fn default() -> Self {
        Self::seeded()
    }
}

pub fn is_beacon_like(method: &str, status: u16, response_size: u64) -> bool {
    method.eq_ignore_ascii_case("POST") && response_size == 0 && matches!(status, 200 | 204)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn seeded_filter_catches_observed_offenders() {
        let filter = NoiseFilter::seeded();
        for url in [
            "https://cloudflareinsights.com/cdn-cgi/rum",
            "https://developers.openai.com/_vercel/insights/view",
            "https://chatgpt.com/ces/v1/t",
            "https://ab.chatgpt.com/v1/rgstr?ec=1",
            "https://us.i.posthog.com/e/",
        ] {
            assert!(filter.is_noise_for(url, false), "{url}");
        }
        assert!(!filter.is_noise_for("https://hn.algolia.com/api/v1/search?query=rust", true));
    }

    #[test]
    fn generic_paths_only_win_without_meaningful_json() {
        let filter = NoiseFilter::seeded();
        assert!(!filter.is_noise_for("https://shop.test/api/v1/log?page=2", true));
        assert!(filter.is_noise_for("https://shop.test/api/v1/log?page=2", false));
        assert!(filter.is_noise_for("https://us.i.posthog.com/decide/", true));
    }

    #[test]
    fn beacon_shape_is_structural() {
        assert!(is_beacon_like("POST", 204, 0));
        assert!(is_beacon_like("POST", 200, 0));
        assert!(!is_beacon_like("POST", 200, 512));
        assert!(!is_beacon_like("GET", 204, 0));
    }
}
