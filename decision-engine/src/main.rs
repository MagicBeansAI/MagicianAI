//! `decision-engine [--config <file>] [--socket <path>] [--print-socket]`
//!
//! `--print-socket` prints the socket this invocation would listen on and
//! exits — how `magic-supervisor` learns the path without resolving the
//! runtime root itself.
//!
//! Defaults: `$MAGICIAN_ROOT_DIR/decision-engine.yaml` (the engine's own
//! settings; Magician's config carries none of them) and
//! `$MAGICIAN_ROOT_DIR/run/decision-engine.sock`, with the root resolved as
//! Magician resolves it (`MAGICIAN_ROOT_DIR`, `MAGICIAN_STORAGE_PATH`,
//! `~/MagicianNotes`). Under `magic-supervisor` the socket is passed
//! explicitly. Changes to the settings file are picked up while running
//! (see `decision_engine::watch_settings`).

use std::path::PathBuf;
use std::sync::Arc;

use tracing_subscriber::EnvFilter;

/// The runtime root, resolved exactly as Magician resolves its default
/// (so both sides agree on the socket): `MAGICIAN_ROOT_DIR`, then
/// `MAGICIAN_STORAGE_PATH`, then `~/MagicianNotes`.
fn root_dir() -> PathBuf {
    for key in ["MAGICIAN_ROOT_DIR", "MAGICIAN_STORAGE_PATH"] {
        if let Some(root) = std::env::var(key)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            return PathBuf::from(root);
        }
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join("MagicianNotes")
}

fn arg(name: &str) -> Option<PathBuf> {
    let mut args = std::env::args().skip(1);
    while let Some(current) = args.next() {
        if current == name {
            return args.next().map(PathBuf::from);
        }
        if let Some(value) = current.strip_prefix(&format!("{name}=")) {
            return Some(PathBuf::from(value));
        }
    }
    None
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let root = root_dir();
    let config_path = arg("--config").unwrap_or_else(|| root.join("decision-engine.yaml"));
    let socket = arg("--socket").unwrap_or_else(|| root.join("run").join("decision-engine.sock"));
    // Hosted models read their keys (`api_key_env`, e.g. TYPESAFE_API_KEY)
    // from the runtime env files Magician loads, in Magician's order; a
    // variable already set wins. Loaded before any thread starts.
    for file in [".env.development", ".env"] {
        let _ = dotenvy::from_path(root.join(file));
    }
    // The engine is the one place that resolves its runtime root; a
    // supervisor asks it where it will listen rather than resolving the
    // root itself.
    if std::env::args().any(|arg| arg == "--print-socket") {
        println!("{}", socket.display());
        return Ok(());
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    // Local model folders: relative `model_dir`s are folders under
    // `models_dir` (default <root>/models/decision) — resolved here, with
    // the engine's one root lookup, at start and on every reload.
    let prepare = {
        let (root, home) = (root.clone(), home.clone());
        move |config: &mut magician_decision::config::DecisionConfig| {
            decision_engine::resolve_model_dirs(config, &root, home.as_deref())
        }
    };
    let text = std::fs::read_to_string(&config_path).ok();
    let config = match text
        .as_deref()
        .ok_or_else(|| format!("read {}", config_path.display()))
        .and_then(|text| decision_engine::parse_config(text, &config_path))
    {
        Ok(mut config) => {
            prepare(&mut config);
            config
        },
        Err(error) => {
            tracing::error!(%error, "decision engine: settings unreadable; serving with the plane off");
            Default::default()
        },
    };
    // Local ONNX models run on the ONNX Runtime `onnxruntime_path` names
    // (default <root>/lib/onnxruntime, where `make setup-decision-models`
    // installs it), unless ORT_DYLIB_PATH already names one.
    if std::env::var_os("ORT_DYLIB_PATH").is_none() {
        let library = decision_engine::onnxruntime_library(&config, &root, home.as_deref());
        if library.exists() {
            std::env::set_var("ORT_DYLIB_PATH", &library);
        } else if config.models.values().any(|m| m.adapter.ends_with("-onnx")) {
            tracing::warn!(path = %library.display(), "decision engine: no ONNX Runtime library there; ONNX models will not load");
        }
    }
    tracing::info!(config = %config_path.display(), socket = %socket.display(), "decision engine starting");
    let engine = Arc::new(
        decision_engine::Engine::from_config(config)
            .with_settings_path(config_path.clone(), root.clone()),
    );
    // Settings changes apply without a restart (`onnxruntime_path` aside:
    // the runtime library loads once per process).
    decision_engine::watch_settings(
        Arc::clone(&engine),
        config_path,
        text,
        decision_engine::SETTINGS_POLL,
        prepare,
    )?;
    decision_engine::serve(engine, &socket).await
}
