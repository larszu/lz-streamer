//! Mehrere Kanäle in einer Instanz (KB-02): je Kanal ein eigener Kindprozess
//! unter eigenem Aufpasser, gemeinsame API.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use lz_streamer::admin::DataDir;
use lz_streamer::caps::Capabilities;
use lz_streamer::channel::{Channel, ChannelOptions, ChildMsg};
use lz_streamer::config::{Config, DecoderConfig, EncoderConfig, ExtraChannel, InputKind, OutputKind, Role, SrtMode};
use lz_streamer::latency::free_port;
use lz_streamer::ops::Ops;
use lz_streamer::server::{App, router};
use lz_streamer::supervisor::Supervisor;
use serde_json::Value;
use tower::ServiceExt;

fn encoder(port: u16) -> EncoderConfig {
    let mut e = EncoderConfig::default();
    e.input.kind = InputKind::Test;
    e.video.width = 1920;
    e.video.height = 1080;
    e.video.fps = 50;
    e.video.bitrate_kbps = 8000;
    e.srt.mode = SrtMode::Listener;
    e.srt.port = port;
    e.srt.latency_ms = 80;
    e
}

/// Decoder im Testprozess; zählt Bilder, bis `stop` gesetzt ist. Verbindet neu, wenn der Encoder neu startet.
fn decode(port: u16, frames: Arc<AtomicU64>, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut d = DecoderConfig::default();
        d.srt.mode = SrtMode::Caller;
        d.srt.host = "127.0.0.1".into();
        d.srt.port = port;
        d.srt.latency_ms = 80;
        d.output = OutputKind::None;
        let mut base = 0;
        while !stop.load(Ordering::Relaxed) {
            let Ok(mut ch) = Channel::start_decoder(&d, &ChannelOptions::default()) else {
                std::thread::sleep(Duration::from_millis(300));
                continue;
            };
            let mut last = (0, Instant::now());
            while !stop.load(Ordering::Relaxed) {
                for m in ch.poll() {
                    if let ChildMsg::Status(s) = m {
                        if s.frames > last.0 {
                            last = (s.frames, Instant::now());
                        }
                        frames.store(base + s.frames, Ordering::Relaxed);
                    }
                }
                // Encoder weg: neu verbinden
                if last.1.elapsed() > Duration::from_secs(2) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            base += last.0;
            ch.stop();
        }
    })
}

async fn get_json(r: &axum::Router, uri: &str) -> Value {
    let resp = r.clone().oneshot(Request::get(uri).body(Body::empty()).unwrap()).await.unwrap();
    serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

async fn wait(what: &str, secs: u64, mut ok: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while !ok() {
        assert!(Instant::now() < end, "Zeit abgelaufen: {what}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_channels_1080p50_and_crash_isolation() {
    let _ = gstreamer::init();
    // SAFETY: einziger Test in diesem Prozess, der die Variable setzt
    unsafe { std::env::set_var("LZS_CHANNEL_EXE", env!("CARGO_BIN_EXE_lz-streamer")) };
    let (p1, p2) = (free_port(), free_port());
    let root = std::env::temp_dir().join(format!("lzs-channels-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let data = DataDir::new(root.clone());
    let mut cfg = Config { role: Role::Encoder, encoder: encoder(p1), ..Default::default() };
    cfg.channels = vec![ExtraChannel { name: "Zweitkamera".into(), autostart: true, role: Role::Encoder, encoder: encoder(p2), ..Default::default() }];
    let sup = Supervisor::new(cfg, root.join("config.json"), root.join("run"), data.recordings());
    let app = App::new(sup.clone(), Ops::new(data), Capabilities::probe(), vec![]);
    tokio::spawn(sup.clone().run());
    app.sync_channels(true).await;
    sup.start().await;
    let r = router(app.clone());

    let stop: Arc<AtomicBool> = Arc::default();
    let (f1, f2): (Arc<AtomicU64>, Arc<AtomicU64>) = Default::default();
    let d1 = decode(p1, f1.clone(), stop.clone());
    let d2 = decode(p2, f2.clone(), stop.clone());
    wait("beide Kanäle liefern Bilder", 30, || f1.load(Ordering::Relaxed) > 100 && f2.load(Ordering::Relaxed) > 100).await;

    // gemeinsame API: der zweite Kanal mit eigenem Prozess und 1080p50
    let list = get_json(&r, "/api/channels").await;
    let ch = &list["channels"][0];
    assert_eq!(ch["name"], "Zweitkamera");
    assert_eq!(ch["phase"], "läuft", "{ch}");
    assert_eq!(ch["status"]["resolution"], "1920×1080", "{ch}");
    let pid2 = ch["pid"].as_u64().expect("keine PID") as u32;
    let pid1 = sup.snapshot().await.pid.expect("keine PID des Hauptkanals");
    assert_ne!(pid1, pid2, "beide Kanäle im selben Prozess");
    let fps1 = sup.snapshot().await.status.unwrap().fps;
    let fps2 = ch["status"]["fps"].as_f64().unwrap();
    assert!(fps1 > 45.0 && fps2 > 45.0, "1080p50 parallel nicht in Echtzeit: {fps1} / {fps2}");

    // Absturz des zweiten Kanals: der Hauptkanal liefert ungestört weiter
    let _ = std::process::Command::new("kill").args(["-9", &pid2.to_string()]).status();
    let before = f1.load(Ordering::Relaxed);
    tokio::time::sleep(Duration::from_secs(3)).await;
    let during = f1.load(Ordering::Relaxed) - before;
    assert!(during >= 120, "Hauptkanal stockte beim Absturz des zweiten: {during} Bilder in 3 s");
    assert_eq!(sup.snapshot().await.restarts, 0, "Hauptkanal wurde neu gestartet");

    // der Aufpasser des zweiten Kanals startet ihn neu
    let mut restarted = false;
    let end = Instant::now() + Duration::from_secs(20);
    while Instant::now() < end {
        let l = get_json(&r, "/api/channels").await;
        let c = &l["channels"][0];
        if c["restarts"].as_u64() == Some(1) && c["phase"] == "läuft" && c["pid"].as_u64().is_some_and(|p| p as u32 != pid2) {
            restarted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert!(restarted, "zweiter Kanal kam nicht wieder");
    let back = f2.load(Ordering::Relaxed);
    wait("zweiter Kanal liefert wieder", 20, || f2.load(Ordering::Relaxed) > back + 50).await;

    // Stopp über die API
    let resp = r.clone().oneshot(Request::post("/api/channels/0/stop").body(Body::empty()).unwrap()).await.unwrap();
    assert!(resp.status().is_success());
    stop.store(true, Ordering::Relaxed);
    sup.stop().await;
    let _ = (d1.join(), d2.join());
    let _ = std::fs::remove_dir_all(&root);
}
