#[cfg(test)]
#[path = "../../../magician/src/magician_v2/apps/indexed_snapshot.rs"]
mod indexed_snapshot;

#[cfg(test)]
mod tests {
    use super::indexed_snapshot::{migrate_keyset, order_key, read_keyset};
    use super::indexed_snapshot::{read, Filter, Order};
    use rusqlite::{params, types::Value, Connection};

    fn database() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        // Deliberately no payload/revision table: this production planner must
        // obtain no message bodies, even when planning thousands of records.
        connection.execute_batch("CREATE TABLE app_record_heads (
          installation_id TEXT, entity_name TEXT, record_id TEXT, record_revision INTEGER, deleted_at TEXT,
          PRIMARY KEY(installation_id, entity_name, record_id));
          CREATE TABLE app_scalar_indexes (installation_id TEXT, entity_name TEXT, record_id TEXT,
          record_revision INTEGER, field_path TEXT, value_kind TEXT, text_value TEXT, integer_value INTEGER,
          PRIMARY KEY(installation_id, entity_name, field_path, record_id));
          CREATE INDEX app_scalar_indexes_text_idx ON app_scalar_indexes(installation_id,entity_name,field_path,text_value,record_id);
          CREATE INDEX app_scalar_indexes_integer_idx ON app_scalar_indexes(installation_id,entity_name,field_path,integer_value,record_id);").unwrap();
        connection
    }
    fn record(c: &Connection, id: &str, kind: &str, text: Option<&str>, integer: Option<i64>) {
        c.execute(
            "INSERT INTO app_record_heads VALUES ('app', 'message', ?1, 1, NULL)",
            [id],
        )
        .unwrap();
        c.execute(
            "INSERT INTO app_scalar_indexes VALUES ('app','message',?1,1,'time',?2,?3,?4)",
            params![id, kind, text, integer],
        )
        .unwrap();
        c.execute(
            "INSERT INTO app_scalar_indexes VALUES ('app','message',?1,1,'id','text',?1,NULL)",
            [id],
        )
        .unwrap();
    }
    fn order() -> Vec<Order> {
        vec![Order {
            field: "time".into(),
            descending: true,
        }]
    }

    fn cursor(c: &Connection, installation: &str, chain: &str, id: &str, expiry: &str, bytes: usize) {
        c.execute("INSERT INTO app_keyset_cursors VALUES (?1,?2,1,'package',1,1,?3,?4,?5,?6)",
            params![id, installation, vec![b'e'; bytes - 2], b"{}".as_slice(), chain, expiry]).unwrap();
    }

    fn cursor_ids(c: &Connection, installation: &str) -> Vec<String> {
        c.prepare("SELECT cursor_ref FROM app_keyset_cursors WHERE installation_id = ?1 ORDER BY cursor_ref")
            .unwrap().query_map([installation], |row| row.get(0)).unwrap()
            .collect::<rusqlite::Result<_>>().unwrap()
    }

    #[test]
    fn keyset_cursor_cache_reclaims_abandoned_roots_without_touching_records_or_other_installations() {
        use super::indexed_snapshot::reserve_keyset_cursor_capacity;
        let mut c = database();
        record(&c, "kept-post", "integer", None, Some(1));
        migrate_keyset(&c).unwrap();
        cursor(&c, "app", "protected", "current", "01", 10);
        cursor(&c, "app", "continued", "old-parent", "01", 10);
        cursor(&c, "app", "continued", "old-child", "02", 10);
        for i in 0..253 {
            cursor(&c, "app", &format!("root-{i:03}"), &format!("unused-{i:03}"), "09", 10);
        }
        cursor(&c, "other-app", "root-000", "other", "00", 10);
        let tx = c.transaction().unwrap();
        assert!(reserve_keyset_cursor_capacity(&tx, "app", "protected", 10, 256, 10_000).unwrap());
        cursor(&tx, "app", "protected", "next", "10", 10);
        assert_eq!(cursor_ids(&tx, "app").len(), 256);
        assert!(!cursor_ids(&tx, "app").contains(&"unused-000".to_owned()));
        for id in ["current", "old-parent", "old-child", "next"] {
            assert!(cursor_ids(&tx, "app").contains(&id.to_owned()));
        }
        assert_eq!(cursor_ids(&tx, "other-app"), ["other"]);
        assert_eq!(tx.query_row("SELECT record_id FROM app_record_heads", [], |row| row.get::<_, String>(0)).unwrap(), "kept-post");
        tx.commit().unwrap();
    }

    #[test]
    fn keyset_cursor_cache_evicts_whole_least_recently_advanced_chain_and_rolls_back_with_insert() {
        use super::indexed_snapshot::reserve_keyset_cursor_capacity;
        let mut c = database();
        migrate_keyset(&c).unwrap();
        cursor(&c, "app", "protected", "current", "00", 10);
        cursor(&c, "app", "recent", "recent-parent", "01", 10);
        cursor(&c, "app", "recent", "recent-child", "09", 10);
        cursor(&c, "app", "stale", "stale-parent", "02", 10);
        cursor(&c, "app", "stale", "stale-child", "03", 10);
        let tx = c.transaction().unwrap();
        assert!(reserve_keyset_cursor_capacity(&tx, "app", "protected", 10, 5, 1000).unwrap());
        assert_eq!(cursor_ids(&tx, "app"), ["current", "recent-child", "recent-parent"]);
        tx.rollback().unwrap();
        assert_eq!(cursor_ids(&c, "app").len(), 5);
    }

    #[test]
    fn keyset_cursor_cache_enforces_byte_budget_without_pointless_eviction_when_parent_cannot_fit() {
        use super::indexed_snapshot::reserve_keyset_cursor_capacity;
        let mut c = database();
        migrate_keyset(&c).unwrap();
        cursor(&c, "app", "protected", "current", "01", 40);
        cursor(&c, "app", "root-a", "a", "02", 30);
        cursor(&c, "app", "root-b", "b", "03", 30);
        let tx = c.transaction().unwrap();
        assert!(!reserve_keyset_cursor_capacity(&tx, "app", "protected", 91, 256, 90).unwrap());
        assert!(!reserve_keyset_cursor_capacity(&tx, "app", "protected", 51, 256, 90).unwrap());
        assert!(!reserve_keyset_cursor_capacity(&tx, "app", "protected", 10, 0, 90).unwrap());
        assert_eq!(cursor_ids(&tx, "app"), ["a", "b", "current"]);
        assert!(reserve_keyset_cursor_capacity(&tx, "app", "protected", 40, 256, 90).unwrap());
        assert_eq!(cursor_ids(&tx, "app"), ["current"]);
    }

    #[test]
    fn keyset_traverses_100001_rows_in_25_row_pages_with_bounded_vm_work_even_at_the_end() {
        let c = database();
        c.execute_batch("BEGIN").unwrap();
        for i in 0..100_001 {
            record(&c, &format!("{i:06}"), "integer", None, Some(i / 3));
            c.execute("INSERT INTO app_scalar_indexes VALUES ('app','message',?1,1,'surface','text','feed',NULL)", [format!("{i:06}")]).unwrap();
        }
        migrate_keyset(&c).unwrap();
        c.execute_batch("COMMIT").unwrap();
        // A scan/sort of the corpus or even its ID list cannot fit this budget.
        // SQLite aborts any page executing 20,000 VM instructions.
        use std::sync::{
            atomic::{AtomicUsize, Ordering as AtomicOrdering},
            Arc,
        };
        let steps = Arc::new(AtomicUsize::new(0));
        let counter = steps.clone();
        c.progress_handler(
            1,
            Some(move || counter.fetch_add(1, AtomicOrdering::Relaxed) >= 20_000),
        );
        let old_id = Filter::Values("id".into(), vec![("text", Value::Text("000000".into()))]);
        assert_eq!(old_id.parameter_count(), 3);
        let old_id = Filter::All(vec![
            Filter::Values("surface".into(), vec![("text", Value::Text("feed".into()))]),
            old_id,
        ]);
        let feed = Filter::Values("surface".into(), vec![("text", Value::Text("feed".into()))]);
        let lookup = read_keyset(
            &c,
            "app",
            "message",
            Some(&old_id),
            order().first(),
            true,
            None,
            25,
            4096,
        )
        .unwrap();
        assert_eq!(lookup.len(), 1);
        assert_eq!(lookup[0].boundary.record_id, "000000");
        let lookup_steps = steps.load(AtomicOrdering::Relaxed);
        let mut max_page_steps = 0;
        let mut after = None;
        let mut all = Vec::new();
        let started = std::time::Instant::now();
        loop {
            steps.store(0, AtomicOrdering::Relaxed);
            let mut page = read_keyset(
                &c,
                "app",
                "message",
                Some(&feed),
                order().first(),
                true,
                after.as_ref(),
                25,
                4096,
            )
            .unwrap();
            max_page_steps = max_page_steps.max(steps.load(AtomicOrdering::Relaxed));
            assert!(page.len() <= 26);
            let more = page.len() > 25;
            page.truncate(25);
            after = page.last().map(|row| row.boundary.clone());
            all.extend(page.into_iter().map(|row| row.boundary.record_id));
            if !more {
                break;
            }
        }
        let expected = (0..100_001).rev().collect::<Vec<_>>();
        let mut expected = expected
            .into_iter()
            .map(|i| (i / 3, format!("{i:06}")))
            .collect::<Vec<_>>();
        expected.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        assert_eq!(
            all,
            expected.into_iter().map(|(_, id)| id).collect::<Vec<_>>()
        );
        eprintln!(
            "100001 records / 4001 keyset pages: {:?}; max aggregate VM steps/page: {}; oldest-ID lookup: {} steps",
            started.elapsed(), max_page_steps, lookup_steps
        );
        steps.store(0, AtomicOrdering::Relaxed);
        assert!(read_keyset(
            &c,
            "another-app",
            "message",
            None,
            order().first(),
            true,
            None,
            25,
            4096
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn keyset_cursor_survives_deleted_anchor_and_newer_inserts_without_restarting_history() {
        let c = database();
        for i in 1..=100 {
            record(&c, &format!("{i:03}"), "integer", None, Some(i));
        }
        migrate_keyset(&c).unwrap();
        let first = read_keyset(
            &c,
            "app",
            "message",
            None,
            order().first(),
            true,
            None,
            25,
            4096,
        )
        .unwrap();
        let after = first[24].boundary.clone();
        assert_eq!(after.record_id, "076");
        c.execute(
            "UPDATE app_record_heads SET deleted_at='deleted' WHERE record_id IN ('076','075')",
            [],
        )
        .unwrap();
        record_after_migration(&c, "101", "integer", None, Some(101));
        let page = read_keyset(
            &c,
            "app",
            "message",
            None,
            order().first(),
            true,
            Some(&after),
            25,
            4096,
        )
        .unwrap();
        assert_eq!(page[0].boundary.record_id, "074");
        assert_eq!(page[24].boundary.record_id, "050");
        assert_eq!(page.len(), 26);
        assert_eq!(
            read_keyset(
                &c,
                "app",
                "message",
                None,
                order().first(),
                true,
                None,
                25,
                4096
            )
            .unwrap()[0]
                .boundary
                .record_id,
            "101"
        );
    }

    fn record_after_migration(
        c: &Connection,
        id: &str,
        kind: &str,
        text: Option<&str>,
        integer: Option<i64>,
    ) {
        c.execute(
            "INSERT INTO app_record_heads VALUES ('app','message',?1,1,NULL)",
            [id],
        )
        .unwrap();
        c.execute("INSERT INTO app_scalar_indexes(installation_id,entity_name,record_id,record_revision,field_path,value_kind,text_value,integer_value,order_key_asc,order_key_desc)
            VALUES ('app','message',?1,1,'time',?2,?3,?4,?5,?6)", params![id,kind,text,integer,order_key(kind,text,integer,false).unwrap(),order_key(kind,text,integer,true).unwrap()]).unwrap();
    }

    #[test]
    fn keyset_typed_keys_match_snapshot_order_in_both_directions_including_null_and_missing() {
        for values in [
            vec![
                ("integer", None, Some(-1)),
                ("integer", Some("18446744073709551615"), None),
                ("integer", None, Some(0)),
            ],
            vec![
                ("decimal", Some("-79228162514264337593543950335"), None),
                ("decimal", Some("-0.0000000000000000000000000001"), None),
                ("decimal", Some("0"), None),
                ("decimal", Some("1.0000000000000000000000000001"), None),
                ("decimal", Some("1.00"), None),
                ("decimal", Some("79228162514264337593543950335"), None),
            ],
            vec![
                ("timestamp", Some("2026-09-11T10:00:00.000000001Z"), None),
                ("timestamp", Some("2026-09-11T15:30:00+05:30"), None),
                ("timestamp", Some("2026-09-11T10:00:00Z"), None),
            ],
            vec![
                ("text", Some("a"), None),
                ("text", Some("ab"), None),
                ("text", Some("a\0"), None),
                ("text", Some("a\u{1}"), None),
                ("text", Some("a\u{2}"), None),
                ("text", Some(""), None),
            ],
        ] {
            let c = database();
            for (i, (kind, text, integer)) in values.into_iter().enumerate() {
                record(&c, &format!("{i:03}"), kind, text, integer);
            }
            record(&c, "null", "null", None, None);
            c.execute(
                "INSERT INTO app_record_heads VALUES ('app','message','missing',1,NULL)",
                [],
            )
            .unwrap();
            migrate_keyset(&c).unwrap();
            for descending in [false, true] {
                let order = [Order {
                    field: "time".into(),
                    descending,
                }];
                let expected = read(&c, "app", "message", None, &order, 100, 4096).unwrap();
                let mut actual = Vec::new();
                let mut after = None;
                loop {
                    let rows = read_keyset(
                        &c,
                        "app",
                        "message",
                        None,
                        order.first(),
                        false,
                        after.as_ref(),
                        1,
                        4096,
                    )
                    .unwrap();
                    let more = rows.len() > 1;
                    if let Some(row) = rows.first() {
                        after = Some(row.boundary.clone());
                        actual.push((row.boundary.record_id.clone(), row.revision));
                    }
                    if !more {
                        break;
                    }
                }
                assert_eq!(actual, expected);
            }
        }
    }

    #[test]
    fn keyset_filters_and_default_record_id_order_are_scoped_and_exclude_stale_indexes() {
        assert_eq!(
            order_key("text", Some("an ordinary post"), None, false)
                .unwrap()
                .len(),
            "an ordinary post".len() + 2
        );
        let c = database();
        for i in 0..20 {
            record(&c, &format!("{i:03}"), "integer", None, Some(i));
        }
        migrate_keyset(&c).unwrap();
        c.execute(
            "UPDATE app_scalar_indexes SET record_revision=2 WHERE record_id='005'",
            [],
        )
        .unwrap();
        let filter = Filter::Values(
            "id".into(),
            vec![
                ("text", Value::Text("005".into())),
                ("text", Value::Text("006".into())),
                ("text", Value::Text("009".into())),
            ],
        );
        let first = read_keyset(
            &c,
            "app",
            "message",
            Some(&filter),
            None,
            false,
            None,
            1,
            4096,
        )
        .unwrap();
        assert_eq!(first[0].boundary.record_id, "006");
        let second = read_keyset(
            &c,
            "app",
            "message",
            Some(&filter),
            None,
            false,
            Some(&first[0].boundary),
            1,
            4096,
        )
        .unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].boundary.record_id, "009");
    }

    #[test]
    fn ten_thousand_rows_use_only_index_metadata_and_identity_lookups_are_exact() {
        let c = database();
        c.execute_batch("BEGIN").unwrap();
        for i in 0..10_000 {
            record(&c, &format!("{i:05}"), "integer", None, Some(i));
        }
        c.execute_batch("COMMIT").unwrap();
        let snapshot = read(
            &c,
            "app",
            "message",
            None,
            &order(),
            10_000,
            64 * 1024 * 1024,
        )
        .unwrap();
        assert_eq!(snapshot.len(), 10_000);
        assert_eq!(snapshot[0].0, "09999");
        let filter = Filter::All(vec![Filter::Values(
            "id".into(),
            vec![
                ("text", Value::Text("00001".into())),
                ("text", Value::Text("09999".into())),
            ],
        )]);
        assert_eq!(
            read(
                &c,
                "app",
                "message",
                Some(&filter),
                &order(),
                10_000,
                64 * 1024 * 1024
            )
            .unwrap(),
            vec![("09999".into(), 1), ("00001".into(), 1)]
        );
        assert!(read(
            &c,
            "other",
            "message",
            None,
            &order(),
            10_000,
            64 * 1024 * 1024
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn timestamp_sort_handles_offsets_and_fractional_precision_and_stable_ties() {
        let c = database();
        record(
            &c,
            "a",
            "timestamp",
            Some("2026-09-11T10:00:00.000000001+00:00"),
            None,
        );
        record(
            &c,
            "b",
            "timestamp",
            Some("2026-09-11T15:30:00+05:30"),
            None,
        );
        record(&c, "c", "timestamp", Some("2026-09-11T10:00:00Z"), None);
        assert_eq!(
            read(&c, "app", "message", None, &order(), 10, 64 * 1024 * 1024)
                .unwrap()
                .iter()
                .map(|r| r.0.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn integers_and_decimals_keep_exact_order_without_float_conversion() {
        let c = database();
        record(&c, "a", "integer", Some("18446744073709551615"), None);
        record(&c, "b", "integer", None, Some(-1));
        assert_eq!(
            read(&c, "app", "message", None, &order(), 10, 64 * 1024 * 1024).unwrap()[0].0,
            "a"
        );
        let c = database();
        record(
            &c,
            "a",
            "decimal",
            Some("0.1000000000000000000000000001"),
            None,
        );
        record(&c, "b", "decimal", Some("0.1"), None);
        assert_eq!(
            read(&c, "app", "message", None, &order(), 10, 64 * 1024 * 1024).unwrap()[0].0,
            "a"
        );
    }

    #[test]
    fn null_and_missing_remain_last_in_both_directions_and_old_indexes_do_not_match() {
        let c = database();
        record(&c, "a", "integer", None, Some(1));
        record(&c, "b", "null", None, None);
        record(&c, "c", "null", None, None);
        c.execute(
            "DELETE FROM app_scalar_indexes WHERE record_id='c' AND field_path='time'",
            [],
        )
        .unwrap();
        for descending in [false, true] {
            assert_eq!(
                read(
                    &c,
                    "app",
                    "message",
                    None,
                    &[Order {
                        field: "time".into(),
                        descending
                    }],
                    10,
                    64 * 1024 * 1024
                )
                .unwrap()
                .iter()
                .map(|r| r.0.as_str())
                .collect::<Vec<_>>(),
                vec!["a", "b", "c"]
            );
        }
        c.execute(
            "UPDATE app_record_heads SET record_revision=2 WHERE record_id='a'",
            [],
        )
        .unwrap();
        let filter = Filter::Any(vec![Filter::Values(
            "id".into(),
            vec![("text", Value::Text("a".into()))],
        )]);
        assert!(read(
            &c,
            "app",
            "message",
            Some(&filter),
            &[],
            10,
            64 * 1024 * 1024
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn large_sort_keys_obey_the_scan_byte_budget_without_reading_payloads() {
        let c = database();
        record(&c, "a", "text", Some(&"x".repeat(4096)), None);
        let result = read(&c, "app", "message", None, &order(), 10, 1024);
        assert!(
            matches!(result, Err(rusqlite::Error::SqliteFailure(code, _))
            if code.extended_code == rusqlite::ffi::SQLITE_TOOBIG)
        );
    }

    #[test]
    fn deletion_and_capacity_sentinel_are_preserved() {
        let c = database();
        for i in 0..4 {
            record(&c, &i.to_string(), "integer", None, Some(i));
        }
        assert_eq!(
            read(&c, "app", "message", None, &[], 2, 64 * 1024 * 1024)
                .unwrap()
                .len(),
            3
        );
        c.execute(
            "UPDATE app_record_heads SET deleted_at='now' WHERE record_id='0'",
            [],
        )
        .unwrap();
        assert!(!read(&c, "app", "message", None, &[], 10, 64 * 1024 * 1024)
            .unwrap()
            .iter()
            .any(|r| r.0 == "0"));
    }
}
