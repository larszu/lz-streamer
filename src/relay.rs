//! HTTP-Wege der gesicherten Übertragung (siehe `rsp.rs`): Kopplung, Segment-Annahme,
//! verzögertes Live-HLS, Mediathek, Zuschauerzählung und der öffentliche Web-Player.

use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use futures_util::StreamExt;

use crate::config::Role;
use crate::rsp::{self, SegMeta, SessionMeta, now_ms};
use crate::server::App;

type S = State<Arc<App>>;

const PLAYER: &str = include_str!("../web/player.html");

fn err(code: StatusCode, text: impl Into<String>) -> Response {
    (code, Json(json!({"ok": false, "error": text.into()}))).into_response()
}

/// Wege ohne Benutzeranmeldung: Geräte melden sich mit ihrem Zugang, der Player ist öffentlich
pub fn public_path(path: &str) -> bool {
    matches!(path, "/api/rsp/pair" | "/api/rsp/segment" | "/api/rsp/cue" | "/api/rsp/index") || path.starts_with("/api/rsp/seg/") || path.starts_with("/api/player/")
}

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/player", get(|| async { ([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], PLAYER) }))
        .route("/api/player/info", get(player_info))
        .route("/api/player/ping", post(player_ping))
        .route("/live/index.m3u8", get(live_playlist))
        .route("/live/{session}/{name}", get(public_segment))
        .route("/vod/{session}/index.m3u8", get(vod_playlist))
        .route("/vod/{session}/{name}", get(public_segment))
        // Empfangsstelle
        .route("/api/rsp/pairing", post(new_pairing))
        .route("/api/rsp/pair", post(pair))
        .route("/api/rsp/devices", get(devices))
        .route("/api/rsp/devices/{id}", delete(delete_device))
        .route("/api/rsp/segment", put(receive_segment))
        .route("/api/rsp/cue", post(receive_cue))
        .route("/api/rsp/index", get(index))
        .route("/api/rsp/seg/{session}/{name}", get(device_segment))
        .route("/api/rsp/sessions", get(sessions))
        .route("/api/rsp/sessions/{id}", delete(delete_session))
        .route("/api/rsp/sessions/{id}/download", get(download_session))
        .route("/api/rsp/catchup", post(catch_up))
        // Encoder / Standort-Decoder
        .route("/api/rsp/pair-with", post(pair_with))
        .route("/api/rsp/unpair", post(unpair))
        .route("/api/rsp/mark", post(mark))
}

fn relay_root(app: &App) -> PathBuf {
    app.sup.relay_dir()
}

// ------------------------------------------------------------------ Geräte und Kopplung

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// "encoder" oder "decoder"
    pub kind: String,
    /// Hash des Zugangs (der Zugang selbst wird nicht gespeichert)
    pub token_hash: String,
    pub created_ms: u64,
    #[serde(default)]
    pub last_seen_ms: u64,
}

fn devices_file(app: &App) -> PathBuf {
    relay_root(app).join("geraete.json")
}

fn load_devices(app: &App) -> Vec<Device> {
    std::fs::read(devices_file(app)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_devices(app: &App, d: &[Device]) {
    let _ = std::fs::create_dir_all(relay_root(app));
    let _ = std::fs::write(devices_file(app), serde_json::to_vec_pretty(d).unwrap_or_default());
}

/// offene Kopplungscodes: Code → Ablaufzeit
fn codes() -> &'static Mutex<HashMap<String, u64>> {
    static C: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn device_for(app: &App, headers: &HeaderMap) -> Option<Device> {
    let t = headers.get(header::AUTHORIZATION)?.to_str().ok()?.strip_prefix("Bearer ")?.trim();
    if t.len() < 16 {
        return None;
    }
    let h = rsp::hash_hex(t.as_bytes());
    let mut all = load_devices(app);
    let d = all.iter_mut().find(|d| d.token_hash == h)?;
    // „zuletzt gesehen“ höchstens alle 30 s schreiben
    if now_ms().saturating_sub(d.last_seen_ms) > 30_000 {
        d.last_seen_ms = now_ms();
        let out = d.clone();
        save_devices(app, &all);
        return Some(out);
    }
    Some(d.clone())
}

async fn new_pairing(State(app): S) -> Response {
    if app.sup.config().await.role != Role::Relay {
        return err(StatusCode::CONFLICT, "Kopplung gibt es nur in der Rolle „Empfangsstelle“");
    }
    let mut b = [0u8; 4];
    getrandom::getrandom(&mut b).expect("Zufall");
    let code = format!("{:06}", u32::from_le_bytes(b) % 1_000_000);
    let until = now_ms() + 10 * 60_000;
    let mut c = codes().lock().unwrap();
    c.retain(|_, t| *t > now_ms());
    c.insert(code.clone(), until);
    Json(json!({"ok": true, "code": code, "expires_ms": until})).into_response()
}

#[derive(Deserialize)]
struct PairReq {
    code: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: String,
}

async fn pair(State(app): S, Json(r): Json<PairReq>) -> Response {
    let code: String = r.code.chars().filter(|c| c.is_ascii_digit()).collect();
    let valid = {
        let mut c = codes().lock().unwrap();
        let ok = c.get(&code).is_some_and(|t| *t > now_ms());
        if ok {
            c.remove(&code);
        }
        ok
    };
    if !valid {
        // gegen Durchprobieren bremsen
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        return err(StatusCode::FORBIDDEN, "Code ungültig oder abgelaufen");
    }
    let token = crate::admin::new_token();
    let mut all = load_devices(&app);
    let id = format!("g{}", &crate::admin::new_token()[..8]);
    let name = if r.name.trim().is_empty() { format!("Gerät {}", all.len() + 1) } else { r.name.trim().chars().take(60).collect() };
    all.push(Device {
        id: id.clone(),
        name: name.clone(),
        kind: if r.kind == "decoder" { "decoder".into() } else { "encoder".into() },
        token_hash: rsp::hash_hex(token.as_bytes()),
        created_ms: now_ms(),
        last_seen_ms: now_ms(),
    });
    save_devices(&app, &all);
    app.sup.log("ok", format!("Gerät gekoppelt: {name}")).await;
    let relay = hostname();
    Json(json!({"ok": true, "token": token, "id": id, "relay": relay})).into_response()
}

async fn devices(State(app): S) -> Response {
    let list: Vec<_> = load_devices(&app)
        .into_iter()
        .map(|d| json!({"id": d.id, "name": d.name, "kind": d.kind, "created_ms": d.created_ms, "last_seen_ms": d.last_seen_ms}))
        .collect();
    Json(list).into_response()
}

async fn delete_device(State(app): S, Path(id): Path<String>) -> Response {
    let mut all = load_devices(&app);
    let n = all.len();
    all.retain(|d| d.id != id);
    if all.len() == n {
        return err(StatusCode::NOT_FOUND, "Gerät nicht gefunden");
    }
    save_devices(&app, &all);
    Json(json!({"ok": true})).into_response()
}

// ------------------------------------------------------------------ Annahme

fn header_str<'a>(h: &'a HeaderMap, k: &str) -> &'a str {
    h.get(k).and_then(|v| v.to_str().ok()).unwrap_or("")
}

fn session_dir(app: &App, dev: &Device, session: &str) -> Option<PathBuf> {
    rsp::safe_id(session).map(|s| relay_root(app).join(format!("{}_{s}", dev.id)))
}

async fn receive_segment(State(app): S, headers: HeaderMap, body: Bytes) -> Response {
    if app.sup.config().await.role != Role::Relay {
        return err(StatusCode::CONFLICT, "Dieses Gerät ist keine Empfangsstelle");
    }
    let Some(dev) = device_for(&app, &headers) else { return err(StatusCode::UNAUTHORIZED, "Gerät nicht gekoppelt") };
    let name = header_str(&headers, "x-name").to_string();
    let Some((session, run, _)) = rsp::parse_name(&name) else { return err(StatusCode::BAD_REQUEST, "Segmentname ungültig") };
    if rsp::safe_id(&name).is_none() {
        return err(StatusCode::BAD_REQUEST, "Segmentname ungültig");
    }
    let hash = header_str(&headers, "x-hash").to_string();
    if rsp::hash_hex(&body) != hash {
        return err(StatusCode::UNPROCESSABLE_ENTITY, "Prüfsumme stimmt nicht – bitte erneut senden");
    }
    let Some(dir) = session_dir(&app, &dev, session) else { return err(StatusCode::BAD_REQUEST, "Sendung ungültig") };
    let meta = SegMeta {
        name: name.clone(),
        run: run.to_string(),
        wall_ms: header_str(&headers, "x-wall-ms").parse().unwrap_or_else(|_| now_ms()),
        dur_ms: header_str(&headers, "x-dur-ms").parse::<u64>().unwrap_or(2000).clamp(1, 60_000),
        size: body.len() as u64,
        hash,
    };
    let title = rsp::urldecode(header_str(&headers, "x-title"));
    let (dev2, session2, root) = (dev.clone(), session.to_string(), relay_root(&app));
    let res = tokio::task::spawn_blocking(move || -> std::io::Result<bool> {
        let fresh = !dir.join("index.jsonl").exists();
        let new = rsp::store_segment(&dir, &meta, &body)?;
        let mpath = dir.join("meta.json");
        let mut m: SessionMeta = std::fs::read(&mpath).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        if fresh || m.id.is_empty() || (!title.is_empty() && m.title != title) {
            m.id = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            m.device = dev2.name.clone();
            if m.started_ms == 0 {
                m.started_ms = meta.wall_ms;
            }
            if !title.is_empty() {
                m.title = title;
            }
            if m.title.is_empty() {
                m.title = format!("Sendung {session2}");
            }
            std::fs::write(&mpath, serde_json::to_vec_pretty(&m).unwrap_or_default())?;
        }
        if fresh {
            prune_sessions(&root);
        }
        Ok(new)
    })
    .await;
    match res {
        Ok(Ok(new)) => Json(json!({"ok": true, "new": new})).into_response(),
        Ok(Err(e)) => err(StatusCode::INSUFFICIENT_STORAGE, format!("Ablage: {e}")),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "Ablage"),
    }
}

/// Aufbewahrung: alte Sendungen löschen (Einstellung `retention_days`)
fn prune_sessions(root: &FsPath) {
    let days = RETENTION.get().map(|m| *m.lock().unwrap()).unwrap_or(30);
    if days == 0 {
        return;
    }
    let limit = now_ms().saturating_sub(days as u64 * 86_400_000);
    for d in rsp::session_dirs(root) {
        if rsp::read_index(&d).last().is_some_and(|s| s.wall_ms < limit) {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

static RETENTION: OnceLock<Mutex<u32>> = OnceLock::new();

#[derive(Deserialize, Serialize)]
struct CueReq {
    #[serde(default)]
    session: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    wall_ms: u64,
}

async fn receive_cue(State(app): S, headers: HeaderMap, Json(c): Json<CueReq>) -> Response {
    let Some(dev) = device_for(&app, &headers) else { return err(StatusCode::UNAUTHORIZED, "Gerät nicht gekoppelt") };
    let Some(dir) = session_dir(&app, &dev, &c.session) else { return err(StatusCode::BAD_REQUEST, "Sendung ungültig") };
    let _ = std::fs::create_dir_all(&dir);
    use std::io::Write;
    let line = serde_json::to_string(&json!({"label": c.label.chars().take(120).collect::<String>(), "wall_ms": c.wall_ms})).unwrap_or_default();
    match std::fs::OpenOptions::new().create(true).append(true).open(dir.join("cues.jsonl")).and_then(|mut f| writeln!(f, "{line}")) {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

fn read_cues(dir: &FsPath) -> Vec<serde_json::Value> {
    std::fs::read_to_string(dir.join("cues.jsonl")).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

// ------------------------------------------------------------------ Abruf durch Standort-Decoder

#[derive(Deserialize)]
struct IndexQ {
    #[serde(default)]
    session: String,
}

/// Sendung finden: leer = neueste
fn find_session(root: &FsPath, id: &str) -> Option<PathBuf> {
    if id.is_empty() {
        return rsp::latest_session(root);
    }
    let id = rsp::safe_id(id)?;
    let d = root.join(&id);
    d.join("index.jsonl").exists().then_some(d)
}

async fn index(State(app): S, headers: HeaderMap, Query(q): Query<IndexQ>) -> Response {
    if device_for(&app, &headers).is_none() {
        return err(StatusCode::UNAUTHORIZED, "Gerät nicht gekoppelt");
    }
    let Some(dir) = find_session(&relay_root(&app), &q.session) else { return err(StatusCode::NOT_FOUND, "keine Sendung") };
    let id = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    Json(json!({"session": id, "segments": rsp::read_index(&dir)})).into_response()
}

async fn serve_segment(root: PathBuf, session: &str, name: &str) -> Response {
    let (Some(s), Some(n)) = (rsp::safe_id(session), rsp::safe_id(name)) else { return err(StatusCode::BAD_REQUEST, "ungültig") };
    if !n.ends_with(".ts") {
        return err(StatusCode::NOT_FOUND, "nicht gefunden");
    }
    match tokio::fs::read(root.join(s).join(n)).await {
        Ok(b) => ([(header::CONTENT_TYPE, "video/mp2t"), (header::CACHE_CONTROL, "max-age=86400"), (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], b).into_response(),
        Err(_) => err(StatusCode::NOT_FOUND, "nicht gefunden"),
    }
}

async fn device_segment(State(app): S, headers: HeaderMap, Path((session, name)): Path<(String, String)>) -> Response {
    if device_for(&app, &headers).is_none() {
        return err(StatusCode::UNAUTHORIZED, "Gerät nicht gekoppelt");
    }
    serve_segment(relay_root(&app), &session, &name).await
}

// ------------------------------------------------------------------ Öffentlich: Live, Mediathek, Player

fn hls(body: String) -> Response {
    ([(header::CONTENT_TYPE, "application/vnd.apple.mpegurl"), (header::CACHE_CONTROL, "no-store"), (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], body).into_response()
}

/// Live-Zustand der neuesten Sendung: (Ordner, Segmente, Verzögerung, läuft gerade)
async fn live_session(app: &App) -> Option<(PathBuf, Vec<SegMeta>, u64, bool)> {
    let cfg = app.sup.config().await;
    if cfg.role != Role::Relay {
        return None;
    }
    let root = relay_root(app);
    let delay = cfg.relay.delay_s as u64 * 1000;
    tokio::task::spawn_blocking(move || {
        let dir = rsp::latest_session(&root)?;
        let segs = rsp::read_index(&dir);
        let now = now_ms();
        let live = match (segs.first(), segs.last()) {
            (Some(f), Some(l)) => f.wall_ms + delay <= now && l.wall_ms + l.dur_ms + delay + 15_000 >= now,
            _ => false,
        };
        Some((dir, segs, delay, live))
    })
    .await
    .ok()
    .flatten()
}

async fn live_playlist(State(app): S) -> Response {
    let Some((dir, segs, delay, _)) = live_session(&app).await else { return err(StatusCode::NOT_FOUND, "keine Sendung") };
    let id = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    hls(rsp::playlist(&segs, Some(now_ms().saturating_sub(delay)), 6, &format!("{id}/")))
}

async fn vod_playlist(State(app): S, Path(session): Path<String>) -> Response {
    let Some(dir) = find_session(&relay_root(&app), &session) else { return err(StatusCode::NOT_FOUND, "keine Sendung") };
    let cfg = app.sup.config().await;
    let delay = cfg.relay.delay_s as u64 * 1000;
    let segs = rsp::read_index(&dir);
    // Was noch nicht ausgestrahlt ist, gehört nicht in die Mediathek
    let now = now_ms();
    let aired: Vec<SegMeta> = segs.into_iter().filter(|s| s.wall_ms + s.dur_ms + delay <= now).collect();
    hls(rsp::playlist(&aired, None, 0, ""))
}

async fn public_segment(State(app): S, Path((session, name)): Path<(String, String)>) -> Response {
    // nur ausgestrahlte Segmente ausliefern (sonst ließe sich die Verzögerung umgehen)
    let delay = app.sup.config().await.relay.delay_s as u64 * 1000;
    if let Some(dir) = rsp::safe_id(&session).map(|s| relay_root(&app).join(s)) {
        let segs = rsp::read_index(&dir);
        if segs.iter().find(|s| s.name == name).is_none_or(|s| s.wall_ms + delay > now_ms()) {
            return err(StatusCode::NOT_FOUND, "noch nicht ausgestrahlt");
        }
    }
    serve_segment(relay_root(&app), &session, &name).await
}

#[derive(Serialize, Default, Clone)]
struct ViewerStats {
    current: usize,
    peak: usize,
    unique: usize,
    watch_minutes: f64,
}

/// Zuschauer je Sendung: Kennung → (erster, letzter Kontakt)
fn viewers() -> &'static Mutex<HashMap<String, (HashMap<String, (u64, u64)>, usize)>> {
    static V: OnceLock<Mutex<HashMap<String, (HashMap<String, (u64, u64)>, usize)>>> = OnceLock::new();
    V.get_or_init(Default::default)
}

fn stats_for(session: &str, dir: Option<&FsPath>) -> ViewerStats {
    let v = viewers().lock().unwrap();
    if let Some((m, peak)) = v.get(session) {
        let now = now_ms();
        return ViewerStats {
            current: m.values().filter(|(_, l)| now.saturating_sub(*l) < 45_000).count(),
            peak: *peak,
            unique: m.len(),
            watch_minutes: (m.values().map(|(f, l)| l.saturating_sub(*f) + 20_000).sum::<u64>() as f64 / 60_000.0 * 10.0).round() / 10.0,
        };
    }
    drop(v);
    dir.and_then(|d| std::fs::read(d.join("zuschauer.json")).ok())
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .map(|j| ViewerStats {
            current: 0,
            peak: j["peak"].as_u64().unwrap_or(0) as usize,
            unique: j["unique"].as_u64().unwrap_or(0) as usize,
            watch_minutes: j["watch_minutes"].as_f64().unwrap_or(0.0),
        })
        .unwrap_or_default()
}

#[derive(Deserialize)]
struct Ping {
    viewer: String,
    #[serde(default)]
    session: String,
}

async fn player_ping(State(app): S, Json(p): Json<Ping>) -> Response {
    let (Some(viewer), Some(session)) = (rsp::safe_id(&p.viewer), rsp::safe_id(if p.session.is_empty() { "live" } else { &p.session })) else {
        return err(StatusCode::BAD_REQUEST, "ungültig");
    };
    let now = now_ms();
    let persist = {
        let mut v = viewers().lock().unwrap();
        if v.len() > 200 {
            v.clear();
        }
        let (m, peak) = v.entry(session.clone()).or_default();
        if m.len() < 100_000 {
            m.entry(viewer).and_modify(|e| e.1 = now).or_insert((now, now));
        }
        let cur = m.values().filter(|(_, l)| now.saturating_sub(*l) < 45_000).count();
        *peak = (*peak).max(cur);
        true
    };
    if persist {
        let dir = relay_root(&app).join(&session);
        if dir.is_dir() {
            let s = stats_for(&session, None);
            let _ = std::fs::write(dir.join("zuschauer.json"), serde_json::to_vec(&json!({"peak": s.peak, "unique": s.unique, "watch_minutes": s.watch_minutes})).unwrap_or_default());
        }
    }
    Json(json!({"ok": true})).into_response()
}

fn session_summary(dir: &FsPath, delay: u64) -> serde_json::Value {
    let segs = rsp::read_index(dir);
    let meta: SessionMeta = std::fs::read(dir.join("meta.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let id = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let dur: u64 = segs.iter().map(|s| s.dur_ms).sum();
    let start = segs.first().map(|s| s.wall_ms).unwrap_or(meta.started_ms);
    let cues: Vec<_> = read_cues(dir)
        .into_iter()
        .map(|c| json!({"label": c["label"], "offset_s": (c["wall_ms"].as_u64().unwrap_or(start).saturating_sub(start)) as f64 / 1000.0}))
        .collect();
    let now = now_ms();
    json!({
        "id": id,
        "title": meta.title,
        "device": meta.device,
        "started_ms": start,
        "duration_s": dur as f64 / 1000.0,
        "segments": segs.len(),
        "bytes": segs.iter().map(|s| s.size).sum::<u64>(),
        "cues": cues,
        "airing": segs.last().is_some_and(|l| l.wall_ms + l.dur_ms + delay + 15_000 >= now),
        "viewers": stats_for(&id, Some(dir)),
    })
}

async fn sessions(State(app): S) -> Response {
    let root = relay_root(&app);
    let delay = app.sup.config().await.relay.delay_s as u64 * 1000;
    let list = tokio::task::spawn_blocking(move || rsp::session_dirs(&root).iter().rev().map(|d| session_summary(d, delay)).collect::<Vec<_>>()).await.unwrap_or_default();
    Json(list).into_response()
}

async fn delete_session(State(app): S, Path(id): Path<String>) -> Response {
    let Some(dir) = find_session(&relay_root(&app), &id).filter(|_| !id.is_empty()) else { return err(StatusCode::NOT_FOUND, "nicht gefunden") };
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// Ganze Sendung als eine TS-Datei
async fn download_session(State(app): S, Path(id): Path<String>) -> Response {
    let Some(dir) = find_session(&relay_root(&app), &id).filter(|_| !id.is_empty()) else { return err(StatusCode::NOT_FOUND, "nicht gefunden") };
    let files: Vec<PathBuf> = rsp::read_index(&dir).into_iter().map(|s| dir.join(s.name)).collect();
    let stream = futures_util::stream::iter(files).then(|p| async move { tokio::fs::read(p).await.map(Bytes::from) });
    (
        [(header::CONTENT_TYPE, "video/mp2t".to_string()), (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{id}.ts\""))],
        Body::from_stream(stream),
    )
        .into_response()
}

async fn catch_up(State(app): S) -> Response {
    if app.sup.command(&crate::channel::ChildCmd::CatchUp).await {
        Json(json!({"ok": true})).into_response()
    } else {
        err(StatusCode::CONFLICT, "Ausspielung läuft nicht")
    }
}

async fn player_info(State(app): S) -> Response {
    let cfg = app.sup.config().await;
    if let Ok(mut r) = RETENTION.get_or_init(Default::default).lock() {
        *r = cfg.relay.retention_days;
    }
    let p = &cfg.relay.player;
    let base = json!({
        "title": if p.title.trim().is_empty() { cfg.encoder.rsp.title.clone() } else { p.title.clone() },
        "action": if p.action_label.trim().is_empty() || p.action_url.trim().is_empty() { serde_json::Value::Null } else { json!({"label": p.action_label, "url": p.action_url}) },
        "offline_text": p.offline_text,
    });
    let mut out = base;
    match cfg.role {
        Role::Relay => {
            let live = live_session(&app).await;
            let delay = cfg.relay.delay_s as u64 * 1000;
            let root = relay_root(&app);
            let library = if p.library {
                tokio::task::spawn_blocking(move || {
                    rsp::session_dirs(&root).iter().rev().take(50).map(|d| session_summary(d, delay)).filter(|s| s["duration_s"].as_f64().unwrap_or(0.0) > 0.0).collect::<Vec<_>>()
                })
                .await
                .unwrap_or_default()
            } else {
                vec![]
            };
            let (session, is_live) = live.as_ref().map(|(d, _, _, l)| (d.file_name().map(|n| n.to_string_lossy().to_string()), *l)).unwrap_or((None, false));
            out["live"] = json!(is_live);
            out["src"] = json!("/live/index.m3u8");
            out["session"] = json!(session);
            out["delay_s"] = json!(cfg.relay.delay_s);
            out["library"] = json!(library);
        }
        _ => {
            // Encoder mit eigener HLS-Ausgabe: direkt, ohne Empfangsstelle
            let snap = app.sup.snapshot().await;
            out["live"] = json!(cfg.role == Role::Encoder && cfg.encoder.hls.enabled && snap.status.is_some());
            out["src"] = json!("/hls/index.m3u8");
            out["session"] = json!(app.sup.session.lock().unwrap().clone());
            out["library"] = json!([]);
        }
    }
    Json(out).into_response()
}

// ------------------------------------------------------------------ Encoder / Standort: koppeln, Marke setzen

#[derive(Deserialize)]
struct PairWith {
    url: String,
    code: String,
    #[serde(default)]
    name: String,
}

async fn pair_with(State(app): S, Json(r): Json<PairWith>) -> Response {
    let mut cfg = app.sup.config().await;
    let kind = if cfg.role == Role::Decoder { "decoder" } else { "encoder" };
    let name = if r.name.trim().is_empty() { hostname() } else { r.name.trim().to_string() };
    let url = r.url.trim().trim_end_matches('/').to_string();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return err(StatusCode::BAD_REQUEST, "Adresse beginnt mit http:// oder https://");
    }
    let (u2, c2, n2) = (url.clone(), r.code.clone(), name.clone());
    let res = tokio::task::spawn_blocking(move || rsp::pair(&u2, &c2, &n2, kind)).await;
    let (token, relay) = match res {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => return err(StatusCode::BAD_GATEWAY, format!("{e:#}")),
        Err(_) => return err(StatusCode::INTERNAL_SERVER_ERROR, "Kopplung"),
    };
    if kind == "decoder" {
        cfg.decoder.relay.url = url;
        cfg.decoder.relay.token = token;
        cfg.decoder.relay.device_name = name;
    } else {
        cfg.encoder.rsp.url = url;
        cfg.encoder.rsp.token = token;
        cfg.encoder.rsp.device_name = name;
        cfg.encoder.rsp.enabled = true;
    }
    if let Err(e) = app.sup.set_config(cfg).await {
        return err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"));
    }
    app.sup.log("ok", format!("Mit Empfangsstelle {relay} gekoppelt")).await;
    Json(json!({"ok": true, "relay": relay})).into_response()
}

async fn unpair(State(app): S) -> Response {
    let mut cfg = app.sup.config().await;
    if cfg.role == Role::Decoder {
        cfg.decoder.relay.token.clear();
    } else {
        cfg.encoder.rsp.token.clear();
        cfg.encoder.rsp.enabled = false;
    }
    match app.sup.set_config(cfg).await {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    }
}

#[derive(Deserialize)]
struct Mark {
    #[serde(default)]
    label: String,
}

/// Marke (Kapitel) in der laufenden Sendung setzen; geht mit dem nächsten Upload an die Empfangsstelle
async fn mark(State(app): S, Json(m): Json<Mark>) -> Response {
    let session = app.sup.session.lock().unwrap().clone();
    let label = if m.label.trim().is_empty() { chrono::Local::now().format("Marke %H:%M:%S").to_string() } else { m.label.trim().to_string() };
    let body = json!({"session": session, "label": label, "wall_ms": now_ms()});
    let path = app.sup.spool_dir().join(format!("{}.cue", now_ms()));
    let _ = std::fs::create_dir_all(app.sup.spool_dir());
    match std::fs::write(path, body.to_string()) {
        Ok(()) => Json(json!({"ok": true, "label": label})).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// Uploader im Aufpasser-Prozess: hält seine Konfiguration aus dem Aufpasser aktuell
pub fn start_uploader(app: Arc<App>) {
    let cfg: Arc<Mutex<rsp::SendConfig>> = Arc::default();
    let up = rsp::Uploader::spawn(cfg.clone());
    let _ = SEND_STATUS.set(up.status.clone());
    tokio::spawn(async move {
        let _keep = up;
        loop {
            let mut c = app.sup.config().await;
            app.sup.fill_rsp(&mut c);
            if let Ok(mut r) = RETENTION.get_or_init(Default::default).lock() {
                *r = c.relay.retention_days;
            }
            // Empfangsstelle: Sendungen auch ohne neue Uploads aufräumen (stündlich reicht)
            *cfg.lock().unwrap() = if c.role == Role::Encoder { c.encoder.rsp } else { rsp::SendConfig { spool_dir: c.encoder.rsp.spool_dir, ..Default::default() } };
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    });
}

static SEND_STATUS: OnceLock<Arc<Mutex<rsp::SendStatus>>> = OnceLock::new();

/// Zustand des Uploaders für /api/state
pub fn send_status() -> Option<rsp::SendStatus> {
    SEND_STATUS.get().map(|s| s.lock().unwrap().clone())
}

pub fn hostname() -> String {
    sysinfo::System::host_name().unwrap_or_else(|| "lz-streamer".into())
}

pub const MASK: &str = "•••";

/// Gerätezugänge und Stream-Schlüssel nicht an die Oberfläche geben
pub fn mask_config(cfg: &mut crate::config::Config) {
    let m = |s: &mut String| {
        if !s.is_empty() {
            *s = MASK.into();
        }
    };
    m(&mut cfg.encoder.rsp.token);
    m(&mut cfg.decoder.relay.token);
    for d in &mut cfg.relay.destinations {
        m(&mut d.key);
    }
}

/// Beim Speichern: maskierte Werte durch die gespeicherten ersetzen
pub fn keep_secrets(new: &mut crate::config::Config, old: &crate::config::Config) {
    if new.encoder.rsp.token == MASK {
        new.encoder.rsp.token = old.encoder.rsp.token.clone();
    }
    if new.decoder.relay.token == MASK {
        new.decoder.relay.token = old.decoder.relay.token.clone();
    }
    for (i, d) in new.relay.destinations.iter_mut().enumerate() {
        if d.key == MASK {
            // gleiche Position oder gleicher Name
            d.key = old.relay.destinations.get(i).filter(|o| o.name == d.name).or_else(|| old.relay.destinations.iter().find(|o| o.name == d.name)).map(|o| o.key.clone()).unwrap_or_default();
        }
    }
}
