use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde_yaml::{Mapping, Value};
use tokio::{fs, sync::Mutex};

use magician::config::BotProcessConfig;
use magician::magician_v2::artifact_v2::io::write_bytes_durably_with_mode;

#[derive(Debug, Clone)]
pub struct BotConfigStore {
    path: Arc<PathBuf>,
    write_lock: Arc<Mutex<()>>,
}

impl BotConfigStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path: Arc::new(path),
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn path(&self) -> &Path {
        self.path.as_path()
    }

    pub async fn upsert_bot(
        &self,
        name: &str,
        config: &BotProcessConfig,
    ) -> Result<(), BotConfigStoreError> {
        let _guard = self.write_lock.lock().await;
        let mut document = self.load_document().await?;
        let root = root_mapping_mut(&mut document, self.path())?;
        let bots = bots_mapping_mut(root);
        bots.insert(
            Value::String(name.to_string()),
            serde_yaml::to_value(config).map_err(|err| BotConfigStoreError::Serialize {
                path: self.path().to_path_buf(),
                reason: err.to_string(),
            })?,
        );
        self.write_document(&document).await
    }

    pub async fn delete_bot(&self, name: &str) -> Result<bool, BotConfigStoreError> {
        let _guard = self.write_lock.lock().await;
        let mut document = self.load_document().await?;
        let root = root_mapping_mut(&mut document, self.path())?;

        let bots_key = Value::String("bots".to_string());
        let Some(bots_value) = root.get_mut(&bots_key) else {
            return Ok(false);
        };
        let bots = value_mapping_mut(bots_value, "bots", self.path())?;
        let removed = bots.remove(Value::String(name.to_string())).is_some();
        if removed {
            if bots.is_empty() {
                root.remove(&bots_key);
            }
            self.write_document(&document).await?;
        }

        Ok(removed)
    }

    pub async fn load_bots(
        &self,
    ) -> Result<BTreeMap<String, BotProcessConfig>, BotConfigStoreError> {
        let document = self.load_document().await?;
        let root = match document {
            Value::Mapping(mapping) => mapping,
            Value::Null => Mapping::new(),
            _ => {
                return Err(BotConfigStoreError::Parse {
                    path: self.path().to_path_buf(),
                    reason: "root document must be a YAML mapping".to_string(),
                })
            },
        };

        let Some(bots_value) = root.get(Value::String("bots".to_string())) else {
            return Ok(BTreeMap::new());
        };
        if bots_value.is_null() {
            return Ok(BTreeMap::new());
        }
        let bots = match bots_value {
            Value::Mapping(mapping) => mapping,
            _ => {
                return Err(BotConfigStoreError::Parse {
                    path: self.path().to_path_buf(),
                    reason: "`bots` must be a YAML mapping".to_string(),
                })
            },
        };

        let mut configs = BTreeMap::new();
        for (key, value) in bots {
            let Some(name) = key.as_str() else {
                continue;
            };
            let config: BotProcessConfig =
                serde_yaml::from_value(value.clone()).map_err(|err| {
                    BotConfigStoreError::Parse {
                        path: self.path().to_path_buf(),
                        reason: err.to_string(),
                    }
                })?;
            configs.insert(name.to_string(), config);
        }
        Ok(configs)
    }

    async fn load_document(&self) -> Result<Value, BotConfigStoreError> {
        let content = match fs::read_to_string(self.path()).await {
            Ok(content) => content,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Value::Mapping(Mapping::new()))
            },
            Err(err) => {
                return Err(BotConfigStoreError::Read {
                    path: self.path().to_path_buf(),
                    reason: err.to_string(),
                })
            },
        };
        serde_yaml::from_str(&content).map_err(|err| BotConfigStoreError::Parse {
            path: self.path().to_path_buf(),
            reason: err.to_string(),
        })
    }

    /// Publish the YAML document through the shared durable writer.
    ///
    /// The hand-rolled write staged under a fixed `<file>.tmp`, shared by every
    /// concurrent writer of this path, and fsynced neither the staging file nor
    /// the parent directory. The helper does both under a unique temp name.
    ///
    /// An operator-tightened mode is carried onto the staging file, so the
    /// rename publishes a file that already has it. This config holds bot
    /// provider tokens, and re-applying the mode *after* the rename would leave
    /// a window — short, but real — where it is readable at whatever the umask
    /// allowed. That window is the one thing the hand-rolled writer this
    /// replaced did not have, and it is not worth trading for the fsync.
    async fn write_document(&self, document: &Value) -> Result<(), BotConfigStoreError> {
        let serialized =
            serde_yaml::to_string(document).map_err(|err| BotConfigStoreError::Serialize {
                path: self.path().to_path_buf(),
                reason: err.to_string(),
            })?;

        // Absent on first write, in which case the file is created at the
        // umask's default exactly as it always was.
        #[cfg(unix)]
        let inherited_mode = {
            use std::os::unix::fs::PermissionsExt;
            fs::metadata(self.path())
                .await
                .ok()
                .map(|metadata| metadata.permissions().mode())
        };
        #[cfg(not(unix))]
        let inherited_mode = None;

        write_bytes_durably_with_mode(self.path(), serialized.as_bytes(), inherited_mode)
            .await
            .map_err(|err| BotConfigStoreError::Write {
                path: self.path().to_path_buf(),
                reason: err.to_string(),
            })?;

        Ok(())
    }
}

fn root_mapping_mut<'a>(
    document: &'a mut Value,
    path: &Path,
) -> Result<&'a mut Mapping, BotConfigStoreError> {
    match document {
        Value::Mapping(mapping) => Ok(mapping),
        Value::Null => {
            *document = Value::Mapping(Mapping::new());
            match document {
                Value::Mapping(mapping) => Ok(mapping),
                _ => unreachable!("document just set to mapping"),
            }
        },
        _ => Err(BotConfigStoreError::Parse {
            path: path.to_path_buf(),
            reason: "root document must be a YAML mapping".to_string(),
        }),
    }
}

fn bots_mapping_mut(root: &mut Mapping) -> &mut Mapping {
    let bots_key = Value::String("bots".to_string());
    let bots_value = root
        .entry(bots_key)
        .or_insert_with(|| Value::Mapping(Mapping::new()));
    match bots_value {
        Value::Mapping(mapping) => mapping,
        _ => {
            *bots_value = Value::Mapping(Mapping::new());
            match bots_value {
                Value::Mapping(mapping) => mapping,
                _ => unreachable!("bots value just set to mapping"),
            }
        },
    }
}

fn value_mapping_mut<'a>(
    value: &'a mut Value,
    field_name: &str,
    path: &Path,
) -> Result<&'a mut Mapping, BotConfigStoreError> {
    match value {
        Value::Mapping(mapping) => Ok(mapping),
        _ => Err(BotConfigStoreError::Parse {
            path: path.to_path_buf(),
            reason: format!("`{field_name}` must be a YAML mapping"),
        }),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BotConfigStoreError {
    #[error("failed to read `{path}`: {reason}")]
    Read { path: PathBuf, reason: String },
    #[error("failed to parse `{path}`: {reason}")]
    Parse { path: PathBuf, reason: String },
    #[error("failed to serialize `{path}`: {reason}")]
    Serialize { path: PathBuf, reason: String },
    #[error("failed to write `{path}`: {reason}")]
    Write { path: PathBuf, reason: String },
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::BTreeMap;

    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn upsert_bot_preserves_unrelated_config_sections() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("bot_configs.yaml");
        fs::write(
            &path,
            r#"
metadata:
  channel: consumer
bots:
  telegram:
    enabled: true
    command: node
    args:
      - "{scope_capabilities_root}/bots/telegram/dist/index.js"
    cwd: "{scope_capabilities_root}/bots/telegram"
"#,
        )
        .await
        .expect("seed config");

        let store = BotConfigStore::new(path.clone());
        store
            .upsert_bot(
                "whatsapp",
                &BotProcessConfig {
                    enabled: false,
                    command: "node".to_string(),
                    args: vec!["{scope_capabilities_root}/bots/whatsapp/dist/index.js".to_string()],
                    env: BTreeMap::from([(
                        "WHATSAPP_AUTH_DIR".to_string(),
                        "{scope_capability_auth_root}/whatsapp/auth_info".to_string(),
                    )]),
                    cwd: Some("{scope_capabilities_root}/bots/whatsapp".to_string()),
                    auto_restart: true,
                    restart_max_backoff_secs: 45,
                },
            )
            .await
            .expect("upsert bot");

        let written = fs::read_to_string(&path).await.expect("written config");
        let document: Value = serde_yaml::from_str(&written).expect("parsed yaml value");
        let root = document.as_mapping().expect("root mapping");

        let metadata = root
            .get(Value::String("metadata".to_string()))
            .and_then(Value::as_mapping)
            .expect("metadata mapping");
        assert_eq!(
            metadata
                .get(Value::String("channel".to_string()))
                .and_then(Value::as_str),
            Some("consumer")
        );

        let bots = root
            .get(Value::String("bots".to_string()))
            .and_then(Value::as_mapping)
            .expect("bots mapping");
        assert!(bots.contains_key(Value::String("telegram".to_string())));
        let whatsapp = bots
            .get(Value::String("whatsapp".to_string()))
            .cloned()
            .expect("whatsapp config");
        let whatsapp: BotProcessConfig =
            serde_yaml::from_value(whatsapp).expect("deserialize whatsapp config");
        assert_eq!(
            whatsapp.args,
            vec!["{scope_capabilities_root}/bots/whatsapp/dist/index.js"]
        );
        assert_eq!(whatsapp.restart_max_backoff_secs, 45);
    }

    #[tokio::test]
    async fn delete_bot_removes_empty_bots_section() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("bot_configs.yaml");
        fs::write(
            &path,
            r#"
enabled: true
bots:
  telegram:
    enabled: true
    command: node
"#,
        )
        .await
        .expect("seed config");

        let store = BotConfigStore::new(path.clone());
        let removed = store.delete_bot("telegram").await.expect("delete bot");
        assert!(removed);

        let written = fs::read_to_string(&path).await.expect("written config");
        let document: Value = serde_yaml::from_str(&written).expect("parsed yaml value");
        let root = document.as_mapping().expect("root mapping");
        assert!(!root.contains_key(Value::String("bots".to_string())));
    }

    fn sample_config() -> BotProcessConfig {
        BotProcessConfig {
            enabled: false,
            command: "node".to_string(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
            auto_restart: true,
            restart_max_backoff_secs: 30,
        }
    }

    /// Two stores writing the same config file must leave parseable YAML and no
    /// staging file.
    ///
    /// The two stores hold independent write mutexes, so one of the two bots is
    /// expected to be lost to the read-modify-write race — Phase 3's problem,
    /// deliberately not asserted. What must hold is that a reader never finds a
    /// half-written config or a leftover temp.
    #[tokio::test]
    async fn concurrent_writes_leave_parseable_yaml_and_no_staging_file() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("bot_configs.yaml");

        let first = BotConfigStore::new(path.clone());
        let second = BotConfigStore::new(path.clone());
        let config = sample_config();

        let (first_result, second_result) = tokio::join!(
            first.upsert_bot("writer-a", &config),
            second.upsert_bot("writer-b", &config),
        );
        first_result.expect("first upsert should succeed");
        second_result.expect("second upsert should succeed");

        let staging: Vec<String> = std::fs::read_dir(dir.path())
            .expect("config directory listing")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(
            staging.is_empty(),
            "durable writes must leave no staging file, found {staging:?}"
        );

        let written = fs::read_to_string(&path).await.expect("written config");
        let document: Value = serde_yaml::from_str(&written).expect("parsed yaml value");
        let bots = document
            .as_mapping()
            .and_then(|root| root.get(Value::String("bots".to_string())))
            .and_then(Value::as_mapping)
            .expect("bots mapping");
        assert!(
            !bots.is_empty(),
            "a completed upsert must survive the concurrent write"
        );
    }

    /// The durable writer owns its staging file, so a tightened mode is
    /// re-applied to the published path. This config carries bot env values, so
    /// a write must not widen it back to the process umask default.
    #[cfg(unix)]
    #[tokio::test]
    async fn write_document_preserves_a_tightened_file_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("bot_configs.yaml");
        fs::write(&path, "bots: {}\n").await.expect("seed config");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("tighten mode");

        let store = BotConfigStore::new(path.clone());
        store
            .upsert_bot("telegram", &sample_config())
            .await
            .expect("upsert bot");

        let mode = std::fs::metadata(&path)
            .expect("config metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "the published config must keep its mode");
    }
}
