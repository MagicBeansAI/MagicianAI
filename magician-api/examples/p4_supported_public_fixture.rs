//! Provider-free real-server fixture for the supported-public Apps consumer gate.
//!
//! This binary deliberately mounts the production `configure_app_routes`
//! function over a real `AppPlatformApi`. It seeds the existing registry and
//! entity-store test owners into one private temporary workspace; it does not
//! duplicate handlers or synthesize HTTP responses.

use std::io;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;

use actix_web::{middleware::from_fn, web, App, HttpServer};
use magician::magician_v2::apps::entity_store::tests::{
    compiled_schema, seed_enabled_installation, seed_records,
};
use magician::magician_v2::apps::lifecycle::AppInstallationStatus;
use magician::magician_v2::apps::registry::AppRegistryService;
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::cloudflare_access::{verify_access_middleware, AccessVerifier};
use magician_api::apps_api::{configure_app_routes, AppPlatformApi};

fn ready_file() -> io::Result<PathBuf> {
    std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing ready-file path"))
}

#[actix_web::main]
async fn main() -> io::Result<()> {
    let ready_file = ready_file()?;
    let canonical_temp_root = std::fs::canonicalize(std::env::temp_dir())?;
    let fixture_root = tempfile::tempdir_in(canonical_temp_root)?;
    let workspace = ArtifactV2Workspace::new(fixture_root.path());

    // Seed through the real registry/entity owners. AppPlatformApi opens the
    // same scoped registry from the same workspace below.
    let registry = AppRegistryService::new(workspace.clone());
    let (schema_digest, schema) = compiled_schema();
    seed_enabled_installation(
        &registry,
        schema_digest,
        schema,
        AppInstallationStatus::Enabled,
    )
    .await;
    seed_records(&registry).await;

    let api = AppPlatformApi::new(workspace);
    let verifier: web::Data<Option<Arc<AccessVerifier>>> = web::Data::new(None);
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let address = listener.local_addr()?;
    let origin = format!("http://{address}");

    let server = HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(api.clone()))
            .app_data(verifier.clone())
            .wrap(from_fn(verify_access_middleware))
            .service(web::scope("/api/magician/v2").configure(configure_app_routes))
    })
    .listen(listener)?
    .run();

    std::fs::write(&ready_file, origin.as_bytes())?;
    println!("Magician supported-public real-server fixture listening at {origin}");
    server.await
}
