use std::time::Duration;

use magician_storage::health::{HealthStatus, StorageHealth};
use magician_storage::profile::{ProfileKind, ResolvedStorageProfile};
use magician_storage::StorageError;
use rustls::ClientConfig;
use tokio_postgres::config::SslMode;
use tokio_postgres::{Client, Config, NoTls};
use tokio_postgres_rustls::MakeRustlsConnect;
use zeroize::Zeroizing;

#[derive(Clone, Debug)]
pub struct PostgresOptions {
    pub require_tls: bool,
    pub max_connections: usize,
    pub connect_timeout: Duration,
}

impl Default for PostgresOptions {
    fn default() -> Self {
        Self {
            require_tls: true,
            max_connections: 8,
            connect_timeout: Duration::from_secs(5),
        }
    }
}

pub struct PostgresPool {
    config: Config,
    options: PostgresOptions,
}

impl std::fmt::Debug for PostgresPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresPool")
            .field("url", &"redacted")
            .field("require_tls", &self.options.require_tls)
            .field("max_connections", &self.options.max_connections)
            .finish()
    }
}

impl PostgresPool {
    pub fn from_url(url: &str, options: PostgresOptions) -> Result<Self, StorageError> {
        let config: Config = url
            .parse()
            .map_err(|_| StorageError::invalid_key("postgres url"))?;
        validate_tls(&config, options.require_tls)?;
        Ok(Self { config, options })
    }

    pub fn health(&self) -> StorageHealth {
        StorageHealth {
            capability: "state_postgres".into(),
            status: HealthStatus::Ok,
            safe_detail: if self.options.require_tls {
                "tls rustls"
            } else {
                "tls disabled"
            }
            .into(),
        }
    }

    pub async fn connect(&self) -> Result<Client, StorageError> {
        if self.options.require_tls {
            let tls = rustls_connector()?;
            let (client, connection) = self.config.connect(tls).await.map_err(pg_err)?;
            tokio::spawn(async move {
                let _ = connection.await;
            });
            return Ok(client);
        }
        let (client, connection) = self.config.connect(NoTls).await.map_err(pg_err)?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(client)
    }
}

fn rustls_connector() -> Result<MakeRustlsConnect, StorageError> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(MakeRustlsConnect::new(config))
}

/// Parse and refuse insecure remote URLs. Does not open a connection.
pub fn open_postgres_config(
    url: &str,
    options: PostgresOptions,
) -> Result<PostgresPool, StorageError> {
    PostgresPool::from_url(url, options)
}

pub fn open_from_profile(
    profile: &ResolvedStorageProfile,
    options: PostgresOptions,
) -> Result<PostgresPool, StorageError> {
    if profile.kind != ProfileKind::RemoteDurable {
        return Err(StorageError::UnsupportedCapability);
    }
    let env_name = profile
        .document
        .state
        .dsn_env
        .as_deref()
        .ok_or_else(|| StorageError::invalid_key("state.dsn_env"))?;
    let url = std::env::var(env_name).map_err(|_| StorageError::invalid_key("state.dsn_env"))?;
    let _guard = Zeroizing::new(url.clone());
    let mut options = options;
    options.require_tls = true;
    PostgresPool::from_url(&url, options)
}

fn validate_tls(config: &Config, require_tls: bool) -> Result<(), StorageError> {
    if !require_tls {
        return Ok(());
    }
    match config.get_ssl_mode() {
        SslMode::Require => Ok(()),
        _ => Err(StorageError::PermissionDenied),
    }
}

pub(crate) fn pg_err(err: tokio_postgres::Error) -> StorageError {
    let text = err.to_string();
    let safe =
        if text.contains("postgres://") || text.contains("password") || text.contains("sslmode") {
            "postgres_error".into()
        } else {
            text
        };
    StorageError::backend(safe)
}
