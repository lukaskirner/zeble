//! Connecting to Postgres: the background runtime, TLS, the shared browsing
//! pool and dedicated query sessions.

use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

use anyhow::anyhow;
use deadpool_postgres::{Manager, ManagerConfig, Pool, PoolError, RecyclingMethod, Runtime};
use rustls::{
    DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use tokio_postgres::{CancelToken, Client};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::config::{ConnectionConfig, SslMode};

/// A connection borrowed from the browsing pool; derefs to a `tokio_postgres::Client`.
pub type PooledClient = deadpool_postgres::Object;

/// Connections in the shared pool used by the sidebar and table tabs.
pub const POOL_SIZE: usize = 4;

/// The Tokio runtime all database I/O runs on. GPUI has its own executor, so
/// the UI awaits the `JoinHandle`s this runtime returns.
pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("zeble-db")
            .enable_all()
            .build()
            .expect("failed to start the database runtime")
    })
}

/// Accepts any server certificate, like libpq's `sslmode=prefer|require`.
#[derive(Debug)]
struct NoVerify(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn tls() -> MakeRustlsConnect {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("ring supports the default TLS versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
        .with_no_client_auth();
    MakeRustlsConnect::new(config)
}

fn pg_config(c: &ConnectionConfig) -> tokio_postgres::Config {
    let mut pg = tokio_postgres::Config::new();
    pg.host(c.host.trim())
        .port(c.port)
        .user(c.user.trim())
        .dbname(c.database.trim())
        .application_name("zeble")
        .connect_timeout(Duration::from_secs(10))
        .keepalives(true)
        .ssl_mode(match c.ssl_mode {
            SslMode::Disable => tokio_postgres::config::SslMode::Disable,
            SslMode::Prefer => tokio_postgres::config::SslMode::Prefer,
            SslMode::Require => tokio_postgres::config::SslMode::Require,
        });
    if !c.password.is_empty() {
        pg.password(c.password.as_str());
    }
    if c.read_only {
        pg.options("-c default_transaction_read_only=on");
    }
    pg
}

/// A readable message for a driver error, using the server's message when
/// there is one (the driver's own `Display` only says "db error").
pub fn describe(e: &tokio_postgres::Error) -> String {
    if let Some(db) = e.as_db_error() {
        let mut s = db.message().to_string();
        if let Some(detail) = db.detail() {
            s.push_str("\n");
            s.push_str(detail);
        }
        if let Some(hint) = db.hint() {
            s.push_str("\nHint: ");
            s.push_str(hint);
        }
        return s;
    }
    let mut s = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(err) = source {
        s.push_str(": ");
        s.push_str(&err.to_string());
        source = err.source();
    }
    s
}

pub(crate) fn pg_err(e: tokio_postgres::Error) -> anyhow::Error {
    let closed = e.is_closed();
    let err = anyhow!(describe(&e));
    if closed { err.context(ConnectionLost) } else { err }
}

fn pool_err(e: PoolError) -> anyhow::Error {
    match e {
        PoolError::Backend(e) => pg_err(e).context(ConnectionLost),
        PoolError::Timeout(_) => anyhow!("Timed out waiting for a database connection"),
        other => anyhow!(other.to_string()),
    }
}

/// Marker context on errors caused by a dropped or unreachable connection.
#[derive(Debug, Clone, Copy)]
pub struct ConnectionLost;

impl std::fmt::Display for ConnectionLost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Connection lost")
    }
}

/// True when `err` means the connection is gone rather than a failed query.
pub fn is_connection_lost(err: &anyhow::Error) -> bool {
    err.downcast_ref::<ConnectionLost>().is_some()
}

/// A handle that asks the server to cancel whatever its connection is running.
#[derive(Clone)]
pub struct Canceller(CancelToken);

impl Canceller {
    pub fn cancel(&self) {
        let token = self.0.clone();
        runtime().spawn(async move {
            let _ = token.cancel_query(tls()).await;
        });
    }
}

/// An open connection target: a shared pool for browsing, and a factory for
/// dedicated query sessions.
#[derive(Clone)]
pub struct Db {
    pool: Pool,
    pg: tokio_postgres::Config,
}

impl Db {
    /// Build the pool and open its first connection, returning the server
    /// version string (e.g. "17.2").
    pub async fn connect(config: &ConnectionConfig) -> anyhow::Result<(Db, String)> {
        config.validate()?;
        let pg = pg_config(config);
        let manager = Manager::from_config(
            pg.clone(),
            tls(),
            ManagerConfig {
                recycling_method: RecyclingMethod::Fast,
            },
        );
        let pool = Pool::builder(manager)
            .max_size(POOL_SIZE)
            .runtime(Runtime::Tokio1)
            .wait_timeout(Some(Duration::from_secs(30)))
            .create_timeout(Some(Duration::from_secs(15)))
            .build()
            .map_err(|e| anyhow!(e.to_string()))?;
        let db = Db { pool, pg };
        let version = db.server_version().await?;
        Ok((db, version))
    }

    /// Connect once and close again: the connection form's "Test" button.
    pub async fn test(config: &ConnectionConfig) -> anyhow::Result<String> {
        config.validate()?;
        let session = QuerySession::open_with(pg_config(config)).await?;
        let row = session
            .client
            .query_one("SHOW server_version", &[])
            .await
            .map_err(pg_err)?;
        Ok(row.get::<_, String>(0))
    }

    async fn server_version(&self) -> anyhow::Result<String> {
        let client = self.client().await?;
        let row = client
            .query_one("SHOW server_version", &[])
            .await
            .map_err(pg_err)?;
        Ok(row.get::<_, String>(0))
    }

    /// Borrow a pooled connection.
    pub async fn client(&self) -> anyhow::Result<deadpool_postgres::Object> {
        self.pool.get().await.map_err(pool_err)
    }

    /// Open a dedicated connection for a query tab.
    pub async fn open_session(&self) -> anyhow::Result<QuerySession> {
        QuerySession::open_with(self.pg.clone()).await
    }

    pub fn close(&self) {
        self.pool.close();
    }
}

/// A connection owned by one query tab, so transactions and `SET`s persist
/// between runs.
pub struct QuerySession {
    pub(crate) client: Client,
}

impl QuerySession {
    async fn open_with(pg: tokio_postgres::Config) -> anyhow::Result<QuerySession> {
        let (client, connection) = pg.connect(tls()).await.map_err(pg_err)?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(QuerySession { client })
    }

    pub fn canceller(&self) -> Canceller {
        Canceller(self.client.cancel_token())
    }

    pub fn is_closed(&self) -> bool {
        self.client.is_closed()
    }

    /// Roll back any open transaction (used before closing a tab).
    pub async fn rollback(&self) -> anyhow::Result<()> {
        self.client.batch_execute("ROLLBACK").await.map_err(pg_err)
    }
}

/// A canceller for a pooled connection, taken before running a query on it.
pub fn pooled_canceller(client: &deadpool_postgres::Object) -> Canceller {
    Canceller(client.cancel_token())
}
