use axum::http::header;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::Router;
use tower_http::cors::CorsLayer;

use crate::assets;
use crate::handlers;
use crate::state::AppState;
use crate::ws;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index_html))
        .route("/app.js", get(app_js))
        .route("/style.css", get(style_css))
        .route("/api/processes", get(handlers::get_processes))
        .route("/ws/events", get(ws::ws_events))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn index_html() -> Html<&'static str> {
    Html(assets::INDEX_HTML)
}

async fn app_js() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/javascript")], assets::APP_JS)
}

async fn style_css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css")], assets::STYLE_CSS)
}
