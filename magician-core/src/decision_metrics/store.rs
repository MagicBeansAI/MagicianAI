use super::types::*;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path, time::Duration};

pub const DAY: i64 = 86_400_000;
pub fn open(path: &Path, write: bool) -> Result<Connection, String> {
    let flags = if write {
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    };
    let conn = Connection::open_with_flags(path, flags).map_err(|e| e.to_string())?;
    conn.busy_timeout(Duration::from_millis(100))
        .map_err(|e| e.to_string())?;
    conn.execute_batch("PRAGMA cache_size=-512; PRAGMA mmap_size=0;")
        .map_err(|e| e.to_string())?;
    if write {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
        }
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=256; PRAGMA journal_size_limit=4194304;
        CREATE TABLE IF NOT EXISTS daily(day INTEGER NOT NULL,kind TEXT NOT NULL,cohort TEXT NOT NULL,operation TEXT NOT NULL,model TEXT NOT NULL,identity TEXT NOT NULL,stats TEXT NOT NULL,PRIMARY KEY(day,kind,cohort));
        CREATE INDEX IF NOT EXISTS daily_filter ON daily(operation,day,kind);
        CREATE INDEX IF NOT EXISTS daily_time ON daily(day);
        CREATE TABLE IF NOT EXISTS cases(id TEXT PRIMARY KEY,at_ms INTEGER NOT NULL,operation TEXT NOT NULL,data TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS cases_time ON cases(at_ms DESC,id);
        CREATE INDEX IF NOT EXISTS cases_operation ON cases(operation,at_ms DESC,id DESC);
        CREATE TABLE IF NOT EXISTS receipts(id TEXT PRIMARY KEY,at_ms INTEGER NOT NULL);
        CREATE INDEX IF NOT EXISTS receipts_time ON receipts(at_ms);
        CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value INTEGER NOT NULL);").map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT OR IGNORE INTO meta VALUES('started',?1)",
            [chrono::Utc::now().timestamp_millis()],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(conn)
}
fn string<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
fn label(v: &Value) -> Option<Value> {
    if v["choice"].is_string() {
        Some(v["choice"].clone())
    } else if let Some(n) = v["noul"].as_f64() {
        Some(json!(n >= 0.5))
    } else {
        v["score"].as_f64().map(|n| json!(n))
    }
}
// Each item is its own cohort: do not pool different backup model identities.
fn quality(case: &Case) -> Vec<DailyRow> {
    let Some(primary) = &case.primary else {
        let mut out = Vec::new();
        for app in case.applications.values() {
            let identity =
                json!({"operation":case.operation,"model":app["model"],"mode":"incumbent_only"});
            let cohort = blake3::hash(identity.to_string().as_bytes())
                .to_hex()
                .to_string();
            let mut stats = Stats::default();
            let status = string(app, "status");
            stats.count(&format!("application:{status}"), 1);
            if app["origin"] != "decision_engine" && app["receipt_available"] != true {
                stats.count("llm_receipt_missing", 1);
            }
            if status == "applied" {
                stats.count("applied_llm", 1);
            }
            if let Some(ms) = app["elapsed_ms"].as_u64() {
                stats.latency(ms, true);
            }
            out.push(DailyRow {
                day: case.at_ms / DAY,
                kind: "quality".into(),
                cohort,
                operation: case.operation.clone(),
                model: string(app, "model").into(),
                identity,
                stats,
            });
        }
        return out;
    };
    let mut grouped: BTreeMap<String, DailyRow> = BTreeMap::new();
    let required = primary["required_questions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let empty = Vec::new();
    for item in primary["items"].as_array().unwrap_or(&empty) {
        let response = &item["response"];
        let id = string(item, "item_id");
        let model = response["model"]["model"].as_str().unwrap_or("unanswered");
        let identity = json!({"operation":case.operation,"model":response["model"],"pack":primary["pack"],"pack_version":response["pack_version"].as_str().unwrap_or(string(primary,"pack_version")),"projection":primary["projection_version"],"reference":primary["reference_version"],"behavior":primary["behavior_fingerprint"],"thresholds":item["thresholds"],"mode":primary["mode"]});
        let cohort = blake3::hash(identity.to_string().as_bytes())
            .to_hex()
            .to_string();
        let row = grouped.entry(cohort.clone()).or_insert_with(|| DailyRow {
            day: case.at_ms / DAY,
            kind: "quality".into(),
            cohort,
            operation: case.operation.clone(),
            model: model.into(),
            identity,
            stats: Stats::default(),
        });
        let s = &mut row.stats;
        s.count("offered", 1);
        if !case.applications.contains_key(id) {
            s.count("application_unknown", 1);
        }
        if item["status"] == "answered" {
            s.count("answered", 1);
        }
        if !required.is_empty()
            && required.iter().all(|h| {
                h.as_str()
                    .is_some_and(|h| item["eligible_answers"][h].is_string())
            })
        {
            s.count("eligible", 1);
        }
        if let Some(app) = case.applications.get(id) {
            let status = string(app, "status");
            s.count(&format!("application:{status}"), 1);
            if app["origin"] != "decision_engine" && app["receipt_available"] != true {
                s.count("llm_receipt_missing", 1);
            }
            if status == "applied" {
                s.count(
                    if app["origin"] == "decision_engine" {
                        "applied_decision"
                    } else {
                        "applied_llm"
                    },
                    1,
                );
            }
            if let Some(ms) = app["elapsed_ms"].as_u64() {
                s.latency(ms, true);
            }
        }
        let reference = case.reference.as_ref().unwrap_or(&primary["reference"]);
        for head in &required {
            let Some(head) = head.as_str() else {
                continue;
            };
            let Some(answer) = label(&response["answers"][head]) else {
                continue;
            };
            if let Some(expected) = reference["labels"][id].get(head).filter(|v| !v.is_null()) {
                s.count(&format!("agreement:{head}:n"), 1);
                if &answer == expected {
                    s.count(&format!("agreement:{head}:correct"), 1);
                }
            }
            if let Some(expected) = case.reviews.get(id).and_then(|m| m.get(head)) {
                s.count(&format!("review:{head}:n"), 1);
                if &answer == expected {
                    s.count(&format!("review:{head}:correct"), 1);
                }
                if let Some(baseline) = reference["labels"][id].get(head) {
                    s.count(&format!("baseline:{head}:n"), 1);
                    if baseline == expected {
                        s.count(&format!("baseline:{head}:correct"), 1);
                    }
                }
                if head == "lane" && expected == "needs_reply" {
                    s.count("needs_reply_positive", 1);
                    if answer != "needs_reply" {
                        s.count("needs_reply_missed", 1);
                    }
                }
            }
        }
    }
    // Comparison health is per batch, never multiplied by batch size.
    let status = case.reference_status.as_deref().unwrap_or(
        if primary["reference"]["labels"]
            .as_object()
            .is_some_and(|m| !m.is_empty())
        {
            "completed"
        } else if primary["reference"].is_object() {
            "no_valid_reference"
        } else {
            "not_sampled"
        },
    );
    let identity = json!({"operation":case.operation,"projection":primary["projection_version"],"reference":primary["reference_version"],"behavior":primary["behavior_fingerprint"],"mode":primary["mode"]});
    let cohort = blake3::hash(identity.to_string().as_bytes())
        .to_hex()
        .to_string();
    let mut stats = Stats::default();
    stats.count(&format!("sampling:{status}"), 1);
    grouped.insert(
        format!("sample:{cohort}"),
        DailyRow {
            day: case.at_ms / DAY,
            kind: "sampling".into(),
            cohort,
            operation: case.operation.clone(),
            model: String::new(),
            identity,
            stats,
        },
    );
    grouped.into_values().collect()
}
fn add(conn: &Connection, row: &DailyRow, sign: i64) -> Result<(), String> {
    let old: Option<String> = conn
        .query_row(
            "SELECT stats FROM daily WHERE day=?1 AND kind=?2 AND cohort=?3",
            params![row.day, row.kind, row.cohort],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let mut stats = match old {
        Some(s) => serde_json::from_str::<Stats>(&s).map_err(|e| e.to_string())?,
        None => Stats::default(),
    };
    stats.merge(&row.stats, sign);
    conn.execute("INSERT INTO daily VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(day,kind,cohort) DO UPDATE SET stats=excluded.stats",params![row.day,row.kind,row.cohort,row.operation,row.model,row.identity.to_string(),serde_json::to_string(&stats).unwrap()]).map_err(|e|e.to_string())?;
    Ok(())
}
fn cost(
    conn: &Connection,
    at: i64,
    operation: &str,
    category: &str,
    call: &Value,
) -> Result<(), String> {
    let id = call["id"]
        .as_str()
        .or_else(|| call["receipt"]["context"]["llm_call_id"].as_str());
    let Some(id) = id.filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    if conn
        .execute(
            "INSERT OR IGNORE INTO receipts VALUES(?1,?2)",
            params![id, at],
        )
        .map_err(|e| e.to_string())?
        == 0
    {
        return Ok(());
    }
    let identity = json!({"operation":operation,"provider":call["provider"],"model":call["model"],"category":category,"pricing_version":call["pricing_version"]});
    let cohort = blake3::hash(identity.to_string().as_bytes())
        .to_hex()
        .to_string();
    let mut stats = Stats::default();
    stats.count("calls", 1);
    if call["success"].is_boolean() {
        stats.count("success_known", 1);
    }
    if call["success"] == true {
        stats.count("succeeded", 1);
    }
    if let Some(n) = call["cost_usd"]
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.0)
    {
        stats.cost_usd = n;
        stats.count("priced_calls", 1);
    }
    if let (Some(i), Some(o)) = (
        call["input_tokens"].as_i64(),
        call["output_tokens"].as_i64(),
    ) {
        stats.input_tokens = i;
        stats.output_tokens = o;
        stats.count("token_known", 1);
    }
    if let Some(n) = call["cache_read_tokens"].as_i64() {
        stats.cache_read_tokens = n;
        stats.count("cache_read_known", 1);
    }
    if let Some(n) = call["cache_write_tokens"].as_i64() {
        stats.cache_write_tokens = n;
        stats.count("cache_write_known", 1);
    }
    if let Some(n) = call["latency_ms"].as_u64() {
        stats.latency(n, false);
    }
    add(
        conn,
        &DailyRow {
            day: at / DAY,
            kind: "usage".into(),
            cohort,
            operation: operation.into(),
            model: string(call, "model").into(),
            identity,
            stats,
        },
        1,
    )
}
fn reference_cost(
    conn: &Connection,
    at: i64,
    op: &str,
    category: &str,
    reference: &Value,
) -> Result<(), String> {
    if reference["call"].is_object() {
        cost(conn, at, op, category, &reference["call"])?;
    }
    if let Some(calls) = reference["calls"].as_array() {
        for call in calls {
            cost(conn, at, op, category, call)?;
        }
    }
    Ok(())
}
pub fn event(conn: &mut Connection, at: i64, data: &Value) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    if data["stage"] == "usage" {
        cost(
            &tx,
            at,
            string(data, "operation"),
            "decision",
            &data["call"],
        )?;
    } else {
        let id = string(data, "comparison_id");
        if id.is_empty() {
            return Ok(());
        }
        let previous: Option<String> = tx
            .query_row("SELECT data FROM cases WHERE id=?1", [id], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())?;
        let mut case = match previous {
            Some(s) => serde_json::from_str::<Case>(&s).map_err(|e| e.to_string())?,
            None => Case {
                id: id.into(),
                at_ms: at,
                operation: string(data, "operation").into(),
                ..Default::default()
            },
        };
        let old = quality(&case);
        match string(data, "stage") {
            "reference" => {
                if data["reference"].is_object() {
                    reference_cost(&tx, at, &case.operation, "monitoring", &data["reference"])?;
                    case.reference = Some(
                        json!({"labels":data["reference"]["labels"],"latency_ms":data["reference"]["latency_ms"]}),
                    );
                }
                // Queued may be observed after completed; never demote a terminal receipt.
                if case.reference.is_none() || data["reference"].is_object() {
                    case.reference_status = Some(string(data, "status").into());
                }
            },
            "application" => {
                let item = string(data, "item_id");
                if !item.is_empty() {
                    let mut application = data.clone();
                    application.as_object_mut().unwrap().remove("reference");
                    case.applications.insert(item.into(), application);
                }
                reference_cost(
                    &tx,
                    at,
                    &case.operation,
                    data["category"].as_str().unwrap_or("fallback"),
                    &data["reference"],
                )?;
            },
            "projection" => case.projection = Some(data["items"].clone()),
            "completion" => cost(&tx, at, &case.operation, "text", &data["text_call"])?,
            "invocation" => {
                let status: String = string(data, "status").chars().take(64).collect();
                if !status.is_empty() {
                    case.invocation_status = Some(status);
                }
            },
            "" => {
                if case.primary.is_none() {
                    case.operation = string(data, "operation").into();
                    case.at_ms = at;
                    case.projection = data.get("review_input").cloned().or(case.projection);
                    let mut primary = data.clone();
                    primary.as_object_mut().unwrap().remove("review_input");
                    primary["reference"] = if data["reference"].is_object() {
                        json!({"labels":data["reference"]["labels"],"latency_ms":data["reference"]["latency_ms"]})
                    } else {
                        Value::Null
                    };
                    case.primary = Some(primary);
                    reference_cost(
                        &tx,
                        at,
                        &case.operation,
                        if data["mode"] == "shadow" {
                            "incumbent"
                        } else {
                            "monitoring"
                        },
                        &data["reference"],
                    )?;
                }
            },
            _ => return Ok(()),
        }
        let reference = case
            .reference
            .as_ref()
            .or_else(|| case.primary.as_ref().map(|v| &v["reference"]));
        if matches!(string(data, "stage"), "reference" | "")
            && reference.is_some_and(|v| v["labels"].as_object().is_some_and(|m| !m.is_empty()))
        {
            tx.execute("INSERT INTO meta VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=MAX(value,excluded.value)",params![format!("last_reference:{}",case.operation),at]).map_err(|e|e.to_string())?;
        }
        for row in old {
            add(&tx, &row, -1)?;
        }
        for row in quality(&case) {
            add(&tx, &row, 1)?;
        }
        if serde_json::to_vec(&case).map_err(|e| e.to_string())?.len() > 64 * 1024 {
            return Err("Joined comparison exceeds retained size limit".into());
        }
        tx.execute("INSERT INTO cases VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET at_ms=excluded.at_ms,operation=excluded.operation,data=excluded.data",params![id,case.at_ms,case.operation,serde_json::to_string(&case).unwrap()]).map_err(|e|e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}
pub fn retain(conn: &Connection, now: i64) -> Result<(), String> {
    // Every statement deletes at most 512 rows; SQLite reuses freed pages.
    for sql in ["DELETE FROM cases WHERE id IN (SELECT id FROM cases ORDER BY at_ms DESC LIMIT 512 OFFSET 2048)","DELETE FROM receipts WHERE id IN (SELECT id FROM receipts ORDER BY at_ms DESC LIMIT 512 OFFSET 100000)"] {conn.execute(sql,[]).map_err(|e|e.to_string())?;}
    for (table, col, cutoff) in [
        ("cases", "at_ms", now - 7 * DAY),
        ("receipts", "at_ms", now - 7 * DAY),
        ("daily", "day", now / DAY - 90),
    ] {
        conn.execute(&format!("DELETE FROM {table} WHERE rowid IN (SELECT rowid FROM {table} WHERE {col}<?1 LIMIT 512)"),[cutoff]).map_err(|e|e.to_string())?;
    }
    Ok(())
}
pub fn summary(
    conn: &Connection,
    q: &MetricsQuery,
    now: i64,
) -> Result<(Vec<DailyRow>, bool, Option<i64>), String> {
    let start = now / DAY - (q.days.unwrap_or(7) as i64) * 2 + 1; // current + preceding window
    let operation_filter = if q.operation.is_some() {
        "operation=?2"
    } else {
        "?2 IS NULL"
    };
    let sql=format!("SELECT day,kind,cohort,operation,model,identity,stats FROM daily WHERE day>=?1 AND {operation_filter} AND (?3 IS NULL OR model=?3) AND (?4 IS NULL OR cohort=?4) ORDER BY day DESC LIMIT 2049");
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![start, q.operation, q.model, q.cohort], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        let (day, kind, cohort, operation, model, identity, stats) =
            row.map_err(|e| e.to_string())?;
        out.push(DailyRow {
            day,
            kind,
            cohort,
            operation,
            model,
            identity: serde_json::from_str(&identity).map_err(|e| e.to_string())?,
            stats: serde_json::from_str(&stats).map_err(|e| e.to_string())?,
        });
    }
    let truncated = out.len() > 2048;
    out.truncate(2048);
    let started = conn
        .query_row("SELECT value FROM meta WHERE key='started'", [], |r| {
            r.get(0)
        })
        .optional()
        .map_err(|e| e.to_string())?;
    Ok((out, truncated, started))
}
pub fn cases(conn: &Connection, q: &MetricsQuery, now: i64) -> Result<Vec<Case>, String> {
    let operation_filter = if q.operation.is_some() {
        "operation=?2"
    } else {
        "?2 IS NULL"
    };
    let sql=format!("SELECT data FROM cases WHERE (at_ms<?1 OR (at_ms=?1 AND id<?3)) AND {operation_filter} AND at_ms>=?4 ORDER BY at_ms DESC,id DESC LIMIT 25");
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in stmt
        .query_map(
            params![
                q.before.unwrap_or(i64::MAX),
                q.operation,
                q.before_id.as_deref().unwrap_or("~"),
                now - 7 * DAY
            ],
            |r| r.get::<_, String>(0),
        )
        .map_err(|e| e.to_string())?
    {
        out.push(
            serde_json::from_str(&row.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?,
        );
    }
    Ok(out)
}
pub fn review(conn: &mut Connection, update: &ReviewUpdate, now: i64) -> Result<u64, String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let data: Option<String> = tx
        .query_row(
            "SELECT data FROM cases WHERE id=?1 AND at_ms>=?2",
            params![update.comparison_id, now - 7 * DAY],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let mut case: Case = serde_json::from_str(&data.ok_or("Case expired or unavailable")?)
        .map_err(|e| e.to_string())?;
    if case.review_revision != update.revision {
        return Err("Review changed elsewhere; reload".into());
    }
    if case.projection.is_none() {
        return Err("Frozen review input is unavailable; cannot judge correctness".into());
    }
    let primary = case
        .primary
        .as_ref()
        .ok_or("Decision evidence is unavailable")?;
    let items = primary["items"].as_array().ok_or("No decision items")?;
    let heads = primary["required_questions"]
        .as_array()
        .ok_or("No question schema")?;
    if update.labels.is_empty() || update.labels.len() > 16 {
        return Err("Select one or more cases to review".into());
    }
    for (id, labels) in &update.labels {
        let item = items
            .iter()
            .find(|v| v["item_id"] == *id)
            .ok_or("Unknown item")?;
        if labels.len() != heads.len() {
            return Err("Review every required question".into());
        }
        for h in heads {
            let h = h.as_str().ok_or("Invalid schema")?;
            let value = labels.get(h).ok_or("Missing answer")?;
            let answer = &item["response"]["answers"][h];
            let valid = if answer["choice"].is_string() {
                value.as_str().is_some_and(|s| {
                    answer["options"]
                        .as_array()
                        .is_some_and(|v| v.iter().any(|v| v == s))
                })
            } else if answer["noul"].is_number() {
                value.is_boolean()
            } else {
                false
            };
            if !valid {
                return Err("Review label is outside the closed question vocabulary".into());
            }
        }
    }
    let old = quality(&case);
    case.reviews.extend(update.labels.clone());
    case.review_revision += 1;
    for r in old {
        add(&tx, &r, -1)?;
    }
    for r in quality(&case) {
        add(&tx, &r, 1)?;
    }
    tx.execute(
        "UPDATE cases SET data=?1 WHERE id=?2",
        params![serde_json::to_string(&case).unwrap(), case.id],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(case.review_revision)
}

pub fn last_comparison(conn: &Connection, operation: Option<&str>) -> Result<Option<i64>, String> {
    conn.query_row(
        "SELECT MAX(value) FROM meta WHERE key LIKE 'last_reference:%' AND (?1 IS NULL OR key=?2)",
        params![
            operation,
            operation.map(|op| format!("last_reference:{op}"))
        ],
        |r| r.get(0),
    )
    .map_err(|e| e.to_string())
}
