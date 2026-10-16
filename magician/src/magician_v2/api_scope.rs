//! Request scope resolution.
//!
//! Auth middleware engraves verified `X-Principal` / `X-Workspace`. This
//! module is the typed seam that turns those values into
//! [`magician_storage::ScopeId`]. `LocalPermissive` reproduces today's
//! header behavior so a later token→scope resolver can replace it without
//! sweeping handlers.

use std::future::{ready, Ready};

use actix_web::dev::Payload;
use actix_web::http::header::HeaderMap;
use actix_web::{FromRequest, HttpRequest, HttpResponse};
use magician_storage::{PrincipalId, ScopeId, WorkspaceId};

use crate::magician_v2::artifact_v2::ScopeRef;

fn normalized_header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn missing_scope_response(missing: &str) -> HttpResponse {
    HttpResponse::BadRequest().json(serde_json::json!({
        "error": "missing_scope",
        "message": format!(
            "A bearer token with an embedded {missing} scope is required."
        )
    }))
}

fn invalid_scope_response() -> HttpResponse {
    HttpResponse::BadRequest().json(serde_json::json!({
        "error": "invalid_scope",
        "message": "The authenticated principal or workspace is not a valid storage scope id."
    }))
}

pub fn resolve_optional_principal(headers: &HeaderMap) -> Option<String> {
    normalized_header(headers, "X-Principal")
}

pub fn resolve_required_workspace(
    headers: &HeaderMap,
    _workspace: Option<String>,
) -> Result<String, HttpResponse> {
    normalized_header(headers, "X-Workspace").ok_or_else(|| missing_scope_response("workspace"))
}

pub fn resolve_required_scope(
    headers: &HeaderMap,
    workspace: Option<String>,
) -> Result<(String, String), HttpResponse> {
    let principal =
        resolve_optional_principal(headers).ok_or_else(|| missing_scope_response("principal"))?;
    let workspace = resolve_required_workspace(headers, workspace)?;
    Ok((principal, workspace))
}

pub fn resolve_required_scope_ref(
    headers: &HeaderMap,
    workspace: Option<String>,
) -> Result<ScopeRef, HttpResponse> {
    let (principal, workspace) = resolve_required_scope(headers, workspace)?;
    Ok(ScopeRef::system_internal_unauthenticated(
        &principal, &workspace,
    ))
}

/// Canonical tenant scope for storage repositories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedScope {
    pub storage: ScopeId,
}

impl ResolvedScope {
    pub fn from_labels(principal: &str, workspace: &str) -> Result<Self, HttpResponse> {
        let principal = PrincipalId::parse(principal).map_err(|_| invalid_scope_response())?;
        let workspace = WorkspaceId::parse(workspace).map_err(|_| invalid_scope_response())?;
        Ok(Self {
            storage: ScopeId::new(principal, workspace),
        })
    }

    pub fn principal(&self) -> &str {
        self.storage.principal.as_str()
    }

    pub fn workspace(&self) -> &str {
        self.storage.workspace.as_str()
    }
}

pub trait ScopeResolver: Send + Sync {
    fn resolve(&self, headers: &HeaderMap) -> Result<ResolvedScope, HttpResponse>;
}

/// Current local behavior: read the engraved principal/workspace headers.
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalPermissive;

impl ScopeResolver for LocalPermissive {
    fn resolve(&self, headers: &HeaderMap) -> Result<ResolvedScope, HttpResponse> {
        let (principal, workspace) = resolve_required_scope(headers, None)?;
        ResolvedScope::from_labels(&principal, &workspace)
    }
}

impl FromRequest for ResolvedScope {
    type Error = actix_web::Error;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _: &mut Payload) -> Self::Future {
        ready(LocalPermissive.resolve(req.headers()).map_err(|response| {
            actix_web::error::InternalError::from_response("scope", response).into()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test::TestRequest;

    #[test]
    fn local_permissive_reads_engraved_headers_as_scope_id() {
        let req = TestRequest::default()
            .insert_header(("X-Principal", "alice"))
            .insert_header(("X-Workspace", "home"))
            .to_http_request();
        let scope = LocalPermissive.resolve(req.headers()).expect("scope");
        assert_eq!(scope.principal(), "alice");
        assert_eq!(scope.workspace(), "home");
        assert_eq!(scope.storage.principal.as_str(), "alice");
        assert_eq!(scope.storage.workspace.as_str(), "home");
    }

    #[test]
    fn local_permissive_requires_both_headers() {
        let req = TestRequest::default()
            .insert_header(("X-Principal", "alice"))
            .to_http_request();
        assert!(LocalPermissive.resolve(req.headers()).is_err());
    }

    #[test]
    fn anonymous_default_is_a_valid_storage_scope() {
        let scope = ResolvedScope::from_labels("anonymous", "default").expect("scope");
        assert_eq!(scope.principal(), "anonymous");
        assert_eq!(scope.workspace(), "default");
    }
}
