//! Phase H5.4 — observability for the legacy HITL respond URLs.
//!
//! Each legacy resolve handler that's been deprecated in H4.7
//! (`/user-requests/{id}/respond`, `/executions/{id}/agentic-resume`,
//! `/approvals/{id}/resolve`, `/v3/.../clarifications/{id}/respond`)
//! emits a structured `tracing::warn!` on `magician::hitl::deprecation`.
//! That tells you when a hit happens but not how often or whether it's
//! still happening at all.
//!
//! H5.4 adds a per-endpoint atomic counter + last-hit timestamp so any
//! operator surface can answer "did anyone hit a legacy URL today?"
//! without log access. The H6 phase gates emit-side drops on the
//! corresponding counter going silent — when the count stops climbing
//! for the agreed dwell time, we know the consumer migration has
//! actually landed.
//!
//! Counters are process-local; production deployments aggregate them
//! via the `GET /api/magician/v2/hitl/deprecation-metrics` endpoint
//! defined below.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::OnceLock;

use actix_web::{web, HttpResponse, Responder};
use dashmap::DashMap;
use serde::Serialize;

/// Per-endpoint counter pair. Reads are relaxed because operators
/// scraping the metrics don't need ordering against the actual hit
/// stream — eventual consistency is fine.
#[derive(Debug)]
struct EndpointMetric {
    count: AtomicU64,
    last_hit_at_ms: AtomicI64,
}

impl EndpointMetric {
    fn new() -> Self {
        Self {
            count: AtomicU64::new(0),
            last_hit_at_ms: AtomicI64::new(0),
        }
    }
}

fn registry() -> &'static DashMap<&'static str, EndpointMetric> {
    static REGISTRY: OnceLock<DashMap<&'static str, EndpointMetric>> = OnceLock::new();
    REGISTRY.get_or_init(DashMap::new)
}

/// Record one hit on the named legacy endpoint. Called next to the
/// `tracing::warn!` line at every legacy resolve handler. The endpoint
/// label must be a `'static` string (matches the keys in the
/// snapshot output) — keep them stable so dashboards can pin a chart
/// per endpoint over time.
pub fn record_hit(endpoint: &'static str) {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let entry = registry()
        .entry(endpoint)
        .or_insert_with(EndpointMetric::new);
    entry.count.fetch_add(1, Ordering::Relaxed);
    // `fetch_max` keeps `last_hit_at_ms` monotonic under contention. A
    // plain `store` lets thread A's earlier-captured `now_ms` overwrite
    // thread B's later value if A's `store` lands second; that breaks
    // the H6 dwell-window gate which reads this field.
    entry.last_hit_at_ms.fetch_max(now_ms, Ordering::Relaxed);
}

#[derive(Debug, Serialize)]
pub struct DeprecationMetricSnapshot {
    pub endpoint: String,
    pub hits_total: u64,
    /// Unix ms of the most recent hit, or `null` when none recorded
    /// in this process.
    pub last_hit_at_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct DeprecationMetricsResponse {
    pub generated_at_ms: i64,
    pub metrics: Vec<DeprecationMetricSnapshot>,
}

/// Snapshot every endpoint that's ever recorded a hit in this process.
/// Empty when no legacy URL has been hit since boot — which is the
/// desired steady state and the signal H6 watches for.
pub fn snapshot() -> Vec<DeprecationMetricSnapshot> {
    let mut out = Vec::new();
    for entry in registry().iter() {
        let last = entry.last_hit_at_ms.load(Ordering::Relaxed);
        out.push(DeprecationMetricSnapshot {
            endpoint: entry.key().to_string(),
            hits_total: entry.count.load(Ordering::Relaxed),
            last_hit_at_ms: if last == 0 { None } else { Some(last) },
        });
    }
    // Stable ordering across requests (alphabetical by endpoint label)
    // so dashboards diffing two snapshots see consistent rows.
    out.sort_by(|a, b| a.endpoint.cmp(&b.endpoint));
    out
}

/// `GET /api/magician/v2/hitl/deprecation-metrics`
///
/// Returns the current legacy-endpoint hit counts. Designed for
/// scraping by ops dashboards / cron jobs. Cheap — no auth, no scope.
/// Process-local so deployments behind a load balancer should fan
/// out per-instance; the H6 gate is "every instance reports zero
/// hits for N days" which a coordinator can derive from per-instance
/// scrapes.
pub async fn list_deprecation_metrics_handler() -> impl Responder {
    HttpResponse::Ok().json(DeprecationMetricsResponse {
        generated_at_ms: chrono::Utc::now().timestamp_millis(),
        metrics: snapshot(),
    })
}

pub fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.route(
        "/hitl/deprecation-metrics",
        web::get().to(list_deprecation_metrics_handler),
    );
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    /// Each `record_hit` call increments the count and updates the
    /// last-hit timestamp. The endpoint key is stable so multiple
    /// hits land on the same row, not a new row per hit.
    #[test]
    fn record_hit_increments_counter() {
        // Use a test-specific endpoint name to avoid colliding with
        // other tests that exercise the singleton registry.
        let endpoint = "/__test/hitl/inc-counter";
        record_hit(endpoint);
        record_hit(endpoint);
        record_hit(endpoint);
        let snap = snapshot();
        let row = snap.iter().find(|r| r.endpoint == endpoint).expect("row");
        assert_eq!(row.hits_total, 3);
        assert!(row.last_hit_at_ms.is_some());
    }

    /// Different endpoint labels produce distinct rows.
    #[test]
    fn record_hit_separates_endpoints() {
        let a = "/__test/hitl/separate-a";
        let b = "/__test/hitl/separate-b";
        record_hit(a);
        record_hit(b);
        record_hit(b);
        let snap = snapshot();
        let row_a = snap.iter().find(|r| r.endpoint == a).expect("row a");
        let row_b = snap.iter().find(|r| r.endpoint == b).expect("row b");
        assert_eq!(row_a.hits_total, 1);
        assert_eq!(row_b.hits_total, 2);
    }

    /// Snapshot is sorted alphabetically so consumers diffing two
    /// reads see stable row ordering.
    #[test]
    fn snapshot_sorted_alphabetically() {
        let z = "/__test/hitl/zzz-sort";
        let a = "/__test/hitl/aaa-sort";
        record_hit(z);
        record_hit(a);
        let snap = snapshot();
        let positions: Vec<usize> = [a, z]
            .iter()
            .map(|name| {
                snap.iter()
                    .position(|r| r.endpoint == *name)
                    .expect("present")
            })
            .collect();
        assert!(
            positions[0] < positions[1],
            "alphabetical: aaa-sort should appear before zzz-sort"
        );
    }
}
