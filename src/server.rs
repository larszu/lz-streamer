//! HTTP-API und Weboberfläche (axum).

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{Mutex, RwLock};

use crate::admin::{self, Role, Settings, User};
use crate::caps::Capabilities;
use crate::channel::ChildCmd;
use crate::config::{Config, StandbyPattern};
use crate::devices::{self, CaptureDevice};
use crate::ops::{self, Ops};
use crate::supervisor::Supervisor;

pub struct App {
    pub sup: Arc<Supervisor>,
    pub ops: Arc<Ops>,
    pub caps: Capabilities,
    pub devices: RwLock<Vec<CaptureDevice>>,
    pub users: RwLock<Vec<User>>,
    sessions: Mutex<HashMap<String, (String, Role)>>,
    pub renderer: Arc<crate::renderer::RendererHub>,
}

impl App {
    pub fn new(sup: Arc<Supervisor>, ops: Arc<Ops>, caps: Capabilities, devices: Vec<CaptureDevice>) -> Arc<Self> {
        let users: Vec<User> = admin::load_json(&ops.data.users());
        Arc::new(Self {
            sup,
            ops,
            caps,
            devices: RwLock::new(devices),
            users: RwLock::new(users),
            sessions: Mutex::new(HashMap::new()),
            renderer: Arc::default(),
        })
    }
}

type S = State<Arc<App>>;

const INDEX: &str = include_str!("../web/index.html");
const CSS: &str = include_str!("../web/app.css");
const JS: &str = include_str!("../web/app.js");
const FONT: &[u8] = include_bytes!("../web/fonts/public-sans-latin-wght-normal.woff2");
const FONT_EXT: &[u8] = include_bytes!("../web/fonts/public-sans-latin-ext-wght-normal.woff2");
const COOKIE: &str = "lzs_session";

fn asset(ct: &'static str, body: impl IntoResponse) -> Response {
    ([(header::CONTENT_TYPE, ct), (header::CACHE_CONTROL, "no-cache")], body).into_response()
}

fn err(code: StatusCode, text: impl Into<String>) -> Response {
    (code, Json(json!({"ok": false, "error": text.into()}))).into_response()
}

fn ok() -> Response {
    Json(json!({"ok": true})).into_response()
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(|| async { asset("text/html; charset=utf-8", INDEX) }))
        .route("/app.css", get(|| async { asset("text/css; charset=utf-8", CSS) }))
        .route("/app.js", get(|| async { asset("text/javascript; charset=utf-8", JS) }))
        .route("/fonts/public-sans.woff2", get(|| async { asset("font/woff2", FONT) }))
        .route("/fonts/public-sans-ext.woff2", get(|| async { asset("font/woff2", FONT_EXT) }))
        .route("/renderer", get(renderer_ws))
        .route("/api/state", get(state))
        .route("/api/config", get(get_config).put(put_config))
        .route("/api/start", post(start))
        .route("/api/stop", post(stop))
        .route("/api/signal", post(signal))
        .route("/api/keyframe", post(keyframe))
        .route("/api/caption", post(caption))
        .route("/api/devices/refresh", post(refresh_devices))
        .route("/api/preview.jpg", get(preview))
        // Verwaltung
        .route("/api/presets", get(presets).post(save_preset))
        .route("/api/presets/{name}", delete(delete_preset))
        .route("/api/presets/{name}/load", post(load_preset))
        .route("/api/settings", get(get_settings).put(put_settings))
        .route("/api/alarms", get(alarms))
        .route("/api/stats", get(stats))
        .route("/api/system", get(system))
        .route("/api/report.csv", get(report))
        .route("/api/report.pdf", get(report_pdf))
        .route("/api/report.html", get(report_html))
        .route("/api/standby", get(standby_list).post(standby_upload))
        .route("/api/standby/{name}", get(standby_get).delete(standby_delete))
        .route("/api/standby/{name}/use", post(standby_use))
        .route("/api/plugins", get(plugins))
        .route("/api/graphics", get(graphics_state).post(graphics_cmd))
        .route("/api/models", get(models))
        .route("/api/models/{name}/download", post(model_download))
        .route("/api/models/{name}", delete(model_delete))
        .route("/api/recordings", get(recordings))
        .route("/api/recordings/{name}", get(recording_get).delete(recording_delete))
        // Anmeldung
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .route("/api/users", get(users).post(add_user))
        .route("/api/users/{name}", delete(delete_user))
        .layer(DefaultBodyLimit::max(25 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(app.clone(), auth))
        .with_state(app)
}

// ------------------------------------------------------------ Anmeldung

fn session_token(headers: &HeaderMap) -> Option<String> {
    let c = headers.get(header::COOKIE)?.to_str().ok()?;
    c.split(';').find_map(|kv| kv.trim().strip_prefix(&format!("{COOKIE}=")).map(|v| v.to_string()))
}

async fn current_user(app: &App, headers: &HeaderMap) -> Option<(String, Role)> {
    let t = session_token(headers)?;
    app.sessions.lock().await.get(&t).cloned()
}

/// Ohne angelegte Benutzer ist die Oberfläche offen (wie bisher, nur
/// lokal erreichbar). Sobald es Benutzer gibt, braucht jeder API-Aufruf
/// eine Sitzung, und die Rolle entscheidet über Schreibzugriffe.
async fn auth(State(app): S, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    // /renderer ist kein /api-Pfad und nimmt nur Verbindungen von 127.0.0.1 an (siehe renderer_ws)
    if !path.starts_with("/api/") || path == "/api/login" || path == "/api/me" {
        return next.run(req).await;
    }
    if app.users.read().await.is_empty() {
        return next.run(req).await;
    }
    let Some((_, role)) = current_user(&app, req.headers()).await else {
        return err(StatusCode::UNAUTHORIZED, "Bitte anmelden");
    };
    if !admin::allowed(role, req.method().as_str(), &path) {
        return err(StatusCode::FORBIDDEN, "Dafür fehlt die Berechtigung");
    }
    next.run(req).await
}

#[derive(Deserialize)]
struct LoginReq {
    name: String,
    password: String,
}

async fn login(State(app): S, Json(r): Json<LoginReq>) -> Response {
    let users = app.users.read().await;
    let Some(u) = users.iter().find(|u| u.name == r.name && admin::verify_password(&r.password, &u.hash)) else {
        drop(users);
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        return err(StatusCode::UNAUTHORIZED, "Name oder Passwort falsch");
    };
    let token = admin::new_token();
    app.sessions.lock().await.insert(token.clone(), (u.name.clone(), u.role));
    let cookie = format!("{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age=604800");
    ([(header::SET_COOKIE, cookie)], Json(json!({"ok": true, "name": u.name, "role": u.role}))).into_response()
}

async fn logout(State(app): S, headers: HeaderMap) -> Response {
    if let Some(t) = session_token(&headers) {
        app.sessions.lock().await.remove(&t);
    }
    ([(header::SET_COOKIE, format!("{COOKIE}=; Path=/; Max-Age=0"))], Json(json!({"ok": true}))).into_response()
}

async fn me(State(app): S, headers: HeaderMap) -> Response {
    let open = app.users.read().await.is_empty();
    match current_user(&app, &headers).await {
        Some((name, role)) => Json(json!({"open": open, "name": name, "role": role})).into_response(),
        None if open => Json(json!({"open": true, "name": null, "role": "admin"})).into_response(),
        None => Json(json!({"open": false, "name": null, "role": null})).into_response(),
    }
}

async fn users(State(app): S) -> Response {
    let list: Vec<_> = app.users.read().await.iter().map(|u| json!({"name": u.name, "role": u.role})).collect();
    Json(list).into_response()
}

#[derive(Deserialize)]
struct NewUser {
    name: String,
    password: String,
    role: Role,
}

async fn add_user(State(app): S, Json(r): Json<NewUser>) -> Response {
    let Some(name) = admin::safe_name(&r.name) else { return err(StatusCode::BAD_REQUEST, "Ungültiger Name") };
    if let Err(e) = admin::check_new_password(&r.password) {
        return err(StatusCode::BAD_REQUEST, e.to_string());
    }
    let mut users = app.users.write().await;
    // Der erste Benutzer ist immer Administrator, sonst sperrt man sich aus.
    let role = if users.is_empty() { Role::Admin } else { r.role };
    let Ok(hash) = admin::hash_password(&r.password) else { return err(StatusCode::INTERNAL_SERVER_ERROR, "Hash") };
    users.retain(|u| u.name != name);
    users.push(User { name: name.clone(), role, hash });
    if let Err(e) = admin::save_json(&app.ops.data.users(), &*users) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    drop(users);
    app.sup.log("info", format!("Benutzer {name} angelegt ({role:?})")).await;
    ok()
}

async fn delete_user(State(app): S, Path(name): Path<String>) -> Response {
    let mut users = app.users.write().await;
    let admins_left = users.iter().filter(|u| u.name != name && u.role == Role::Admin).count();
    if admins_left == 0 && users.iter().any(|u| u.name != name) {
        return err(StatusCode::BAD_REQUEST, "Der letzte Administrator bleibt");
    }
    users.retain(|u| u.name != name);
    let _ = admin::save_json(&app.ops.data.users(), &*users);
    app.sessions.lock().await.retain(|_, (n, _)| *n != name);
    ok()
}

// ------------------------------------------------------------ Grafik-Renderer

async fn renderer_ws(
    State(app): S,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    ws: axum::extract::WebSocketUpgrade,
) -> Response {
    if !addr.ip().is_loopback() {
        return err(StatusCode::FORBIDDEN, "Renderer nur lokal");
    }
    let (sup, hub) = (app.sup.clone(), app.renderer.clone());
    ws.on_upgrade(move |s| crate::renderer::serve(s, sup, hub))
}

// ------------------------------------------------------------ Kanal

async fn state(State(app): S, headers: HeaderMap) -> impl IntoResponse {
    let cfg = app.sup.config().await;
    let snap = app.sup.snapshot().await;
    let devices = app.devices.read().await.clone();
    let alarms = app.ops.alarms.lock().await.clone();
    let open = app.users.read().await.is_empty();
    let me = current_user(&app, &headers).await;
    Json(json!({
        "config": cfg,
        "hints": cfg.hints(),
        "caps": app.caps,
        "missing": app.caps.missing(),
        "devices": devices,
        "detected_input": devices::resolve_input(&cfg.encoder.input, &devices).label(),
        "runtime": snap,
        "alarms": alarms,
        "system": *app.ops.system.lock().await,
        "loudness_target": app.ops.settings.lock().await.alarms.loudness_target,
        "renderer": app.renderer.state.lock().await.clone(),
        "user": {"open": open, "name": me.as_ref().map(|m| m.0.clone()), "role": me.map(|m| m.1).unwrap_or(Role::Admin)},
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn get_config(State(app): S) -> impl IntoResponse {
    Json(app.sup.config().await)
}

async fn put_config(State(app): S, Json(cfg): Json<Config>) -> Response {
    match app.sup.set_config(cfg).await {
        Ok(()) => ok(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn start(State(app): S) -> Response {
    app.sup.start().await;
    ok()
}

async fn stop(State(app): S) -> Response {
    app.sup.stop().await;
    ok()
}

#[derive(Deserialize)]
struct SignalReq {
    lost: bool,
}

async fn signal(State(app): S, Json(r): Json<SignalReq>) -> impl IntoResponse {
    Json(json!({"ok": app.sup.command(&ChildCmd::Signal { lost: r.lost }).await}))
}

async fn keyframe(State(app): S) -> impl IntoResponse {
    Json(json!({"ok": app.sup.command(&ChildCmd::Keyframe).await}))
}

#[derive(Deserialize)]
struct CaptionReq {
    text: String,
}

async fn caption(State(app): S, Json(r): Json<CaptionReq>) -> impl IntoResponse {
    Json(json!({"ok": app.sup.command(&ChildCmd::Caption { text: r.text }).await}))
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

// ------------------------------------------------------------ Profile

async fn presets(State(app): S) -> impl IntoResponse {
    Json(ops::list_presets(&app.ops.data))
}

#[derive(Deserialize)]
struct NameReq {
    name: String,
}

async fn save_preset(State(app): S, Json(r): Json<NameReq>) -> Response {
    let Some(name) = admin::safe_name(&r.name) else { return err(StatusCode::BAD_REQUEST, "Ungültiger Name") };
    let cfg = app.sup.config().await;
    match admin::save_json(&app.ops.data.presets().join(format!("{name}.json")), &cfg) {
        Ok(()) => ok(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn load_preset(State(app): S, Path(name): Path<String>) -> Response {
    let Some(p) = ops::load_preset(&app.ops.data, &name) else { return err(StatusCode::NOT_FOUND, "Profil nicht gefunden") };
    let cfg = admin::apply_preset(&app.sup.config().await, &p);
    match app.sup.set_config(cfg).await {
        Ok(()) => {
            app.sup.log("info", format!("Profil „{name}“ geladen")).await;
            ok()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn delete_preset(State(app): S, Path(name): Path<String>) -> Response {
    let Some(n) = admin::safe_name(&name) else { return err(StatusCode::BAD_REQUEST, "Ungültiger Name") };
    let _ = std::fs::remove_file(app.ops.data.presets().join(format!("{n}.json")));
    ok()
}

// ------------------------------------------------------------ Zeitplan, Alarme, Statistik

async fn get_settings(State(app): S) -> impl IntoResponse {
    Json(app.ops.settings.lock().await.clone())
}

async fn put_settings(State(app): S, Json(s): Json<Settings>) -> Response {
    match app.ops.save_settings(s).await {
        Ok(()) => ok(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn alarms(State(app): S) -> impl IntoResponse {
    Json(app.ops.alarms.lock().await.clone())
}

#[derive(Deserialize)]
struct StatsQ {
    minutes: Option<u64>,
}

async fn stats(State(app): S, Query(q): Query<StatsQ>) -> impl IntoResponse {
    let n = (q.minutes.unwrap_or(60).clamp(1, 24 * 60) * 12) as usize;
    let all = app.ops.stats.lock().await;
    let skip = all.len().saturating_sub(n);
    Json(all.iter().skip(skip).cloned().collect::<Vec<_>>())
}

async fn system(State(app): S) -> impl IntoResponse {
    Json(app.ops.system.lock().await.clone())
}

async fn report(State(app): S) -> Response {
    let stats: Vec<_> = app.ops.stats.lock().await.iter().cloned().collect();
    let events = app.sup.snapshot().await.events;
    let name = format!("lz-streamer-bericht-{}.csv", chrono::Local::now().format("%Y-%m-%d_%H-%M"));
    (
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
        ],
        ops::report_csv(&stats, &events),
    )
        .into_response()
}

// ------------------------------------------------------------ Standbilder

async fn standby_list(State(app): S) -> impl IntoResponse {
    Json(admin::list_files(&app.ops.data.standby(), &[".png", ".jpg", ".jpeg"]))
}

#[derive(Deserialize)]
struct UploadQ {
    name: String,
}

async fn standby_upload(State(app): S, Query(q): Query<UploadQ>, body: Bytes) -> Response {
    let Some(kind) = admin::image_kind(&body) else { return err(StatusCode::BAD_REQUEST, "Nur PNG oder JPEG") };
    let Some(base) = admin::safe_name(&q.name) else { return err(StatusCode::BAD_REQUEST, "Ungültiger Name") };
    let stem = base.rsplit_once('.').map(|(a, _)| a.to_string()).unwrap_or(base);
    let name = format!("{stem}.{kind}");
    match std::fs::write(app.ops.data.standby().join(&name), &body) {
        Ok(()) => Json(json!({"ok": true, "name": name})).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn send_file(path: std::path::PathBuf, ct: &'static str, download: bool) -> Response {
    let Ok(f) = tokio::fs::File::open(&path).await else { return err(StatusCode::NOT_FOUND, "Datei nicht gefunden") };
    let len = f.metadata().await.map(|m| m.len()).unwrap_or(0);
    let mut r = Response::new(Body::from_stream(tokio_util::io::ReaderStream::new(f)));
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, ct.parse().unwrap());
    h.insert(header::CONTENT_LENGTH, len.into());
    if download {
        let n = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if let Ok(v) = format!("attachment; filename=\"{n}\"").parse() {
            h.insert(header::CONTENT_DISPOSITION, v);
        }
    }
    r
}

async fn standby_get(State(app): S, Path(name): Path<String>) -> Response {
    let Some(n) = admin::safe_name(&name) else { return err(StatusCode::BAD_REQUEST, "Ungültiger Name") };
    let ct = if n.ends_with(".png") { "image/png" } else { "image/jpeg" };
    send_file(app.ops.data.standby().join(n), ct, false).await
}

async fn standby_delete(State(app): S, Path(name): Path<String>) -> Response {
    let Some(n) = admin::safe_name(&name) else { return err(StatusCode::BAD_REQUEST, "Ungültiger Name") };
    let _ = std::fs::remove_file(app.ops.data.standby().join(n));
    ok()
}

async fn standby_use(State(app): S, Path(name): Path<String>) -> Response {
    let Some(n) = admin::safe_name(&name) else { return err(StatusCode::BAD_REQUEST, "Ungültiger Name") };
    let path = app.ops.data.standby().join(&n);
    if !path.is_file() {
        return err(StatusCode::NOT_FOUND, "Bild nicht gefunden");
    }
    let mut cfg = app.sup.config().await;
    cfg.encoder.standby.pattern = StandbyPattern::Image;
    cfg.encoder.standby.image_path = path.to_string_lossy().to_string();
    match app.sup.set_config(cfg).await {
        Ok(()) => ok(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ------------------------------------------------------------ Aufnahmen

fn record_dir(app: &App, cfg: &Config) -> std::path::PathBuf {
    let d = cfg.encoder.record.dir.trim();
    if d.is_empty() { app.ops.data.recordings() } else { std::path::PathBuf::from(d) }
}

async fn recordings(State(app): S) -> impl IntoResponse {
    let cfg = app.sup.config().await;
    let dir = record_dir(&app, &cfg);
    let list: Vec<serde_json::Value> = admin::list_files(&dir, &[".ts", ".mp4"])
        .into_iter()
        .map(|f| {
            let subs = admin::subtitle_files(&dir, &f.name);
            let mut v = serde_json::to_value(&f).unwrap_or_default();
            v["subtitles"] = json!(subs);
            v
        })
        .collect();
    Json(list)
}

async fn recording_get(State(app): S, Path(name): Path<String>) -> Response {
    let Some(n) = admin::safe_name(&name) else { return err(StatusCode::BAD_REQUEST, "Ungültiger Name") };
    let cfg = app.sup.config().await;
    let ct = if n.ends_with(".mp4") {
        "video/mp4"
    } else if n.ends_with(".vtt") {
        "text/vtt; charset=utf-8"
    } else if n.ends_with(".srt") {
        "application/x-subrip; charset=utf-8"
    } else {
        "video/mp2t"
    };
    send_file(record_dir(&app, &cfg).join(n), ct, true).await
}

async fn recording_delete(State(app): S, Path(name): Path<String>) -> Response {
    let Some(n) = admin::safe_name(&name) else { return err(StatusCode::BAD_REQUEST, "Ungültiger Name") };
    let cfg = app.sup.config().await;
    match admin::remove_recording(&record_dir(&app, &cfg), &n) {
        Ok(()) => {
            app.sup.log("info", format!("Aufnahme {n} gelöscht")).await;
            ok()
        }
        Err(e) => err(StatusCode::NOT_FOUND, e.to_string()),
    }
}

// ------------------------------------------------------------ Whisper-Modelle

async fn graphics_state(State(app): S) -> impl IntoResponse {
    Json(app.renderer.state.lock().await.clone())
}

async fn graphics_cmd(State(app): S, Json(c): Json<crate::renderer::GfxCommand>) -> axum::response::Response {
    match crate::renderer::gfx_command(&app.renderer, &c).await {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => (axum::http::StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response(),
    }
}

async fn plugins() -> impl IntoResponse {
    Json(crate::plugin::installed().into_iter().map(|(name, path)| json!({ "name": name, "path": path })).collect::<Vec<_>>())
}

async fn models(State(app): S) -> impl IntoResponse {
    let dir = app.sup.models_dir();
    let list: Vec<_> = crate::asr::MODELS
        .iter()
        .map(|(name, _, size)| {
            let f = dir.join(format!("ggml-{name}.bin"));
            let part = dir.join(format!("ggml-{name}.bin.part"));
            json!({
                "name": name,
                "bytes": *size,
                "present": f.is_file(),
                "downloading": part.is_file(),
                "loaded_bytes": std::fs::metadata(&part).map(|m| m.len()).ok(),
                "url": crate::asr::model_url(name),
            })
        })
        .collect();
    Json(list)
}

/// Modell von whisper.cpp (Hugging Face) laden – nur auf Klick in der Oberfläche.
async fn model_download(State(app): S, Path(name): Path<String>) -> Response {
    if !crate::asr::MODELS.iter().any(|(n, _, _)| *n == name) {
        return err(StatusCode::NOT_FOUND, "Unbekanntes Modell");
    }
    let dir = app.sup.models_dir();
    let _ = std::fs::create_dir_all(&dir);
    let part = dir.join(format!("ggml-{name}.bin.part"));
    let done = dir.join(format!("ggml-{name}.bin"));
    if part.is_file() {
        return err(StatusCode::CONFLICT, "Lädt schon");
    }
    let url = crate::asr::model_url(&name);
    let sup = app.sup.clone();
    tokio::spawn(async move {
        sup.log("info", format!("Lade Modell {name} …")).await;
        let st = tokio::process::Command::new("curl").args(["-L", "-f", "-s", "-o"]).arg(&part).arg(&url).status().await;
        match st {
            Ok(s) if s.success() && std::fs::rename(&part, &done).is_ok() => sup.log("ok", format!("Modell {name} geladen")).await,
            _ => {
                let _ = std::fs::remove_file(&part);
                sup.log("error", format!("Modell {name} nicht ladbar")).await
            }
        }
    });
    ok()
}

async fn model_delete(State(app): S, Path(name): Path<String>) -> Response {
    let Some(n) = admin::safe_name(&name) else { return err(StatusCode::BAD_REQUEST, "Ungültiger Name") };
    let _ = std::fs::remove_file(app.sup.models_dir().join(format!("ggml-{n}.bin")));
    ok()
}

// ------------------------------------------------------------ PDF-Bericht

#[derive(Deserialize)]
struct RangeQ {
    from: Option<String>,
    to: Option<String>,
}

/// Zeitpunkt als ms seit 1970 oder „JJJJ-MM-TTTHH:MM“ (Ortszeit).
fn parse_time(s: &str) -> Option<u64> {
    if let Ok(n) = s.parse::<u64>() {
        return Some(n);
    }
    let dt = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M").ok()?;
    dt.and_local_timezone(chrono::Local).single().map(|d| d.timestamp_millis() as u64)
}

async fn report_data(app: &App, q: &RangeQ) -> (String, String) {
    let all: Vec<_> = app.ops.stats.lock().await.iter().cloned().collect();
    let to = q.to.as_deref().and_then(parse_time).unwrap_or_else(|| all.last().map(|p| p.at_ms).unwrap_or(0));
    let from = q.from.as_deref().and_then(parse_time).unwrap_or_else(|| all.first().map(|p| p.at_ms).unwrap_or(0));
    let pts: Vec<_> = all.into_iter().filter(|p| p.at_ms >= from && p.at_ms <= to).collect();
    let events = app.sup.snapshot().await.events;
    let host = app.ops.system.lock().await.hostname.clone();
    let name = format!("lz-streamer-bericht-{}", chrono::Local::now().format("%Y-%m-%d_%H-%M"));
    (crate::report::html(&pts, &events, from, to, &host), name)
}

async fn report_html(State(app): S, Query(q): Query<RangeQ>) -> Response {
    let (html, _) = report_data(&app, &q).await;
    asset("text/html; charset=utf-8", html)
}

async fn report_pdf(State(app): S, Query(q): Query<RangeQ>) -> Response {
    let (html, name) = report_data(&app, &q).await;
    match crate::report::to_pdf(&html, &std::env::temp_dir()).await {
        Ok(pdf) => (
            [
                (header::CONTENT_TYPE, "application/pdf".to_string()),
                (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}.pdf\"")),
            ],
            pdf,
        )
            .into_response(),
        // ohne Chrome: druckfertiges HTML (im Browser „Als PDF sichern“)
        Err(_) => asset("text/html; charset=utf-8", html),
    }
}
