//! Process-owned abortable runtime for request-path LanceDB searches.
//!
//! The `Runtime` itself is owned by `bin/magician.rs` for process lifetime.
//! Only the handle is registered here so search construction and polling can
//! move off HTTP and execution workers. `MAGICIAN_LANCE_RUNTIME=ambient`
//! restores today's `tokio::spawn` on the caller runtime after restart.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use tracing::{info, warn};

static MISSING_DEDICATED_HANDLE_WARNED: AtomicBool = AtomicBool::new(false);

pub const LANCE_WORKER_THREADS: usize = 4;
pub const LANCE_THREAD_NAME: &str = "magician-lance";

static LANCE_RUNTIME_HANDLE: OnceLock<tokio::runtime::Handle> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanceRuntimeMode {
    Ambient,
    Dedicated,
}

fn env_lance_runtime_mode() -> Option<LanceRuntimeMode> {
    match std::env::var("MAGICIAN_LANCE_RUNTIME") {
        Ok(value) if value.eq_ignore_ascii_case("ambient") => Some(LanceRuntimeMode::Ambient),
        Ok(value) if value.eq_ignore_ascii_case("dedicated") => Some(LanceRuntimeMode::Dedicated),
        _ => None,
    }
}

/// Configured mode used to decide whether the process should *build* a
/// dedicated runtime. Unset means dedicated so production starts the pool
/// without an env flag. `ambient` is the explicit rollback.
pub fn configured_lance_runtime_mode() -> LanceRuntimeMode {
    env_lance_runtime_mode().unwrap_or(LanceRuntimeMode::Dedicated)
}

pub fn should_build_lance_runtime() -> bool {
    configured_lance_runtime_mode() == LanceRuntimeMode::Dedicated
}

/// Effective spawn target. A dedicated pool is used only once its handle is
/// registered, so tests and library callers without `bin/magician` stay on
/// the ambient runtime without a warn-per-search.
pub fn lance_runtime_mode() -> LanceRuntimeMode {
    if env_lance_runtime_mode() == Some(LanceRuntimeMode::Ambient) {
        return LanceRuntimeMode::Ambient;
    }
    if LANCE_RUNTIME_HANDLE.get().is_some() {
        LanceRuntimeMode::Dedicated
    } else {
        LanceRuntimeMode::Ambient
    }
}

pub fn lance_runtime_mode_label() -> &'static str {
    match lance_runtime_mode() {
        LanceRuntimeMode::Ambient => "ambient",
        LanceRuntimeMode::Dedicated => "dedicated",
    }
}

pub fn build_lance_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    build_lance_runtime_with_threads(LANCE_WORKER_THREADS)
}

/// Build the Lance runtime with an explicit worker-thread count from the
/// pre-bootstrap plan. [`LANCE_WORKER_THREADS`] remains the `current` default.
pub fn build_lance_runtime_with_threads(
    worker_threads: usize,
) -> std::io::Result<tokio::runtime::Runtime> {
    let worker_threads = worker_threads.max(1);
    info!(
        worker_threads,
        thread_name = LANCE_THREAD_NAME,
        "built dedicated Lance search runtime"
    );
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .thread_name(LANCE_THREAD_NAME)
        .enable_all()
        .build()
}

pub fn set_lance_runtime_handle(handle: tokio::runtime::Handle) {
    if LANCE_RUNTIME_HANDLE.set(handle).is_ok() {
        info!(
            thread_name = LANCE_THREAD_NAME,
            mode = lance_runtime_mode_label(),
            "registered dedicated Lance search runtime handle"
        );
    }
}

pub(crate) fn registered_lance_runtime_handle() -> Option<&'static tokio::runtime::Handle> {
    if lance_runtime_mode() == LanceRuntimeMode::Ambient {
        return None;
    }
    LANCE_RUNTIME_HANDLE.get()
}

pub(crate) fn spawn_on_lance_runtime<T>(
    task: std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'static>>,
) -> tokio::task::JoinHandle<T>
where
    T: Send + 'static,
{
    match registered_lance_runtime_handle() {
        Some(handle) => handle.spawn(task),
        None => {
            if env_lance_runtime_mode() == Some(LanceRuntimeMode::Dedicated)
                && MISSING_DEDICATED_HANDLE_WARNED
                    .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
            {
                warn!(
                    "Lance runtime mode is dedicated but no handle is registered; spawning on the ambient runtime"
                );
            }
            tokio::spawn(task)
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }

        fn clear(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn ambient_env_selects_ambient_mode() {
        let _env = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _guard = EnvVarGuard::set("MAGICIAN_LANCE_RUNTIME", "ambient");
        assert_eq!(lance_runtime_mode(), LanceRuntimeMode::Ambient);
        assert_eq!(lance_runtime_mode_label(), "ambient");
    }

    #[test]
    fn unset_mode_builds_dedicated_but_spawns_ambient_until_registered() {
        let _env = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _guard = EnvVarGuard::clear("MAGICIAN_LANCE_RUNTIME");
        assert_eq!(configured_lance_runtime_mode(), LanceRuntimeMode::Dedicated);
        assert!(should_build_lance_runtime());
        if LANCE_RUNTIME_HANDLE.get().is_some() {
            assert_eq!(lance_runtime_mode(), LanceRuntimeMode::Dedicated);
        } else {
            assert_eq!(lance_runtime_mode(), LanceRuntimeMode::Ambient);
        }
    }

    #[test]
    fn explicit_dedicated_env_requests_a_dedicated_pool() {
        let _env = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _guard = EnvVarGuard::set("MAGICIAN_LANCE_RUNTIME", "dedicated");
        assert_eq!(configured_lance_runtime_mode(), LanceRuntimeMode::Dedicated);
        assert!(should_build_lance_runtime());
    }

    #[test]
    fn ambient_env_does_not_build_a_dedicated_pool() {
        let _env = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _guard = EnvVarGuard::set("MAGICIAN_LANCE_RUNTIME", "ambient");
        assert!(!should_build_lance_runtime());
        assert_eq!(lance_runtime_mode(), LanceRuntimeMode::Ambient);
    }

    #[test]
    fn dedicated_runtime_uses_named_workers() {
        let runtime = build_lance_runtime().expect("lance runtime");
        let thread_name = runtime.block_on(async {
            tokio::spawn(async {
                std::thread::current()
                    .name()
                    .unwrap_or("unnamed")
                    .to_string()
            })
            .await
            .expect("named lance worker")
        });
        assert_eq!(thread_name, LANCE_THREAD_NAME);
    }
}
