//! Passive ambient page signal buffering for the bridge/CDP-proxy route.
//!
//! The browser extension emits coarse page identity/change signals for
//! thread-owned tabs. Magicutor buffers them by thread so Magician can drain
//! them at browser-loop boundaries and persist them beside API-mining traces.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use once_cell::sync::Lazy;
use tracing::warn;

use crate::types::AmbientPageSignal;

const MAX_PAGE_SIGNALS_PER_THREAD: usize = 5_000;

struct ThreadPageSignals {
    thread_id: String,
    completed: Mutex<Vec<AmbientPageSignal>>,
}

impl ThreadPageSignals {
    fn new(thread_id: String) -> Self {
        Self {
            thread_id,
            completed: Mutex::new(Vec::new()),
        }
    }

    fn push(&self, mut signal: AmbientPageSignal) {
        if signal.capture_source.is_none() {
            signal.capture_source = Some("extension_ambient_page".to_string());
        }
        let mut completed = match self.completed.lock() {
            Ok(g) => g,
            Err(e) => {
                warn!(
                    thread = %self.thread_id,
                    error = %e,
                    "page_signals: completed buffer poisoned"
                );
                return;
            },
        };
        if completed.len() >= MAX_PAGE_SIGNALS_PER_THREAD {
            completed.remove(0);
        }
        completed.push(signal);
    }

    fn drain(&self) -> Vec<AmbientPageSignal> {
        match self.completed.lock() {
            Ok(mut g) => std::mem::take(&mut *g),
            Err(e) => {
                warn!(
                    thread = %self.thread_id,
                    error = %e,
                    "page_signals: drain failed"
                );
                Vec::new()
            },
        }
    }
}

struct PageSignalRegistry {
    threads: Mutex<HashMap<String, Arc<ThreadPageSignals>>>,
}

impl PageSignalRegistry {
    fn new() -> Self {
        Self {
            threads: Mutex::new(HashMap::new()),
        }
    }

    fn get_or_create(&self, thread_id: &str) -> Arc<ThreadPageSignals> {
        let mut threads = match self.threads.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        threads
            .entry(thread_id.to_string())
            .or_insert_with(|| Arc::new(ThreadPageSignals::new(thread_id.to_string())))
            .clone()
    }

    fn get(&self, thread_id: &str) -> Option<Arc<ThreadPageSignals>> {
        let threads = match self.threads.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        threads.get(thread_id).cloned()
    }

    fn remove(&self, thread_id: &str) -> Option<Arc<ThreadPageSignals>> {
        let mut threads = match self.threads.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        threads.remove(thread_id)
    }
}

static REGISTRY: Lazy<PageSignalRegistry> = Lazy::new(PageSignalRegistry::new);

pub fn record(signal: AmbientPageSignal) {
    if signal.thread_id.trim().is_empty() {
        return;
    }
    REGISTRY.get_or_create(&signal.thread_id).push(signal);
}

pub fn drain(thread_id: &str) -> Vec<AmbientPageSignal> {
    REGISTRY
        .get(thread_id)
        .map(|signals| signals.drain())
        .unwrap_or_default()
}

pub fn disable_thread(thread_id: &str) -> Vec<AmbientPageSignal> {
    REGISTRY
        .remove(thread_id)
        .map(|signals| signals.drain())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(thread_id: &str, idx: usize) -> AmbientPageSignal {
        AmbientPageSignal {
            event_id: format!("event-{idx}"),
            thread_id: thread_id.to_string(),
            tab_id: 7,
            event_kind: "test".to_string(),
            timestamp: idx as i64,
            structural_hash: Some(format!("s-{idx}")),
            ..Default::default()
        }
    }

    #[test]
    fn record_and_drain_signals_by_thread() {
        let thread_id = "page-signal-record-and-drain";
        let _ = disable_thread(thread_id);

        record(signal(thread_id, 1));
        record(signal(thread_id, 2));

        let drained = drain(thread_id);
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].event_id, "event-1");
        assert_eq!(
            drained[0].capture_source.as_deref(),
            Some("extension_ambient_page")
        );
        assert!(drain(thread_id).is_empty());
    }

    #[test]
    fn record_ignores_empty_thread() {
        record(signal("", 1));
        assert!(drain("").is_empty());
    }

    #[test]
    fn buffer_evicts_oldest_when_full() {
        let thread_id = "page-signal-cap";
        let _ = disable_thread(thread_id);
        for idx in 0..(MAX_PAGE_SIGNALS_PER_THREAD + 3) {
            record(signal(thread_id, idx));
        }
        let drained = drain(thread_id);
        assert_eq!(drained.len(), MAX_PAGE_SIGNALS_PER_THREAD);
        assert_eq!(drained.first().unwrap().event_id, "event-3");
        assert_eq!(
            drained.last().unwrap().event_id,
            format!("event-{}", MAX_PAGE_SIGNALS_PER_THREAD + 2)
        );
    }
}
