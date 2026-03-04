use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::StreamExt;
use tokio::sync::broadcast::error::RecvError;

use crate::AppState;

pub async fn ws_events(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    // Send initial snapshot
    let snapshot = state.table.snapshot();
    let init = serde_json::json!({
        "type": "snapshot",
        "processes": snapshot,
    });

    use futures_util::SinkExt;
    if ws_tx
        .send(Message::Text(serde_json::to_string(&init).unwrap().into()))
        .await
        .is_err()
    {
        return;
    }

    // Stream events, detect client disconnect via select
    let mut rx = state.event_bus.subscribe();
    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    Ok(event) => {
                        let json = match serde_json::to_string(&event) {
                            Ok(j) => j,
                            Err(_) => continue,
                        };
                        if ws_tx.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(RecvError::Lagged(n)) => {
                        let msg = serde_json::json!({
                            "type": "lagged",
                            "missed": n,
                        });
                        if ws_tx
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
            msg = ws_rx.next() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }
}
