//! Storage bootstrap documents. Loaded before the selected store is opened.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const CURRENT_PROFILE_SCHEMA: u32 = 1;
pub const BOOTSTRAP_ENV: &str = "MAGICIAN_STORAGE_BOOTSTRAP_CONFIG";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileKind {
    LocalEmbedded,
    LocalSilverbulletSpace,
    RemoteDurable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapSource {
    MissingDefault,
    File(PathBuf),
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ProfileError {
    #[error("storage bootstrap not found: {0}")]
    MissingFile(String),
    #[error("storage bootstrap schema {found} is newer than {CURRENT_PROFILE_SCHEMA}")]
    NewerSchema { found: u32 },
    #[error("unknown storage profile {0}")]
    UnknownProfile(String),
    #[error("invalid storage bootstrap: {0}")]
    Invalid(String),
    #[error("inline secret forbidden in storage bootstrap: {0}")]
    InlineSecret(String),
    #[error("weak TLS forbidden for remote durable storage")]
    WeakTls,
    #[error("sqlite/duckdb cannot be the remote durable state driver")]
    SqliteOnRemote,
    #[error("object and dataset prefixes overlap")]
    OverlappingPrefix,
    #[error("scratch root sits inside the object store")]
    ScratchInsideObjects,
    #[error("leases are incompatible with the selected state driver")]
    IncompatibleLease,
    #[error("unreadable storage bootstrap: {0}")]
    Io(String),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DriverConfig {
    #[serde(default)]
    pub driver: Option<String>,
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub dsn: Option<String>,
    #[serde(default)]
    pub dsn_env: Option<String>,
    #[serde(default)]
    pub tls: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub endpoint_env: Option<String>,
    #[serde(default)]
    pub region_env: Option<String>,
    #[serde(default)]
    pub bucket: Option<String>,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub credentials: Option<String>,
    #[serde(default)]
    pub credentials_ref: Option<String>,
    #[serde(default)]
    pub encryption: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub max_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageProfileDocument {
    pub schema_version: u32,
    pub profile: String,
    #[serde(default)]
    pub state: DriverConfig,
    #[serde(default)]
    pub objects: DriverConfig,
    #[serde(default)]
    pub datasets: DriverConfig,
    #[serde(default)]
    pub indexes: DriverConfig,
    #[serde(default)]
    pub leases: DriverConfig,
    #[serde(default)]
    pub scratch: DriverConfig,
    #[serde(default)]
    pub secrets: DriverConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedStorageProfile {
    pub kind: ProfileKind,
    pub source: BootstrapSource,
    pub document: StorageProfileDocument,
}

#[derive(Debug, Clone, Default)]
pub struct ResolveOptions<'a> {
    pub cli_path: Option<&'a Path>,
    pub env_path: Option<&'a Path>,
    /// Tests must pass an explicit path and leave this false.
    pub allow_ambient: bool,
}

impl Default for StorageProfileDocument {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_PROFILE_SCHEMA,
            profile: "local_embedded".into(),
            state: DriverConfig::default(),
            objects: DriverConfig::default(),
            datasets: DriverConfig::default(),
            indexes: DriverConfig::default(),
            leases: DriverConfig::default(),
            scratch: DriverConfig::default(),
            secrets: DriverConfig::default(),
        }
    }
}

impl StorageProfileDocument {
    pub fn from_yaml(text: &str) -> Result<Self, ProfileError> {
        serde_yaml::from_str(text).map_err(|err| ProfileError::Invalid(err.to_string()))
    }

    pub fn kind(&self) -> Result<ProfileKind, ProfileError> {
        match self.profile.as_str() {
            "local_embedded" => Ok(ProfileKind::LocalEmbedded),
            "local_silverbullet_space" => Ok(ProfileKind::LocalSilverbulletSpace),
            "remote_durable" => Ok(ProfileKind::RemoteDurable),
            other => Err(ProfileError::UnknownProfile(other.to_string())),
        }
    }

    pub fn validate(&self) -> Result<ProfileKind, ProfileError> {
        if self.schema_version > CURRENT_PROFILE_SCHEMA {
            return Err(ProfileError::NewerSchema {
                found: self.schema_version,
            });
        }
        if self.schema_version == 0 {
            return Err(ProfileError::Invalid("schema_version must be >= 1".into()));
        }
        let kind = self.kind()?;
        forbid_inline_secrets(self)?;
        match kind {
            ProfileKind::RemoteDurable => validate_remote(self)?,
            ProfileKind::LocalEmbedded | ProfileKind::LocalSilverbulletSpace => {
                validate_local(self)?;
            },
        }
        Ok(kind)
    }
}

pub fn resolve(options: ResolveOptions<'_>) -> Result<ResolvedStorageProfile, ProfileError> {
    if let Some(path) = options.cli_path {
        return load_file(path);
    }
    if let Some(path) = options.env_path {
        return load_file(path);
    }
    if options.allow_ambient {
        let server = Path::new("/etc/magician/storage.yaml");
        if server.is_file() {
            return load_file(server);
        }
    }
    let document = StorageProfileDocument::default();
    let kind = document.validate()?;
    Ok(ResolvedStorageProfile {
        kind,
        source: BootstrapSource::MissingDefault,
        document,
    })
}

fn load_file(path: &Path) -> Result<ResolvedStorageProfile, ProfileError> {
    let text = fs::read_to_string(path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            ProfileError::MissingFile(path.display().to_string())
        } else {
            ProfileError::Io(err.to_string())
        }
    })?;
    let document = StorageProfileDocument::from_yaml(&text)?;
    let kind = document.validate()?;
    Ok(ResolvedStorageProfile {
        kind,
        source: BootstrapSource::File(path.to_path_buf()),
        document,
    })
}

fn forbid_inline_secrets(doc: &StorageProfileDocument) -> Result<(), ProfileError> {
    for (name, cfg) in named_drivers(doc) {
        if cfg.password.is_some() || cfg.secret.is_some() || cfg.credentials.is_some() {
            return Err(ProfileError::InlineSecret(name.into()));
        }
        if let Some(dsn) = &cfg.dsn {
            if looks_like_secret(dsn) {
                return Err(ProfileError::InlineSecret(format!("{name}.dsn")));
            }
        }
        if let Some(dsn_env) = &cfg.dsn_env {
            if !is_environment_name(dsn_env) {
                return Err(ProfileError::InlineSecret(format!("{name}.dsn_env")));
            }
        }
        if let Some(endpoint_env) = &cfg.endpoint_env {
            if !is_environment_name(endpoint_env) {
                return Err(ProfileError::InlineSecret(format!("{name}.endpoint_env")));
            }
        }
        if let Some(region_env) = &cfg.region_env {
            if !is_environment_name(region_env) {
                return Err(ProfileError::InlineSecret(format!("{name}.region_env")));
            }
        }
        if let Some(credentials_ref) = &cfg.credentials_ref {
            if crate::secret::SecretRef::parse(credentials_ref).is_err() {
                return Err(ProfileError::InlineSecret(format!(
                    "{name}.credentials_ref"
                )));
            }
        }
        if let Some(endpoint) = &cfg.endpoint {
            if looks_like_secret(endpoint) {
                return Err(ProfileError::InlineSecret(format!("{name}.endpoint")));
            }
        }
    }
    Ok(())
}

fn looks_like_secret(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("password")
        || lower.contains("secret=")
        || lower.contains("access_key")
        || lower.contains("token=")
        || (value.contains("://") && value.contains('@'))
}

fn is_environment_name(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {
            chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        },
        _ => false,
    }
}

fn validate_local(doc: &StorageProfileDocument) -> Result<(), ProfileError> {
    if let (Some(objects), Some(scratch)) = (&doc.objects.root, &doc.scratch.root) {
        if path_is_inside(scratch, objects) {
            return Err(ProfileError::ScratchInsideObjects);
        }
    }
    Ok(())
}

fn validate_remote(doc: &StorageProfileDocument) -> Result<(), ProfileError> {
    let state_driver = doc
        .state
        .driver
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if matches!(
        state_driver.as_str(),
        "sqlite" | "sqlite3" | "duckdb" | "libsql"
    ) {
        return Err(ProfileError::SqliteOnRemote);
    }
    match doc.state.tls.as_deref() {
        Some("require") => {},
        _ => return Err(ProfileError::WeakTls),
    }
    let dsn_env = doc
        .state
        .dsn_env
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if dsn_env.is_none() || doc.state.dsn.is_some() {
        return Err(ProfileError::Invalid(
            "remote state requires dsn_env, not an inline DSN".into(),
        ));
    }
    if let Some(encryption) = &doc.objects.encryption {
        if encryption != "required" {
            return Err(ProfileError::Invalid(
                "remote objects.encryption must be required".into(),
            ));
        }
    } else {
        return Err(ProfileError::Invalid(
            "remote objects.encryption must be required".into(),
        ));
    }
    let object_prefix = namespace_prefix(doc.objects.prefix.as_deref().unwrap_or(""));
    let dataset_prefix = namespace_prefix(doc.datasets.prefix.as_deref().unwrap_or(""));
    if object_prefix.is_empty() || dataset_prefix.is_empty() {
        return Err(ProfileError::OverlappingPrefix);
    }
    if prefixes_overlap(object_prefix, dataset_prefix) {
        return Err(ProfileError::OverlappingPrefix);
    }
    if doc.leases.driver.as_deref() != Some("state_database") {
        return Err(ProfileError::IncompatibleLease);
    }
    if let (Some(objects), Some(scratch)) = (&doc.objects.root, &doc.scratch.root) {
        if path_is_inside(scratch, objects) {
            return Err(ProfileError::ScratchInsideObjects);
        }
    }
    for cfg in [&doc.objects, &doc.datasets] {
        if let Some(endpoint) = &cfg.endpoint {
            if !endpoint.starts_with("https://") {
                return Err(ProfileError::WeakTls);
            }
        }
    }
    Ok(())
}

fn named_drivers(doc: &StorageProfileDocument) -> [(&'static str, &DriverConfig); 7] {
    [
        ("state", &doc.state),
        ("objects", &doc.objects),
        ("datasets", &doc.datasets),
        ("indexes", &doc.indexes),
        ("leases", &doc.leases),
        ("scratch", &doc.scratch),
        ("secrets", &doc.secrets),
    ]
}

fn path_is_inside(child: &str, parent: &str) -> bool {
    let child = Path::new(child);
    let parent = Path::new(parent);
    child.starts_with(parent)
}

fn namespace_prefix(raw: &str) -> &str {
    raw.trim_matches('/')
}

fn prefixes_overlap(left: &str, right: &str) -> bool {
    left == right
        || left.starts_with(&format!("{right}/"))
        || right.starts_with(&format!("{left}/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(contents: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "magician-storage-profile-{}.yaml",
            uuid::Uuid::new_v4()
        ));
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(contents.as_bytes()).unwrap();
        path
    }

    #[test]
    fn missing_bootstrap_is_local_embedded() {
        let resolved = resolve(ResolveOptions {
            cli_path: None,
            env_path: None,
            allow_ambient: false,
        })
        .unwrap();
        assert_eq!(resolved.kind, ProfileKind::LocalEmbedded);
        assert_eq!(resolved.source, BootstrapSource::MissingDefault);
    }

    #[test]
    fn unknown_profile_fails() {
        let err = StorageProfileDocument::from_yaml("schema_version: 1\nprofile: mystery\n")
            .unwrap()
            .validate()
            .unwrap_err();
        assert!(matches!(err, ProfileError::UnknownProfile(_)));
    }

    #[test]
    fn newer_schema_fails() {
        let err =
            StorageProfileDocument::from_yaml("schema_version: 99\nprofile: local_embedded\n")
                .unwrap()
                .validate()
                .unwrap_err();
        assert!(matches!(err, ProfileError::NewerSchema { found: 99 }));
    }

    #[test]
    fn inline_secret_fails() {
        let err = StorageProfileDocument::from_yaml(
            "schema_version: 1\nprofile: local_embedded\nstate:\n  password: hunter2\n",
        )
        .unwrap()
        .validate()
        .unwrap_err();
        assert!(matches!(err, ProfileError::InlineSecret(_)));
    }

    #[test]
    fn remote_without_tls_fails() {
        let err = StorageProfileDocument::from_yaml(
            r#"
schema_version: 1
profile: remote_durable
state:
  driver: postgres
  dsn_env: MAGICIAN_STATE_DATABASE_URL
  tls: disable
objects:
  encryption: required
leases:
  driver: state_database
"#,
        )
        .unwrap()
        .validate()
        .unwrap_err();
        assert_eq!(err, ProfileError::WeakTls);
    }

    #[test]
    fn sqlite_remote_fails() {
        let err = StorageProfileDocument::from_yaml(
            r#"
schema_version: 1
profile: remote_durable
state:
  driver: sqlite
  dsn_env: MAGICIAN_STATE_DATABASE_URL
  tls: require
objects:
  encryption: required
leases:
  driver: state_database
"#,
        )
        .unwrap()
        .validate()
        .unwrap_err();
        assert_eq!(err, ProfileError::SqliteOnRemote);
    }

    #[test]
    fn overlapping_prefixes_fail() {
        let err = StorageProfileDocument::from_yaml(
            r#"
schema_version: 1
profile: remote_durable
state:
  driver: postgres
  dsn_env: MAGICIAN_STATE_DATABASE_URL
  tls: require
objects:
  prefix: production
  encryption: required
datasets:
  prefix: production/datasets
leases:
  driver: state_database
"#,
        )
        .unwrap()
        .validate()
        .unwrap_err();
        assert_eq!(err, ProfileError::OverlappingPrefix);
    }

    #[test]
    fn incompatible_lease_fails() {
        let err = StorageProfileDocument::from_yaml(
            r#"
schema_version: 1
profile: remote_durable
state:
  driver: postgres
  dsn_env: MAGICIAN_STATE_DATABASE_URL
  tls: require
objects:
  prefix: production/objects
  encryption: required
datasets:
  prefix: production/datasets
leases:
  driver: filesystem
"#,
        )
        .unwrap()
        .validate()
        .unwrap_err();
        assert_eq!(err, ProfileError::IncompatibleLease);
    }

    #[test]
    fn remote_http_endpoint_fails() {
        let err = StorageProfileDocument::from_yaml(
            r#"
schema_version: 1
profile: remote_durable
state:
  driver: postgres
  dsn_env: MAGICIAN_STATE_DATABASE_URL
  tls: require
objects:
  encryption: required
  prefix: production/objects
  endpoint: http://s3.internal
datasets:
  prefix: production/datasets
leases:
  driver: state_database
"#,
        )
        .unwrap()
        .validate()
        .unwrap_err();
        assert_eq!(err, ProfileError::WeakTls);
    }

    #[test]
    fn valid_remote_round_trips() {
        let yaml = r#"
schema_version: 1
profile: remote_durable
state:
  driver: postgres
  dsn_env: MAGICIAN_STATE_DATABASE_URL
  tls: require
objects:
  driver: s3
  bucket: magician-data
  prefix: production/objects
  credentials_ref: object-store/production
  encryption: required
datasets:
  driver: s3
  prefix: production/datasets
leases:
  driver: state_database
scratch:
  root: /var/lib/magician/scratch
secrets:
  driver: external
"#;
        let path = write_temp(yaml);
        let resolved = resolve(ResolveOptions {
            cli_path: Some(&path),
            env_path: None,
            allow_ambient: false,
        })
        .unwrap();
        assert_eq!(resolved.kind, ProfileKind::RemoteDurable);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn missing_named_file_fails_loud() {
        let err = resolve(ResolveOptions {
            cli_path: Some(Path::new("/no/such/storage.yaml")),
            env_path: None,
            allow_ambient: false,
        })
        .unwrap_err();
        assert!(matches!(err, ProfileError::MissingFile(_)));
    }

    #[test]
    fn default_document_round_trips() {
        let yaml = serde_yaml::to_string(&StorageProfileDocument::default()).unwrap();
        let parsed = StorageProfileDocument::from_yaml(&yaml).unwrap();
        assert_eq!(parsed.validate().unwrap(), ProfileKind::LocalEmbedded);
    }

    #[test]
    fn example_documents_validate() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples");
        let local = fs::read_to_string(root.join("local_embedded.yaml")).unwrap();
        assert_eq!(
            StorageProfileDocument::from_yaml(&local)
                .unwrap()
                .validate()
                .unwrap(),
            ProfileKind::LocalEmbedded
        );
        let remote = fs::read_to_string(root.join("remote_durable.yaml")).unwrap();
        assert_eq!(
            StorageProfileDocument::from_yaml(&remote)
                .unwrap()
                .validate()
                .unwrap(),
            ProfileKind::RemoteDurable
        );
    }
}
