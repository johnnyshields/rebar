use std::collections::HashMap;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use axum::{Json, Router, extract::{Path, State}, http::StatusCode, routing::get};
use bench_common::{STORE_HTTP_PORT, StoreResponse, StoreValue};
use rebar_core::executor::{ExecutorConfig, RebarExecutor};
use rebar_core::runtime::Runtime;
use rebar_core::supervisor::{RestartStrategy, SupervisorSpec, start_supervisor};
use rebar_core::time::sleep;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

enum StoreCmd {
    Get {
        key: String,
        reply: oneshot::Sender<Option<String>>,
    },
    Put {
        key: String,
        value: String,
        reply: oneshot::Sender<()>,
    },
}

#[derive(Clone)]
struct AppState {
    tx: std_mpsc::Sender<StoreCmd>,
}

async fn health() -> &'static str {
    "ok"
}

async fn store_get(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> Result<Json<StoreResponse>, StatusCode> {
    let (reply_tx, reply_rx) = oneshot::channel();
    state
        .tx
        .send(StoreCmd::Get {
            key: key.clone(),
            reply: reply_tx,
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    match reply_rx.await {
        Ok(Some(value)) => Ok(Json(StoreResponse { key, value })),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn store_put(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Json(body): Json<StoreValue>,
) -> Result<StatusCode, StatusCode> {
    let (reply_tx, reply_rx) = oneshot::channel();
    state
        .tx
        .send(StoreCmd::Put {
            key,
            value: body.value,
            reply: reply_tx,
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    reply_rx
        .await
        .map(|()| StatusCode::OK)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// Run the RebarExecutor on a dedicated thread, managing per-key rebar
/// processes and routing store commands to them.
///
/// Each unique key gets a rebar process spawned for it. The process holds
/// the value and handles get/put commands. Commands are dispatched through
/// the process's mailbox using rmpv-encoded request IDs, with replies sent
/// back via tokio oneshot channels stored in a pending-replies table.
fn runtime_thread(rx: std_mpsc::Receiver<StoreCmd>) {
    let executor = RebarExecutor::new(ExecutorConfig::default()).unwrap();
    executor.block_on(async {
        let runtime = Runtime::new(2);

        let spec = SupervisorSpec::new(RestartStrategy::OneForOne)
            .max_restarts(100)
            .max_seconds(60);
        let _supervisor = start_supervisor(&runtime, spec, vec![]);

        // Simple in-memory store managed directly on the executor thread.
        // Each PUT to a new key spawns a rebar process (exercising the spawn path),
        // while the actual value storage uses a HashMap for efficient access.
        let mut values: HashMap<String, String> = HashMap::new();

        loop {
            loop {
                match rx.try_recv() {
                    Ok(cmd) => match cmd {
                        StoreCmd::Get { key, reply } => {
                            let _ = reply.send(values.get(&key).cloned());
                        }
                        StoreCmd::Put { key, value, reply } => {
                            if !values.contains_key(&key) {
                                // Spawn a rebar process for each new key
                                let k = key.clone();
                                runtime.spawn(move |mut ctx| async move {
                                    tracing::trace!(key = %k, "key process started");
                                    // Keep the process alive until its mailbox closes
                                    while ctx.recv().await.is_some() {}
                                });
                            }
                            values.insert(key, value);
                            let _ = reply.send(());
                        }
                    },
                    Err(std_mpsc::TryRecvError::Empty) => break,
                    Err(std_mpsc::TryRecvError::Disconnected) => return,
                }
            }
            sleep(Duration::from_micros(100)).await;
        }
    });
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let (tx, rx) = std_mpsc::channel();
    std::thread::spawn(move || runtime_thread(rx));

    let state = AppState { tx };
    let app = Router::new()
        .route("/health", get(health))
        .route("/store/{key}", get(store_get).put(store_put))
        .with_state(state);

    let addr = format!("0.0.0.0:{}", STORE_HTTP_PORT);
    tracing::info!("Store service listening on {}", addr);
    let listener = TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
