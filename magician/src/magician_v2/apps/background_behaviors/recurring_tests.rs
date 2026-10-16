use super::*;

fn observation(id: &str, status: &str, posts: u64) -> AppBehaviorExecutionObservation {
    let terminal = matches!(status, "completed" | "failed" | "cancelled");
    AppBehaviorExecutionObservation {
        task_id: "task_app_same".into(),
        execution_id: id.into(),
        status: status.into(),
        terminal,
        settled: terminal,
        completed_at: terminal.then(|| parse_timestamp("2026-09-11T10:09:00Z").unwrap()),
        partial: false,
        published_records: BTreeMap::from([("post".into(), posts)]),
        error: None,
        deferred_until: None,
    }
}

#[test]
fn recurring_scheduler_waits_for_settlement_and_uses_completion_time_once() {
    let (_root, mut db) =
        crate::magician_v2::apps::registry::tests::background_scheduler_database_fixture();
    let install = AppInstallationId::parse("install_legacy").unwrap();
    let behavior = AppName::parse("daily_digest").unwrap();
    db.execute("INSERT INTO app_behavior_heads (installation_id, behavior_id, installation_generation,
        package_revision_ref, schema_revision, grant_revision, behavior_digest, state, revision, fence,
        effective_interval_seconds, next_due_at, available_at, updated_at, period_started_at,
        period_seconds, max_starts_per_period, accepted_count, attempt_count, period_starts, consecutive_failures) VALUES ('install_legacy', 'daily_digest', 1,
        'package:1', 1, 1, ?1, 'idle', 1, 0, 300, '2026-09-11T10:05:00Z', '2026-09-11T10:05:00Z',
        '2026-09-11T10:00:00Z', '2026-09-11T10:00:00Z', 3600, 60, 0, 0, 0, 0)", [AppDigest::blake3(b"test").as_str()]).unwrap();
    let now = parse_timestamp("2026-09-11T10:10:00Z").unwrap();
    for _ in 0..3 {
        assert!(!persist_recurring_observation(
            &mut db,
            &install,
            &behavior,
            300,
            observation("one", "running", 0),
            now
        )
        .unwrap());
    }
    let mut terminal = observation("one", "completed", 16);
    terminal.settled = false;
    assert!(!persist_recurring_observation(
        &mut db,
        &install,
        &behavior,
        300,
        terminal.clone(),
        now
    )
    .unwrap());
    terminal.settled = true;
    for _ in 0..3 {
        assert!(persist_recurring_observation(
            &mut db,
            &install,
            &behavior,
            300,
            terminal.clone(),
            now
        )
        .unwrap());
    }
    let (due, revision): (String, i64) = db
        .query_row(
            "SELECT next_due_at, revision FROM app_behavior_heads",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        parse_timestamp(&due).unwrap(),
        parse_timestamp("2026-09-11T10:14:00Z").unwrap()
    );
    assert_eq!(revision, 2);
    let bytes: Vec<u8> = db
        .query_row(
            "SELECT record_json FROM app_behavior_execution_state",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let retained: AppBehaviorRecurringState = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(retained.completed_count, 1);
    assert_eq!(retained.published_records["post"], 16);

    // The execution result must not hide a separate owner-actionable
    // scheduler block or make that block impossible to retry.
    db.execute("UPDATE app_behavior_heads SET state = 'blocked', revision = revision + 1, last_error = 'workflow_launch_blocked'", []).unwrap();
    let mut failed = observation("two", "failed", 0);
    failed.error = Some("workflow_execution_failed".into());
    persist_recurring_observation(&mut db, &install, &behavior, 300, failed.clone(), now).unwrap();
    assert_eq!(
        db.query_row("SELECT last_error FROM app_behavior_heads", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap(),
        "workflow_launch_blocked"
    );

    // A previously deployed observer overwrote this reason. Owner retry
    // requires a settled failure matching the current occurrence locator.
    db.execute(
        "UPDATE app_behavior_heads SET revision = revision + 1, last_error = 'workflow_execution_failed'",
        [],
    )
    .unwrap();
    let revision: u64 = db
        .query_row("SELECT revision FROM app_behavior_heads", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(retry_blocked_launch_blocking(&mut db, &install, &behavior, 1, revision, now).is_err());
    db.execute("INSERT INTO app_recurring_occurrence_locators (occurrence_id, task_id, execution_id, sealed_blob) VALUES ('occurrence_two', 'task_app_same', 'two', ?1)", [b"fixture".as_slice()]).unwrap();
    db.execute("INSERT INTO app_recurring_task_heads (task_id, occurrence_id) VALUES ('task_app_same', 'occurrence_two')", []).unwrap();
    for (status, settled, needs_observation) in [
        ("running", false, 1),
        ("failed", false, 1),
        ("failed", true, 1),
    ] {
        let mut state = reduce_observation(None, failed.clone());
        state.latest.status = status.into();
        state.latest.settled = settled;
        db.execute(
            "UPDATE app_behavior_execution_state SET record_json = ?1, needs_observation = ?2",
            params![serde_json::to_vec(&state).unwrap(), needs_observation],
        )
        .unwrap();
        assert!(
            retry_blocked_launch_blocking(&mut db, &install, &behavior, 1, revision, now).is_err()
        );
    }
    db.execute(
        "UPDATE app_behavior_execution_state SET record_json = ?1, needs_observation = 0",
        [serde_json::to_vec(&reduce_observation(None, failed)).unwrap()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO app_recurring_occurrence_locators (occurrence_id, task_id, execution_id, sealed_blob) VALUES ('occurrence_newer', 'task_app_same', 'newer', ?1)",
        [b"fixture".as_slice()],
    )
    .unwrap();
    db.execute(
        "UPDATE app_recurring_task_heads SET occurrence_id = 'occurrence_newer'",
        [],
    )
    .unwrap();
    assert!(retry_blocked_launch_blocking(&mut db, &install, &behavior, 1, revision, now).is_err());
    db.execute(
        "UPDATE app_recurring_task_heads SET occurrence_id = 'occurrence_two'",
        [],
    )
    .unwrap();
    assert_eq!(
        retry_blocked_launch_blocking(&mut db, &install, &behavior, 1, revision, now).unwrap(),
        revision + 1
    );
}

#[test]
fn recurring_health_counts_execution_outcomes_and_committed_records_once() {
    let active = reduce_observation(None, observation("first", "running", 0));
    assert_eq!(active.completed_count, 0);
    assert_eq!(active.published_records.get("post"), None);
    let completed = observation("first", "completed", 16);
    let once = reduce_observation(Some(active), completed.clone());
    let replay = reduce_observation(Some(once.clone()), completed);
    assert_eq!(once, replay);
    assert_eq!(replay.completed_count, 1);
    assert_eq!(replay.published_records["post"], 16);
    let next = reduce_observation(Some(replay), observation("second", "running", 0));
    assert_eq!(next.completed_count, 1);
    let mut partial = observation("second", "failed", 3);
    partial.partial = true;
    partial.settled = false;
    let failed = reduce_observation(Some(next), partial.clone());
    assert_eq!(failed.failed_count, 0);
    assert_eq!(failed.published_records["post"], 16);
    partial.settled = true;
    // Recovery may discover another durable commit before settlement.
    partial.published_records.insert("post".into(), 4);
    let settled = reduce_observation(Some(failed), partial);
    assert_eq!(settled.failed_count, 1);
    assert_eq!(settled.published_records["post"], 20);
}

#[test]
fn a_period_ceiling_failure_defers_the_next_fire_to_the_period_end() {
    // The launch was accepted, the run failed because every model attempt
    // was refused on the installation's monthly ceiling, and the head was
    // rescheduled at completion + interval: 112 launches and 2,993 refusals
    // in one day on the measured store. A run that died on a period ceiling
    // parks the behavior until that period ends.
    let (_root, mut db) =
        crate::magician_v2::apps::registry::tests::background_scheduler_database_fixture();
    let install = AppInstallationId::parse("install_legacy").unwrap();
    let behavior = AppName::parse("ambient_turn").unwrap();
    db.execute("INSERT INTO app_behavior_heads (installation_id, behavior_id, installation_generation,
        package_revision_ref, schema_revision, grant_revision, behavior_digest, state, revision, fence,
        effective_interval_seconds, next_due_at, available_at, updated_at, period_started_at,
        period_seconds, max_starts_per_period, accepted_count, attempt_count, period_starts, consecutive_failures) VALUES ('install_legacy',
        'ambient_turn', 1, 'package:1', 1, 1, ?1, 'idle', 1, 0, 540, '2026-09-17T19:00:00Z', '2026-09-17T19:00:00Z',
        '2026-09-17T18:51:00Z', '2026-09-17T18:00:00Z', 3600, 60, 1, 1, 1, 0)", [AppDigest::blake3(b"test").as_str()]).unwrap();
    let now = parse_timestamp("2026-09-17T18:57:00Z").unwrap();
    // The round "completes" without its participants: status is completed,
    // nothing was published, and the run carries the ceiling marker.
    let mut refused = observation("round-1", "completed", 0);
    refused.completed_at = Some(parse_timestamp("2026-09-17T18:56:45Z").unwrap());
    refused.error = Some("period_resource_ceiling:monthly_tokens".to_owned());
    refused.deferred_until = Some(parse_timestamp("2026-10-01T00:00:00Z").unwrap());
    assert!(
        persist_recurring_observation(&mut db, &install, &behavior, 540, refused, now).unwrap()
    );
    let (next_due_at, last_error): (String, Option<String>) = db
        .query_row(
            "SELECT next_due_at, last_error FROM app_behavior_heads WHERE installation_id = 'install_legacy' AND behavior_id = 'ambient_turn'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        next_due_at, "2026-10-01T00:00:00.000000Z",
        "the next fire waits for the period to roll"
    );
    assert_eq!(
        last_error.as_deref(),
        Some("period_resource_ceiling:monthly_tokens")
    );

    // A later run that completes normally returns the head to its interval.
    let mut completed = observation("round-2", "completed", 3);
    completed.completed_at = Some(parse_timestamp("2026-10-01T00:10:00Z").unwrap());
    let later = parse_timestamp("2026-10-01T00:11:00Z").unwrap();
    assert!(
        persist_recurring_observation(&mut db, &install, &behavior, 540, completed, later).unwrap()
    );
    let next_due_at: String = db
        .query_row(
            "SELECT next_due_at FROM app_behavior_heads WHERE installation_id = 'install_legacy' AND behavior_id = 'ambient_turn'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(next_due_at, "2026-10-01T00:19:00.000000Z");
}
