use super::{store, types::*};
use serde_json::{json, Value};
fn primary() -> Value {
    json!({"comparison_id":"case","operation":"channel_classify","required_questions":["lane","repeat"],"offered_item_ids":["one"],"projection_version":"1","reference_version":"llm1","behavior_fingerprint":"behavior","mode":"gate","pack":"channel_classify","pack_version":"1","review_input":{"one":{"summary":"Synthetic review input"}},"items":[{"item_id":"one","status":"answered","eligible_answers":{"lane":"operator","repeat":"operator"},"thresholds":{"lane":0.7,"repeat":0.7},"response":{"model":{"adapter":"typesafe","model":"jev"},"pack_version":"1","answers":{"lane":{"choice":"later","options":["later","needs_reply"]},"repeat":{"noul":0.1}}}}]})
}
fn rows(conn: &rusqlite::Connection) -> Vec<DailyRow> {
    store::summary(
        conn,
        &MetricsQuery {
            days: Some(7),
            ..Default::default()
        },
        10 * store::DAY,
    )
    .unwrap()
    .0
}
fn totals(conn: &rusqlite::Connection, kind: &str) -> Stats {
    let mut s = Stats::default();
    for r in rows(conn).iter().filter(|r| r.kind == kind) {
        s.merge(&r.stats, 1);
    }
    s
}
fn count(s: &Stats, key: &str) -> i64 {
    *s.counts.get(key).unwrap_or(&0)
}
#[test]
fn decision_metrics_joins_deduplication_application_and_review_are_distinct() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metrics.sqlite");
    let mut c = store::open(&path, true).unwrap();
    let at = 10 * store::DAY;
    // Late stages can arrive first. They remain unevaluated until primary evidence arrives.
    let reference = json!({"comparison_id":"case","operation":"channel_classify","stage":"reference","status":"completed","reference":{"labels":{"one":{"lane":"needs_reply","repeat":false}}}});
    store::event(&mut c, at, &reference).unwrap();
    assert_eq!(count(&totals(&c, "quality"), "agreement:lane:n"), 0);
    store::event(&mut c, at, &primary()).unwrap();
    store::event(&mut c, at, &primary()).unwrap();
    store::event(&mut c, at, &reference).unwrap();
    let s = totals(&c, "quality");
    assert_eq!(count(&s, "offered"), 1);
    assert_eq!(count(&s, "agreement:lane:n"), 1);
    assert_eq!(count(&s, "agreement:lane:correct"), 0);
    assert_eq!(count(&s, "review:lane:n"), 0);
    assert_eq!(count(&s, "applied_decision"), 0);
    for status in ["stale", "preserved", "applied"] {
        store::event(&mut c,at,&json!({"comparison_id":"case","operation":"channel_classify","stage":"application","item_id":"one","status":status,"origin":"decision_engine","elapsed_ms":100})).unwrap();
        let s = totals(&c, "quality");
        assert_eq!(
            count(&s, "applied_decision"),
            i64::from(status == "applied")
        );
    }
    let update = ReviewUpdate {
        comparison_id: "case".into(),
        revision: 0,
        labels: std::collections::BTreeMap::from([(
            "one".into(),
            std::collections::BTreeMap::from([
                ("lane".into(), json!("needs_reply")),
                ("repeat".into(), json!(false)),
            ]),
        )]),
    };
    assert_eq!(store::review(&mut c, &update, at).unwrap(), 1);
    assert!(store::review(&mut c, &update, at)
        .unwrap_err()
        .contains("changed elsewhere"));
    let s = totals(&c, "quality");
    assert_eq!(count(&s, "review:lane:n"), 1);
    assert_eq!(count(&s, "baseline:lane:correct"), 1);
    assert_eq!(count(&s, "needs_reply_missed"), 1);
    drop(c);
    let c = store::open(&path, false).unwrap();
    assert_eq!(count(&totals(&c, "quality"), "needs_reply_missed"), 1);
}
#[test]
fn decision_metrics_cost_unknowns_and_cohorts_do_not_pool_operations() {
    let d = tempfile::tempdir().unwrap();
    let mut c = store::open(&d.path().join("db"), true).unwrap();
    let at = 10 * store::DAY;
    for (id, cost) in [("a", Some(0.01)), ("b", None)] {
        let v = json!({"stage":"usage","operation":"channel_classify","call":{"id":id,"model":"jev","provider":"decision:typesafe","cost_usd":cost,"input_tokens":10,"output_tokens":3,"cache_read_tokens":null,"latency_ms":20}});
        store::event(&mut c, at, &v).unwrap();
        store::event(&mut c, at, &v).unwrap();
    }
    let s = totals(&c, "usage");
    assert_eq!(count(&s, "calls"), 2);
    assert_eq!(count(&s, "priced_calls"), 1);
    assert_eq!(count(&s, "cache_read_known"), 0);
    assert!((s.cost_usd - 0.01).abs() < 1e-10);
    for (i, op, threshold) in [
        ("1", "channel_classify", 0.7),
        ("2", "channel_classify", 0.8),
        ("3", "other", 0.7),
    ] {
        let mut v = primary();
        v["comparison_id"] = json!(i);
        v["operation"] = json!(op);
        v["items"][0]["thresholds"]["lane"] = json!(threshold);
        store::event(&mut c, at, &v).unwrap();
    }
    assert_eq!(rows(&c).iter().filter(|r| r.kind == "quality").count(), 3);
}
#[test]
fn decision_metrics_review_rejects_unknown_labels_and_missing_inputs() {
    let d = tempfile::tempdir().unwrap();
    let mut c = store::open(&d.path().join("db"), true).unwrap();
    let at = 10 * store::DAY;
    let mut p = primary();
    p.as_object_mut().unwrap().remove("review_input");
    store::event(&mut c, at, &p).unwrap();
    let u = ReviewUpdate {
        comparison_id: "case".into(),
        revision: 0,
        labels: Default::default(),
    };
    assert!(store::review(&mut c, &u, at)
        .unwrap_err()
        .contains("Frozen"));
}
#[test]
fn decision_metrics_bounded_indexed_reads_and_incremental_retention() {
    let d = tempfile::tempdir().unwrap();
    let mut c = store::open(&d.path().join("db"), true).unwrap();
    let tx = c.transaction().unwrap();
    let text = serde_json::to_string(&Case::default()).unwrap();
    for i in 0..100_000 {
        tx.execute(
            "INSERT INTO cases VALUES(?1,?2,'other',?3)",
            rusqlite::params![format!("{i:08}"), 10 * store::DAY + i, text],
        )
        .unwrap();
    }
    for i in 0..1000 {
        let row = DailyRow {
            day: 10,
            kind: "usage".into(),
            cohort: i.to_string(),
            operation: "other".into(),
            model: "jev".into(),
            identity: json!({}),
            stats: Stats::default(),
        };
        tx.execute(
            "INSERT INTO daily VALUES(10,'usage',?1,'other','jev','{}',?2)",
            rusqlite::params![row.cohort, serde_json::to_string(&row.stats).unwrap()],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let q = MetricsQuery {
        days: Some(7),
        operation: Some("other".into()),
        ..Default::default()
    };
    let started = std::time::Instant::now();
    let r = store::summary(&c, &q, 10 * store::DAY).unwrap();
    let elapsed = started.elapsed();
    println!(
        "Decision metrics: 100k recent rows + 1000 daily summaries, read {:.2}ms",
        elapsed.as_secs_f64() * 1000.0
    );
    assert_eq!(r.0.len(), 1000);
    assert!(elapsed.as_secs_f64() < 1.0);
    let plan:String=c.query_row("EXPLAIN QUERY PLAN SELECT data FROM cases WHERE operation='other' AND at_ms<1000 ORDER BY at_ms DESC LIMIT 25",[],|r|r.get(3)).unwrap();
    assert!(plan.contains("cases_operation"), "{plan}");
    assert_eq!(store::cases(&c, &q, 10 * store::DAY).unwrap().len(), 25);
    store::retain(&c, 10 * store::DAY).unwrap();
    let left: i64 = c
        .query_row("SELECT count(*) FROM cases", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 99_488); // one bounded 512-row cap delete
}
#[test]
fn decision_metrics_query_is_bounded() {
    assert!(MetricsQuery {
        days: Some(365),
        ..Default::default()
    }
    .validate()
    .is_err());
}

#[test]
fn decision_metrics_cursor_keeps_tied_timestamps_and_expired_reviews_are_rejected() {
    let d = tempfile::tempdir().unwrap();
    let mut c = store::open(&d.path().join("db"), true).unwrap();
    let at = 10 * store::DAY;
    for i in 0..30 {
        let mut p = primary();
        p["comparison_id"] = json!(format!("{i:03}"));
        store::event(&mut c, at, &p).unwrap();
    }
    let first = store::cases(&c, &MetricsQuery::default(), at).unwrap();
    assert_eq!(first.len(), 25);
    let q = MetricsQuery {
        before: Some(at),
        before_id: Some(first.last().unwrap().id.clone()),
        ..Default::default()
    };
    let next = store::cases(&c, &q, at).unwrap();
    assert_eq!(next.len(), 5);
    assert!(!next.iter().any(|n| first.iter().any(|f| f.id == n.id)));
    let update = ReviewUpdate {
        comparison_id: "000".into(),
        revision: 0,
        labels: std::collections::BTreeMap::from([(
            "one".into(),
            std::collections::BTreeMap::from([
                ("lane".into(), json!("invented")),
                ("repeat".into(), json!(false)),
            ]),
        )]),
    };
    assert!(store::review(&mut c, &update, at)
        .unwrap_err()
        .contains("vocabulary"));
    assert!(store::review(&mut c, &update, at + 8 * store::DAY)
        .unwrap_err()
        .contains("expired"));
    assert!(
        store::cases(&c, &MetricsQuery::default(), at + 8 * store::DAY)
            .unwrap()
            .is_empty()
    );
}
