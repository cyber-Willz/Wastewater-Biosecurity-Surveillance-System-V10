use std::net::SocketAddr;

use tracing_subscriber::EnvFilter;
use ww_api::{db, router, AppState};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        eprintln!("DATABASE_URL is required, e.g. postgres://user:pass@localhost/ww_biosec");
        std::process::exit(2);
    });
    let bind: SocketAddr = std::env::var("WW_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8080".into())
        .parse()
        .expect("WW_BIND must be host:port");

    let token = match std::env::var("WW_API_TOKEN") {
        Ok(t) if !t.is_empty() => Some(t),
        _ if std::env::var("WW_ALLOW_ANONYMOUS").as_deref() == Ok("1") => {
            tracing::warn!("WW_ALLOW_ANONYMOUS=1: API authentication is DISABLED");
            None
        }
        _ => {
            eprintln!("Set WW_API_TOKEN (bearer token for /v1), or WW_ALLOW_ANONYMOUS=1 to disable auth.");
            std::process::exit(2);
        }
    };

    // Optional namespace: run inside a dedicated schema (created if missing).
    let schema = std::env::var("WW_PG_SCHEMA").ok().filter(|s| !s.is_empty());
    let pool = db::build_pool(&database_url, schema.as_deref()).await.expect("database pool");
    let state = AppState::init_with_url(pool, token, &database_url)
        .await
        .expect("startup (migrate/seed/rebuild)");

    let listener = tokio::net::TcpListener::bind(bind).await.expect("bind");
    tracing::info!(%bind, "ww_api listening");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutting down");
        })
        .await
        .expect("server");
}
