use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use axum::{Json, Router, extract::State, http::StatusCode, routing::{get, post}};
use bench_common::{COMPUTE_HTTP_PORT, ComputeRequest, ComputeResponse, fib};
use rebar_core::executor::{ExecutorConfig, RebarExecutor};
use rebar_core::runtime::Runtime;
use rebar_core::supervisor::{RestartStrategy, SupervisorSpec, start_supervisor};
use rebar_core::time::sleep;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

struct SpawnRequest {
    n: u64,
    reply: oneshot::Sender<u64>,
}

#[derive(Clone)]
struct AppState {
    tx: std_mpsc::Sender<SpawnRequest>,
}

async fn health() -> &'static str {
    "ok"
}

async fn compute(
    State(state): State<AppState>,
    Json(req): Json<ComputeRequest>,
) -> Result<Json<ComputeResponse>, StatusCode> {
    if req.n < 0 || req.n > 92 {
        return Err(StatusCode::BAD_REQUEST);
    }

    let (reply_tx, reply_rx) = oneshot::channel();
    state
        .tx
        .send(SpawnRequest {
            n: req.n as u64,
            reply: reply_tx,
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    reply_rx
        .await
        .map(|result| Json(ComputeResponse { result }))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// Run the RebarExecutor on a dedicated thread, receiving spawn requests
/// from the HTTP layer and dispatching them as rebar processes.
fn runtime_thread(rx: std_mpsc::Receiver<SpawnRequest>) {
    let executor = RebarExecutor::new(ExecutorConfig::default()).unwrap();
    executor.block_on(async {
        let runtime = Runtime::new(1);

        let spec = SupervisorSpec::new(RestartStrategy::OneForOne)
            .max_restarts(100)
            .max_seconds(60);
        let _supervisor = start_supervisor(&runtime, spec, vec![]);

        loop {
            // Drain all pending requests
            loop {
                match rx.try_recv() {
                    Ok(req) => {
                        let reply = req.reply;
                        let n = req.n;
                        runtime.spawn(move |_ctx| async move {
                            let _ = reply.send(fib(n));
                        });
                    }
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
        .route("/compute", post(compute))
        .with_state(state);

    let addr = format!("0.0.0.0:{}", COMPUTE_HTTP_PORT);
    tracing::info!("Compute service listening on {}", addr);
    let listener = TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
