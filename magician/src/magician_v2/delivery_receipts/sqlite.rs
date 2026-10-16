//! SQLite sent-index adapter for remote qualification. Not default startup.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};

use super::sent_index::{identifier_from_parts, SendBinding, SentIndexRecord, SentMessage};
use super::store::SentMessageStore;
use super::{caller_field, SendIdentifier};
use crate::magician_v2::delivery::DeliveryScope;

pub struct SqliteSentMessageStore {
    conn: Mutex<Connection>,
}

impl SqliteSentMessageStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS sent_messages (
                principal TEXT NOT NULL,
                workspace TEXT NOT NULL,
                provider TEXT NOT NULL,
                id_kind TEXT NOT NULL,
                id_value TEXT NOT NULL,
                act_ref TEXT NOT NULL,
                audience_json TEXT NOT NULL,
                sent_at TEXT NOT NULL,
                recorded_at TEXT NOT NULL,
                PRIMARY KEY (
                    principal, workspace, provider, id_kind, id_value,
                    act_ref, audience_json, sent_at
                )
             );",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

impl SentMessageStore for SqliteSentMessageStore {
    fn record(
        &self,
        scope: &DeliveryScope,
        provider: &str,
        identifier: &SendIdentifier,
        sent: &SentMessage,
        now: DateTime<Utc>,
    ) -> Result<()> {
        super::sent_index::validate_scope(scope)?;
        let provider = caller_field(provider, "a provider name")?;
        let act_ref = caller_field(&sent.act_ref, "an act ref")?;
        let identifier = super::sent_index::canonical(identifier);
        let id_value = caller_field(identifier.value(), "a send identifier")?;
        if sent.audience.is_empty() {
            anyhow::bail!(
                "a send registered with no audience can never have a bounce attributed to it: \
                 every reported address is checked against this list, so an empty one refuses \
                 every receipt for act `{act_ref}` while looking exactly like a rail that never \
                 bounces"
            );
        }
        let mut audience: Vec<String> = sent
            .audience
            .iter()
            .map(|address| crate::magician_v2::delivery::normalise_identity(address))
            .collect::<Result<Vec<_>>>()?;
        audience.sort();
        audience.dedup();
        let audience_json = serde_json::to_string(&audience)?;
        let conn = self.conn.lock().expect("sqlite sent-index lock");
        let existing: Option<(String, String, String)> = conn
            .query_row(
                "SELECT act_ref, audience_json, sent_at FROM sent_messages
                 WHERE principal=?1 AND workspace=?2 AND provider=?3 AND id_kind=?4 AND id_value=?5",
                params![
                    scope.principal,
                    scope.workspace,
                    provider,
                    identifier.kind(),
                    id_value
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((held_act, held_audience, held_sent)) = existing {
            let held: Vec<String> = serde_json::from_str(&held_audience)?;
            if held_act == act_ref && held == audience && held_sent == sent.sent_at.to_rfc3339() {
                return Ok(());
            }
            anyhow::bail!(
                "{} `{id_value}` is already recorded for act `{held_act}` and this call says \
                 act `{act_ref}`: an identical replay resumes, but two accounts of one message \
                 mean somebody upstream is reusing identifiers",
                identifier.kind()
            );
        }
        conn.execute(
            "INSERT INTO sent_messages(
                principal, workspace, provider, id_kind, id_value, act_ref,
                audience_json, sent_at, recorded_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                scope.principal,
                scope.workspace,
                provider,
                identifier.kind(),
                id_value,
                act_ref,
                audience_json,
                sent.sent_at.to_rfc3339(),
                now.to_rfc3339()
            ],
        )?;
        Ok(())
    }

    fn lookup(
        &self,
        scope: &DeliveryScope,
        provider: &str,
        identifier: &SendIdentifier,
    ) -> Result<SendBinding> {
        super::sent_index::validate_scope(scope)?;
        let provider = caller_field(provider, "a provider name")?;
        let identifier = super::sent_index::canonical(identifier);
        let conn = self.conn.lock().expect("sqlite sent-index lock");
        let mut stmt = conn.prepare(
            "SELECT act_ref, provider, id_kind, id_value, audience_json, sent_at, recorded_at
             FROM sent_messages
             WHERE principal=?1 AND workspace=?2 AND provider=?3 AND id_kind=?4 AND id_value=?5",
        )?;
        let rows = stmt.query_map(
            params![
                scope.principal,
                scope.workspace,
                provider,
                identifier.kind(),
                identifier.value()
            ],
            row_to_raw,
        )?;
        let mut records = Vec::new();
        for row in rows {
            records.push(raw_to_record(row?)?);
        }
        Ok(super::sent_index::binding_from_records(records))
    }

    fn export_scope(&self, scope: &DeliveryScope) -> Result<Vec<u8>> {
        let records = self.list_scope(scope)?;
        Ok(serde_json::to_vec(&records)?)
    }

    fn import_scope(&self, scope: &DeliveryScope, bytes: &[u8]) -> Result<()> {
        super::sent_index::validate_scope(scope)?;
        let records: Vec<SentIndexRecord> = serde_json::from_slice(bytes)?;
        let records = super::sent_index::fold_scope_records(records);
        let mut conn = self.conn.lock().expect("sqlite sent-index lock");
        let tx = conn.transaction()?;
        for record in records {
            if record.provider.is_empty() {
                anyhow::bail!("export row missing provider");
            }
            let identifier = identifier_from_parts(&record.id_kind, &record.id_value)?;
            let audience_json = serde_json::to_string(&record.audience)?;
            tx.execute(
                "INSERT INTO sent_messages(
                    principal, workspace, provider, id_kind, id_value, act_ref,
                    audience_json, sent_at, recorded_at
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
                 ON CONFLICT DO NOTHING",
                params![
                    scope.principal,
                    scope.workspace,
                    record.provider,
                    identifier.kind(),
                    identifier.value(),
                    record.act_ref,
                    audience_json,
                    record.sent_at.to_rfc3339(),
                    record.recorded_at.to_rfc3339()
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn list_scope(&self, scope: &DeliveryScope) -> Result<Vec<SentIndexRecord>> {
        super::sent_index::validate_scope(scope)?;
        let conn = self.conn.lock().expect("sqlite sent-index lock");
        let mut stmt = conn.prepare(
            "SELECT act_ref, provider, id_kind, id_value, audience_json, sent_at, recorded_at
             FROM sent_messages WHERE principal=?1 AND workspace=?2
             ORDER BY provider, id_kind, id_value",
        )?;
        let rows = stmt.query_map(params![scope.principal, scope.workspace], row_to_raw)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(raw_to_record(row?)?);
        }
        Ok(super::sent_index::fold_scope_records(out))
    }
}

struct RawRow {
    act_ref: String,
    provider: String,
    id_kind: String,
    id_value: String,
    audience_json: String,
    sent_at: String,
    recorded_at: String,
}

fn row_to_raw(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawRow> {
    Ok(RawRow {
        act_ref: row.get(0)?,
        provider: row.get(1)?,
        id_kind: row.get(2)?,
        id_value: row.get(3)?,
        audience_json: row.get(4)?,
        sent_at: row.get(5)?,
        recorded_at: row.get(6)?,
    })
}

fn raw_to_record(raw: RawRow) -> Result<SentIndexRecord> {
    Ok(SentIndexRecord {
        act_ref: raw.act_ref,
        provider: raw.provider,
        id_kind: raw.id_kind,
        id_value: raw.id_value,
        audience: serde_json::from_str(&raw.audience_json)?,
        sent_at: DateTime::parse_from_rfc3339(&raw.sent_at)?.with_timezone(&Utc),
        recorded_at: DateTime::parse_from_rfc3339(&raw.recorded_at)?.with_timezone(&Utc),
    })
}

#[allow(dead_code)]
pub fn sqlite_path_for_tests(root: &Path) -> PathBuf {
    root.join("sent_index.sqlite")
}
