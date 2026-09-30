//! Origin-bound native sessions. Tests use a private in-memory store, never the
//! developer's Keychain. The production adapter uses the existing keyring crate.

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

#[cfg(not(test))]
const SERVICE: &str = "ai.magicbeans.magican.desktop.engine-session.v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Origin(String);

impl Origin {
    pub(super) fn parse(value: &str) -> Option<Self> {
        let url = reqwest::Url::parse(value).ok()?;
        let scheme = match url.scheme() {
            "ws" => "http",
            "wss" => "https",
            other => other,
        };
        if !matches!(scheme, "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.host_str().is_none()
            || (scheme == "http"
                && !matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")))
        {
            return None;
        }
        Some(Self(format!(
            "{scheme}://{}:{}",
            url.host_str()?,
            url.port_or_known_default()?
        )))
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

/// `Some(None)` is a durable logout, distinct from an unprovisioned origin.
pub(super) trait SessionStore {
    fn read(&self, origin: &Origin) -> Result<Option<Option<String>>, String>;
    fn write(&self, origin: &Origin, token: Option<&str>) -> Result<(), String>;
}

pub(super) struct OsSessionStore;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSession {
    version: u8,
    origin: String,
    token: Option<String>,
}

#[cfg(not(test))]
impl OsSessionStore {
    fn entry(origin: &Origin) -> Result<keyring::Entry, String> {
        let account = blake3::hash(origin.as_str().as_bytes())
            .to_hex()
            .to_string();
        keyring::Entry::new(SERVICE, &account)
            .map_err(|_| "The desktop credential store is unavailable".to_owned())
    }
}

impl SessionStore for OsSessionStore {
    fn read(&self, origin: &Origin) -> Result<Option<Option<String>>, String> {
        // Hosted unit tests must never read or replace the real desktop login.
        #[cfg(test)]
        return {
            let _ = origin;
            Ok(None)
        };
        #[cfg(not(test))]
        {
            let encoded = match Self::entry(origin)?.get_password() {
                Ok(value) => Zeroizing::new(value),
                Err(keyring::Error::NoEntry) => return Ok(None),
                Err(_) => return Err("The desktop credential store could not be read".into()),
            };
            decode_session(origin, &encoded).map(Some)
        }
    }

    fn write(&self, origin: &Origin, token: Option<&str>) -> Result<(), String> {
        #[cfg(test)]
        return {
            let _ = (origin, token);
            Ok(())
        };
        #[cfg(not(test))]
        {
            let encoded = encode_session(origin, token)?;
            Self::entry(origin)?
                .set_password(&encoded)
                .map_err(|_| "The desktop session could not be saved securely".to_owned())
        }
    }
}

fn encode_session(origin: &Origin, token: Option<&str>) -> Result<Zeroizing<String>, String> {
    serde_json::to_string(&StoredSession {
        version: 1,
        origin: origin.as_str().to_owned(),
        token: token.map(str::to_owned),
    })
    .map(Zeroizing::new)
    .map_err(|_| "The desktop session could not be encoded".into())
}

fn decode_session(origin: &Origin, encoded: &str) -> Result<Option<String>, String> {
    if encoded.len() > 32_768 {
        return Err("The saved desktop session is invalid".into());
    }
    let session: StoredSession = serde_json::from_str(encoded)
        .map_err(|_| "The saved desktop session is invalid".to_owned())?;
    if session.version != 1 || session.origin != origin.as_str() {
        return Err("The saved desktop session belongs to a different server".into());
    }
    normalize_token(session.token)
}

fn normalize_token(token: Option<String>) -> Result<Option<String>, String> {
    let token = token
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if token.as_ref().is_some_and(|value| {
        value.len() > 16_384 || !value.bytes().all(|byte| byte.is_ascii_graphic())
    }) {
        return Err("The desktop session token is invalid".into());
    }
    Ok(token)
}

#[derive(Default)]
pub(super) struct EngineSession {
    pub(super) origin: Option<Origin>,
    pub(super) revision: u64,
    // Never debug-format this object: it contains credentials.
    decision: Option<Option<String>>,
    launch_binding: Option<(Origin, Option<String>)>,
    launch_bound: bool,
}

impl EngineSession {
    pub(super) fn select(&mut self, base: &str, launch_token: Option<String>) {
        let origin = Origin::parse(base);
        if !self.launch_bound {
            self.launch_bound = true;
            self.launch_binding = origin.clone().map(|origin| (origin, launch_token));
        }
        if self.origin != origin {
            self.origin = origin;
            self.decision = None;
            self.revision += 1;
        }
    }

    pub(super) fn token(&mut self, store: &impl SessionStore) -> Result<Option<String>, String> {
        let Some(origin) = self.origin.as_ref() else {
            return Ok(None);
        };
        if let Some(decision) = &self.decision {
            return Ok(decision.clone());
        }
        let decision = if let Some(saved) = store.read(origin)? {
            saved
        } else {
            let launch = self
                .launch_binding
                .as_ref()
                .filter(|(bound, _)| bound == origin)
                .and_then(|(_, token)| token.clone());
            let launch = normalize_token(launch)?;
            if launch.is_some() {
                // A supplied bootstrap bearer must survive a normal cold launch.
                // On a locked/unavailable keyring fail closed and report the error.
                store.write(origin, launch.as_deref())?;
            }
            launch
        };
        self.decision = Some(decision.clone());
        Ok(decision)
    }

    pub(super) fn install(
        &mut self,
        expected: &Origin,
        expected_revision: u64,
        token: Option<String>,
        store: &impl SessionStore,
    ) -> Result<u64, String> {
        if self.origin.as_ref() != Some(expected) || self.revision != expected_revision {
            return Err("The desktop session changed; refresh and sign in again".into());
        }
        let token = normalize_token(token)?;
        // Logout takes effect in memory even if the keyring is locked. The
        // caller receives the persistence error and must not claim it was saved.
        if token.is_none() {
            self.decision = Some(None);
            self.revision += 1;
        }
        store.write(expected, token.as_deref())?;
        if token.is_some() {
            self.revision += 1;
        }
        self.decision = Some(token);
        Ok(self.revision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    #[derive(Default)]
    struct MemoryStore {
        values: RefCell<HashMap<String, Option<String>>>,
        fail: Cell<bool>,
    }
    impl SessionStore for MemoryStore {
        fn read(&self, origin: &Origin) -> Result<Option<Option<String>>, String> {
            if self.fail.get() {
                return Err("locked".into());
            }
            Ok(self.values.borrow().get(origin.as_str()).cloned())
        }
        fn write(&self, origin: &Origin, token: Option<&str>) -> Result<(), String> {
            if self.fail.get() {
                return Err("locked".into());
            }
            self.values
                .borrow_mut()
                .insert(origin.as_str().to_owned(), token.map(str::to_owned));
            Ok(())
        }
    }
    const A: &str = "http://127.0.0.1:13002";
    const B: &str = "http://127.0.0.1:3002";

    #[test]
    fn cold_launch_restores_only_the_selected_origin() {
        let store = MemoryStore::default();
        let mut first = EngineSession::default();
        first.select(A, Some("owner-a".into()));
        assert_eq!(first.token(&store).unwrap().as_deref(), Some("owner-a"));
        let mut restart = EngineSession::default();
        restart.select(A, None);
        assert_eq!(restart.token(&store).unwrap().as_deref(), Some("owner-a"));
        restart.select(B, None);
        assert_eq!(restart.token(&store).unwrap(), None);
        restart
            .install(
                &Origin::parse(B).unwrap(),
                restart.revision,
                Some("owner-b".into()),
                &store,
            )
            .unwrap();
        restart.select(A, None);
        assert_eq!(restart.token(&store).unwrap().as_deref(), Some("owner-a"));
    }

    #[test]
    fn launch_token_cannot_follow_an_engine_change() {
        let store = MemoryStore::default();
        let mut session = EngineSession::default();
        session.select(A, Some("owner-a".into()));
        session.select(B, Some("owner-a".into()));
        assert_eq!(session.token(&store).unwrap(), None);
    }

    #[test]
    fn logout_survives_restart_even_with_the_old_launch_environment() {
        let store = MemoryStore::default();
        let mut session = EngineSession::default();
        session.select(A, Some("old-token".into()));
        session
            .install(&Origin::parse(A).unwrap(), session.revision, None, &store)
            .unwrap();
        let mut restart = EngineSession::default();
        restart.select(A, Some("old-token".into()));
        assert_eq!(restart.token(&store).unwrap(), None);
    }

    #[test]
    fn stale_login_cannot_replace_the_new_servers_session() {
        let store = MemoryStore::default();
        let mut session = EngineSession::default();
        session.select(B, None);
        assert!(session
            .install(
                &Origin::parse(A).unwrap(),
                session.revision,
                Some("owner-a".into()),
                &store
            )
            .is_err());
        assert!(store.values.borrow().is_empty());
    }

    #[test]
    fn an_older_window_cannot_log_out_or_overwrite_a_new_login() {
        let store = MemoryStore::default();
        let mut session = EngineSession::default();
        session.select(A, None);
        let origin = Origin::parse(A).unwrap();
        let stale = session.revision;
        session
            .install(&origin, stale, Some("fresh-login".into()), &store)
            .unwrap();
        assert!(session.install(&origin, stale, None, &store).is_err());
        assert!(session
            .install(&origin, stale, Some("old-login".into()), &store)
            .is_err());
        assert_eq!(
            session.token(&store).unwrap().as_deref(),
            Some("fresh-login")
        );
        let mut restart = EngineSession::default();
        restart.select(A, None);
        assert_eq!(
            restart.token(&store).unwrap().as_deref(),
            Some("fresh-login")
        );
    }

    #[test]
    fn locked_store_does_not_fall_back_to_environment_or_replace_a_working_token() {
        let store = MemoryStore::default();
        let mut session = EngineSession::default();
        session.select(A, Some("owner-a".into()));
        store.fail.set(true);
        assert!(session.token(&store).is_err());
        store.fail.set(false);
        assert_eq!(session.token(&store).unwrap().as_deref(), Some("owner-a"));
        store.fail.set(true);
        assert!(session
            .install(
                &Origin::parse(A).unwrap(),
                session.revision,
                Some("new".into()),
                &store
            )
            .is_err());
        assert_eq!(session.token(&store).unwrap().as_deref(), Some("owner-a"));
        assert!(session
            .install(&Origin::parse(A).unwrap(), session.revision, None, &store)
            .is_err());
        assert_eq!(session.token(&store).unwrap(), None);
    }

    #[test]
    fn stored_documents_cannot_be_moved_between_origins() {
        let origin = Origin::parse(A).unwrap();
        let encoded = encode_session(&origin, Some("secret")).unwrap();
        assert_eq!(
            decode_session(&origin, &encoded).unwrap().as_deref(),
            Some("secret")
        );
        let error = decode_session(&Origin::parse(B).unwrap(), &encoded).unwrap_err();
        assert!(!error.contains("secret"));
        assert!(decode_session(&origin, "not-json-secret").is_err());
        assert!(normalize_token(Some("header\r\ninjection".into())).is_err());
    }

    #[test]
    fn origins_preserve_port_scheme_and_hostname_boundaries() {
        assert_eq!(
            Origin::parse("wss://engine.example/api/magician/ws"),
            Origin::parse("https://engine.example:443")
        );
        assert_ne!(Origin::parse(A), Origin::parse(B));
        assert_ne!(Origin::parse(A), Origin::parse("http://localhost:13002"));
        assert!(Origin::parse("http://engine.example").is_none());
        assert!(Origin::parse("https://user:password@engine.example").is_none());
        assert!(Origin::parse("file:///tmp/test").is_none());
        assert!(Origin::parse("http://[::1]:3002").is_some());
    }
}
