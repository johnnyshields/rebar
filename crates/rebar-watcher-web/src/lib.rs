mod assets;
mod handlers;
mod server;
mod state;
mod ws;

use std::net::SocketAddr;
use std::sync::Arc;

use rebar_core::runtime::Runtime;

use crate::state::AppState;

/// Configuration for the web watcher.
pub struct WatcherWebConfig {
    pub bind: SocketAddr,
}

impl Default for WatcherWebConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 9090)),
        }
    }
}

/// Handle to a running web watcher server.
pub struct WatcherWebHandle {
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
}

impl WatcherWebHandle {
    /// Shut down the web watcher server.
    pub fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
    }
}

/// Start the web watcher server.
///
/// Requires the runtime to have an attached EventBus.
/// Returns a handle that can be used to shut down the server.
pub async fn start(runtime: &Runtime, config: WatcherWebConfig) -> Result<WatcherWebHandle, String> {
    let event_bus = runtime
        .event_bus()
        .ok_or("runtime has no event bus attached")?
        .clone();

    let state = AppState {
        table: Arc::clone(runtime.table()),
        event_bus,
    };

    let router = server::build_router(state);

    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .map_err(|e| format!("failed to bind {}: {e}", config.bind))?;

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .ok();
    });

    Ok(WatcherWebHandle { shutdown_tx })
}
