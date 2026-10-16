use std::process::Command;

use magician_storage_gate1::{run_matrix, Gate1Store, PostgresStore, SqliteStore};

#[tokio::test]
async fn sqlite_section_14_matrix_passes() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::file(dir.path().join("gate1.sqlite")).unwrap();
    let reports = run_matrix(&store).await;
    for report in &reports {
        assert!(report.passed, "{}: {}", report.name, report.detail);
    }
    assert_eq!(reports.len(), 10);
}

#[tokio::test]
async fn postgres_section_14_matrix_when_configured() {
    let Ok(url) = std::env::var("MAGICIAN_GATE1_POSTGRES_URL") else {
        return;
    };
    let store = PostgresStore::connect(&url).await.expect("postgres");
    let reports = run_matrix(&store).await;
    for report in &reports {
        if report.name == "backup_restore" {
            assert!(
                !report.passed,
                "postgres file-copy backup must stay unsupported"
            );
            continue;
        }
        assert!(report.passed, "{}: {}", report.name, report.detail);
    }
}

#[tokio::test]
async fn two_process_sqlite_writer_is_busy() {
    if let Ok(path) = std::env::var("MAGICIAN_GATE1_CHILD_DB") {
        let store = SqliteStore::file(path).unwrap();
        let err = store.begin_immediate().unwrap_err();
        let text = err.safe_diagnostic().to_lowercase();
        assert!(text.contains("busy") || text.contains("locked"), "{text}");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("busy.sqlite");
    let store = SqliteStore::file(&path).unwrap();
    store.migrate().await.unwrap();
    let output = store
        .with_immediate_write(|| {
            Command::new(std::env::current_exe().unwrap())
                .env("MAGICIAN_GATE1_CHILD_DB", &path)
                .args(["two_process_sqlite_writer_is_busy", "--exact"])
                .output()
                .unwrap()
        })
        .unwrap();
    assert!(
        output.status.success(),
        "stderr={} stdout={}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}
