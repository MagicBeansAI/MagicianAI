use std::fmt;

use async_trait::async_trait;
use zeroize::Zeroizing;

use crate::error::StorageError;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SecretRef(String);

impl SecretRef {
    pub fn parse(raw: &str) -> Result<Self, StorageError> {
        if raw.is_empty() || raw.len() > 256 {
            return Err(StorageError::invalid_key("secret ref length"));
        }
        // Profile credentials_ref values are LogicalObjectId segments joined
        // by `/` (example: `object-store/production`). Each segment is the
        // same charset as a storage object id; the slash is a namespace
        // separator, not a filesystem path.
        for part in raw.split('/') {
            crate::identifiers::LogicalObjectId::parse(part)
                .map_err(|_| StorageError::invalid_key("secret ref"))?;
        }
        Ok(Self(raw.to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretPurpose {
    ObjectStore,
    Database,
    Index,
    Bootstrap,
}

/// Non-serializable secret wrapper. Debug is redacted; values are zeroized.
pub struct SecretValue(Zeroizing<Vec<u8>>);

impl SecretValue {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }

    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretValue(redacted)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretMetadata {
    pub reference: SecretRef,
    pub revision: String,
}

#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn resolve(
        &self,
        reference: &SecretRef,
        purpose: SecretPurpose,
    ) -> Result<SecretValue, StorageError>;
    async fn metadata(&self, reference: &SecretRef) -> Result<SecretMetadata, StorageError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_separated_credentials_ref_parses() {
        let parsed = SecretRef::parse("object-store/production").unwrap();
        assert_eq!(parsed.as_str(), "object-store/production");
        assert!(SecretRef::parse("object-store/prod uction").is_err());
        assert!(SecretRef::parse("../escape").is_err());
    }

    #[test]
    fn secret_value_is_not_serializable_and_debug_is_redacted() {
        let secret = SecretValue::new(b"super-secret".to_vec());
        let rendered = format!("{secret:?}");
        assert_eq!(rendered, "SecretValue(redacted)");
        assert!(!rendered.contains("super-secret"));
    }
}
