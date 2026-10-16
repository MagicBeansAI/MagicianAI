//! Setup catalog and capability planning for local and remote Desktop clients.
//!
//! Probes run beside Magician, where its files, services, and environment
//! actually live. A desktop connected to a remote engine must not inspect its
//! own filesystem and mistake that for the server's installation state.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use actix_web::{web, HttpRequest, HttpResponse};
use magician_components::setup::{ConfigurationFileValidator, SetupDriver};
use magician_components::{Graph, Host, InstallAction, Observed, Report};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};
use uuid::Uuid;

use crate::secret_vault_api::SecretVaultApi;

const INSTALL_OUTPUT_LIMIT: usize = 64 * 1024;

#[derive(Clone)]
pub struct ComponentSetupApi {
    repo_root: PathBuf,
    jobs: Arc<Mutex<BTreeMap<String, ComponentInstallJob>>>,
}

impl ComponentSetupApi {
    pub fn new(repo_root: PathBuf) -> Self {
        Self {
            repo_root,
            jobs: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentInstallJob {
    pub id: String,
    pub component_id: String,
    pub component_name: String,
    /// `queued`, `running`, `succeeded`, or `failed`.
    pub phase: String,
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed: Option<Observed>,
}

#[derive(Debug, Serialize)]
pub struct ComponentCatalogResponse {
    pub host: Host,
    pub graph: Graph,
    pub observed: BTreeMap<String, Observed>,
    pub report: Report,
}

#[derive(Debug, Deserialize)]
pub struct ComponentPlanRequest {
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ComponentConfigurationFileRequest {
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct ComponentPlanResponse {
    pub host: Host,
    pub graph: Graph,
    pub observed: BTreeMap<String, Observed>,
    pub plan: magician_components::selection::SelectionPlan,
    pub projected: Report,
}

async fn snapshot() -> (Graph, Host, BTreeMap<String, Observed>) {
    let graph = magician_components::loader::load(&magician_components::loader::Paths::from_env());
    let host = magician_components::probe::detect_host();
    let observed = magician_components::probe::observe_all(&graph.components).await;
    (graph, host, observed)
}

/// `GET /api/magician/v2/components/catalog`
///
/// Read-only setup vocabulary plus a fresh server-side observation. Install
/// actions contain instructions and credential names, never credential values.
pub async fn get_component_catalog_handler() -> HttpResponse {
    let (graph, host, observed) = snapshot().await;
    let report = magician_components::resolve(&graph, &BTreeMap::new(), &observed);
    HttpResponse::Ok().json(ComponentCatalogResponse {
        host,
        graph,
        observed,
        report,
    })
}

/// `POST /api/magician/v2/components/plan`
///
/// Resolve required components and provider choices for a selected set of
/// capability ids. Unknown ids fail closed instead of silently producing an
/// incomplete plan.
pub async fn plan_components_handler(body: web::Json<ComponentPlanRequest>) -> HttpResponse {
    let graph = magician_components::loader::load(&magician_components::loader::Paths::from_env());
    let host = magician_components::probe::detect_host();
    let known: BTreeSet<&str> = graph
        .features
        .iter()
        .map(|feature| feature.id.as_str())
        .collect();
    let unknown: Vec<&str> = body
        .features
        .iter()
        .map(String::as_str)
        .filter(|id| !known.contains(id))
        .collect();
    if !unknown.is_empty() {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "message": format!("unknown capability ids: {}", unknown.join(", "))
        }));
    }

    let observed = magician_components::probe::observe_all(&graph.components).await;
    let wanted: BTreeSet<String> = body.features.iter().cloned().collect();
    let plan = magician_components::selection::plan_selection(&graph, &observed, &wanted, &host);
    let projected = magician_components::selection::projected_report(&graph, &observed, &plan);
    HttpResponse::Ok().json(ComponentPlanResponse {
        host,
        graph,
        observed,
        plan,
        projected,
    })
}

/// `GET /api/magician/v2/components/admin-access`
///
/// A side-effect-free setup-token check for installers. The setup token is
/// accepted remotely while its creation/rotation endpoints remain
/// localhost-only, so a Desktop enrolled with a remote engine can administer
/// only the engine whose token the operator explicitly supplied.
pub async fn check_component_admin_access_handler(
    req: HttpRequest,
    vault: web::Data<SecretVaultApi>,
) -> HttpResponse {
    match vault.require_setup_token(&req) {
        Ok(_) => HttpResponse::Ok().json(serde_json::json!({ "authorized": true })),
        Err(response) => response,
    }
}

/// Store one graph-declared setup file on the Magician server. Desktop sends
/// the selected file to the connected engine, so remote/container installs do
/// not accidentally configure the laptop instead. Destinations and validators
/// come from the reviewed setup catalog; the response never echoes content.
pub async fn put_component_configuration_file_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ComponentConfigurationFileRequest>,
    vault: web::Data<SecretVaultApi>,
) -> HttpResponse {
    if let Err(response) = vault.require_setup_token(&req) {
        return response;
    }
    let paths = magician_components::loader::Paths::from_env();
    let graph = magician_components::loader::load(&paths);
    let component_id = path.into_inner();
    let Some(component) = graph.component(&component_id) else {
        return HttpResponse::NotFound().json(serde_json::json!({
            "message": format!("unknown component {component_id}")
        }));
    };
    let Some(setup) = component.setup.as_ref() else {
        return HttpResponse::UnprocessableEntity().json(serde_json::json!({
            "message": "this component does not accept a configuration file"
        }));
    };
    let SetupDriver::ConfigurationFile {
        destination,
        validator,
        max_bytes,
        ..
    } = &setup.driver
    else {
        return HttpResponse::UnprocessableEntity().json(serde_json::json!({
            "message": "this component does not accept a configuration file"
        }));
    };
    if body.content.is_empty() || body.content.len() > *max_bytes {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "message": "configuration file is empty or too large"
        }));
    }
    let validation = match validator {
        ConfigurationFileValidator::GoogleOauthDesktopClient => {
            validate_google_oauth_client(&body.content)
        },
    };
    if let Err(message) = validation {
        return HttpResponse::UnprocessableEntity().json(serde_json::json!({"message": message}));
    }
    let Some(relative_destination) = destination.strip_prefix("{data_root}/") else {
        return HttpResponse::InternalServerError().json(serde_json::json!({
            "message": "compiled setup destination is invalid"
        }));
    };
    let target = PathBuf::from(paths.data_root).join(relative_destination);
    match write_private_file(&target, body.content.as_bytes()) {
        Ok(()) => HttpResponse::Ok().json(serde_json::json!({"configured": true})),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "message": format!("Could not store configuration file: {error}")
        })),
    }
}

fn validate_google_oauth_client(content: &str) -> Result<(), &'static str> {
    let value: serde_json::Value =
        serde_json::from_str(content).map_err(|_| "OAuth client file is not valid JSON")?;
    let installed = value
        .get("installed")
        .and_then(serde_json::Value::as_object)
        .ok_or("Select a Google OAuth Desktop client JSON file")?;
    for key in ["client_id", "client_secret", "auth_uri", "token_uri"] {
        if !installed
            .get(key)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
        {
            return Err("OAuth Desktop client JSON is missing required fields");
        }
    }
    let auth_uri = installed
        .get("auth_uri")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| reqwest::Url::parse(value).ok())
        .filter(|url| url.scheme() == "https" && url.host_str() == Some("accounts.google.com"));
    let token_uri = installed
        .get("token_uri")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| reqwest::Url::parse(value).ok())
        .filter(|url| url.scheme() == "https" && url.host_str() == Some("oauth2.googleapis.com"));
    if auth_uri.is_none() || token_uri.is_none() {
        return Err("OAuth client endpoints are not the expected Google endpoints");
    }
    Ok(())
}

fn write_private_file(path: &std::path::Path, content: &[u8]) -> std::io::Result<()> {
    magician::magician_v2::artifact_v2::io::write_bytes_durably_with_mode_sync(
        path,
        content,
        Some(0o600),
    )
}

/// Start one graph-declared automatic installer action. The client supplies
/// only the component id; target, script and probe always come from the
/// compiled graph. Jobs are serialized because some setup targets download or
/// build large artifacts and this service may be running on a small host.
pub async fn start_component_install_handler(
    req: HttpRequest,
    path: web::Path<String>,
    api: web::Data<ComponentSetupApi>,
    vault: web::Data<SecretVaultApi>,
) -> HttpResponse {
    if let Err(response) = vault.require_setup_token(&req) {
        return response;
    }
    let graph = magician_components::loader::load(&magician_components::loader::Paths::from_env());
    let component_id = path.into_inner();
    let Some(component) = graph.component(&component_id) else {
        return HttpResponse::NotFound().json(serde_json::json!({
            "message": format!("unknown component {component_id}")
        }));
    };
    if let Some(reason) = component.unsupported_on(&magician_components::probe::detect_host()) {
        return HttpResponse::UnprocessableEntity().json(serde_json::json!({"message": reason}));
    }
    let InstallAction::Make { target, script } = &component.install else {
        return HttpResponse::UnprocessableEntity().json(serde_json::json!({
            "message": "this component does not have an automatic installer action"
        }));
    };
    let launch = match install_command(&api.repo_root, target, script.as_deref()) {
        Ok(launch) => launch,
        Err(message) => {
            return HttpResponse::UnprocessableEntity()
                .json(serde_json::json!({"message": message}));
        },
    };

    let mut jobs = api
        .jobs
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if jobs
        .values()
        .any(|job| matches!(job.phase.as_str(), "queued" | "running"))
    {
        return HttpResponse::Conflict().json(serde_json::json!({
            "message": "another component installation is already running"
        }));
    }
    let id = Uuid::new_v4().to_string();
    let job = ComponentInstallJob {
        id: id.clone(),
        component_id: component.id.clone(),
        component_name: component.name.clone(),
        phase: "queued".to_string(),
        output: String::new(),
        error: None,
        observed: None,
    };
    jobs.insert(id.clone(), job.clone());
    drop(jobs);

    let api = api.get_ref().clone();
    let probe = component.probe.clone();
    actix_web::rt::spawn(async move {
        run_install_job(api, id, launch, probe).await;
    });
    HttpResponse::Accepted().json(job)
}

pub async fn get_component_install_handler(
    req: HttpRequest,
    path: web::Path<String>,
    api: web::Data<ComponentSetupApi>,
    vault: web::Data<SecretVaultApi>,
) -> HttpResponse {
    if let Err(response) = vault.require_setup_token(&req) {
        return response;
    }
    let id = path.into_inner();
    let jobs = api
        .jobs
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match jobs.get(&id) {
        Some(job) => HttpResponse::Ok().json(job),
        None => HttpResponse::NotFound().json(serde_json::json!({
            "message": "component installation job not found"
        })),
    }
}

struct InstallCommand {
    program: PathBuf,
    args: Vec<String>,
    current_dir: PathBuf,
}

fn install_command(
    repo_root: &std::path::Path,
    target: &str,
    script: Option<&str>,
) -> Result<InstallCommand, String> {
    if repo_root.join("Makefile").is_file() {
        let path = std::env::var_os("PATH");
        return Ok(InstallCommand {
            program: runtime_core::process::resolve_program(
                std::ffi::OsStr::new("make"),
                path.as_deref(),
            ),
            args: vec![target.to_string()],
            current_dir: repo_root.to_path_buf(),
        });
    }
    let script = script.ok_or_else(|| {
        format!("{target} requires a source checkout and this installation has no Makefile")
    })?;
    if script.contains('/') || script.contains('\\') || matches!(script, "." | "..") {
        return Err(
            "the compiled component graph contains an invalid installer script".to_string(),
        );
    }
    let script_path = repo_root.join("scripts").join(script);
    if !script_path.is_file() {
        return Err(format!(
            "{script} was not included in this Magician installation"
        ));
    }
    let path = std::env::var_os("PATH");
    Ok(InstallCommand {
        program: runtime_core::process::resolve_program(
            std::ffi::OsStr::new("bash"),
            path.as_deref(),
        ),
        args: vec![script_path.display().to_string()],
        current_dir: repo_root.to_path_buf(),
    })
}

async fn run_install_job(
    api: ComponentSetupApi,
    id: String,
    launch: InstallCommand,
    probe: magician_components::ProbeSpec,
) {
    update_job(&api, &id, |job| job.phase = "running".to_string());
    let mut command = tokio::process::Command::new(&launch.program);
    command
        .args(&launch.args)
        .current_dir(&launch.current_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            fail_job(&api, &id, format!("installer could not start: {error}"));
            return;
        },
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_task = stdout.map(|reader| {
        let api = api.clone();
        let id = id.clone();
        tokio::spawn(async move { stream_install_output(reader, api, id).await })
    });
    let stderr_task = stderr.map(|reader| {
        let api = api.clone();
        let id = id.clone();
        tokio::spawn(async move { stream_install_output(reader, api, id).await })
    });
    let status = child.wait().await;
    if let Some(task) = stdout_task {
        let _ = task.await;
    }
    if let Some(task) = stderr_task {
        let _ = task.await;
    }
    let observed = magician_components::probe::observe(&probe).await;
    update_job(&api, &id, |job| job.observed = Some(observed.clone()));
    if status.as_ref().is_ok_and(std::process::ExitStatus::success) && observed.is_present() {
        update_job(&api, &id, |job| job.phase = "succeeded".to_string());
        return;
    }
    let error = match status {
        Ok(status) if status.success() => {
            format!(
                "installer finished but verification failed: {}",
                observed.detail()
            )
        },
        Ok(status) => format!(
            "installer exited with {status}; verification: {}",
            observed.detail()
        ),
        Err(error) => format!(
            "installer wait failed: {error}; verification: {}",
            observed.detail()
        ),
    };
    fail_job(&api, &id, error);
}

async fn stream_install_output<R: AsyncRead + Unpin>(
    mut reader: R,
    api: ComponentSetupApi,
    id: String,
) {
    let mut buffer = [0_u8; 4096];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => {
                let chunk = String::from_utf8_lossy(&buffer[..read]);
                update_job(&api, &id, |job| append_output(&mut job.output, &chunk));
            },
            Err(_) => break,
        }
    }
}

fn append_output(output: &mut String, chunk: &str) {
    output.push_str(chunk);
    if output.len() <= INSTALL_OUTPUT_LIMIT {
        return;
    }
    let mut split = output.len() - INSTALL_OUTPUT_LIMIT;
    while !output.is_char_boundary(split) {
        split += 1;
    }
    output.drain(..split);
}

fn update_job(api: &ComponentSetupApi, id: &str, update: impl FnOnce(&mut ComponentInstallJob)) {
    if let Some(job) = api
        .jobs
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get_mut(id)
    {
        update(job);
    }
}

fn fail_job(api: &ComponentSetupApi, id: &str, error: String) {
    update_job(api, id, |job| {
        job.phase = "failed".to_string();
        job.error = Some(error);
    });
}

pub fn configure_component_routes(cfg: &mut web::ServiceConfig) {
    cfg.route(
        "/components/catalog",
        web::get().to(get_component_catalog_handler),
    )
    .route(
        "/components/admin-access",
        web::get().to(check_component_admin_access_handler),
    )
    .route(
        "/components/{id}/configuration-file",
        web::put().to(put_component_configuration_file_handler),
    )
    .route(
        "/components/installations/{id}",
        web::get().to(get_component_install_handler),
    )
    .route(
        "/components/{id}/install",
        web::post().to(start_component_install_handler),
    )
    .route("/components/plan", web::post().to(plan_components_handler));
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{test as actix_test, App};

    #[actix_web::test]
    async fn plan_rejects_unknown_capabilities() {
        let app = actix_test::init_service(App::new().configure(configure_component_routes)).await;
        let request = actix_test::TestRequest::post()
            .uri("/components/plan")
            .set_json(serde_json::json!({"features": ["not-a-capability"]}))
            .to_request();
        let response = actix_test::call_service(&app, request).await;
        assert_eq!(response.status(), actix_web::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn google_oauth_client_validation_accepts_only_desktop_google_clients() {
        let valid = serde_json::json!({
            "installed": {
                "client_id": "client.apps.googleusercontent.com",
                "client_secret": "secret",
                "auth_uri": "https://accounts.google.com/o/oauth2/auth",
                "token_uri": "https://oauth2.googleapis.com/token"
            }
        });
        assert!(validate_google_oauth_client(&valid.to_string()).is_ok());

        let web = serde_json::json!({"web": valid["installed"].clone()});
        assert!(validate_google_oauth_client(&web.to_string()).is_err());

        let foreign = serde_json::json!({
            "installed": {
                "client_id": "client",
                "client_secret": "secret",
                "auth_uri": "https://example.com/oauth",
                "token_uri": "https://oauth2.googleapis.com/token"
            }
        });
        assert!(validate_google_oauth_client(&foreign.to_string()).is_err());
    }
}
