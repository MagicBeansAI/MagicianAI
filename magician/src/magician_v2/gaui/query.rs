//! MUIJ JSONPath query engine — evaluates RFC 9535 JSONPath queries on
//! `serde_json::Value` data using `serde_json_path`.
//!
//! Used by the GAUI pipeline to extract display data from agent memory tiers.
//! Read-only: no mutations, no eval, no write operations.
//!
//! Parsed `JsonPath` expressions are cached per-instance to avoid re-parsing
//! the same query string on repeated invocations.

use std::collections::HashMap;

use serde_json::Value;
use serde_json_path::JsonPath;

/// R367: Maximum number of cached parse results before eviction.
const MAX_QUERY_CACHE_SIZE: usize = 512;

/// JSONPath query engine with per-instance parse cache.
///
/// Create one instance per pipeline stage and reuse it across invocations
/// to benefit from the cache. Cache is bounded to [`MAX_QUERY_CACHE_SIZE`]
/// entries — when full, the least recently used half is evicted (R649).
pub struct MuijQueryEngine {
    /// `None` value means the path was attempted and failed to parse.
    cache: HashMap<String, Option<JsonPath>>,
    /// R649: Monotonic generation counter — incremented on each `query()` call.
    /// Each cache entry records the generation at which it was last used,
    /// enabling half-eviction of least-recently-used entries.
    generation: u64,
    /// R649: Per-entry last-used generation for LRU eviction.
    last_used: HashMap<String, u64>,
}

impl Default for MuijQueryEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl MuijQueryEngine {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
            generation: 0,
            last_used: HashMap::new(),
        }
    }

    /// Evaluate a JSONPath query on a JSON value.
    /// Returns matching nodes, or empty vec on invalid query/no matches.
    /// Parsed paths are cached — the second call with the same `path` string
    /// skips parsing entirely.
    pub fn query<'a>(&mut self, data: &'a Value, path: &str) -> Vec<&'a Value> {
        self.generation += 1;

        // R649: When cache is full, evict the least-recently-used half instead
        // of clearing entirely. Preserves the working set of frequently used
        // queries, avoiding the periodic performance cliff of full-clear.
        if self.cache.len() >= MAX_QUERY_CACHE_SIZE && !self.cache.contains_key(path) {
            let before = self.cache.len();
            let mut generations: Vec<u64> = self.last_used.values().copied().collect();
            generations.sort_unstable();
            let median = generations[generations.len() / 2];
            self.cache
                .retain(|k, _| self.last_used.get(k).copied().unwrap_or(0) >= median);
            self.last_used.retain(|k, _| self.cache.contains_key(k));
            // R730: Log eviction so operators can monitor cache pressure
            tracing::debug!(
                before = before,
                after = self.cache.len(),
                "JSONPath query cache LRU eviction (R649)"
            );
        }

        // Record last-used generation for this path
        self.last_used.insert(path.to_string(), self.generation);

        let entry =
            self.cache
                .entry(path.to_string())
                .or_insert_with(|| match JsonPath::parse(path) {
                    Ok(p) => Some(p),
                    Err(e) => {
                        tracing::warn!(query = path, error = %e, "Invalid JSONPath query");
                        None
                    },
                });

        match entry {
            Some(p) => p.query(data).all(),
            None => vec![],
        }
    }

    /// Number of cached parse results (valid + invalid).
    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    /// Clear all cached parse results to bound memory usage (R54).
    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.last_used.clear();
    }

    /// Default query for collection data: `$.items[*]`
    pub fn default_query() -> &'static str {
        "$.items[*]"
    }
}

// ---------------------------------------------------------------------------
// Query materialization — R680: shared between REST and WS paths
// ---------------------------------------------------------------------------

use crate::magician_v2::gaui::MuijComponent;

/// Materialize JSONPath queries on components' `props` into `static_snapshot`.
///
/// Recurses into children. Preserves existing snapshots (R496) and always
/// queries from `props` not `static_snapshot` (R361).
pub fn materialize_component_queries(
    components: &mut [MuijComponent],
    query_engine: &mut MuijQueryEngine,
) {
    for component in components {
        if let Some(query) = component.query.clone() {
            // R361: Always query from props — static_snapshot is the materialized
            // output, not the input.
            // R496: Only materialize when static_snapshot is absent — preserve
            // delta-provided rich snapshots from the emitter pipeline.
            if component.static_snapshot.is_none() {
                let matches = query_engine.query(&component.props, &query);
                component.static_snapshot = Some(serde_json::Value::Array(
                    matches.into_iter().cloned().collect(),
                ));
            }
        }
        if !component.children.is_empty() {
            materialize_component_queries(&mut component.children, query_engine);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn query_items_wildcard() {
        let mut engine = MuijQueryEngine::new();
        let data = json!({"items": [1, 2, 3]});
        let results = engine.query(&data, "$.items[*]");
        assert_eq!(results.len(), 3);
        assert_eq!(*results[0], json!(1));
        assert_eq!(*results[1], json!(2));
        assert_eq!(*results[2], json!(3));
    }

    #[test]
    fn query_with_filter() {
        let mut engine = MuijQueryEngine::new();
        let data = json!({
            "items": [
                {"name": "a", "value": 3},
                {"name": "b", "value": 7},
                {"name": "c", "value": 10}
            ]
        });
        let results = engine.query(&data, "$.items[?(@.value > 5)]");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["name"], json!("b"));
        assert_eq!(results[1]["name"], json!("c"));
    }

    #[test]
    fn invalid_jsonpath_returns_empty() {
        let mut engine = MuijQueryEngine::new();
        let data = json!({"items": [1]});
        let results = engine.query(&data, "$[invalid!!!");
        assert!(
            results.is_empty(),
            "invalid JSONPath should return empty vec"
        );
    }

    #[test]
    fn empty_data_returns_empty() {
        let mut engine = MuijQueryEngine::new();
        let data = json!({});
        let results = engine.query(&data, "$.items[*]");
        assert!(results.is_empty());
    }

    #[test]
    fn default_query_is_items_wildcard() {
        assert_eq!(MuijQueryEngine::default_query(), "$.items[*]");
    }

    #[test]
    fn query_nested_path() {
        let mut engine = MuijQueryEngine::new();
        let data = json!({"a": {"b": {"c": 42}}});
        let results = engine.query(&data, "$.a.b.c");
        assert_eq!(results.len(), 1);
        assert_eq!(*results[0], json!(42));
    }

    #[test]
    fn query_no_matches_returns_empty() {
        let mut engine = MuijQueryEngine::new();
        let data = json!({"items": [1, 2, 3]});
        let results = engine.query(&data, "$.nonexistent[*]");
        assert!(results.is_empty());
    }

    #[test]
    fn repeated_query_uses_cache() {
        let mut engine = MuijQueryEngine::new();
        let data = json!({"items": [1, 2, 3]});

        // First call — parses and caches
        let r1 = engine.query(&data, "$.items[*]");
        assert_eq!(r1.len(), 3);
        assert_eq!(engine.cache_len(), 1);

        // Second call — cache hit, same results
        let r2 = engine.query(&data, "$.items[*]");
        assert_eq!(r2.len(), 3);
        assert_eq!(
            engine.cache_len(),
            1,
            "cache should not grow on repeated query"
        );

        // Different query — new cache entry
        let r3 = engine.query(&data, "$.items[0]");
        assert_eq!(r3.len(), 1);
        assert_eq!(engine.cache_len(), 2);
    }

    #[test]
    fn invalid_query_is_cached_too() {
        let mut engine = MuijQueryEngine::new();
        let data = json!({"items": [1]});

        let r1 = engine.query(&data, "$[invalid!!!");
        assert!(r1.is_empty());
        assert_eq!(engine.cache_len(), 1);

        // Second call with same invalid path — cache hit (no re-parse, no warning)
        let r2 = engine.query(&data, "$[invalid!!!");
        assert!(r2.is_empty());
        assert_eq!(engine.cache_len(), 1);
    }
}
