use axum::extract::State;
use axum::response::Json;

use crate::AppState;

pub async fn get_processes(State(state): State<AppState>) -> Json<serde_json::Value> {
    let snapshot = state.table.snapshot();
    Json(serde_json::to_value(snapshot).unwrap())
}
