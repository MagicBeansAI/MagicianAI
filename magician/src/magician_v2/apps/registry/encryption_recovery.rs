//! Explicit local recovery for debug builds that accidentally enabled test
//! fixtures. No server route or ordinary key lookup invokes this code.

use super::*;
use crate::magician_v2::apps::authoring::AppEncryptionRecoveryArgs;
use crate::magician_v2::secrets::encryption::legacy_fixture_app_data_scope_key;
use serde_json::{json, Value};

pub(crate) fn recover_legacy_fixture_encryption(
    args: &AppEncryptionRecoveryArgs,
) -> anyhow::Result<Value> {
    let scope: AppScope = serde_json::from_value(json!({
        "principal": args.principal, "workspace": args.workspace,
    }))?;
    let root = args.root.canonicalize()?;
    let workspace = ArtifactV2Workspace::new(&root);
    let path = workspace.app_store_db_path(scope.principal.as_str(), scope.workspace.as_str());
    validate_existing_registry_paths(&root, &path)?;
    anyhow::ensure!(path.is_file(), "the scoped Apps database does not exist");
    anyhow::ensure!(
        !registry_has_plaintext_header(&path)?,
        "this command only recovers an existing encrypted Apps database"
    );
    let keys = app_data_scope_key_candidates(scope.principal.as_str(), scope.workspace.as_str())?;
    let active = keys
        .first()
        .ok_or(AppRegistryError::AtRestEncryptionKeyUnavailable)?;
    let legacy =
        legacy_fixture_app_data_scope_key(scope.principal.as_str(), scope.workspace.as_str())?;
    anyhow::ensure!(
        active.key_id() != legacy.key_id(),
        "recovery requires a production build using a durable OS Keychain key"
    );
    let flags = (if args.commit {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    }) | OpenFlags::SQLITE_OPEN_FULL_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let mut selected = None;
    for key in keys.iter().chain(std::iter::once(&legacy)) {
        let connection = Connection::open_with_flags(&path, flags)?;
        connection.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
        if args.commit {
            // Retain SQLite's exclusive lock across checkpoint, backup and
            // rekey. A competing process fails this operation, never races it.
            connection.pragma_update(None, "locking_mode", "EXCLUSIVE")?;
        }
        if configure_registry_cipher(&connection, key).is_ok() {
            verify_schema_scope_and_opened_encryption(
                &connection,
                &scope,
                key.key_id(),
                active.key_id(),
            )?;
            selected = Some((connection, key));
            break;
        }
    }
    let (mut connection, opened) = selected.ok_or(AppRegistryError::AtRestEncryptionFailed)?;
    verify_integrity(&connection)?;
    if opened.key_id() == active.key_id() {
        return Ok(
            json!({"status":"already_current", "principal":args.principal, "workspace":args.workspace}),
        );
    }
    if !args.commit {
        return Ok(
            json!({"status":"recoverable", "principal":args.principal, "workspace":args.workspace,
            "legacy_fixture_key": opened.key_id() == legacy.key_id(), "records_changed":false}),
        );
    }
    let backup = args
        .backup
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--commit requires --backup"))?;
    connection.execute_batch(
        "BEGIN EXCLUSIVE; COMMIT; PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;",
    )?;
    write_encrypted_backup(&connection, &scope, active, backup)?;
    rekey_registry_to_active_generation(&mut connection, &scope, opened, active)?;
    verify_integrity(&connection)?;
    drop(connection);
    // Reopen using only the durable key, independently of the recovery path.
    let verified = Connection::open_with_flags(&path, flags)?;
    configure_registry_cipher(&verified, active)?;
    verify_schema_scope_and_encryption(&verified, &scope, active.key_id())?;
    verify_integrity(&verified)?;
    fs::File::open(&path)?.sync_all()?;
    Ok(
        json!({"status":"recovered", "principal":args.principal, "workspace":args.workspace,
        "backup":backup, "records_changed":false, "key_material_reported":false}),
    )
}

fn verify_integrity(connection: &Connection) -> anyhow::Result<()> {
    let mut statement = connection.prepare("PRAGMA integrity_check")?;
    let mut rows = statement.query([])?;
    let first = rows
        .next()?
        .map(|row| row.get::<_, String>(0))
        .transpose()?;
    anyhow::ensure!(
        first.as_deref() == Some("ok") && rows.next()?.is_none(),
        "Apps database integrity verification failed"
    );
    Ok(())
}

fn write_encrypted_backup(
    source: &Connection,
    scope: &AppScope,
    active: &AppDataScopeKey,
    backup: &Path,
) -> anyhow::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(backup)?;
    let mut passphrase = hex::encode(active.expose_for_sqlcipher());
    let attached = source.execute(
        "ATTACH DATABASE ?1 AS recovery_backup KEY ?2",
        params![backup.to_string_lossy().as_ref(), passphrase.as_str()],
    );
    use zeroize::Zeroize;
    passphrase.zeroize();
    attached.map_err(|_| AppRegistryError::AtRestEncryptionFailed)?;
    source.execute_batch("PRAGMA recovery_backup.cipher_compatibility=4; SELECT sqlcipher_export('recovery_backup');")?;
    for pragma in ["application_id", "user_version"] {
        source.pragma_update(
            Some(rusqlite::DatabaseName::Attached("recovery_backup")),
            pragma,
            pragma_i32(source, pragma)?,
        )?;
    }
    source.execute(
        "UPDATE recovery_backup.app_data_encryption_metadata SET key_id=?1 WHERE singleton=1",
        params![active.key_id()],
    )?;
    source.execute_batch("DETACH DATABASE recovery_backup")?;
    let verified = Connection::open_with_flags(
        backup,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    configure_registry_cipher(&verified, active)?;
    verify_schema_scope_and_encryption(&verified, scope, active.key_id())?;
    verify_integrity(&verified)?;
    file.sync_all()?;
    let parent = backup
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_and_rekey_preserve_records_and_reject_the_legacy_key() {
        let temp = super::super::tests::canonical_tempdir();
        let path = temp.path().join("source.sqlite3");
        let backup = temp.path().join("backup.sqlite3");
        let scope: AppScope =
            serde_json::from_value(json!({"principal":"recovery-test", "workspace":"default"}))
                .unwrap();
        let old = legacy_fixture_app_data_scope_key("recovery-test", "default").unwrap();
        let active = legacy_fixture_app_data_scope_key("different-test-key", "default").unwrap();
        let mut source = Connection::open(&path).unwrap();
        configure_registry_cipher(&source, &old).unwrap();
        initialize_or_verify_schema(&mut source, &scope, old.key_id(), old.key_id()).unwrap();
        source
            .execute_batch(
                "CREATE TABLE recovery_test_payload (payload BLOB NOT NULL, policy TEXT NOT NULL);",
            )
            .unwrap();
        source
            .execute(
                "INSERT INTO recovery_test_payload VALUES (?1, ?2)",
                params![b"unchanged private bytes".as_slice(), "local_only"],
            )
            .unwrap();
        write_encrypted_backup(&source, &scope, &active, &backup).unwrap();
        // Existing backups are not replaced, and refusal leaves the source usable.
        assert!(write_encrypted_backup(&source, &scope, &active, &backup).is_err());
        verify_schema_scope_and_encryption(&source, &scope, old.key_id()).unwrap();
        rekey_registry_to_active_generation(&mut source, &scope, &old, &active).unwrap();
        drop(source);
        for file in [&path, &backup] {
            let wrong = Connection::open(file).unwrap();
            assert!(configure_registry_cipher(&wrong, &old).is_err());
            drop(wrong);
            let recovered = Connection::open(file).unwrap();
            configure_registry_cipher(&recovered, &active).unwrap();
            verify_schema_scope_and_encryption(&recovered, &scope, active.key_id()).unwrap();
            verify_integrity(&recovered).unwrap();
            let record = recovered
                .query_row(
                    "SELECT payload, policy FROM recovery_test_payload",
                    [],
                    |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
                )
                .unwrap();
            assert_eq!(
                record,
                (b"unchanged private bytes".to_vec(), "local_only".to_owned())
            );
        }
    }
}
