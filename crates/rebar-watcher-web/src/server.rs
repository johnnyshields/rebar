use axum::http::header;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::Router;
use tower_http::cors::CorsLayer;

use crate::handlers;
use crate::AppState;
use crate::ws;

const INDEX_HTML: &str = include_str!("../assets/index.html");
const APP_JS: &str = include_str!("../assets/app.js");
const STYLE_CSS: &str = include_str!("../assets/style.css");

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
    Html(INDEX_HTML)
}

async fn app_js() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/javascript")], APP_JS)
}

async fn style_css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css")], STYLE_CSS)
}
