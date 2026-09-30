#![cfg(test)]
//! Focused checks against production admission and SQLCipher connection reuse.
#[cfg(test)]
#[path = "../../../magician/src/magician_v2/apps/registry/admission.rs"]
mod registry_admission;

#[path = "../../../magician/src/magician_v2/apps/registry/connection_pool.rs"]
mod registry_connections;

#[path = "../../../magician/src/magician_v2/apps/registry/metrics.rs"]
#[allow(dead_code)] // Async admission correlation lives in the parent registry.
mod metrics;

#[path = "../../../magician/src/magician_v2/apps/registry/maintenance.rs"]
#[allow(dead_code)] // HTTP report types are used by the server, outside this component lane.
mod registry_maintenance;

mod pool_contract;

#[tokio::test(start_paused = true)]
async fn independent_registry_owners_share_database_gates_without_hoarding_workers() {
    use std::sync::Arc;
    use tokio::sync::Semaphore;
    let root = tempfile::tempdir().unwrap();
    let path = root
        .path()
        .join("scopes/anonymous/default/apps/app_store.sqlite3");
    let other = root
        .path()
        .join("scopes/anonymous/other/apps/app_store.sqlite3");
    // Each lookup models an independently constructed service, not a clone
    // retaining the first service's private lock map.
    let maintenance = registry_admission::database_maintenance_lock(&path)
        .write_owned()
        .await;
    let owners = [Arc::new(Semaphore::new(2)), Arc::new(Semaphore::new(2))];
    let mut waiting = Vec::new();
    for index in 0..32 {
        let path = path.clone();
        let slots = Arc::clone(&owners[index % owners.len()]);
        waiting.push(tokio::spawn(async move {
            if index % 2 == 0 {
                drop(
                    registry_admission::scoped_read(
                        slots,
                        registry_admission::database_maintenance_lock(&path),
                    )
                    .await
                    .unwrap(),
                );
            } else {
                drop(
                    registry_admission::scoped_write(
                        slots,
                        registry_admission::database_write_lock(&path),
                        registry_admission::database_maintenance_lock(&path),
                    )
                    .await
                    .unwrap(),
                );
            }
        }));
    }
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(60)).await;
    assert!(waiting.iter().all(|job| !job.is_finished()));
    for slots in &owners {
        assert_eq!(slots.available_permits(), 2);
        let reader = registry_admission::scoped_read(
            Arc::clone(slots),
            registry_admission::database_maintenance_lock(&other),
        )
        .await
        .unwrap();
        let writer = registry_admission::scoped_write(
            Arc::clone(slots),
            registry_admission::database_write_lock(&other),
            registry_admission::database_maintenance_lock(&other),
        )
        .await
        .unwrap();
        assert_eq!(slots.available_permits(), 0);
        drop((reader, writer));
    }
    waiting[0].abort();
    assert!(waiting.remove(0).await.unwrap_err().is_cancelled());
    drop(maintenance);
    for job in waiting {
        job.await.unwrap();
    }
    assert!(owners.iter().all(|slots| slots.available_permits() == 2));
    assert!(
        !path.exists(),
        "admission must not initialize an absent App store"
    );
}

#[tokio::test]
async fn database_gates_retain_owned_guards_and_isolate_runtime_roots() {
    let first_root = tempfile::tempdir().unwrap();
    let second_root = tempfile::tempdir().unwrap();
    let first = first_root
        .path()
        .join("scopes/anonymous/default/apps/app_store.sqlite3");
    let second = second_root
        .path()
        .join("scopes/anonymous/default/apps/app_store.sqlite3");
    let owner = registry_admission::database_write_lock(&first)
        .lock_owned()
        .await;
    assert!(registry_admission::database_write_lock(&first)
        .try_lock_owned()
        .is_err());
    assert!(registry_admission::database_write_lock(&second)
        .try_lock_owned()
        .is_ok());
    // Background fairness is independent of ordinary foreground ownership.
    let background = registry_admission::database_background_turnstile(&first)
        .lock_owned()
        .await;
    assert!(registry_admission::database_background_turnstile(&first)
        .try_lock_owned()
        .is_err());
    assert!(registry_admission::database_background_turnstile(&second)
        .try_lock_owned()
        .is_ok());
    drop(owner);
    assert!(registry_admission::database_write_lock(&first)
        .try_lock_owned()
        .is_ok());
    drop(background);
    assert!(registry_admission::database_background_turnstile(&first)
        .try_lock_owned()
        .is_ok());
}

#[tokio::test(start_paused = true)]
async fn maintenance_waiters_do_not_take_slots_from_other_scopes() {
    use std::sync::Arc;
    use tokio::sync::{Mutex, RwLock, Semaphore};
    let slots = Arc::new(Semaphore::new(2));
    let maintained = Arc::new(RwLock::new(()));
    let maintenance = Arc::clone(&maintained).write_owned().await;
    let mut waiting = Vec::new();
    for index in 0..32 {
        let slots = Arc::clone(&slots);
        let maintained = Arc::clone(&maintained);
        waiting.push(tokio::spawn(async move {
            if index % 2 == 0 {
                drop(
                    registry_admission::scoped_read(slots, maintained)
                        .await
                        .unwrap(),
                );
            } else {
                drop(
                    registry_admission::scoped_write(slots, Arc::new(Mutex::new(())), maintained)
                        .await
                        .unwrap(),
                );
            }
        }));
    }
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(60)).await;
    assert!(waiting.iter().all(|task| !task.is_finished()));
    assert_eq!(slots.available_permits(), 2);
    drop(
        registry_admission::scoped_read(Arc::clone(&slots), Arc::new(RwLock::new(())))
            .await
            .unwrap(),
    );
    waiting[0].abort();
    drop(maintenance);
    for (index, task) in waiting.into_iter().enumerate() {
        if index == 0 {
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            task.await.unwrap();
        }
    }
    assert_eq!(slots.available_permits(), 2);
}

#[tokio::test(start_paused = true)]
async fn registry_writer_waits_for_capacity_without_holding_the_scope() {
    use std::sync::Arc;
    use tokio::sync::{Mutex, Semaphore};
    let slots = Arc::new(Semaphore::new(1));
    let scope = Arc::new(Mutex::new(()));
    let occupied = Arc::clone(&slots).acquire_owned().await.unwrap();
    let waiting = tokio::spawn(registry_admission::write(
        Arc::clone(&slots),
        Arc::clone(&scope),
    ));
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(60)).await;
    assert!(
        !waiting.is_finished(),
        "brief pool contention must queue, not fail the App"
    );
    assert!(Arc::clone(&scope).try_lock_owned().is_ok());
    drop(occupied);
    let admitted = waiting.await.unwrap().unwrap();
    assert_eq!(slots.available_permits(), 0);
    assert!(Arc::clone(&scope).try_lock_owned().is_err());
    drop(admitted);
    assert_eq!(slots.available_permits(), 1);
    assert!(scope.try_lock_owned().is_ok());
}

#[tokio::test(start_paused = true)]
async fn registry_busy_scope_does_not_hoard_slots_from_other_work() {
    use std::sync::Arc;
    use tokio::sync::{Mutex, Semaphore};
    let slots = Arc::new(Semaphore::new(1));
    let scope = Arc::new(Mutex::new(()));
    let occupied = Arc::clone(&scope).lock_owned().await;
    let waiting = tokio::spawn(registry_admission::write(
        Arc::clone(&slots),
        Arc::clone(&scope),
    ));
    tokio::task::yield_now().await;
    let other = registry_admission::read(Arc::clone(&slots)).await.unwrap();
    assert!(!waiting.is_finished());
    drop(other);
    drop(occupied);
    drop(waiting.await.unwrap().unwrap());
    assert_eq!(slots.available_permits(), 1);
}

#[tokio::test(start_paused = true)]
async fn registry_read_burst_waits_beyond_the_former_quarter_second_window() {
    use std::sync::Arc;
    use tokio::sync::Semaphore;
    let slots = Arc::new(Semaphore::new(1));
    let occupied = Arc::clone(&slots).acquire_owned().await.unwrap();
    let waiting = tokio::spawn(registry_admission::read(Arc::clone(&slots)));
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(60)).await;
    assert!(!waiting.is_finished());
    drop(occupied);
    drop(waiting.await.unwrap().unwrap());
    assert_eq!(slots.available_permits(), 1);
}

#[tokio::test(start_paused = true)]
async fn registry_caller_deadline_and_cancellation_release_all_waiting_ownership() {
    use std::sync::Arc;
    use tokio::sync::{Mutex, Semaphore};
    let slots = Arc::new(Semaphore::new(1));
    let scope = Arc::new(Mutex::new(()));
    let occupied = Arc::clone(&slots).acquire_owned().await.unwrap();
    let waiting_slots = Arc::clone(&slots);
    let waiting_scope = Arc::clone(&scope);
    let waiting = tokio::spawn(async move {
        // Only the caller's actual operation deadline bounds queued admission.
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            registry_admission::write(waiting_slots, waiting_scope),
        )
        .await
    });
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    assert!(waiting.await.unwrap().is_err());
    assert!(Arc::clone(&scope).try_lock_owned().is_ok());
    drop(occupied);
    let occupied = Arc::clone(&scope).lock_owned().await;
    let cancelled = tokio::spawn(registry_admission::write(
        Arc::clone(&slots),
        Arc::clone(&scope),
    ));
    tokio::task::yield_now().await;
    cancelled.abort();
    assert!(cancelled.await.unwrap_err().is_cancelled());
    drop(occupied);
    assert_eq!(slots.available_permits(), 1);
    assert!(scope.try_lock_owned().is_ok());
    slots.close();
    assert!(matches!(
        registry_admission::read(slots).await,
        Err(registry_admission::AdmissionError::Closed)
    ));
}

#[tokio::test(start_paused = true)]
async fn one_hundred_concurrent_callers_share_eight_slots_without_rejection() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tokio::sync::{Mutex, Semaphore};
    let slots = Arc::new(Semaphore::new(8));
    let scopes: Vec<_> = (0..16).map(|_| Arc::new(Mutex::new(()))).collect();
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for index in 0..100 {
        let (slots, scope, active, peak, completed) = (
            Arc::clone(&slots),
            Arc::clone(&scopes[index % scopes.len()]),
            Arc::clone(&active),
            Arc::clone(&peak),
            Arc::clone(&completed),
        );
        tasks.push(tokio::spawn(async move {
            let admitted = registry_admission::write(slots, scope).await.unwrap();
            let running = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(running, Ordering::SeqCst);
            assert!(running <= 8);
            // Yield with both permits held to exercise competing scopes and
            // repeated writers to the same scope, not sequential acquisitions.
            tokio::task::yield_now().await;
            active.fetch_sub(1, Ordering::SeqCst);
            drop(admitted);
            completed.fetch_add(1, Ordering::SeqCst);
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(completed.load(Ordering::SeqCst), 100);
    assert_eq!(peak.load(Ordering::SeqCst), 8);
    assert_eq!(active.load(Ordering::SeqCst), 0);
    assert_eq!(slots.available_permits(), 8);
    assert!(scopes
        .into_iter()
        .all(|scope| scope.try_lock_owned().is_ok()));
}
