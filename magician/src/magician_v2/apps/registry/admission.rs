//! Cancellation-aware registry admission without an independent failure timer.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex, OnceLock, Weak},
};
use tokio::sync::{Mutex, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};

type LockTable<T> = StdMutex<HashMap<PathBuf, Weak<T>>>;

/// Separate registry services share the process-owned connection pool. Their
/// asynchronous gates must therefore identify the database too, not a service
/// instance. Otherwise a second service can fill its blocking workers while
/// waiting on maintenance started by the first service.
///
/// Owned guards and queued acquisitions retain each lock. Empty entries are
/// weak and swept; this is neither a database-count limit nor a filesystem scan.
fn shared_lock<T: Default>(table: &LockTable<T>, path: &Path) -> Arc<T> {
    let mut locks = table
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(lock) = locks.get(path).and_then(Weak::upgrade) {
        return lock;
    }
    if locks.len() >= 256 {
        locks.retain(|_, lock| lock.strong_count() > 0);
    }
    let lock = Arc::new(T::default());
    locks.insert(path.to_owned(), Arc::downgrade(&lock));
    lock
}

pub(crate) fn database_maintenance_lock(path: &Path) -> Arc<tokio::sync::RwLock<()>> {
    static LOCKS: OnceLock<LockTable<tokio::sync::RwLock<()>>> = OnceLock::new();
    shared_lock(LOCKS.get_or_init(Default::default), path)
}

pub(crate) fn database_write_lock(path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<LockTable<Mutex<()>>> = OnceLock::new();
    shared_lock(LOCKS.get_or_init(Default::default), path)
}

pub(crate) fn database_background_turnstile(path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<LockTable<Mutex<()>>> = OnceLock::new();
    shared_lock(LOCKS.get_or_init(Default::default), path)
}

/// Maintenance is awaited before consuming a blocking worker. Otherwise a
/// burst of readers to the maintained scope could exhaust every worker while
/// they wait on the synchronous SQLCipher gate, stalling unrelated scopes.
pub(crate) struct ScopedPermit {
    _slot: OwnedSemaphorePermit,
    _maintenance: tokio::sync::OwnedRwLockReadGuard<()>,
}

impl ScopedPermit {
    pub(crate) fn new(
        slot: OwnedSemaphorePermit,
        maintenance: tokio::sync::OwnedRwLockReadGuard<()>,
    ) -> Self {
        Self {
            _slot: slot,
            _maintenance: maintenance,
        }
    }
}

pub(crate) async fn scoped_read(
    slots: Arc<Semaphore>,
    maintenance: Arc<tokio::sync::RwLock<()>>,
) -> Result<ScopedPermit, AdmissionError> {
    let maintenance = maintenance.read_owned().await;
    Ok(ScopedPermit::new(read(slots).await?, maintenance))
}

pub(crate) async fn scoped_write(
    slots: Arc<Semaphore>,
    scope: Arc<Mutex<()>>,
    maintenance: Arc<tokio::sync::RwLock<()>>,
) -> Result<(ScopedPermit, OwnedMutexGuard<()>), AdmissionError> {
    let maintenance = maintenance.read_owned().await;
    let (slot, guard) = write(slots, scope).await?;
    Ok((ScopedPermit::new(slot, maintenance), guard))
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AdmissionError {
    Closed,
}

pub(crate) async fn read(slots: Arc<Semaphore>) -> Result<OwnedSemaphorePermit, AdmissionError> {
    slots
        .acquire_owned()
        .await
        .map_err(|_| AdmissionError::Closed)
}

pub(crate) async fn write(
    slots: Arc<Semaphore>,
    scope: Arc<Mutex<()>>,
) -> Result<(OwnedSemaphorePermit, OwnedMutexGuard<()>), AdmissionError> {
    loop {
        let permit = Arc::clone(&slots)
            .acquire_owned()
            .await
            .map_err(|_| AdmissionError::Closed)?;
        if let Ok(guard) = Arc::clone(&scope).try_lock_owned() {
            return Ok((permit, guard));
        }
        // Other scopes must retain access to the pool while this scope is busy.
        drop(permit);
        let guard = Arc::clone(&scope).lock_owned().await;
        if let Ok(permit) = Arc::clone(&slots).try_acquire_owned() {
            return Ok((permit, guard));
        }
        // Likewise, do not park a scope owner behind an awaited global slot.
        drop(guard);
        tokio::task::yield_now().await;
    }
}
