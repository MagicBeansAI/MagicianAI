use actix_web::{web, App, HttpServer};
use magicutor::server::configure_routes;
use tracing::info;
use tracing_subscriber;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Structured logging uses stderr; the supervisor preserves its level.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr) // Write logs to stderr, not stdout
        .init();

    info!(pid = std::process::id(), "Magicutor starting");

    info!("Starting in HTTP CDP-proxy server mode");
    run_http_server().await
}

/// Run as HTTP server
async fn run_http_server() -> std::io::Result<()> {
    info!("Starting Magicutor HTTP server...");

    // Load configuration
    let config = magicutor::load_config().expect("Failed to load configuration");

    info!("Configuration loaded successfully");
    info!(
        "Server will listen on {}:{}",
        config.server.host, config.server.port
    );
    info!(
        "Bridge: enabled={}, path={}",
        config.bridge.enabled, config.bridge.path
    );

    // Bind address for the server
    let bind_addr = format!("{}:{}", config.server.host, config.server.port);

    // ========================================================================
    // EARLY PORT BINDING CHECK - Fail fast if port is already in use
    // ========================================================================
    info!("Attempting to bind to {} (pre-flight check)...", bind_addr);
    match std::net::TcpListener::bind(&bind_addr) {
        Ok(listener) => {
            info!("✅ Successfully bound to {} (port is available)", bind_addr);
            // Drop the listener to release the port for actix-web
            drop(listener);
        },
        Err(e) => {
            eprintln!("❌ Failed to bind to {}: {}", bind_addr, e);
            eprintln!("   Port {} may already be in use", config.server.port);
            eprintln!("   Check with: lsof -i :{}", config.server.port);
            return Err(e);
        },
    }

    let config_data = config.clone();

    // Start HTTP server
    info!("🚀 Magicutor CDP proxy ready on http://{}", bind_addr);

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(config_data.clone()))
            .configure(configure_routes)
    })
    .bind(&bind_addr)?
    .run()
    .await
}
