//! HTTP-API und Weboberfläche (axum).

use std::sync::Arc;

use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::RwLock;

use crate::caps::Capabilities;
use crate::channel::ChildCmd;
use crate::config::Config;
use crate::devices::{self, CaptureDevice};
use crate::supervisor::Supervisor;

pub struct App {
    pub sup: Arc<Supervisor>,
    pub caps: Capabilities,
    pub devices: RwLock<Vec<CaptureDevice>>,
}

type S = State<Arc<App>>;

const INDEX: &str = include_str!("../web/index.html");
const CSS: &str = include_str!("../web/app.css");
const JS: &str = include_str!("../web/app.js");
const FONT: &[u8] = include_bytes!("../web/fonts/public-sans-latin-wght-normal.woff2");
const FONT_EXT: &[u8] = include_bytes!("../web/fonts/public-sans-latin-ext-wght-normal.woff2");

fn asset(ct: &'static str, body: impl IntoResponse) -> Response {
    ([(header::CONTENT_TYPE, ct), (header::CACHE_CONTROL, "no-cache")], body).into_response()
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(|| async { asset("text/html; charset=utf-8", INDEX) }))
        .route("/app.css", get(|| async { asset("text/css; charset=utf-8", CSS) }))
        .route("/app.js", get(|| async { asset("text/javascript; charset=utf-8", JS) }))
        .route("/fonts/public-sans.woff2", get(|| async { asset("font/woff2", FONT) }))
        .route("/fonts/public-sans-ext.woff2", get(|| async { asset("font/woff2", FONT_EXT) }))
        .route("/api/state", get(state))
        .route("/api/config", get(get_config).put(put_config))
        .route("/api/start", post(start))
        .route("/api/stop", post(stop))
        .route("/api/signal", post(signal))
        .route("/api/keyframe", post(keyframe))
        .route("/api/devices/refresh", post(refresh_devices))
        .route("/api/preview.jpg", get(preview))
        .with_state(app)
}

async fn state(State(app): S) -> impl IntoResponse {
    let cfg = app.sup.config().await;
    let snap = app.sup.snapshot().await;
    let devices = app.devices.read().await.clone();
    Json(json!({
        "config": cfg,
        "hints": cfg.hints(),
        "caps": app.caps,
        "missing": app.caps.missing(),
        "devices": devices,
        "detected_input": devices::resolve_input(&cfg.encoder.input, &devices).label(),
        "runtime": snap,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn get_config(State(app): S) -> impl IntoResponse {
    Json(app.sup.config().await)
}

async fn put_config(State(app): S, Json(cfg): Json<Config>) -> impl IntoResponse {
    match app.sup.set_config(cfg).await {
        Ok(()) => (StatusCode::OK, Json(json!({"ok": true}))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e.to_string()}))),
    }
}

async fn start(State(app): S) -> impl IntoResponse {
    app.sup.start().await;
    Json(json!({"ok": true}))
}

async fn stop(State(app): S) -> impl IntoResponse {
    app.sup.stop().await;
    Json(json!({"ok": true}))
}

#[derive(Deserialize)]
struct SignalReq {
    lost: bool,
}

async fn signal(State(app): S, Json(r): Json<SignalReq>) -> impl IntoResponse {
    let ok = app.sup.command(&ChildCmd::Signal { lost: r.lost }).await;
    Json(json!({"ok": ok}))
}

async fn keyframe(State(app): S) -> impl IntoResponse {
    let ok = app.sup.command(&ChildCmd::Keyframe).await;
    Json(json!({"ok": ok}))
}

async fn refresh_devices(State(app): S) -> impl IntoResponse {
    let list = tokio::task::spawn_blocking(devices::decklink_devices).await.unwrap_or_default();
    *app.devices.write().await = list.clone();
    Json(list)
}

async fn preview(State(app): S) -> Response {
    match tokio::fs::read(&app.sup.preview_path).await {
        Ok(b) => ([(header::CONTENT_TYPE, "image/jpeg"), (header::CACHE_CONTROL, "no-store")], b).into_response(),
        Err(_) => StatusCode::NO_CONTENT.into_response(),
    }
}
