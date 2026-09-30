//! Index-only snapshot/keyset planning. Record bodies are fetched separately for one page.
use chrono::{DateTime, FixedOffset};
use rusqlite::{types::Value, Connection};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

pub(super) enum Filter {
    Values(String, Vec<(&'static str, Value)>),
    All(Vec<Filter>),
    Any(Vec<Filter>),
}

impl Filter {
    pub(super) fn parameter_count(&self) -> usize {
        match self {
            Self::Values(_, values) => 1 + values.len() * 2,
            Self::All(children) | Self::Any(children) => {
                children.iter().map(Self::parameter_count).sum()
            },
        }
    }
}

pub(super) struct Order {
    pub field: String,
    pub descending: bool,
}

fn bind(parameters: &mut Vec<Value>, value: Value) -> String {
    parameters.push(value);
    format!("?{}", parameters.len())
}

fn predicate(filter: &Filter, parameters: &mut Vec<Value>) -> String {
    match filter {
        Filter::All(children) | Filter::Any(children) => {
            if children.is_empty() {
                return if matches!(filter, Filter::All(_)) {
                    "1"
                } else {
                    "0"
                }
                .into();
            }
            let join = if matches!(filter, Filter::All(_)) {
                " AND "
            } else {
                " OR "
            };
            format!(
                "({})",
                children
                    .iter()
                    .map(|child| predicate(child, parameters))
                    .collect::<Vec<_>>()
                    .join(join)
            )
        },
        Filter::Values(field, values) => {
            if values.is_empty() {
                return "0".into();
            }
            let field = bind(parameters, Value::Text(field.clone()));
            let values = values
                .iter()
                .map(|(kind, value)| {
                    let kind = bind(parameters, Value::Text((*kind).into()));
                    let column = if matches!(value, Value::Integer(_)) {
                        "integer_value"
                    } else {
                        "text_value"
                    };
                    let value = bind(parameters, value.clone());
                    format!("(i.value_kind = {kind} AND i.{column} = {value})")
                })
                .collect::<Vec<_>>()
                .join(" OR ");
            format!("EXISTS (SELECT 1 FROM app_scalar_indexes i WHERE i.installation_id = h.installation_id
                AND i.entity_name = h.entity_name AND i.record_id = h.record_id
                AND i.record_revision = h.record_revision AND i.field_path = {field} AND ({values}))")
        },
    }
}

/// Versioned materialized keys keep SQLite ordering identical to the typed
/// comparator (including u64, exact decimals, UTC nanoseconds and text prefixes).
/// Null sorts after values in either direction; absent fields are a final phase.
pub(super) fn order_key(
    kind: &str,
    text: Option<&str>,
    integer: Option<i64>,
    descending: bool,
) -> rusqlite::Result<Vec<u8>> {
    let value = sort_value(Some(kind.into()), text.map(str::to_owned), integer)?;
    let mut bytes = Vec::new();
    match value {
        SortValue::Null => return Ok(vec![1]),
        SortValue::Missing => return Err(invalid()),
        SortValue::Integer(value) => {
            bytes.extend_from_slice(&((value as u128) ^ (1 << 127)).to_be_bytes())
        },
        SortValue::Timestamp(value) => {
            bytes.extend_from_slice(&((value.timestamp() as u64) ^ (1 << 63)).to_be_bytes());
            bytes.extend_from_slice(&value.timestamp_subsec_nanos().to_be_bytes());
        },
        SortValue::Decimal(value) => {
            // Decimal has at most 29 integral and 28 fractional digits. Encode
            // absolute magnitude without floating point or rescaling overflow.
            let raw = value.mantissa().unsigned_abs().to_string();
            let scale = value.scale() as usize;
            let zeros = 57usize
                .checked_sub(raw.len() + 28 - scale)
                .ok_or_else(invalid)?;
            bytes.push(if value.is_sign_negative() && !value.is_zero() {
                0
            } else {
                1
            });
            bytes.extend(std::iter::repeat_n(b'0', zeros));
            bytes.extend_from_slice(raw.as_bytes());
            bytes.extend(std::iter::repeat_n(b'0', 28 - scale));
            if bytes[0] == 0 {
                for byte in &mut bytes[1..] {
                    *byte = b'9' - (*byte - b'0');
                }
            }
        },
        SortValue::Text(value) => {
            // A terminator is needed when reversing prefix order ("a" < "ab").
            // Escape only the terminator and escape byte, keeping ordinary
            // UTF-8 keys compact instead of doubling every indexed body.
            for byte in value.bytes() {
                match byte {
                    0 | 1 => bytes.extend_from_slice(&[1, byte + 1]),
                    _ => bytes.push(byte),
                }
            }
            bytes.push(0);
        },
    }
    if descending {
        for byte in &mut bytes {
            *byte = !*byte;
        }
    }
    bytes.insert(0, 0);
    Ok(bytes)
}

pub(super) const KEYSET_SCHEMA: &str = r#"
ALTER TABLE app_scalar_indexes ADD COLUMN order_key_asc BLOB;
ALTER TABLE app_scalar_indexes ADD COLUMN order_key_desc BLOB;
CREATE INDEX app_scalar_order_asc_idx ON app_scalar_indexes
 (installation_id, entity_name, field_path, order_key_asc, record_id);
CREATE INDEX app_scalar_order_desc_idx ON app_scalar_indexes
 (installation_id, entity_name, field_path, order_key_desc, record_id);
CREATE TRIGGER app_scalar_order_insert_guard BEFORE INSERT ON app_scalar_indexes
 WHEN NEW.order_key_asc IS NULL OR NEW.order_key_desc IS NULL
 BEGIN SELECT RAISE(ABORT, 'app scalar ordering keys are required'); END;
CREATE TRIGGER app_scalar_order_update_guard BEFORE UPDATE ON app_scalar_indexes
 WHEN NEW.order_key_asc IS NULL OR NEW.order_key_desc IS NULL
 BEGIN SELECT RAISE(ABORT, 'app scalar ordering keys are required'); END;
CREATE TABLE app_keyset_cursors (
 cursor_ref TEXT PRIMARY KEY, installation_id TEXT NOT NULL,
 installation_generation INTEGER NOT NULL, package_revision_ref TEXT NOT NULL,
 schema_revision INTEGER NOT NULL, dataset_generation INTEGER NOT NULL,
 evidence_json BLOB NOT NULL, boundary_json BLOB NOT NULL,
 chain_ref TEXT NOT NULL, expires_at TEXT NOT NULL
) STRICT;
CREATE INDEX app_keyset_cursors_chain_idx ON app_keyset_cursors(installation_id, chain_ref);
CREATE INDEX app_keyset_cursors_expiry_idx ON app_keyset_cursors(expires_at);
"#;

/// Make room for one boundary in the caller's insertion transaction. Cursor
/// metadata is a bounded cache: abandoned readers must not block new readers.
/// Keep the chain being continued; prefer uncontinued roots, then least recently
/// advanced chains. Evicted readers recover through the normal cursor reset.
pub(super) fn reserve_keyset_cursor_capacity(
    transaction: &rusqlite::Transaction<'_>,
    installation: &str,
    protected_chain: &str,
    incoming_bytes: i64,
    max_rows: i64,
    max_bytes: i64,
) -> rusqlite::Result<bool> {
    if incoming_bytes < 0 || incoming_bytes > max_bytes || max_rows < 1 {
        return Ok(false);
    }
    let chains = transaction
        .prepare(
            "SELECT chain_ref, COUNT(*), SUM(length(evidence_json)+length(boundary_json))
             FROM app_keyset_cursors WHERE installation_id = ?1
             GROUP BY chain_ref
             ORDER BY COUNT(*) > 1, MAX(expires_at), chain_ref",
        )?
        .query_map([installation], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let (mut rows, mut bytes) = chains
        .iter()
        .fold((1i64, incoming_bytes), |(rows, bytes), chain| {
            (rows.saturating_add(chain.1), bytes.saturating_add(chain.2))
        });
    let mut evicted = Vec::new();
    for (chain, count, size) in &chains {
        if rows <= max_rows && bytes <= max_bytes {
            break;
        }
        if chain != protected_chain {
            rows -= count;
            bytes -= size;
            evicted.push(chain);
        }
    }
    // A boundary that cannot fit alongside its protected parent must fail
    // without pointlessly invalidating other readers.
    if rows > max_rows || bytes > max_bytes {
        return Ok(false);
    }
    for chain in evicted {
        transaction.execute(
            "DELETE FROM app_keyset_cursors WHERE installation_id = ?1 AND chain_ref = ?2",
            rusqlite::params![installation, chain],
        )?;
    }
    Ok(true)
}

/// One transactional platform migration, bounded to 256 index entries in
/// memory. Existing app payloads, package versions and approval receipts are untouched.
pub(super) fn migrate_keyset(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(KEYSET_SCHEMA)?;
    let mut after = 0i64;
    loop {
        let mut statement = connection.prepare(
            "SELECT rowid, value_kind, text_value, integer_value
            FROM app_scalar_indexes WHERE rowid > ?1 ORDER BY rowid LIMIT 256",
        )?;
        let rows = statement
            .query_map([after], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if rows.is_empty() {
            break;
        }
        for (id, kind, text, integer) in rows {
            connection.execute("UPDATE app_scalar_indexes SET order_key_asc = ?1, order_key_desc = ?2 WHERE rowid = ?3",
                rusqlite::params![order_key(&kind, text.as_deref(), integer, false)?, order_key(&kind, text.as_deref(), integer, true)?, id])?;
            after = id;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Boundary {
    pub key: Option<Vec<u8>>,
    pub record_id: String,
}

pub(super) struct KeysetRow {
    pub boundary: Boundary,
    pub revision: i64,
}

// A changed-record lookup should start from its selective identity index,
// rather than walk the chronology looking for one old ID. Probe only a fixed
// number of entries; common broad filters immediately keep the ordering spine.
fn bounded_candidates(
    connection: &Connection,
    installation: &str,
    entity: &str,
    filter: &Filter,
) -> rusqlite::Result<Option<Vec<String>>> {
    match filter {
        Filter::All(children) => {
            for child in children {
                if let Some(ids) = bounded_candidates(connection, installation, entity, child)? {
                    return Ok(Some(ids));
                }
            }
            Ok(None)
        },
        Filter::Any(_) => Ok(None),
        Filter::Values(field, values) => {
            let mut parameters = vec![
                Value::Text(installation.into()),
                Value::Text(entity.into()),
                Value::Text(field.clone()),
            ];
            let mut terms = Vec::new();
            for (kind, value) in values {
                let column = if matches!(value, Value::Integer(_)) {
                    "integer_value"
                } else {
                    "text_value"
                };
                let kind = bind(&mut parameters, Value::Text((*kind).into()));
                let value = bind(&mut parameters, value.clone());
                terms.push(format!("(value_kind = {kind} AND {column} = {value})"));
            }
            if terms.is_empty() {
                return Ok(Some(Vec::new()));
            }
            let sql = format!("SELECT record_id FROM app_scalar_indexes WHERE installation_id=?1 AND entity_name=?2 AND field_path=?3 AND ({}) LIMIT 101",terms.join(" OR "));
            let mut statement = connection.prepare(&sql)?;
            let ids = statement
                .query_map(rusqlite::params_from_iter(parameters), |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok((ids.len() <= 100).then_some(ids))
        },
    }
}

/// Seeks one scalar index, with record_id as the ascending tie breaker. Fetches
/// only page_size + 1 metadata rows, independent of total collection size.
/// Missing optional sort values follow the indexed nulls, in record_id order.
pub(super) fn read_keyset(
    connection: &Connection,
    installation: &str,
    entity: &str,
    filter: Option<&Filter>,
    order: Option<&Order>,
    required: bool,
    after: Option<&Boundary>,
    page_size: usize,
    max_bytes: usize,
) -> rusqlite::Result<Vec<KeysetRow>> {
    let limit = page_size.checked_add(1).ok_or_else(invalid)?;
    let mut rows = Vec::new();
    let mut bytes = 0usize;
    if let Some(ids) = filter
        .map(|filter| bounded_candidates(connection, installation, entity, filter))
        .transpose()?
        .flatten()
    {
        if ids.is_empty() {
            return Ok(rows);
        }
        let mut parameters = vec![Value::Text(installation.into()), Value::Text(entity.into())];
        let ids = ids
            .into_iter()
            .map(|id| bind(&mut parameters, Value::Text(id)))
            .collect::<Vec<_>>()
            .join(",");
        let (join, key) = if let Some(order) = order {
            let field = bind(&mut parameters, Value::Text(order.field.clone()));
            let key = if order.descending {
                "s.order_key_desc"
            } else {
                "s.order_key_asc"
            };
            (format!("LEFT JOIN app_scalar_indexes s ON s.installation_id=h.installation_id AND s.entity_name=h.entity_name
                AND s.record_id=h.record_id AND s.record_revision=h.record_revision AND s.field_path={field}"), key)
        } else {
            (String::new(), "NULL")
        };
        let condition = predicate(filter.ok_or_else(invalid)?, &mut parameters);
        let seek = if let Some(after) = after {
            let id = bind(&mut parameters, Value::Text(after.record_id.clone()));
            if let Some(value) = &after.key {
                let value = bind(&mut parameters, Value::Blob(value.clone()));
                format!("AND ({key} IS NULL OR ({key},h.record_id)>({value},{id}))")
            } else {
                format!("AND {key} IS NULL AND h.record_id>{id}")
            }
        } else {
            String::new()
        };
        let limit = bind(&mut parameters, Value::Integer(limit as i64));
        let sql = format!("SELECT h.record_id,h.record_revision,{key} FROM app_record_heads h {join}
            WHERE h.installation_id=?1 AND h.entity_name=?2 AND h.record_id IN ({ids}) AND h.deleted_at IS NULL
            AND {condition} {seek} ORDER BY ({key} IS NULL), {key}, h.record_id LIMIT {limit}");
        read_keyset_rows(
            connection, &sql, parameters, &mut rows, &mut bytes, max_bytes,
        )?;
        return Ok(rows);
    }
    if let Some(order) = order {
        if after.is_none_or(|boundary| boundary.key.is_some()) {
            let mut parameters = vec![
                Value::Text(installation.into()),
                Value::Text(entity.into()),
                Value::Text(order.field.clone()),
            ];
            let condition = filter
                .map(|f| predicate(f, &mut parameters))
                .unwrap_or_else(|| "1".into());
            let column = if order.descending {
                "order_key_desc"
            } else {
                "order_key_asc"
            };
            let index = if order.descending {
                "app_scalar_order_desc_idx"
            } else {
                "app_scalar_order_asc_idx"
            };
            let seek = if let Some(after) = after {
                let key = bind(
                    &mut parameters,
                    Value::Blob(after.key.clone().ok_or_else(invalid)?),
                );
                let id = bind(&mut parameters, Value::Text(after.record_id.clone()));
                format!("AND (s.{column}, s.record_id) > ({key}, {id})")
            } else {
                String::new()
            };
            let limit = bind(&mut parameters, Value::Integer(limit as i64));
            let sql = format!(
                "SELECT s.record_id, s.record_revision, s.{column}
                FROM app_scalar_indexes s INDEXED BY {index}
                CROSS JOIN app_record_heads h ON h.installation_id = s.installation_id
                 AND h.entity_name = s.entity_name AND h.record_id = s.record_id
                 AND h.record_revision = s.record_revision AND h.deleted_at IS NULL
                WHERE s.installation_id = ?1 AND s.entity_name = ?2 AND s.field_path = ?3
                 AND s.{column} IS NOT NULL {seek} AND {condition}
                ORDER BY s.{column}, s.record_id LIMIT {limit}"
            );
            read_keyset_rows(
                connection, &sql, parameters, &mut rows, &mut bytes, max_bytes,
            )?;
        }
    }
    if rows.len() < limit && (order.is_none() || !required) {
        let mut parameters = vec![Value::Text(installation.into()), Value::Text(entity.into())];
        let condition = filter
            .map(|f| predicate(f, &mut parameters))
            .unwrap_or_else(|| "1".into());
        let missing = if let Some(order) = order {
            let field = bind(&mut parameters, Value::Text(order.field.clone()));
            format!("AND NOT EXISTS (SELECT 1 FROM app_scalar_indexes s WHERE s.installation_id = h.installation_id
                AND s.entity_name = h.entity_name AND s.field_path = {field} AND s.record_id = h.record_id
                AND s.record_revision = h.record_revision)")
        } else {
            String::new()
        };
        let seek = if let Some(after) = after.filter(|boundary| boundary.key.is_none()) {
            let id = bind(&mut parameters, Value::Text(after.record_id.clone()));
            format!("AND h.record_id > {id}")
        } else {
            String::new()
        };
        let limit = bind(&mut parameters, Value::Integer((limit - rows.len()) as i64));
        let sql = format!(
            "SELECT h.record_id, h.record_revision, NULL FROM app_record_heads h
            WHERE h.installation_id = ?1 AND h.entity_name = ?2 AND h.deleted_at IS NULL
            {seek} {missing} AND {condition} ORDER BY h.record_id LIMIT {limit}"
        );
        read_keyset_rows(
            connection, &sql, parameters, &mut rows, &mut bytes, max_bytes,
        )?;
    }
    Ok(rows)
}

fn read_keyset_rows(
    connection: &Connection,
    sql: &str,
    parameters: Vec<Value>,
    output: &mut Vec<KeysetRow>,
    bytes: &mut usize,
    max_bytes: usize,
) -> rusqlite::Result<()> {
    let mut statement = connection.prepare(sql)?;
    let mut rows = statement.query(rusqlite::params_from_iter(parameters))?;
    while let Some(row) = rows.next()? {
        let record_id: String = row.get(0)?;
        let key: Option<Vec<u8>> = row.get(2)?;
        *bytes = bytes.saturating_add(record_id.len() + key.as_ref().map_or(0, Vec::len) + 16);
        if *bytes > max_bytes {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_TOOBIG),
                None,
            ));
        }
        output.push(KeysetRow {
            boundary: Boundary { key, record_id },
            revision: row.get(1)?,
        });
    }
    Ok(())
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum SortValue {
    Integer(i128),
    Decimal(Decimal),
    Timestamp(DateTime<FixedOffset>),
    Text(String),
    Null,
    Missing,
}

fn invalid() -> rusqlite::Error {
    rusqlite::Error::InvalidQuery
}

fn sort_value(
    kind: Option<String>,
    text: Option<String>,
    integer: Option<i64>,
) -> rusqlite::Result<SortValue> {
    Ok(match kind.as_deref() {
        None => SortValue::Missing,
        Some("null") => SortValue::Null,
        Some("integer" | "boolean") => SortValue::Integer(match integer {
            Some(value) => i128::from(value),
            None => text.ok_or_else(invalid)?.parse().map_err(|_| invalid())?,
        }),
        Some("decimal") => {
            SortValue::Decimal(text.ok_or_else(invalid)?.parse().map_err(|_| invalid())?)
        },
        Some("timestamp") => SortValue::Timestamp(
            DateTime::parse_from_rfc3339(&text.ok_or_else(invalid)?).map_err(|_| invalid())?,
        ),
        Some("text" | "enum" | "reference") => SortValue::Text(text.ok_or_else(invalid)?),
        _ => return Err(invalid()),
    })
}

/// Returns at most max_rows + 1 identifiers so the caller preserves its existing
/// snapshot capacity check. No payload/digest/policy blob is selected or decoded.
pub(super) fn read(
    connection: &Connection,
    installation: &str,
    entity: &str,
    filter: Option<&Filter>,
    order: &[Order],
    max_rows: usize,
    max_bytes: usize,
) -> rusqlite::Result<Vec<(String, i64)>> {
    let mut parameters = vec![Value::Text(installation.into()), Value::Text(entity.into())];
    let condition = filter
        .map(|f| predicate(f, &mut parameters))
        .unwrap_or_else(|| "1".into());
    let mut joins = String::new();
    let mut select = String::from("h.record_id, h.record_revision");
    for (index, field) in order.iter().enumerate() {
        let parameter = bind(&mut parameters, Value::Text(field.field.clone()));
        joins.push_str(&format!(
            " LEFT JOIN app_scalar_indexes s{index}
          ON s{index}.installation_id = h.installation_id AND s{index}.entity_name = h.entity_name
          AND s{index}.record_id = h.record_id AND s{index}.record_revision = h.record_revision
          AND s{index}.field_path = {parameter}"
        ));
        select.push_str(&format!(
            ", s{index}.value_kind, s{index}.text_value, s{index}.integer_value"
        ));
    }
    let limit = bind(
        &mut parameters,
        Value::Integer(i64::try_from(max_rows.saturating_add(1)).map_err(|_| invalid())?),
    );
    let sql = format!(
        "SELECT {select} FROM app_record_heads h {joins}
      WHERE h.installation_id = ?1 AND h.entity_name = ?2 AND h.deleted_at IS NULL
      AND {condition} ORDER BY h.record_id ASC LIMIT {limit}"
    );
    let mut statement = connection.prepare(&sql)?;
    let mut metadata_bytes = 0usize;
    let mut rows = statement
        .query_map(rusqlite::params_from_iter(parameters), |row| {
            let id: String = row.get(0)?;
            metadata_bytes = metadata_bytes.saturating_add(id.len() + 16);
            let mut values = Vec::with_capacity(order.len());
            for index in 0..order.len() {
                let text: Option<String> = row.get(3 + index * 3)?;
                metadata_bytes =
                    metadata_bytes.saturating_add(text.as_ref().map_or(0, String::len) + 32);
                if metadata_bytes > max_bytes {
                    return Err(rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_TOOBIG),
                        Some("app query index metadata exceeds scan budget".into()),
                    ));
                }
                values.push(sort_value(
                    row.get(2 + index * 3)?,
                    text,
                    row.get(4 + index * 3)?,
                )?);
            }
            if metadata_bytes > max_bytes {
                return Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_TOOBIG),
                    None,
                ));
            }
            Ok((id, row.get::<_, i64>(1)?, values))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.sort_by(|left, right| {
        for (index, field) in order.iter().enumerate() {
            let a = &left.2[index];
            let b = &right.2[index];
            let mut comparison = a.cmp(b);
            // Null/missing stay last in either direction, matching the canonical comparator.
            if field.descending
                && !matches!(a, SortValue::Null | SortValue::Missing)
                && !matches!(b, SortValue::Null | SortValue::Missing)
            {
                comparison = comparison.reverse();
            }
            if comparison != Ordering::Equal {
                return comparison;
            }
        }
        left.0.cmp(&right.0)
    });
    Ok(rows
        .into_iter()
        .map(|(id, revision, _)| (id, revision))
        .collect())
}
