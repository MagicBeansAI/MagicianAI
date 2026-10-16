use std::{
    collections::HashMap,
    fs,
    io::{self, ErrorKind},
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
};

use actix_web::{http::StatusCode, web, HttpRequest, HttpResponse};
use chrono::Utc;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::{
    apps_api::{authenticated_app_scope, AppPlatformApi},
    web_api::api_error_response,
};
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::secrets::{
    find_unsupported_provisioned_policy_routes, provisioned_policy_route_catalog, CookieSpec,
    InjectionTarget, ProvisionedPolicyRouteCatalog, SameSite, SecretAuditEvent, SecretEntry,
    SecretPolicy, SecretRuntimeCapabilities, SecretSourceKind, SecretStore, SecretStoreError,
    SecretStoreResolver,
};

const SETUP_TOKEN_HEADER: &str = "X-Magician-Setup-Token";
const SETUP_TOKEN_RECORD_FILENAME: &str = "setup_token.json";
const SETUP_TOKEN_PENDING_FILENAME: &str = "setup_token.pending";
const SETUP_TOKEN_VERSION: u32 = 1;

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct AppDataRootKeyRotationResponse {
    scope_key_id: String,
    rotated_at: chrono::DateTime<Utc>,
}

#[derive(Clone)]
pub struct SecretVaultApi {
    workspace_layout: ArtifactV2Workspace,
    secret_store_resolver: Arc<SecretStoreResolver>,
    runtime_capabilities: SecretRuntimeCapabilities,
    setup_token_lock: Arc<Mutex<()>>,
}

impl SecretVaultApi {
    pub fn new(base_root: PathBuf, secret_store_resolver: Arc<SecretStoreResolver>) -> Self {
        Self::with_workspace_layout(
            ArtifactV2Workspace::new(ArtifactV2Workspace::resolve_scoped_root(&base_root)),
            secret_store_resolver,
        )
    }

    pub fn with_workspace_layout(
        workspace_layout: ArtifactV2Workspace,
        secret_store_resolver: Arc<SecretStoreResolver>,
    ) -> Self {
        Self {
            workspace_layout,
            runtime_capabilities: secret_store_resolver.runtime_capabilities().clone(),
            secret_store_resolver,
            setup_token_lock: Arc::new(Mutex::new(())),
        }
    }

    #[cfg(test)]
    pub fn new_for_tests(base_root: PathBuf, store: Arc<SecretStore>) -> Self {
        let secret_store_resolver = Arc::new(SecretStoreResolver::new_with_capabilities(
            Box::new(magician::magician_v2::secrets::InMemoryKeyProvider::new()),
            base_root.clone(),
            store.runtime_capabilities().clone(),
        ));
        let api = Self {
            workspace_layout: ArtifactV2Workspace::new(ArtifactV2Workspace::resolve_scoped_root(
                &base_root,
            )),
            runtime_capabilities: store.runtime_capabilities().clone(),
            secret_store_resolver,
            setup_token_lock: Arc::new(Mutex::new(())),
        };
        let scoped_root = api.workspace_layout.secrets_root("test", "test");
        fs::create_dir_all(&scoped_root).expect("create test scoped secret root");
        api.secret_store_resolver
            .seed_store_for_scope("test", "test", store);
        api
    }

    fn setup_token_record_path(&self) -> PathBuf {
        self.workspace_layout
            .system_secrets_root()
            .join(SETUP_TOKEN_RECORD_FILENAME)
    }

    fn setup_token_pending_path(&self) -> PathBuf {
        self.workspace_layout
            .system_secrets_root()
            .join(SETUP_TOKEN_PENDING_FILENAME)
    }

    pub(crate) fn resolve_required_scope(
        &self,
        req: &HttpRequest,
    ) -> Result<(String, String), HttpResponse> {
        let principal = req
            .headers()
            .get("X-Principal")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                invalid_request(
                    "A bearer with an embedded principal is required for scoped secret access",
                    None,
                )
            })?;
        let workspace = req
            .headers()
            .get("X-Workspace")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                invalid_request(
                    "A bearer with an embedded workspace is required for scoped secret access",
                    None,
                )
            })?;
        Ok((principal, workspace))
    }

    pub(crate) fn scoped_store(&self, req: &HttpRequest) -> Result<Arc<SecretStore>, HttpResponse> {
        let (principal, workspace) = self.resolve_required_scope(req)?;
        self.secret_store_resolver
            .resolve_for_scope(&principal, &workspace)
            .map_err(secret_store_error_response)
    }

    fn require_local_request(&self, req: &HttpRequest) -> Result<(), HttpResponse> {
        if let Some(peer) = req.peer_addr() {
            if peer.ip().is_loopback() {
                return Ok(());
            }
            return Err(api_error_response(
                StatusCode::FORBIDDEN,
                "forbidden",
                "Secret vault endpoints are localhost-only",
                Some(json!({
                    "remote": peer.to_string(),
                })),
            ));
        }

        if let Some(remote) = req.connection_info().realip_remote_addr() {
            if is_loopback_remote(remote) {
                return Ok(());
            }
        }

        Err(api_error_response(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Secret vault endpoints are localhost-only",
            Some(json!({
                "header_name": SETUP_TOKEN_HEADER,
            })),
        ))
    }

    fn disabled_reason(&self) -> Option<String> {
        let status = self
            .runtime_capabilities
            .status_for(SecretSourceKind::Provisioned);
        if status.is_available() {
            None
        } else {
            status.reason.clone()
        }
    }

    fn disabled_status_response(&self) -> SetupTokenStatusResponse {
        SetupTokenStatusResponse {
            configured: false,
            available: false,
            header_name: SETUP_TOKEN_HEADER.to_string(),
            pending_acknowledgement: false,
            created_at: None,
            acknowledged_at: None,
            pending_token: None,
            unavailable_reason: self.disabled_reason(),
            supported_policy_routes: provisioned_policy_route_catalog(),
        }
    }

    fn ensure_vault_available(&self, store: &SecretStore) -> Result<(), HttpResponse> {
        store
            .ensure_feature_enabled(SecretSourceKind::Provisioned)
            .map_err(secret_store_error_response)
    }

    fn ensure_runtime_vault_available(&self) -> Result<(), HttpResponse> {
        if let Some(reason) = self.disabled_reason() {
            Err(api_error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "feature_disabled",
                "The provisioned secret feature is unavailable on this machine",
                Some(json!({
                    "feature": SecretSourceKind::Provisioned.as_str(),
                    "reason": reason,
                })),
            ))
        } else {
            Ok(())
        }
    }

    fn read_setup_token_status(&self) -> Result<SetupTokenStatusResponse, HttpResponse> {
        if self.disabled_reason().is_some() {
            return Ok(self.disabled_status_response());
        }
        let _guard = self
            .setup_token_lock
            .lock()
            .expect("secret vault setup token lock poisoned");
        let snapshot = self.load_or_create_setup_token_locked()?;
        Ok(snapshot.into_response())
    }

    /// Verify the `X-Magician-Setup-Token` header against the stored
    /// admin token. Used by SecretVaultApi internally and re-used by
    /// SkillsApi (both endpoints are admin-privileged).
    pub fn require_setup_token(&self, req: &HttpRequest) -> Result<String, HttpResponse> {
        self.ensure_runtime_vault_available()?;
        let Some(raw_header) = req.headers().get(SETUP_TOKEN_HEADER) else {
            return Err(api_error_response(
                StatusCode::UNAUTHORIZED,
                "setup_token_required",
                "Setup token is required for this operation",
                Some(json!({
                    "header_name": SETUP_TOKEN_HEADER,
                })),
            ));
        };

        let token = raw_header
            .to_str()
            .map_err(|_| {
                api_error_response(
                    StatusCode::UNAUTHORIZED,
                    "setup_token_invalid",
                    "Setup token header is not valid UTF-8",
                    Some(json!({
                        "header_name": SETUP_TOKEN_HEADER,
                    })),
                )
            })?
            .trim()
            .to_string();

        if token.is_empty() {
            return Err(api_error_response(
                StatusCode::UNAUTHORIZED,
                "setup_token_invalid",
                "Setup token header is empty",
                Some(json!({
                    "header_name": SETUP_TOKEN_HEADER,
                })),
            ));
        }

        let _guard = self
            .setup_token_lock
            .lock()
            .expect("secret vault setup token lock poisoned");
        let snapshot = self.load_or_create_setup_token_locked()?;
        if verify_setup_token(&snapshot.record, &token) {
            Ok(token)
        } else {
            Err(api_error_response(
                StatusCode::UNAUTHORIZED,
                "setup_token_invalid",
                "Setup token is invalid",
                Some(json!({
                    "header_name": SETUP_TOKEN_HEADER,
                })),
            ))
        }
    }

    fn acknowledge_setup_token(
        &self,
        token: &str,
    ) -> Result<SetupTokenStatusResponse, HttpResponse> {
        self.ensure_runtime_vault_available()?;
        let _guard = self
            .setup_token_lock
            .lock()
            .expect("secret vault setup token lock poisoned");
        let mut snapshot = self.load_or_create_setup_token_locked()?;
        if !verify_setup_token(&snapshot.record, token) {
            return Err(api_error_response(
                StatusCode::UNAUTHORIZED,
                "setup_token_invalid",
                "Setup token is invalid",
                Some(json!({
                    "header_name": SETUP_TOKEN_HEADER,
                })),
            ));
        }

        snapshot.record.acknowledged_at = Some(Utc::now().timestamp());
        snapshot.pending_token = None;
        self.persist_setup_token_snapshot(&snapshot)?;
        Ok(snapshot.into_response())
    }

    fn rotate_setup_token(
        &self,
        current_token: &str,
    ) -> Result<SetupTokenStatusResponse, HttpResponse> {
        self.ensure_runtime_vault_available()?;
        let _guard = self
            .setup_token_lock
            .lock()
            .expect("secret vault setup token lock poisoned");
        let snapshot = self.load_or_create_setup_token_locked()?;
        if !verify_setup_token(&snapshot.record, current_token) {
            return Err(api_error_response(
                StatusCode::UNAUTHORIZED,
                "setup_token_invalid",
                "Setup token is invalid",
                Some(json!({
                    "header_name": SETUP_TOKEN_HEADER,
                })),
            ));
        }

        let next_snapshot = self.create_new_setup_token()?;
        self.persist_setup_token_snapshot(&next_snapshot)?;
        Ok(next_snapshot.into_response())
    }

    fn load_or_create_setup_token_locked(&self) -> Result<SetupTokenSnapshot, HttpResponse> {
        fs::create_dir_all(self.workspace_layout.system_secrets_root()).map_err(|err| {
            api_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "Failed to initialize secret vault storage",
                Some(json!({
                    "details": err.to_string(),
                })),
            )
        })?;

        match self.load_setup_token_record() {
            Ok(Some(record)) => {
                let pending_token = read_optional_string(&self.setup_token_pending_path())
                    .map_err(|err| {
                        api_error_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "internal_error",
                            "Failed to read setup token status",
                            Some(json!({
                                "details": err.to_string(),
                            })),
                        )
                    })?;
                Ok(SetupTokenSnapshot {
                    record,
                    pending_token,
                })
            },
            Ok(None) => {
                let snapshot = self.create_new_setup_token()?;
                self.persist_setup_token_snapshot(&snapshot)?;
                Ok(snapshot)
            },
            Err(err) => Err(api_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "Failed to load setup token state",
                Some(json!({
                    "details": err.to_string(),
                })),
            )),
        }
    }

    fn load_setup_token_record(&self) -> io::Result<Option<SetupTokenRecord>> {
        match fs::read(self.setup_token_record_path()) {
            Ok(bytes) => serde_json::from_slice::<SetupTokenRecord>(&bytes)
                .map(Some)
                .map_err(|err| io::Error::new(ErrorKind::InvalidData, err)),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn create_new_setup_token(&self) -> Result<SetupTokenSnapshot, HttpResponse> {
        let token = generate_setup_token();
        let mut salt = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        let created_at = Utc::now().timestamp();
        let record = SetupTokenRecord {
            version: SETUP_TOKEN_VERSION,
            salt_hex: hex::encode(salt),
            hash_hex: hex::encode(hash_setup_token(&salt, &token)),
            created_at,
            acknowledged_at: None,
        };
        Ok(SetupTokenSnapshot {
            record,
            pending_token: Some(token),
        })
    }

    fn persist_setup_token_snapshot(
        &self,
        snapshot: &SetupTokenSnapshot,
    ) -> Result<(), HttpResponse> {
        if let Some(token) = snapshot.pending_token.as_deref() {
            let pending_path = self.setup_token_pending_path();
            write_text_atomic(&pending_path, token.as_bytes()).map_err(|err| {
                api_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "Failed to persist setup token display state",
                    Some(json!({
                        "details": err.to_string(),
                    })),
                )
            })?;
            if let Err(response) = self.persist_setup_token_record(&snapshot.record) {
                if let Err(cleanup_err) = remove_file_if_exists(&pending_path) {
                    warn!(
                        "secret vault: failed to roll back pending setup token '{}' after record write error: {}",
                        pending_path.display(),
                        cleanup_err
                    );
                }
                return Err(response);
            }
        } else {
            self.persist_setup_token_record(&snapshot.record)?;
            remove_file_if_exists(&self.setup_token_pending_path()).map_err(|err| {
                api_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "Failed to clear setup token display state",
                    Some(json!({
                        "details": err.to_string(),
                    })),
                )
            })?;
        }
        Ok(())
    }

    fn persist_setup_token_record(&self, record: &SetupTokenRecord) -> Result<(), HttpResponse> {
        let bytes = serde_json::to_vec_pretty(record).map_err(|err| {
            api_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "Failed to serialize setup token state",
                Some(json!({
                    "details": err.to_string(),
                })),
            )
        })?;
        write_text_atomic(&self.setup_token_record_path(), &bytes).map_err(|err| {
            api_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "Failed to persist setup token state",
                Some(json!({
                    "details": err.to_string(),
                })),
            )
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SetupTokenRecord {
    version: u32,
    salt_hex: String,
    hash_hex: String,
    created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    acknowledged_at: Option<i64>,
}

#[derive(Debug, Clone)]
struct SetupTokenSnapshot {
    record: SetupTokenRecord,
    pending_token: Option<String>,
}

impl SetupTokenSnapshot {
    fn into_response(self) -> SetupTokenStatusResponse {
        SetupTokenStatusResponse {
            configured: true,
            available: true,
            header_name: SETUP_TOKEN_HEADER.to_string(),
            pending_acknowledgement: self.pending_token.is_some(),
            created_at: Some(self.record.created_at),
            acknowledged_at: self.record.acknowledged_at,
            pending_token: self.pending_token,
            unavailable_reason: None,
            supported_policy_routes: provisioned_policy_route_catalog(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupTokenStatusResponse {
    pub configured: bool,
    #[serde(default = "default_true")]
    pub available: bool,
    pub header_name: String,
    pub pending_acknowledgement: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acknowledged_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    pub supported_policy_routes: ProvisionedPolicyRouteCatalog,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SecretDetailQuery {
    #[serde(default)]
    pub include_fields: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupTokenAcknowledgeRequest {
    pub token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SecretCreateRequest {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub fields: HashMap<String, String>,
    pub injection: ApiInjectionTarget,
    #[serde(default)]
    pub policy: SecretPolicy,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SecretUpdateRequest {
    pub label: String,
    #[serde(default)]
    pub fields: HashMap<String, String>,
    pub injection: ApiInjectionTarget,
    #[serde(default)]
    pub policy: SecretPolicy,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SecretApprovalRequest {
    pub challenge_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SecretSummary {
    pub id: String,
    pub label: String,
    pub created_at: i64,
    pub field_names: Vec<String>,
    pub injection: ApiInjectionTarget,
    pub policy: SecretPolicy,
}

#[derive(Debug, Clone, Serialize)]
pub struct SecretListResponse {
    pub secrets: Vec<SecretSummary>,
    pub total_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SecretDetail {
    pub id: String,
    pub label: String,
    pub created_at: i64,
    pub field_names: Vec<String>,
    pub injection: ApiInjectionTarget,
    pub policy: SecretPolicy,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SecretDetailResponse {
    pub secret: SecretDetail,
}

#[derive(Debug, Clone, Serialize)]
pub struct SecretApprovalsResponse {
    pub approvals: Vec<magician::magician_v2::secrets::store::PendingSecretApproval>,
    pub total_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SecretMutationResponse {
    pub secret: SecretDetail,
}

#[derive(Debug, Clone, Serialize)]
pub struct SecretDeleteResponse {
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SecretApprovalMutationResponse {
    pub approved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApiInjectionTarget {
    Header {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefix: Option<String>,
    },
    FormFields {
        mapping: HashMap<String, String>,
    },
    Cookies {
        cookies: Vec<ApiCookieSpec>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiCookieSpec {
    pub name: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same_site: Option<SameSite>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<i64>,
}

impl TryFrom<InjectionTarget> for ApiInjectionTarget {
    type Error = HttpResponse;

    fn try_from(value: InjectionTarget) -> Result<Self, Self::Error> {
        match value {
            InjectionTarget::Header { name, prefix } => Ok(Self::Header { name, prefix }),
            InjectionTarget::FormFields(mapping) => Ok(Self::FormFields { mapping }),
            InjectionTarget::Cookies(cookies) => Ok(Self::Cookies {
                cookies: cookies
                    .into_iter()
                    .map(|cookie| ApiCookieSpec {
                        name: cookie.name,
                        domain: cookie.domain,
                        path: cookie.path,
                        secure: cookie.secure,
                        http_only: cookie.http_only,
                        same_site: cookie.same_site,
                        expires: cookie.expires,
                    })
                    .collect(),
            }),
            InjectionTarget::Inline => Err(api_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "Provisioned secret contains an invalid inline injection target",
                None,
            )),
        }
    }
}

impl TryFrom<ApiInjectionTarget> for InjectionTarget {
    type Error = HttpResponse;

    fn try_from(value: ApiInjectionTarget) -> Result<Self, Self::Error> {
        match value {
            ApiInjectionTarget::Header { name, prefix } => {
                let trimmed_name = name.trim();
                if trimmed_name.is_empty() {
                    return Err(invalid_request(
                        "Header injection requires a header name",
                        Some(json!({ "field": "injection.name" })),
                    ));
                }
                Ok(InjectionTarget::Header {
                    name: trimmed_name.to_string(),
                    prefix,
                })
            },
            ApiInjectionTarget::FormFields { mapping } => {
                if mapping.is_empty() {
                    return Err(invalid_request(
                        "Form field injection requires at least one field mapping",
                        Some(json!({ "field": "injection.mapping" })),
                    ));
                }
                let mut normalized = HashMap::new();
                for (source_key, target_key) in mapping {
                    let source_key = source_key.trim();
                    let target_key = target_key.trim();
                    if source_key.is_empty() || target_key.is_empty() {
                        return Err(invalid_request(
                            "Form field mappings require non-empty source and target names",
                            Some(json!({ "field": "injection.mapping" })),
                        ));
                    }
                    normalized.insert(source_key.to_string(), target_key.to_string());
                }
                Ok(InjectionTarget::FormFields(normalized))
            },
            ApiInjectionTarget::Cookies { cookies } => {
                if cookies.is_empty() {
                    return Err(invalid_request(
                        "Cookie injection requires at least one cookie spec",
                        Some(json!({ "field": "injection.cookies" })),
                    ));
                }
                let mut normalized = Vec::with_capacity(cookies.len());
                for cookie in cookies {
                    let name = cookie.name.trim();
                    let domain = cookie.domain.trim();
                    let path = cookie.path.trim();
                    if name.is_empty() || domain.is_empty() || path.is_empty() {
                        return Err(invalid_request(
                            "Cookie injection requires non-empty name, domain, and path",
                            Some(json!({ "field": "injection.cookies" })),
                        ));
                    }
                    normalized.push(CookieSpec {
                        name: name.to_string(),
                        domain: domain.to_string(),
                        path: path.to_string(),
                        secure: cookie.secure,
                        http_only: cookie.http_only,
                        same_site: cookie.same_site,
                        expires: cookie.expires,
                    });
                }
                Ok(InjectionTarget::Cookies(normalized))
            },
        }
    }
}

pub fn configure_secret_vault_routes(cfg: &mut web::ServiceConfig) {
    cfg.route(
        "/secrets/setup-token",
        web::get().to(get_setup_token_handler),
    )
    .route(
        "/secrets/setup-token/acknowledge",
        web::post().to(acknowledge_setup_token_handler),
    )
    .route(
        "/secrets/setup-token/rotate",
        web::post().to(rotate_setup_token_handler),
    )
    .route(
        "/secrets/app-data-root-key/rotate",
        web::post().to(rotate_app_data_root_key_handler),
    )
    .route(
        "/secrets/approvals",
        web::get().to(list_secret_approvals_handler),
    )
    .route(
        "/secrets/approve",
        web::post().to(approve_secret_request_handler),
    )
    .route("/secrets", web::get().to(list_secrets_handler))
    .route("/secrets", web::post().to(create_secret_handler))
    .route("/secrets/{id}", web::get().to(get_secret_handler))
    .route("/secrets/{id}", web::put().to(update_secret_handler))
    .route("/secrets/{id}", web::delete().to(delete_secret_handler));
}

pub async fn get_setup_token_handler(
    api: web::Data<SecretVaultApi>,
    req: HttpRequest,
) -> HttpResponse {
    if let Err(response) = api.require_local_request(&req) {
        return response;
    }

    match api.read_setup_token_status() {
        Ok(response) => HttpResponse::Ok().json(response),
        Err(response) => response,
    }
}

pub async fn acknowledge_setup_token_handler(
    api: web::Data<SecretVaultApi>,
    req: HttpRequest,
    body: web::Json<SetupTokenAcknowledgeRequest>,
) -> HttpResponse {
    if let Err(response) = api.require_local_request(&req) {
        return response;
    }

    let token = body.token.trim();
    if token.is_empty() {
        return invalid_request("Setup token is required", Some(json!({ "field": "token" })));
    }

    match api.acknowledge_setup_token(token) {
        Ok(response) => HttpResponse::Ok().json(response),
        Err(response) => response,
    }
}

pub async fn rotate_setup_token_handler(
    api: web::Data<SecretVaultApi>,
    req: HttpRequest,
) -> HttpResponse {
    if let Err(response) = api.require_local_request(&req) {
        return response;
    }

    let current_token = match api.require_setup_token(&req) {
        Ok(token) => token,
        Err(response) => return response,
    };

    match api.rotate_setup_token(&current_token) {
        Ok(response) => HttpResponse::Ok().json(response),
        Err(response) => response,
    }
}

/// Rotate the app-data root only through the localhost-only setup-token
/// boundary. The verified request identity selects the scope that is rekeyed
/// immediately; all other scopes retain the historical generation and lazily
/// rekey on their next writable open.
pub async fn rotate_app_data_root_key_handler(
    vault: web::Data<SecretVaultApi>,
    platform: Option<web::Data<AppPlatformApi>>,
    req: HttpRequest,
) -> HttpResponse {
    if let Err(response) = vault.require_local_request(&req) {
        return response;
    }
    if let Err(response) = vault.require_setup_token(&req) {
        return response;
    }
    let Some(platform) = platform else {
        return api_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "app_data_key_rotation_unavailable",
            "App-data key rotation is unavailable on this runtime",
            None,
        );
    };
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(authenticated) => authenticated,
        Err(response) => return response,
    };
    match platform
        .rotate_app_data_root_key_generation(&authenticated, now)
        .await
    {
        Ok(scope_key_id) => HttpResponse::Ok().json(AppDataRootKeyRotationResponse {
            scope_key_id,
            rotated_at: now,
        }),
        Err(error) => {
            warn!(error = %error, "app-data root-key rotation failed");
            api_error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "app_data_key_rotation_failed",
                "App-data root-key rotation failed closed",
                None,
            )
        },
    }
}

pub async fn list_secrets_handler(
    api: web::Data<SecretVaultApi>,
    req: HttpRequest,
) -> HttpResponse {
    if let Err(response) = api.require_local_request(&req) {
        return response;
    }
    let store = match api.scoped_store(&req) {
        Ok(store) => store,
        Err(response) => return response,
    };
    if let Err(response) = api.ensure_vault_available(store.as_ref()) {
        return response;
    }

    let secrets = store
        .list_provisioned_entries()
        .into_iter()
        .map(|entry| summarize_secret_entry(&entry))
        .collect::<Result<Vec<_>, _>>();

    let secrets = match secrets {
        Ok(secrets) => secrets,
        Err(response) => return response,
    };

    HttpResponse::Ok().json(SecretListResponse {
        total_count: secrets.len(),
        secrets,
    })
}

pub async fn get_secret_handler(
    api: web::Data<SecretVaultApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<SecretDetailQuery>,
) -> HttpResponse {
    if let Err(response) = api.require_local_request(&req) {
        return response;
    }
    let store = match api.scoped_store(&req) {
        Ok(store) => store,
        Err(response) => return response,
    };
    if let Err(response) = api.ensure_vault_available(store.as_ref()) {
        return response;
    }

    let Some(entry) = store.get_provisioned(path.as_str()) else {
        return not_found(
            "Provisioned secret was not found",
            Some(json!({ "secret_id": path.as_str() })),
        );
    };

    if query.include_fields {
        if let Err(response) = api.require_setup_token(&req) {
            return response;
        }
    }

    HttpResponse::Ok().json(SecretDetailResponse {
        secret: match detail_for_entry(&entry, query.include_fields) {
            Ok(secret) => secret,
            Err(response) => return response,
        },
    })
}

pub async fn create_secret_handler(
    api: web::Data<SecretVaultApi>,
    req: HttpRequest,
    body: web::Json<SecretCreateRequest>,
) -> HttpResponse {
    if let Err(response) = api.require_local_request(&req) {
        return response;
    }
    let store = match api.scoped_store(&req) {
        Ok(store) => store,
        Err(response) => return response,
    };
    if let Err(response) = api.ensure_vault_available(store.as_ref()) {
        return response;
    }
    if let Err(response) = api.require_setup_token(&req) {
        return response;
    }

    let payload = body.into_inner();
    let secret_id = match validate_secret_id(&payload.id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    if store.get_provisioned(&secret_id).is_some() {
        return conflict(
            "Provisioned secret already exists",
            Some(json!({ "secret_id": secret_id })),
        );
    }

    let request = match normalize_secret_request_fields(
        &payload.label,
        payload.fields,
        payload.injection,
        payload.policy,
    ) {
        Ok(value) => value,
        Err(response) => return response,
    };

    match store.store_provisioned(
        secret_id.clone(),
        request.label.clone(),
        request.fields,
        request.injection,
        request.policy.clone(),
    ) {
        Ok(_) => {
            store.audit_event(
                SecretAuditEvent::new("provisioned_secret_created")
                    .with_secret_id(secret_id.clone())
                    .with_detail("api"),
            );
            let entry = store
                .get_provisioned(&secret_id)
                .expect("newly created secret missing from store");
            HttpResponse::Created().json(SecretMutationResponse {
                secret: match detail_for_entry(&entry, false) {
                    Ok(secret) => secret,
                    Err(response) => return response,
                },
            })
        },
        Err(err) => secret_store_error_response(err),
    }
}

pub async fn update_secret_handler(
    api: web::Data<SecretVaultApi>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SecretUpdateRequest>,
) -> HttpResponse {
    if let Err(response) = api.require_local_request(&req) {
        return response;
    }
    let store = match api.scoped_store(&req) {
        Ok(store) => store,
        Err(response) => return response,
    };
    if let Err(response) = api.ensure_vault_available(store.as_ref()) {
        return response;
    }
    if let Err(response) = api.require_setup_token(&req) {
        return response;
    }

    let secret_id = path.into_inner();
    if store.get_provisioned(&secret_id).is_none() {
        return not_found(
            "Provisioned secret was not found",
            Some(json!({ "secret_id": secret_id })),
        );
    }

    let payload = body.into_inner();
    let request = match normalize_secret_request_fields(
        &payload.label,
        payload.fields,
        payload.injection,
        payload.policy,
    ) {
        Ok(value) => value,
        Err(response) => return response,
    };

    match store.store_provisioned(
        secret_id.clone(),
        request.label.clone(),
        request.fields,
        request.injection,
        request.policy.clone(),
    ) {
        Ok(_) => {
            store.audit_event(
                SecretAuditEvent::new("provisioned_secret_updated")
                    .with_secret_id(secret_id.clone())
                    .with_detail("api"),
            );
            let entry = store
                .get_provisioned(&secret_id)
                .expect("updated secret missing from store");
            HttpResponse::Ok().json(SecretMutationResponse {
                secret: match detail_for_entry(&entry, false) {
                    Ok(secret) => secret,
                    Err(response) => return response,
                },
            })
        },
        Err(err) => secret_store_error_response(err),
    }
}

pub async fn delete_secret_handler(
    api: web::Data<SecretVaultApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    if let Err(response) = api.require_local_request(&req) {
        return response;
    }
    let store = match api.scoped_store(&req) {
        Ok(store) => store,
        Err(response) => return response,
    };
    if let Err(response) = api.ensure_vault_available(store.as_ref()) {
        return response;
    }
    if let Err(response) = api.require_setup_token(&req) {
        return response;
    }

    let secret_id = path.into_inner();
    match store.delete_provisioned(&secret_id) {
        Ok(true) => {
            store.audit_event(
                SecretAuditEvent::new("provisioned_secret_deleted")
                    .with_secret_id(secret_id)
                    .with_detail("api"),
            );
            HttpResponse::Ok().json(SecretDeleteResponse { deleted: true })
        },
        Ok(false) => not_found(
            "Provisioned secret was not found",
            Some(json!({ "secret_id": secret_id })),
        ),
        Err(err) => secret_store_error_response(err),
    }
}

pub async fn list_secret_approvals_handler(
    api: web::Data<SecretVaultApi>,
    req: HttpRequest,
) -> HttpResponse {
    if let Err(response) = api.require_local_request(&req) {
        return response;
    }
    let store = match api.scoped_store(&req) {
        Ok(store) => store,
        Err(response) => return response,
    };
    if let Err(response) = api.ensure_vault_available(store.as_ref()) {
        return response;
    }

    let approvals = store.list_pending_approvals();
    HttpResponse::Ok().json(SecretApprovalsResponse {
        total_count: approvals.len(),
        approvals,
    })
}

pub async fn approve_secret_request_handler(
    api: web::Data<SecretVaultApi>,
    req: HttpRequest,
    body: web::Json<SecretApprovalRequest>,
) -> HttpResponse {
    if let Err(response) = api.require_local_request(&req) {
        return response;
    }
    let store = match api.scoped_store(&req) {
        Ok(store) => store,
        Err(response) => return response,
    };
    if let Err(response) = api.ensure_vault_available(store.as_ref()) {
        return response;
    }
    if let Err(response) = api.require_setup_token(&req) {
        return response;
    }

    let challenge_id = body.challenge_id.trim();
    if challenge_id.is_empty() {
        return invalid_request(
            "Challenge id is required",
            Some(json!({ "field": "challenge_id" })),
        );
    }

    match store.grant_approval(challenge_id) {
        Ok(approved) => HttpResponse::Ok().json(SecretApprovalMutationResponse { approved }),
        Err(err) => secret_store_error_response(err),
    }
}

#[derive(Debug)]
struct NormalizedSecretRequest {
    label: String,
    fields: HashMap<String, String>,
    injection: InjectionTarget,
    policy: SecretPolicy,
}

fn normalize_secret_request_fields(
    label: &str,
    fields: HashMap<String, String>,
    injection: ApiInjectionTarget,
    policy: SecretPolicy,
) -> Result<NormalizedSecretRequest, HttpResponse> {
    let label = label.trim();
    if label.is_empty() {
        return Err(invalid_request(
            "Secret label is required",
            Some(json!({ "field": "label" })),
        ));
    }

    let fields = normalize_secret_fields(fields)?;
    let injection = InjectionTarget::try_from(injection)?;
    validate_injection_fields(&fields, &injection)?;
    let policy = normalize_policy(policy, &injection)?;

    Ok(NormalizedSecretRequest {
        label: label.to_string(),
        fields,
        injection,
        policy,
    })
}

fn normalize_policy(
    policy: SecretPolicy,
    injection: &InjectionTarget,
) -> Result<SecretPolicy, HttpResponse> {
    let allowed_tools = normalize_string_list(policy.allowed_tools);
    let unsupported_routes = find_unsupported_provisioned_policy_routes(injection, &allowed_tools);
    if !unsupported_routes.is_empty() {
        return Err(invalid_request(
            "Policy allowed_tools contains routes that are not supported for this injection target",
            Some(json!({
                "field": "policy.allowed_tools",
                "unsupported_routes": unsupported_routes,
                "supported_routes": provisioned_policy_route_catalog().for_target(injection),
            })),
        ));
    }

    Ok(SecretPolicy {
        allowed_tools,
        allowed_domains: normalize_string_list(policy.allowed_domains),
        max_uses_per_day: policy.max_uses_per_day.filter(|value| *value > 0),
        requires_approval: policy.requires_approval,
    })
}

fn normalize_secret_fields(
    fields: HashMap<String, String>,
) -> Result<HashMap<String, String>, HttpResponse> {
    if fields.is_empty() {
        return Err(invalid_request(
            "Secret must contain at least one field",
            Some(json!({ "field": "fields" })),
        ));
    }

    let mut normalized = HashMap::with_capacity(fields.len());
    for (key, value) in fields {
        let normalized_key = key.trim();
        if normalized_key.is_empty() {
            return Err(invalid_request(
                "Secret fields require non-empty names",
                Some(json!({ "field": "fields" })),
            ));
        }
        if value.trim().is_empty() {
            return Err(invalid_request(
                "Secret fields require non-empty values",
                Some(json!({
                    "field": "fields",
                    "field_name": normalized_key,
                })),
            ));
        }
        normalized.insert(normalized_key.to_string(), value);
    }
    Ok(normalized)
}

fn validate_injection_fields(
    fields: &HashMap<String, String>,
    injection: &InjectionTarget,
) -> Result<(), HttpResponse> {
    match injection {
        InjectionTarget::Header { .. } => {
            let has_primary = fields.contains_key("value") || fields.len() == 1;
            if !has_primary {
                return Err(invalid_request(
                    "Header injection requires a single field or a field named 'value'",
                    Some(json!({
                        "field": "fields",
                        "expected_field": "value",
                    })),
                ));
            }
        },
        InjectionTarget::FormFields(mapping) => {
            for source_key in mapping.keys() {
                if !fields.contains_key(source_key) {
                    return Err(invalid_request(
                        "Form field injection references a missing secret field",
                        Some(json!({
                            "field": "injection.mapping",
                            "source_key": source_key,
                        })),
                    ));
                }
            }
        },
        InjectionTarget::Cookies(specs) => {
            for (idx, spec) in specs.iter().enumerate() {
                let cookie_key = format!("cookie:{}", spec.name);
                if magician::magician_v2::secrets::cookie_field_value(fields, idx, &spec.name)
                    .is_none()
                {
                    return Err(invalid_request(
                        "Cookie injection references a missing secret field",
                        Some(json!({
                            "field": "injection.cookies",
                            "cookie_name": spec.name,
                            "expected_field": cookie_key,
                        })),
                    ));
                }
            }
        },
        InjectionTarget::Inline => {
            return Err(invalid_request(
                "Provisioned secrets cannot use inline injection",
                Some(json!({
                    "field": "injection.kind",
                })),
            ));
        },
    }
    Ok(())
}

fn normalize_string_list(values: Vec<String>) -> Vec<String> {
    let mut normalized = values
        .into_iter()
        .filter_map(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
}

fn validate_secret_id(raw: &str) -> Result<String, HttpResponse> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(invalid_request(
            "Secret id is required",
            Some(json!({ "field": "id" })),
        ));
    }
    if trimmed.len() > 128 {
        return Err(invalid_request(
            "Secret id is too long",
            Some(json!({ "field": "id" })),
        ));
    }
    let valid = trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':'));
    if !valid {
        return Err(invalid_request(
            "Secret id may only contain ASCII letters, digits, '-', '_', '.', or ':'",
            Some(json!({ "field": "id" })),
        ));
    }
    Ok(trimmed.to_string())
}

fn summarize_secret_entry(entry: &SecretEntry) -> Result<SecretSummary, HttpResponse> {
    Ok(SecretSummary {
        id: entry.id.clone(),
        label: entry.label.clone(),
        created_at: entry.created_at,
        field_names: sorted_field_names(&entry.fields),
        injection: ApiInjectionTarget::try_from(entry.injection.clone())?,
        policy: entry.policy.clone().unwrap_or_default(),
    })
}

fn detail_for_entry(
    entry: &SecretEntry,
    include_fields: bool,
) -> Result<SecretDetail, HttpResponse> {
    Ok(SecretDetail {
        id: entry.id.clone(),
        label: entry.label.clone(),
        created_at: entry.created_at,
        field_names: sorted_field_names(&entry.fields),
        injection: ApiInjectionTarget::try_from(entry.injection.clone())?,
        policy: entry.policy.clone().unwrap_or_default(),
        fields: include_fields.then(|| entry.fields.clone()),
    })
}

fn sorted_field_names(fields: &HashMap<String, String>) -> Vec<String> {
    let mut names = fields.keys().cloned().collect::<Vec<_>>();
    names.sort();
    names
}

const fn default_true() -> bool {
    true
}

fn secret_store_error_response(err: SecretStoreError) -> HttpResponse {
    match err {
        SecretStoreError::InlineProvisionedSecret => invalid_request(
            "Provisioned secrets cannot use inline injection",
            Some(json!({ "field": "injection.kind" })),
        ),
        SecretStoreError::SecretNotFound(secret_id) => not_found(
            "Provisioned secret was not found",
            Some(json!({ "secret_id": secret_id })),
        ),
        SecretStoreError::PolicyDenied(reason) => api_error_response(
            StatusCode::FORBIDDEN,
            "policy_denied",
            "Secret policy denied the request",
            Some(json!({ "reason": reason })),
        ),
        SecretStoreError::ApprovalRequired(challenge_id) => api_error_response(
            StatusCode::CONFLICT,
            "approval_required",
            "Secret use requires approval",
            Some(json!({ "challenge_id": challenge_id })),
        ),
        SecretStoreError::FeatureDisabled { feature, reason } => api_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "feature_disabled",
            format!(
                "The {} secret feature is unavailable on this machine",
                feature.as_str()
            ),
            Some(json!({
                "feature": feature.as_str(),
                "reason": reason,
            })),
        ),
        other => api_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "Secret vault operation failed",
            Some(json!({ "details": other.to_string() })),
        ),
    }
}

fn invalid_request(error: impl Into<String>, details: Option<serde_json::Value>) -> HttpResponse {
    api_error_response(StatusCode::BAD_REQUEST, "invalid_request", error, details)
}

fn not_found(error: impl Into<String>, details: Option<serde_json::Value>) -> HttpResponse {
    api_error_response(StatusCode::NOT_FOUND, "resource_not_found", error, details)
}

fn conflict(error: impl Into<String>, details: Option<serde_json::Value>) -> HttpResponse {
    api_error_response(StatusCode::CONFLICT, "resource_conflict", error, details)
}

fn read_optional_string(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(value) => Ok(Some(value.trim().to_string())),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

fn remove_file_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

fn write_text_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    // The shared durable writer: parent created, unique temp, fsync, rename,
    // parent-dir sync. The previous hand-roll never fsynced at all, so a crash
    // after the rename could publish an empty vault file — the failure the
    // atomic write exists to prevent.
    magician::magician_v2::artifact_v2::io::write_bytes_durably_sync(path, bytes)
}

fn generate_setup_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn hash_setup_token(salt: &[u8], token: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(salt);
    hasher.update(token.as_bytes());
    hasher.finalize().into()
}

fn verify_setup_token(record: &SetupTokenRecord, candidate: &str) -> bool {
    let salt = match hex::decode(&record.salt_hex) {
        Ok(value) => value,
        Err(err) => {
            warn!("secret vault: invalid setup token salt encoding: {}", err);
            return false;
        },
    };
    let expected_hash = match hex::decode(&record.hash_hex) {
        Ok(value) => value,
        Err(err) => {
            warn!("secret vault: invalid setup token hash encoding: {}", err);
            return false;
        },
    };

    hash_setup_token(&salt, candidate).as_slice() == expected_hash.as_slice()
}

fn is_loopback_remote(raw: &str) -> bool {
    let candidate = raw
        .split(',')
        .next()
        .map(str::trim)
        .unwrap_or_default()
        .trim_matches('"');
    if candidate.is_empty() {
        return false;
    }

    if let Ok(addr) = SocketAddr::from_str(candidate) {
        return addr.ip().is_loopback();
    }
    if let Ok(ip) = IpAddr::from_str(candidate) {
        return ip.is_loopback();
    }

    let host = extract_host(candidate);
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    IpAddr::from_str(host)
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

fn extract_host(candidate: &str) -> &str {
    if let Some(stripped) = candidate.strip_prefix('[') {
        if let Some(end_bracket) = stripped.find(']') {
            return &stripped[..end_bracket];
        }
    }
    if candidate.matches(':').count() > 1 {
        return candidate;
    }
    candidate
        .rsplit_once(':')
        .map(|(host, _)| host)
        .unwrap_or(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    use actix_web::{http::StatusCode, test, App};
    use serde_json::json;
    use tempfile::TempDir;

    use magician::magician_v2::secrets::{
        InMemoryKeyProvider, SecretRuntimeCapabilities, SecretStore,
    };

    fn test_v3_root(dir: &TempDir) -> PathBuf {
        dir.path().join("magician_data_v3")
    }

    fn test_scoped_secret_root(dir: &TempDir) -> PathBuf {
        test_v3_root(dir).join("scopes/test/test/secrets")
    }

    fn test_store() -> (TempDir, Arc<SecretStore>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(SecretStore::open(
            Box::new(InMemoryKeyProvider::new()),
            test_scoped_secret_root(&dir),
        ));
        (dir, store)
    }

    fn disabled_test_store() -> (TempDir, Arc<SecretStore>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(SecretStore::open_with_capabilities(
            Box::new(InMemoryKeyProvider::new()),
            test_scoped_secret_root(&dir),
            SecretRuntimeCapabilities::without_durable_storage(
                "macos_keychain",
                "OS keychain backend unavailable",
            ),
        ));
        (dir, store)
    }

    fn local_peer() -> SocketAddr {
        "127.0.0.1:43121".parse().expect("local peer addr")
    }

    fn remote_peer() -> SocketAddr {
        "203.0.113.10:43121".parse().expect("remote peer addr")
    }

    #[actix_rt::test]
    async fn setup_token_is_created_and_acknowledged() {
        let (dir, store) = test_store();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(SecretVaultApi::new_for_tests(
                    test_v3_root(&dir),
                    store,
                )))
                .configure(configure_secret_vault_routes),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/secrets/setup-token")
            .peer_addr(local_peer())
            .to_request();
        let response: SetupTokenStatusResponse = test::call_and_read_body_json(&app, req).await;
        let pending_token = response.pending_token.expect("pending token");

        let req = test::TestRequest::post()
            .uri("/secrets/setup-token/acknowledge")
            .peer_addr(local_peer())
            .set_json(&SetupTokenAcknowledgeRequest {
                token: pending_token.clone(),
            })
            .to_request();
        let response: SetupTokenStatusResponse = test::call_and_read_body_json(&app, req).await;
        assert!(!response.pending_acknowledgement);
        assert!(response.pending_token.is_none());

        let req = test::TestRequest::get()
            .uri("/secrets/setup-token")
            .peer_addr(local_peer())
            .to_request();
        let response: SetupTokenStatusResponse = test::call_and_read_body_json(&app, req).await;
        assert!(!response.pending_acknowledgement);
        assert!(response.pending_token.is_none());
    }

    #[actix_rt::test]
    async fn create_secret_requires_setup_token() {
        let (dir, store) = test_store();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(SecretVaultApi::new_for_tests(
                    test_v3_root(&dir),
                    store,
                )))
                .configure(configure_secret_vault_routes),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/secrets")
            .peer_addr(local_peer())
            .insert_header(("X-Principal", "test"))
            .insert_header(("X-Workspace", "test"))
            .set_json(json!({
                "id": "api-key",
                "label": "API Key",
                "fields": { "value": "top-secret-value" },
                "injection": { "kind": "header", "name": "Authorization", "prefix": "Bearer " },
                "policy": { "allowed_tools": ["http:post"], "allowed_domains": ["api.example.com"] }
            }))
            .to_request();
        let response = test::call_service(&app, req).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_rt::test]
    async fn setup_token_reports_vault_unavailable_when_keychain_is_missing() {
        let (dir, store) = disabled_test_store();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(SecretVaultApi::new_for_tests(
                    test_v3_root(&dir),
                    store,
                )))
                .configure(configure_secret_vault_routes),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/secrets/setup-token")
            .peer_addr(local_peer())
            .to_request();
        let response: SetupTokenStatusResponse = test::call_and_read_body_json(&app, req).await;

        assert!(!response.available);
        assert!(!response.configured);
        assert_eq!(
            response.unavailable_reason.as_deref(),
            Some("OS keychain backend unavailable")
        );
        assert!(response
            .supported_policy_routes
            .http
            .contains(&"http:post".to_string()));
        assert!(response
            .supported_policy_routes
            .browser
            .contains(&"browser:execute".to_string()));
    }

    #[actix_rt::test]
    async fn create_secret_returns_service_unavailable_when_vault_is_disabled() {
        let (dir, store) = disabled_test_store();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(SecretVaultApi::new_for_tests(
                    test_v3_root(&dir),
                    store,
                )))
                .configure(configure_secret_vault_routes),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/secrets")
            .peer_addr(local_peer())
            .insert_header(("X-Principal", "test"))
            .insert_header(("X-Workspace", "test"))
            .insert_header(("X-Magician-Setup-Token", "ignored"))
            .set_json(json!({
                "id": "api-key",
                "label": "API Key",
                "fields": { "value": "top-secret-value" },
                "injection": { "kind": "header", "name": "Authorization", "prefix": "Bearer " },
                "policy": { "allowed_tools": ["http:post"], "allowed_domains": ["api.example.com"] }
            }))
            .to_request();
        let response = test::call_service(&app, req).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[actix_rt::test]
    async fn create_secret_rejects_non_canonical_allowed_tool_route() {
        let (dir, store) = test_store();
        let api = SecretVaultApi::new_for_tests(test_v3_root(&dir), store.clone());
        let status = api
            .read_setup_token_status()
            .expect("setup token status should initialize");
        let token = status
            .pending_token
            .clone()
            .expect("setup token should be returned on first read");

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(api))
                .configure(configure_secret_vault_routes),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/secrets")
            .peer_addr(local_peer())
            .insert_header(("X-Principal", "test"))
            .insert_header(("X-Workspace", "test"))
            .insert_header((SETUP_TOKEN_HEADER, token))
            .set_json(json!({
                "id": "api-key",
                "label": "API Key",
                "fields": { "value": "top-secret-value" },
                "injection": { "kind": "header", "name": "Authorization", "prefix": "Bearer " },
                "policy": { "allowed_tools": ["http:request"], "allowed_domains": ["api.example.com"] }
            }))
            .to_request();
        let response = test::call_service(&app, req).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(
            body.get("details")
                .and_then(|details| details.get("field"))
                .and_then(serde_json::Value::as_str),
            Some("policy.allowed_tools")
        );
    }

    #[actix_rt::test]
    async fn create_secret_rejects_internal_browser_policy_route() {
        let (dir, store) = test_store();
        let api = SecretVaultApi::new_for_tests(test_v3_root(&dir), store.clone());
        let status = api
            .read_setup_token_status()
            .expect("setup token status should initialize");
        let token = status
            .pending_token
            .clone()
            .expect("setup token should be returned on first read");

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(api))
                .configure(configure_secret_vault_routes),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/secrets")
            .peer_addr(local_peer())
            .insert_header(("X-Principal", "test"))
            .insert_header(("X-Workspace", "test"))
            .insert_header((SETUP_TOKEN_HEADER, token))
            .set_json(json!({
                "id": "merchant-session",
                "label": "Merchant Session",
                "fields": { "cookie:0:session": "top-secret-value" },
                "injection": {
                    "kind": "cookies",
                    "cookies": [{ "name": "session", "domain": "merchant.example.com", "path": "/", "secure": true, "http_only": true }]
                },
                "policy": { "allowed_tools": ["browser:restore_state"], "allowed_domains": ["merchant.example.com"] }
            }))
            .to_request();
        let response = test::call_service(&app, req).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = test::read_body_json(response).await;
        let unsupported = body
            .get("details")
            .and_then(|details| details.get("unsupported_routes"))
            .and_then(serde_json::Value::as_array)
            .expect("unsupported routes should be returned");
        assert!(unsupported.contains(&serde_json::Value::String(
            "browser:restore_state".to_string()
        )));
    }

    #[actix_rt::test]
    async fn non_local_requests_are_rejected() {
        let (dir, store) = test_store();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(SecretVaultApi::new_for_tests(
                    test_v3_root(&dir),
                    store,
                )))
                .configure(configure_secret_vault_routes),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/secrets")
            .peer_addr(remote_peer())
            .to_request();
        let response = test::call_service(&app, req).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[actix_rt::test]
    async fn forwarded_loopback_does_not_bypass_non_local_peer_check() {
        let (dir, store) = test_store();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(SecretVaultApi::new_for_tests(
                    test_v3_root(&dir),
                    store,
                )))
                .configure(configure_secret_vault_routes),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/secrets")
            .peer_addr(remote_peer())
            .insert_header(("x-forwarded-for", "127.0.0.1"))
            .to_request();
        let response = test::call_service(&app, req).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[actix_rt::test]
    async fn detail_fields_require_setup_token() {
        let (dir, store) = test_store();
        store
            .store_provisioned(
                "api-key",
                "API Key",
                HashMap::from([("value".to_string(), "top-secret-value".to_string())]),
                InjectionTarget::Header {
                    name: "Authorization".to_string(),
                    prefix: Some("Bearer ".to_string()),
                },
                SecretPolicy::default(),
            )
            .expect("store secret");

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(SecretVaultApi::new_for_tests(
                    test_v3_root(&dir),
                    store,
                )))
                .configure(configure_secret_vault_routes),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/secrets/api-key?include_fields=true")
            .peer_addr(local_peer())
            .insert_header(("X-Principal", "test"))
            .insert_header(("X-Workspace", "test"))
            .to_request();
        let response = test::call_service(&app, req).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
