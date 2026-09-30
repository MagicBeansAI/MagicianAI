//! Process-wide scope ownership leases.
//!
//! One process may hold many scopes. A second process targeting a held local
//! scope is refused. Generation is the fencing token for singular writers.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::error::StorageError;
use crate::identifiers::ScopeId;
use crate::lease::{LeaseResource, LeaseStore, LeaseToken, OwnerId};

const DEFAULT_TTL: Duration = Duration::from_secs(24 * 60 * 60);

type LossCallback = Arc<dyn Fn(StorageError) + Send + Sync>;

pub fn lease_resource_for_scope(scope: &ScopeId) -> Result<LeaseResource, StorageError> {
    let raw = format!(
        "scope.{}.{}",
        scope.principal.as_str(),
        scope.workspace.as_str()
    );
    if raw.len() <= 256 {
        return LeaseResource::parse(&raw);
    }
    let hex = blake3::hash(raw.as_bytes()).to_hex();
    LeaseResource::parse(&format!("scope.{hex}"))
}

struct HeldScope {
    scope: ScopeId,
    token: LeaseToken,
}

/// Holds acquired scope leases for process lifetime.
pub struct ScopeLeaseManager {
    store: Arc<dyn LeaseStore>,
    owner: OwnerId,
    ttl: Duration,
    gate: tokio::sync::Mutex<()>,
    held: Mutex<HashMap<String, HeldScope>>,
    lease_lost: AtomicBool,
    on_loss: Mutex<Option<LossCallback>>,
}

impl ScopeLeaseManager {
    pub fn new(store: Arc<dyn LeaseStore>, owner: OwnerId) -> Self {
        Self {
            store,
            owner,
            ttl: DEFAULT_TTL,
            gate: tokio::sync::Mutex::new(()),
            held: Mutex::new(HashMap::new()),
            lease_lost: AtomicBool::new(false),
            on_loss: Mutex::new(None),
        }
    }

    pub fn set_on_loss(&self, callback: Arc<dyn Fn(StorageError) + Send + Sync>) {
        *self.on_loss.lock().expect("on_loss lock") = Some(callback);
    }

    pub fn lease_lost(&self) -> bool {
        self.lease_lost.load(Ordering::SeqCst)
    }

    pub async fn acquire(&self, scope: &ScopeId) -> Result<LeaseToken, StorageError> {
        let resource = lease_resource_for_scope(scope)?;
        let _gate = self.gate.lock().await;
        if self.lease_lost() {
            return Err(StorageError::LeaseLost {
                resource: resource.as_str().to_string(),
                generation: self.generation(scope).unwrap_or(0),
            });
        }
        {
            let held = self.held.lock().expect("held lock");
            if let Some(existing) = held.get(resource.as_str()) {
                return Ok(existing.token.clone());
            }
        }
        let token = self
            .store
            .acquire(resource.clone(), self.owner.clone(), self.ttl)
            .await?;
        if self.lease_lost() {
            return Err(StorageError::LeaseLost {
                resource: resource.as_str().to_string(),
                generation: token.generation,
            });
        }
        self.held.lock().expect("held lock").insert(
            resource.as_str().to_string(),
            HeldScope {
                scope: scope.clone(),
                token: token.clone(),
            },
        );
        Ok(token)
    }

    pub fn generation(&self, scope: &ScopeId) -> Option<u64> {
        let resource = lease_resource_for_scope(scope).ok()?;
        self.held
            .lock()
            .expect("held lock")
            .get(resource.as_str())
            .map(|held| held.token.generation)
    }

    pub fn held_scopes(&self) -> Vec<(ScopeId, u64)> {
        let mut out: Vec<_> = self
            .held
            .lock()
            .expect("held lock")
            .values()
            .map(|held| (held.scope.clone(), held.token.generation))
            .collect();
        out.sort_by(|a, b| {
            a.0.principal
                .as_str()
                .cmp(b.0.principal.as_str())
                .then(a.0.workspace.as_str().cmp(b.0.workspace.as_str()))
        });
        out
    }

    pub async fn renew_all(&self) -> Result<(), StorageError> {
        let _gate = self.gate.lock().await;
        let tokens: Vec<LeaseToken> = self
            .held
            .lock()
            .expect("held lock")
            .values()
            .map(|held| held.token.clone())
            .collect();
        for token in tokens {
            match self.store.renew(&token, self.ttl).await {
                Ok(next) => {
                    let mut held = self.held.lock().expect("held lock");
                    if let Some(entry) = held.get_mut(token.resource.as_str()) {
                        entry.token = next;
                    }
                },
                Err(err) => {
                    if matches!(err, StorageError::LeaseLost { .. }) {
                        self.lease_lost.store(true, Ordering::SeqCst);
                        if let Some(callback) = self.on_loss.lock().expect("on_loss lock").clone() {
                            callback(err.clone());
                        }
                    }
                    return Err(err);
                },
            }
        }
        Ok(())
    }

    pub async fn release_all(&self) -> Result<(), StorageError> {
        let tokens: Vec<LeaseToken> = {
            let mut held = self.held.lock().expect("held lock");
            held.drain().map(|(_, held)| held.token).collect()
        };
        for token in tokens {
            self.store.release(token).await?;
        }
        Ok(())
    }
}
