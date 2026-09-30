use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use magician_storage::{
    can_advance_readiness, validate_readiness_evidence, EvidenceLink, ReadinessState,
    StorageCatalogId, StorageError,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClosurePacket {
    pub owner_id: StorageCatalogId,
    pub state: ReadinessState,
    pub evidence: BTreeMap<String, EvidenceLink>,
}

pub struct ClosureCoordinator {
    root: PathBuf,
}

impl ClosureCoordinator {
    pub async fn open(root: impl AsRef<Path>) -> Result<Self, StorageError> {
        let root = root.as_ref().join("readiness");
        tokio::fs::create_dir_all(&root)
            .await
            .map_err(|err| StorageError::backend(err.to_string()))?;
        Ok(Self { root })
    }

    pub async fn load(&self, owner_id: &StorageCatalogId) -> Result<ClosurePacket, StorageError> {
        let path = self.path(owner_id);
        match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|err| StorageError::Corrupt {
                detail: err.to_string(),
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(ClosurePacket {
                owner_id: owner_id.clone(),
                state: ReadinessState::Discovered,
                evidence: BTreeMap::new(),
            }),
            Err(err) => Err(StorageError::backend(err.to_string())),
        }
    }

    pub async fn advance(
        &self,
        owner_id: &StorageCatalogId,
        to: ReadinessState,
        evidence: BTreeMap<String, EvidenceLink>,
    ) -> Result<ClosurePacket, StorageError> {
        let mut packet = self.load(owner_id).await?;
        if packet.state == to {
            validate_readiness_evidence(to, &packet.evidence)?;
            return Ok(packet);
        }
        if !can_advance_readiness(packet.state, to) {
            return Err(StorageError::Conflict {
                expected: packet
                    .state
                    .successor()
                    .map(|state| state.as_str().to_string()),
                actual: Some(to.as_str().into()),
            });
        }
        for (key, link) in evidence {
            packet.evidence.insert(key, link);
        }
        validate_readiness_evidence(to, &packet.evidence)?;
        packet.state = to;
        self.persist(&packet).await?;
        Ok(packet)
    }

    async fn persist(&self, packet: &ClosurePacket) -> Result<(), StorageError> {
        let path = self.path(&packet.owner_id);
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(packet)
            .map_err(|err| StorageError::backend(err.to_string()))?;
        tokio::fs::write(&tmp, bytes)
            .await
            .map_err(|err| StorageError::backend(err.to_string()))?;
        tokio::fs::rename(&tmp, &path)
            .await
            .map_err(|err| StorageError::backend(err.to_string()))
    }

    fn path(&self, owner_id: &StorageCatalogId) -> PathBuf {
        self.root.join(format!("{}.json", owner_id.as_str()))
    }
}
