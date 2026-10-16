//! Bounded, read-only access to the signed desktop owner's Messages database.
//! The request never selects a filesystem path. No database files are mounted
//! into Linux; only the requested query result crosses the private relay.
//!
//! A native macOS backend reaches the same handler over the loopback gateway,
//! because Full Disk Access is judged on the process macOS holds responsible —
//! launchd's `magic-supervisor.bin` or a developer's terminal — never on
//! `magician.bin` itself, so a grant there does nothing. The signed desktop is
//! its own responsible process and holds the one grant that applies. That route is not open to every local
//! caller: it requires the per-launch token this module writes to a 0600 file
//! under the runtime root.
use rusqlite::{
    hooks::{AuthAction, AuthContext, Authorization},
    limits::Limit,
    types::ValueRef,
    Connection, OpenFlags,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::LazyLock,
    time::{Duration, Instant},
};

static QUERY_SLOTS: LazyLock<tokio::sync::Semaphore> =
    LazyLock::new(|| tokio::sync::Semaphore::new(2));
const MAX_RESULT: usize = 2 * 1024 * 1024;

/// Header a native backend presents on the loopback gateway route.
pub const LOCAL_TOKEN_HEADER: &str = "x-magician-imessage-token";
/// Relative to the runtime root; the backend reads the same path.
const LOCAL_TOKEN_FILE: &str = "run/host-imessage.token";
static LOCAL_TOKEN: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
});

/// Write this launch's token where the native backend can read it. A stale
/// file from an earlier launch is replaced, so an old token stops working.
/// Only macOS serves the route, so only a Unix build writes the file.
#[cfg(unix)]
pub fn publish_local_token() -> Result<(), String> {
    publish_local_token_at(&crate::runtime_paths::runtime_root_dir().join(LOCAL_TOKEN_FILE))
}

#[cfg(not(unix))]
pub fn publish_local_token() -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn publish_local_token_at(path: &Path) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let dir = path.parent().ok_or("Messages token path has no parent")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let staging = path.with_extension("token.tmp");
    let _ = std::fs::remove_file(&staging);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&staging)
        .map_err(|e| format!("cannot write {}: {e}", staging.display()))?;
    file.write_all(LOCAL_TOKEN.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", staging.display()))?;
    std::fs::rename(&staging, path).map_err(|e| format!("cannot publish {}: {e}", path.display()))
}

/// Whether this desktop can open the Messages database: `None` when there is
/// no database to try. Answers only about the grant, never with content, so
/// the setup wizard may ask without the token.
pub fn database_readable() -> Option<bool> {
    let path = dirs::home_dir()?.join("Library/Messages/chat.db");
    match std::fs::File::open(path) {
        Ok(_) => Some(true),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Some(false),
        Err(_) => None,
    }
}

/// Constant-time comparison against this launch's token.
pub fn local_token_matches(presented: Option<&str>) -> bool {
    let Some(presented) = presented else {
        return false;
    };
    let expected = LOCAL_TOKEN.as_bytes();
    let presented = presented.as_bytes();
    presented.len() == expected.len()
        && presented
            .iter()
            .zip(expected)
            .fold(0_u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    sql: String,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
}
fn default_timeout() -> u64 {
    10
}

pub async fn handle(body: &[u8]) -> Result<Value, String> {
    if !cfg!(target_os = "macos") {
        return Err("iMessage is available only on a macOS desktop host".into());
    }
    let query: Query = serde_json::from_slice(body).map_err(|_| "Invalid Messages query")?;
    if query.sql.len() > 32 * 1024 || query.sql.trim().is_empty() {
        return Err("Messages SQL must contain 1..32768 bytes".into());
    }
    let permit = QUERY_SLOTS
        .try_acquire()
        .map_err(|_| "Messages query capacity exhausted")?;
    let path = dirs::home_dir()
        .ok_or("Host home directory unavailable")?
        .join("Library/Messages/chat.db");
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        query_path(
            &path,
            &query.sql,
            Duration::from_secs(query.timeout_secs.clamp(1, 30)),
        )
    })
    .await
    .map_err(|_| "Messages query worker failed")?;
    result
}

fn authorize(context: AuthContext<'_>) -> Authorization {
    match context.action {
        AuthAction::Select | AuthAction::Recursive => Authorization::Allow,
        // SQLite's count(*) optimization can report the table-level read
        // without a database name. ATTACH and temp schema mutations remain
        // denied, so the only readable database is our fixed read-only file.
        AuthAction::Read { .. } if matches!(context.database_name, None | Some("main")) => {
            Authorization::Allow
        },
        AuthAction::Function { function_name }
            if !matches!(
                function_name.to_ascii_lowercase().as_str(),
                "load_extension" | "readfile" | "writefile"
            ) =>
        {
            Authorization::Allow
        },
        AuthAction::Pragma { pragma_name, .. }
            if matches!(
                pragma_name.to_ascii_lowercase().as_str(),
                "table_info" | "table_xinfo" | "index_info" | "index_list" | "foreign_key_list"
            ) =>
        {
            Authorization::Allow
        },
        _ => Authorization::Deny,
    }
}

fn query_path(path: &Path, sql: &str, duration: Duration) -> Result<Value, String> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|_| "Cannot read the host Messages database; grant Full Disk Access to Magican and ensure Messages is configured")?;
    query_connection(&db, sql, duration)
}

fn query_connection(db: &Connection, sql: &str, duration: Duration) -> Result<Value, String> {
    db.busy_timeout(duration)
        .map_err(|_| "Cannot set Messages query timeout")?;
    db.execute_batch("PRAGMA query_only=ON; PRAGMA temp_store=MEMORY;")
        .map_err(|_| "Cannot enable read-only Messages queries")?;
    for (limit, value) in [
        (Limit::SQLITE_LIMIT_LENGTH, 256 * 1024),
        (Limit::SQLITE_LIMIT_SQL_LENGTH, 32 * 1024),
        (Limit::SQLITE_LIMIT_COLUMN, 128),
        (Limit::SQLITE_LIMIT_ATTACHED, 0),
        (Limit::SQLITE_LIMIT_WORKER_THREADS, 0),
        (Limit::SQLITE_LIMIT_COMPOUND_SELECT, 32),
    ] {
        db.set_limit(limit, value);
    }
    let deadline = Instant::now() + duration;
    db.progress_handler(1000, Some(move || Instant::now() >= deadline));
    db.authorizer(Some(authorize));
    let mut statement = db
        .prepare(sql)
        .map_err(|e| format!("Messages query rejected: {e}"))?;
    if !statement.readonly() || statement.column_count() == 0 {
        return Err("Messages queries must return read-only rows".into());
    }
    let columns: Vec<String> = statement
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect();
    let mut rows = statement
        .query([])
        .map_err(|e| format!("Messages query failed: {e}"))?;
    let mut result = Vec::new();
    let mut size = 0;
    while let Some(row) = rows
        .next()
        .map_err(|e| format!("Messages query stopped: {e}"))?
    {
        if result.len() >= 1000 || Instant::now() >= deadline {
            return Err(
                "Messages query exceeded 1000 rows or its deadline; narrow the query".into(),
            );
        }
        let mut values = Vec::new();
        for index in 0..columns.len() {
            let value = match row
                .get_ref(index)
                .map_err(|_| "Cannot read Messages query value")?
            {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(n) => json!(n),
                ValueRef::Real(n) => json!(n),
                ValueRef::Text(text) => json!(String::from_utf8_lossy(text)),
                ValueRef::Blob(bytes) => json!(format!(
                    "0x{}",
                    bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
                )),
            };
            size += serde_json::to_vec(&value)
                .map_err(|_| "Cannot encode Messages value")?
                .len();
            if size > MAX_RESULT {
                return Err("Messages result exceeds 2 MiB; narrow the query".into());
            }
            values.push(value);
        }
        result.push(values);
    }
    Ok(json!({"columns": columns, "rows": result}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn database() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE message(text TEXT); INSERT INTO message VALUES ('fixture');",
        )
        .unwrap();
        db
    }
    #[test]
    fn container_routing_imessage_reads_fixture_and_rejects_writes_and_other_files() {
        assert_eq!(
            query_connection(
                &database(),
                "SELECT text FROM message",
                Duration::from_secs(1)
            )
            .unwrap(),
            json!({"columns":["text"],"rows":[["fixture"]]})
        );
        assert_eq!(
            query_connection(
                &database(),
                "SELECT count(*) AS count FROM message",
                Duration::from_secs(1)
            )
            .unwrap(),
            json!({"columns":["count"],"rows":[[1]]})
        );
        for sql in [
            "DELETE FROM message RETURNING text",
            "ATTACH ':memory:' AS extra",
            "PRAGMA query_only=OFF",
            "SELECT load_extension('/tmp/library')",
            "SELECT readfile('/etc/passwd')",
        ] {
            assert!(
                query_connection(&database(), sql, Duration::from_secs(1)).is_err(),
                "{sql}"
            );
        }
    }
    #[cfg(unix)]
    #[test]
    fn local_token_is_published_owner_only_and_required() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run/host-imessage.token");
        std::fs::write(&path.with_extension("token.tmp"), "stale").ok();
        publish_local_token_at(&path).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(local_token_matches(Some(&written)));
        assert!(!local_token_matches(None));
        assert!(!local_token_matches(Some("")));
        assert!(!local_token_matches(Some(&written[1..])));
        let mut flipped = written.clone().into_bytes();
        flipped[0] = if flipped[0] == b'a' { b'b' } else { b'a' };
        assert!(!local_token_matches(Some(
            &String::from_utf8(flipped).unwrap()
        )));
    }
    #[test]
    fn container_routing_imessage_enforces_row_size_and_execution_limits() {
        for sql in ["SELECT zeroblob(1048576)",
            "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1002) SELECT x FROM n"] {
            assert!(query_connection(&database(), sql, Duration::from_secs(1)).is_err());
        }
        assert!(query_connection(
            &database(),
            "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n) SELECT sum(x) FROM n",
            Duration::from_millis(5)
        )
        .is_err());
    }
}
