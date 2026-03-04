use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use tokio::sync::broadcast::error::RecvError;

use crate::state::AppState;

pub async fn ws_events(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    // Send initial snapshot
    let snapshot = state.table.snapshot();
    let init = serde_json::json!({
        "type": "snapshot",
        "processes": snapshot,
    });
    if socket
        .send(Message::Text(serde_json::to_string(&init).unwrap().into()))
        .await
        .is_err()
    {
        return;
    }

    // Stream events
    let mut rx = state.event_bus.subscribe();
    loop {
        match rx.recv().await {
            Ok(event) => {
                let json = match serde_json::to_string(&event) {
                    Ok(j) => j,
                    Err(_) => continue,
                };
                if socket.send(Message::Text(json.into())).await.is_err() {
                    break;
                }
            }
            Err(RecvError::Lagged(n)) => {
                let msg = serde_json::json!({
                    "type": "lagged",
                    "missed": n,
                });
                if socket
                    .send(Message::Text(serde_json::to_string(&msg).unwrap().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Err(RecvError::Closed) => break,
        }
    }
}
