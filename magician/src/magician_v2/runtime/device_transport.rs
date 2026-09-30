//! Machine-bound dials (browser CDP, local Ollama, later audio/presence).
//!
//! `LocalLoopback` returns the same URLs current call sites read from config.
//! A remote-device bridge becomes a second implementation; missing
//! capabilities use the existing tool-unavailable path.

use std::sync::{Arc, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceCapabilities {
    pub browser: bool,
    pub ollama: bool,
    pub audio_engine: bool,
    pub speech: bool,
    pub presence: bool,
}

pub trait DeviceTransport: Send + Sync {
    fn capabilities(&self) -> DeviceCapabilities;
    fn browser_cdp_url(&self) -> Option<String>;
    fn ollama_embedding_base_url(&self) -> Option<String>;
}

#[derive(Debug, Clone)]
pub struct LocalLoopback {
    browser_cdp_url: String,
    ollama_embedding_base_url: String,
}

impl LocalLoopback {
    pub fn from_urls(
        browser_cdp_url: impl Into<String>,
        ollama_embedding_base_url: impl Into<String>,
    ) -> Self {
        Self {
            browser_cdp_url: browser_cdp_url.into(),
            ollama_embedding_base_url: ollama_embedding_base_url.into(),
        }
    }
}

impl DeviceTransport for LocalLoopback {
    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            browser: true,
            ollama: true,
            audio_engine: true,
            speech: true,
            presence: true,
        }
    }

    fn browser_cdp_url(&self) -> Option<String> {
        Some(self.browser_cdp_url.clone())
    }

    fn ollama_embedding_base_url(&self) -> Option<String> {
        Some(self.ollama_embedding_base_url.clone())
    }
}

static CURRENT: OnceLock<Arc<dyn DeviceTransport>> = OnceLock::new();

pub fn install(transport: Arc<dyn DeviceTransport>) {
    let _ = CURRENT.set(transport);
}

pub fn current() -> Option<Arc<dyn DeviceTransport>> {
    CURRENT.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_loopback_exposes_configured_urls() {
        let transport = LocalLoopback::from_urls(
            "ws://127.0.0.1:3003/devtools/browser/magicutor-proxy",
            "http://127.0.0.1:11435",
        );
        let caps = transport.capabilities();
        assert!(caps.browser && caps.ollama);
        assert_eq!(
            transport.browser_cdp_url().as_deref(),
            Some("ws://127.0.0.1:3003/devtools/browser/magicutor-proxy")
        );
        assert_eq!(
            transport.ollama_embedding_base_url().as_deref(),
            Some("http://127.0.0.1:11435")
        );
    }
}
