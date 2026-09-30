//! Cookie-session helper shared by the `portal` and `notes` sites.
//! Fixture-only credentials: `eval` / `eval-pass`. Session ids and CSRF
//! tokens are random per login so the eval can prove a recipe never stores
//! them as literals.

use std::sync::Mutex;

use actix_web::HttpRequest;
use serde_json::{json, Value};

use super::{now_ms, Knobs};

pub const USERNAME: &str = "eval";
pub const PASSWORD: &str = "eval-pass";
/// Default cookie name. Each site overrides it via [`SessionTable::for_site`]:
/// cookies are host-scoped, not port-scoped (RFC 6265), so two fixture sites
/// on 127.0.0.1 sharing one name would share one jar entry — the second login
/// would silently evict the first, and a replay against the first site would
/// carry the other site's session. Distinct real sites differ by host and
/// never collide; distinct names restore that separation here.
pub const COOKIE_NAME: &str = "sid";

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub username: String,
    pub csrf: String,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionError {
    Missing,
    Expired,
}

pub struct SessionTable {
    inner: Mutex<Vec<Session>>,
    cookie_name: String,
}

impl Default for SessionTable {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Vec::new()),
            cookie_name: COOKIE_NAME.to_owned(),
        }
    }
}

fn random_token(prefix: &str) -> String {
    format!("{prefix}{}", uuid::Uuid::new_v4().simple())
}

impl SessionTable {
    /// A table whose cookie is unique to `site`, so sites sharing a host do
    /// not share a jar entry.
    pub fn for_site(site: &str) -> Self {
        Self {
            inner: Mutex::new(Vec::new()),
            cookie_name: format!("{site}_{COOKIE_NAME}"),
        }
    }

    #[cfg(test)]
    pub fn cookie_name(&self) -> &str {
        &self.cookie_name
    }

    /// The `Set-Cookie` value that starts this site's session.
    pub fn set_cookie_value(&self, session: &Session) -> String {
        format!(
            "{}={}; Path=/; HttpOnly; SameSite=Lax",
            self.cookie_name, session.id
        )
    }

    pub fn clear_cookie_value(&self) -> String {
        format!("{}=; Path=/; HttpOnly; Max-Age=0", self.cookie_name)
    }

    pub fn create(&self, username: &str) -> Session {
        let session = Session {
            id: random_token("s"),
            username: username.to_owned(),
            csrf: random_token("c"),
            created_at_ms: now_ms(),
        };
        self.inner
            .lock()
            .expect("sessions poisoned")
            .push(session.clone());
        session
    }

    pub fn get(&self, id: &str) -> Option<Session> {
        self.inner
            .lock()
            .expect("sessions poisoned")
            .iter()
            .find(|session| session.id == id)
            .cloned()
    }

    /// Resolve the request's session against the current knobs.
    pub fn resolve(&self, req: &HttpRequest, knobs: &Knobs) -> Result<Session, SessionError> {
        let id = cookie_sid(req, &self.cookie_name).ok_or(SessionError::Missing)?;
        let session = self.get(&id).ok_or(SessionError::Missing)?;
        if let Some(ttl) = knobs.session_ttl_secs {
            let age_ms = now_ms().saturating_sub(session.created_at_ms);
            if age_ms >= ttl.saturating_mul(1000) {
                return Err(SessionError::Expired);
            }
        }
        Ok(session)
    }

    /// Ids and tokens are fixture-only secrets; the driver reads them to
    /// prove a recipe never carries them.
    pub fn snapshot(&self) -> Value {
        let sessions = self.inner.lock().expect("sessions poisoned");
        json!({
            "sessions": sessions.iter().map(|session| json!({
                "id": session.id,
                "username": session.username,
                "csrf": session.csrf,
                "created_at_ms": session.created_at_ms,
            })).collect::<Vec<_>>()
        })
    }
}

pub fn cookie_sid(req: &HttpRequest, cookie_name: &str) -> Option<String> {
    req.cookie(cookie_name)
        .map(|cookie| cookie.value().to_owned())
}

/// CSRF token to embed in a shell: the session's when the cookie resolves,
/// otherwise a stable anonymous token.
pub fn csrf_for_shell(
    table: &SessionTable,
    req: &HttpRequest,
    knobs: &Knobs,
    site: &str,
) -> String {
    table
        .resolve(req, knobs)
        .map(|session| session.csrf)
        .unwrap_or_else(|_| format!("anon-{site}"))
}

pub fn credentials_ok(username: &str, password: &str) -> bool {
    username == USERNAME && password == PASSWORD
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_are_random_and_expire_by_knob() {
        let table = SessionTable::default();
        let a = table.create(USERNAME);
        let b = table.create(USERNAME);
        assert_ne!(a.id, b.id);
        assert_ne!(a.csrf, b.csrf);
        assert!(table.get(&a.id).is_some());
        let req = actix_web::test::TestRequest::default()
            .cookie(actix_web::cookie::Cookie::new(COOKIE_NAME, a.id.clone()))
            .to_http_request();
        assert!(table.resolve(&req, &Knobs::default()).is_ok());
        let expired = Knobs {
            session_ttl_secs: Some(0),
            ..Knobs::default()
        };
        assert_eq!(
            table.resolve(&req, &expired).err(),
            Some(SessionError::Expired)
        );
        let anonymous = actix_web::test::TestRequest::default().to_http_request();
        assert_eq!(
            table.resolve(&anonymous, &Knobs::default()).err(),
            Some(SessionError::Missing)
        );
    }

    #[test]
    fn each_site_owns_its_cookie_name_so_one_host_can_hold_two_sessions() {
        let portal = SessionTable::for_site("portal");
        let notes = SessionTable::for_site("notes");
        assert_eq!(portal.cookie_name(), "portal_sid");
        assert_eq!(notes.cookie_name(), "notes_sid");
        let portal_session = portal.create(USERNAME);
        let notes_session = notes.create(USERNAME);
        // A browser holds both at once because the names differ; with one
        // shared name the second login would evict the first.
        let req = actix_web::test::TestRequest::default()
            .cookie(actix_web::cookie::Cookie::new(
                portal.cookie_name(),
                portal_session.id.clone(),
            ))
            .cookie(actix_web::cookie::Cookie::new(
                notes.cookie_name(),
                notes_session.id.clone(),
            ))
            .to_http_request();
        assert_eq!(
            portal.resolve(&req, &Knobs::default()).map(|s| s.id).ok(),
            Some(portal_session.id)
        );
        assert_eq!(
            notes.resolve(&req, &Knobs::default()).map(|s| s.id).ok(),
            Some(notes_session.id)
        );
        // Neither site accepts the other's session.
        let crossed = actix_web::test::TestRequest::default()
            .cookie(actix_web::cookie::Cookie::new(
                notes.cookie_name(),
                random_token("s"),
            ))
            .to_http_request();
        assert_eq!(
            portal.resolve(&crossed, &Knobs::default()).err(),
            Some(SessionError::Missing)
        );
    }

    #[test]
    fn only_the_fixture_credentials_log_in() {
        assert!(credentials_ok("eval", "eval-pass"));
        assert!(!credentials_ok("eval", "wrong"));
        assert!(!credentials_ok("admin", "eval-pass"));
    }
}
