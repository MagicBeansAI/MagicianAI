//! Program-document read API — the Fleet Civilization game's window onto the
//! scope's `programs/*.md` (guild "mission chips" and, later, the CEO
//! decomposition surfaces; see `docs/archive/plans/2026-07-10-ceo-decomposition-design.md`).
//!
//! - `GET /api/magician/v2/programs` — `{ programs: [{name, title}] }`
//! - `GET /api/magician/v2/programs/{name}` — full doc content + the managed
//!   `## Missions (CEO)` section when present + its `.history/` snapshot list
//!   (404 when absent, 400 on an invalid name — names are bare `*.md` file
//!   names, no traversal).
//! - `POST /api/magician/v2/programs/{name}/revert` — the owner's revert
//!   affordance (P3 of the CEO-decomposition design): restore the program from
//!   a history snapshot (`{snapshot?: string}`; default = newest). The current
//!   content is snapshotted first, so a revert is itself revertible.
//! - `PUT /api/magician/v2/programs/{name}` — the owner's whole-document save
//!   (`{content, base_sha256?}`), from a signed-in owner session or the owner's
//!   paired iOS/Android app (never an agent grant or token). `base_sha256`
//!   is the `sha256` the GET returned; it is required for an existing program
//!   and omitted to create one. A changed, new or vanished document answers
//!   409 `program_changed` with the current hash instead of being overwritten.
//!   The replaced content is snapshotted to `.history/` first, so a save is
//!   revertible.
//!
//! Scope-aware in the house style (`resolve_required_scope`); the workspace
//! comes from the shared `ArtifactV2Service` app-data.

use std::sync::Arc;

use actix_web::{web, HttpMessage, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::json;

use crate::scope::resolve_required_scope;
use magician::magician_v2::artifact_v2::service::ArtifactV2Service;
use magician::magician_v2::auth::middleware::AuthRuntime;
use magician::magician_v2::cloudflare_access::{
    VerifiedRequestAuthentication, VerifiedRequestIdentity, LEGACY_ANDROID_DEVICE_ID_HEADER,
    MOBILE_DEVICE_ID_HEADER,
};
use magician::magician_v2::device_pairing::{
    DeviceCapability, DevicePairingStore, MobileClientKind,
};
use magician::magician_v2::harness::program_doc::{
    list_history_snapshots, list_program_docs, read_program_doc, ProgramDocEditor,
    ProgramSaveConflict,
};

#[derive(Debug, Deserialize)]
pub struct ProgramsScopeQuery {
    pub workspace: Option<String>,
}

/// GET /api/magician/v2/programs
pub async fn list_programs_handler(
    service: web::Data<Arc<ArtifactV2Service>>,
    req: HttpRequest,
    query: web::Query<ProgramsScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    match list_program_docs(service.workspace(), &principal, &workspace).await {
        Ok(programs) => HttpResponse::Ok().json(json!({ "programs": programs })),
        Err(err) => HttpResponse::InternalServerError()
            .json(json!({ "error": format!("failed to list programs: {err}") })),
    }
}

/// GET /api/magician/v2/programs/{name}
pub async fn get_program_handler(
    service: web::Data<Arc<ArtifactV2Service>>,
    path: web::Path<String>,
    req: HttpRequest,
    query: web::Query<ProgramsScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let name = path.into_inner();
    match read_program_doc(service.workspace(), &principal, &workspace, &name).await {
        Ok(Some(doc)) => {
            let history =
                list_history_snapshots(service.workspace(), &principal, &workspace, &name)
                    .await
                    .unwrap_or_default();
            let mut body = serde_json::to_value(&doc).unwrap_or_else(|_| json!({}));
            if let Some(map) = body.as_object_mut() {
                map.insert(
                    "history".into(),
                    serde_json::to_value(history).unwrap_or_default(),
                );
            }
            HttpResponse::Ok().json(body)
        },
        Ok(None) => HttpResponse::NotFound().json(json!({ "error": "program not found" })),
        Err(err) => {
            HttpResponse::BadRequest().json(json!({ "error": format!("invalid program: {err}") }))
        },
    }
}

#[derive(Debug, Deserialize)]
pub struct RevertProgramBody {
    /// History snapshot to restore; omitted = the newest one.
    pub snapshot: Option<String>,
}

/// POST /api/magician/v2/programs/{name}/revert
pub async fn revert_program_handler(
    service: web::Data<Arc<ArtifactV2Service>>,
    path: web::Path<String>,
    req: HttpRequest,
    query: web::Query<ProgramsScopeQuery>,
    body: Option<web::Json<RevertProgramBody>>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let name = path.into_inner();
    let snapshot = body.as_ref().and_then(|b| b.snapshot.clone());
    let editor = ProgramDocEditor::new(service.workspace().clone());
    match editor
        .revert_from_history(&principal, &workspace, &name, snapshot.as_deref())
        .await
    {
        Ok(reverted) => HttpResponse::Ok().json(json!({
            "reverted": true,
            "program": reverted.program,
            "restored_from": reverted.restored_from,
            "pre_revert_snapshot": reverted.pre_revert_snapshot,
        })),
        Err(err) => {
            HttpResponse::BadRequest().json(json!({ "error": format!("revert failed: {err}") }))
        },
    }
}

#[derive(Debug, Deserialize)]
pub struct SaveProgramBody {
    /// The full new document.
    pub content: String,
    /// `sha256` of the version being edited; omitted to create a program.
    pub base_sha256: Option<String>,
}

/// Whether the request comes from the owner's own paired phone app: the outer
/// boundary verified a paired-device bearer, and the roster entry for the named
/// device is an iOS/Android client without Apps automation authority. An ESP or
/// desktop Edge credential, or the separate Android automation credential, is
/// not an owner editing surface.
pub(crate) async fn is_paired_owner_phone(
    req: &HttpRequest,
    pairing: Option<&DevicePairingStore>,
) -> bool {
    let Some(identity) = req.extensions().get::<VerifiedRequestIdentity>().cloned() else {
        return false;
    };
    if identity.authentication() != VerifiedRequestAuthentication::PairedDevice {
        return false;
    }
    let (Some(pairing), Some(workspace)) = (pairing, identity.workspace()) else {
        return false;
    };
    let device_id = [MOBILE_DEVICE_ID_HEADER, LEGACY_ANDROID_DEVICE_ID_HEADER]
        .iter()
        .find_map(|name| req.headers().get(*name)?.to_str().ok())
        .map(str::trim)
        .unwrap_or_default();
    if device_id.is_empty() {
        return false;
    }
    pairing
        .list(identity.principal(), workspace)
        .await
        .iter()
        .any(|device| {
            device.device_id == device_id
                && matches!(
                    device.client_kind,
                    MobileClientKind::Ios | MobileClientKind::Android
                )
                && device.allows(DeviceCapability::MobileClient)
                && !device.allows(DeviceCapability::DeviceAutomation)
        })
}

/// Owner authority for a settings write: a signed-in owner session
/// (`plane_api::owner_identity`) or the owner's verified paired phone. The
/// phone must carry a middleware-verified paired-device identity no older than
/// 5 s, for a workspace that hosts the principal's user subsystems, with no
/// conflicting `X-Principal`/`X-Workspace` header, and the roster entry must
/// be an iOS/Android owner client ([`is_paired_owner_phone`]). Agent grants,
/// PATs and automation devices are refused. Errors are JSON
/// `{ error, message }`.
pub(crate) async fn require_owner_session_or_phone(
    req: &HttpRequest,
    auth: Option<&AuthRuntime>,
) -> Result<(), HttpResponse> {
    let identity = req.extensions().get::<VerifiedRequestIdentity>().cloned();
    if let Some(identity) = identity
        .filter(|identity| identity.authentication() == VerifiedRequestAuthentication::PairedDevice)
    {
        let now = chrono::Utc::now();
        let scope_matches = identity.workspace().is_some_and(|workspace| {
            magician::magician_v2::artifact_v2::workspace::scope_hosts_user_subsystems(
                identity.principal(),
                workspace,
            ) && ["X-Principal", "X-Workspace"]
                .iter()
                .zip([identity.principal(), workspace])
                .all(|(name, expected)| {
                    req.headers()
                        .get(*name)
                        .is_none_or(|actual| actual.to_str().ok() == Some(expected))
                })
        });
        let pairing = req
            .app_data::<web::Data<Arc<DevicePairingStore>>>()
            .map(|store| store.get_ref().as_ref());
        if scope_matches
            && now >= identity.verified_at()
            && now - identity.verified_at() <= chrono::Duration::seconds(5)
            && is_paired_owner_phone(req, pairing).await
        {
            return Ok(());
        }
        return Err(HttpResponse::Unauthorized().json(json!({
            "error": "owner_phone_required",
            "message": "A verified paired owner phone is required.",
        })));
    }
    crate::plane_api::owner_identity(req, auth).map(|_| ())
}

/// PUT /api/magician/v2/programs/{name}
///
/// Programs steer autonomous harness cycles, so only the owner may rewrite
/// one: a signed-in owner session (`owner_identity`) or the owner's paired
/// iOS/Android app. Agent grants and tokens are refused.
pub async fn save_program_handler(
    service: web::Data<Arc<ArtifactV2Service>>,
    path: web::Path<String>,
    req: HttpRequest,
    query: web::Query<ProgramsScopeQuery>,
    body: web::Json<SaveProgramBody>,
    auth: Option<web::Data<AuthRuntime>>,
    pairing: Option<web::Data<Arc<DevicePairingStore>>>,
) -> HttpResponse {
    let phone =
        is_paired_owner_phone(&req, pairing.as_ref().map(|data| data.get_ref().as_ref())).await;
    if !phone {
        if let Err(response) =
            crate::plane_api::owner_identity(&req, auth.as_ref().map(|data| data.get_ref()))
        {
            return response;
        }
    }
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let name = path.into_inner();
    let body = body.into_inner();
    let editor = ProgramDocEditor::new(service.workspace().clone());
    match editor
        .save_document(
            &principal,
            &workspace,
            &name,
            &body.content,
            body.base_sha256.as_deref(),
        )
        .await
    {
        Ok(saved) => HttpResponse::Ok().json(json!({
            "saved": true,
            "program": saved.program,
            "created": saved.created,
            "unchanged": saved.unchanged,
            "history_snapshot": saved.history_snapshot,
            "sha256": saved.sha256,
        })),
        Err(err) => match err.downcast_ref::<ProgramSaveConflict>() {
            Some(conflict) => HttpResponse::Conflict().json(json!({
                "error": "program_changed",
                "message": conflict.to_string(),
                "current_sha256": conflict.current_sha256,
            })),
            None => {
                HttpResponse::BadRequest().json(json!({ "error": format!("save failed: {err}") }))
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test::TestRequest;
    use magician::magician_v2::device_bridge::DeviceKey;

    async fn store_with_devices() -> (tempfile::TempDir, DevicePairingStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DevicePairingStore::open(dir.path()).await.unwrap();
        store
            .pair(DeviceKey::new("owner", "default", "phone-1"), "Phone", 0)
            .await
            .unwrap();
        store
            .pair_esp32(DeviceKey::new("owner", "default", "esp-1"), "Badge", 0)
            .await
            .unwrap();
        (dir, store)
    }

    fn request(device: &str, authentication: Option<VerifiedRequestAuthentication>) -> HttpRequest {
        let req = TestRequest::default()
            .insert_header((MOBILE_DEVICE_ID_HEADER, device))
            .to_http_request();
        if let Some(authentication) = authentication {
            req.extensions_mut()
                .insert(VerifiedRequestIdentity::for_test(
                    "owner",
                    Some("default"),
                    authentication,
                ));
        }
        req
    }

    #[actix_web::test]
    async fn only_a_verified_paired_phone_counts_as_the_owner() {
        let (_dir, store) = store_with_devices().await;
        let paired = Some(VerifiedRequestAuthentication::PairedDevice);
        assert!(is_paired_owner_phone(&request("phone-1", paired), Some(&store)).await);
        // An ESP companion has mobile-client authority but is not an editor.
        assert!(!is_paired_owner_phone(&request("esp-1", paired), Some(&store)).await);
        // A device id the roster does not hold for this scope.
        assert!(!is_paired_owner_phone(&request("phone-2", paired), Some(&store)).await);
        // Not verified as a paired device at the boundary.
        assert!(!is_paired_owner_phone(&request("phone-1", None), Some(&store)).await);
        let bearer = Some(VerifiedRequestAuthentication::MagicianBearer);
        assert!(!is_paired_owner_phone(&request("phone-1", bearer), Some(&store)).await);
        // No roster available.
        assert!(!is_paired_owner_phone(&request("phone-1", paired), None).await);
    }
}
