//! Canonical scope and object identity. Human-readable labels are metadata.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::StorageError;

const PRINCIPAL_MAX: usize = 128;
const WORKSPACE_MAX: usize = 128;
const NAMESPACE_MAX: usize = 64;
const OBJECT_MAX: usize = 256;

/// Tenant principal. Bounded, one canonical encoding, never a path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct PrincipalId(String);

/// Tenant workspace. Bounded, one canonical encoding, never a path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct WorkspaceId(String);

/// Tenant pair that Magician-storage owns. API middleware must produce this
/// type rather than a parallel principal/workspace tuple.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScopeId {
    pub principal: PrincipalId,
    pub workspace: WorkspaceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StorageScope {
    Tenant(ScopeId),
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct StorageNamespace(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct LogicalObjectId(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StorageKey {
    pub scope: StorageScope,
    pub namespace: StorageNamespace,
    pub object: LogicalObjectId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DigestAlgorithm {
    Sha256,
    Blake3,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentDigest {
    pub algorithm: DigestAlgorithm,
    pub hex: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PublicationId(Uuid);

impl PrincipalId {
    pub fn parse(raw: &str) -> Result<Self, StorageError> {
        parse_segment(raw, PRINCIPAL_MAX).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl WorkspaceId {
    pub fn parse(raw: &str) -> Result<Self, StorageError> {
        parse_segment(raw, WORKSPACE_MAX).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl ScopeId {
    pub fn new(principal: PrincipalId, workspace: WorkspaceId) -> Self {
        Self {
            principal,
            workspace,
        }
    }
}

impl StorageNamespace {
    pub fn parse(raw: &str) -> Result<Self, StorageError> {
        if raw.is_empty() || raw.len() > NAMESPACE_MAX {
            return Err(StorageError::invalid_key("namespace length"));
        }
        let mut chars = raw.chars();
        let first = chars.next().expect("non-empty");
        if !first.is_ascii_lowercase() {
            return Err(StorageError::invalid_key("namespace must start with a-z"));
        }
        if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
            return Err(StorageError::invalid_key("namespace charset"));
        }
        Ok(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl LogicalObjectId {
    pub fn parse(raw: &str) -> Result<Self, StorageError> {
        parse_segment(raw, OBJECT_MAX).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl StorageKey {
    pub fn tenant(
        principal: &str,
        workspace: &str,
        namespace: &str,
        object: &str,
    ) -> Result<Self, StorageError> {
        Ok(Self {
            scope: StorageScope::Tenant(ScopeId::new(
                PrincipalId::parse(principal)?,
                WorkspaceId::parse(workspace)?,
            )),
            namespace: StorageNamespace::parse(namespace)?,
            object: LogicalObjectId::parse(object)?,
        })
    }

    pub fn system(namespace: &str, object: &str) -> Result<Self, StorageError> {
        Ok(Self {
            scope: StorageScope::System,
            namespace: StorageNamespace::parse(namespace)?,
            object: LogicalObjectId::parse(object)?,
        })
    }

    /// One canonical encoding. Never contains unvalidated path segments.
    pub fn encode(&self) -> String {
        match &self.scope {
            StorageScope::Tenant(scope) => format!(
                "t/{}/{}/{}/{}",
                scope.principal.as_str(),
                scope.workspace.as_str(),
                self.namespace.as_str(),
                self.object.as_str()
            ),
            StorageScope::System => {
                format!("s/{}/{}", self.namespace.as_str(), self.object.as_str())
            },
        }
    }

    pub fn decode(raw: &str) -> Result<Self, StorageError> {
        let parts: Vec<&str> = raw.split('/').collect();
        match parts.as_slice() {
            ["t", principal, workspace, namespace, object] => {
                Self::tenant(principal, workspace, namespace, object)
            },
            ["s", namespace, object] => Self::system(namespace, object),
            _ => Err(StorageError::invalid_key("canonical key shape")),
        }
    }
}

impl ContentDigest {
    pub fn parse(algorithm: DigestAlgorithm, hex: &str) -> Result<Self, StorageError> {
        let expected = match algorithm {
            DigestAlgorithm::Sha256 => 64,
            DigestAlgorithm::Blake3 => 64,
        };
        if hex.len() != expected || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(StorageError::invalid_key("digest hex"));
        }
        Ok(Self {
            algorithm,
            hex: hex.to_ascii_lowercase(),
        })
    }
}

impl PublicationId {
    pub fn new(id: Uuid) -> Self {
        Self(id)
    }

    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl fmt::Display for StorageKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.encode())
    }
}

impl FromStr for StorageKey {
    type Err = StorageError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::decode(s)
    }
}

impl<'de> Deserialize<'de> for PrincipalId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

impl<'de> Deserialize<'de> for WorkspaceId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

impl<'de> Deserialize<'de> for StorageNamespace {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

impl<'de> Deserialize<'de> for LogicalObjectId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

fn parse_segment(raw: &str, max: usize) -> Result<String, StorageError> {
    if raw.is_empty() || raw.len() > max {
        return Err(StorageError::invalid_key("segment length"));
    }
    if raw == "." || raw == ".." {
        return Err(StorageError::invalid_key("path segment"));
    }
    if !raw
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(StorageError::invalid_key("segment charset"));
    }
    Ok(raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_key_round_trips() {
        let key = StorageKey::tenant("alice", "home", "tasks", "task-1").unwrap();
        assert_eq!(key.encode(), "t/alice/home/tasks/task-1");
        assert_eq!(StorageKey::decode(&key.encode()).unwrap(), key);
    }

    #[test]
    fn system_key_round_trips() {
        let key = StorageKey::system("leases", "scope-lock").unwrap();
        assert_eq!(key.encode(), "s/leases/scope-lock");
        assert_eq!(StorageKey::decode(&key.encode()).unwrap(), key);
    }

    #[test]
    fn rejects_path_segments_and_separators() {
        let too_long = "x".repeat(129);
        for raw in ["../etc", "a/b", "a\\b", "", too_long.as_str(), ".", ".."] {
            assert!(
                PrincipalId::parse(raw).is_err(),
                "principal should reject {raw:?}"
            );
        }
        assert!(StorageNamespace::parse("Tasks").is_err());
        assert!(StorageKey::decode("t/alice/home/tasks").is_err());
        assert!(StorageKey::decode("/etc/passwd").is_err());
    }

    #[test]
    fn tenant_and_system_scopes_do_not_collide() {
        let tenant = StorageKey::tenant("sys", "sys", "leases", "x").unwrap();
        let system = StorageKey::system("leases", "x").unwrap();
        assert_ne!(tenant.encode(), system.encode());
        assert!(tenant.encode().starts_with("t/"));
        assert!(system.encode().starts_with("s/"));
    }

    #[test]
    fn digest_is_lowercase_hex() {
        let hex = "A".repeat(64);
        let digest = ContentDigest::parse(DigestAlgorithm::Sha256, &hex).unwrap();
        assert_eq!(digest.hex, "a".repeat(64));
        assert!(ContentDigest::parse(DigestAlgorithm::Sha256, "zz").is_err());
    }

    #[test]
    fn serde_rejects_unparsed_path_segments() {
        assert!(serde_json::from_str::<PrincipalId>(r#""../etc""#).is_err());
        assert!(serde_json::from_str::<WorkspaceId>(r#""a/b""#).is_err());
        assert!(serde_json::from_str::<StorageNamespace>(r#""Tasks""#).is_err());
        assert!(serde_json::from_str::<LogicalObjectId>(r#""..""#).is_err());
        let ok: PrincipalId = serde_json::from_str(r#""alice""#).unwrap();
        assert_eq!(ok.as_str(), "alice");
    }
}
