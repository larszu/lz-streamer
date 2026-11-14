//! Gesicherte Übertragung Ende zu Ende: Encoder → Spool → Upload → Empfangsstelle →
//! Standort-Decoder und Ausspielung an ein Ziel, mit einem Netzausfall kürzer als die Verzögerung.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lz_streamer::admin::DataDir;
use lz_streamer::caps::Capabilities;
use lz_streamer::channel::{Channel, ChannelOptions, ChannelStatus, ChildMsg};
use lz_streamer::config::{Config, DecoderConfig, EncoderConfig, InputKind, OutputKind, Role, SrtMode};
use lz_streamer::ops::Ops;
use lz_streamer::rsp::{self, DecoderSource, DestKind, Destination, Playout, RelayConfig, SendConfig, Uploader};
use lz_streamer::server::{App, router};
use lz_streamer::supervisor::Supervisor;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn status(ch: &mut Channel) -> Option<ChannelStatus> {
    let mut st = None;
    for m in ch.poll() {
        match m {
            ChildMsg::Status(s) => st = Some(s),
            ChildMsg::Fatal { text } => panic!("Kanal gescheitert: {text}"),
            _ => {}
        }
    }
    st
}

/// Empfangsstelle als echter HTTP-Server in einem eigenen Thread
fn start_relay(root: &std::path::Path, delay_s: u32) -> (String, Arc<Supervisor>) {
    let data = DataDir::new(root.to_path_buf());
    let mut cfg = Config { role: Role::Relay, ..Default::default() };
    cfg.relay.delay_s = delay_s;
    let sup = Supervisor::new(cfg, root.join("config.json"), root.join("run"), data.recordings());
    let ops = Ops::new(data);
    let app = App::new(sup.clone(), ops, Capabilities::probe(), vec![]);
    let port = free_port();
    std::thread::spawn(move || {
        tokio::runtime::Runtime::new().unwrap().block_on(async move {
            let l = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.unwrap();
            axum::serve(l, router(app).into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap();
        });
    });
    let url = format!("http://127.0.0.1:{port}");
    let end = Instant::now() + Duration::from_secs(5);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    (url, sup)
}

fn http(method: &str, url: &str) -> (u16, String) {
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let r = if method == "POST" { agent.post(url).header("Content-Type", "application/json").send("{}") } else { agent.get(url).call() }.unwrap();
    let code = r.status().as_u16();
    (code, r.into_body().read_to_string().unwrap_or_default())
}

fn pairing_code(url: &str) -> String {
    let (code, body) = http("POST", &format!("{url}/api/rsp/pairing"));
    assert_eq!(code, 200, "{body}");
    serde_json::from_str::<serde_json::Value>(&body).unwrap()["code"].as_str().unwrap().to_string()
}

#[test]
fn resilient_transfer_survives_outage_shorter_than_delay() {
    let _ = gstreamer::init();
    let root = std::env::temp_dir().join(format!("lzs-resi-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let delay = 8;
    let (url, relay_sup) = start_relay(&root.join("relay"), delay);

    // Kopplung: falscher Code wird abgelehnt, richtiger liefert einen Zugang
    assert!(rsp::pair(&url, "000000", "x", "encoder").is_err());
    let (enc_token, _) = rsp::pair(&url, &pairing_code(&url), "Testencoder", "encoder").unwrap();
    let (dec_token, _) = rsp::pair(&url, &pairing_code(&url), "Standort Nord", "decoder").unwrap();
    // ohne Zugang kein Upload
    let (code, _) = http("GET", &format!("{url}/api/rsp/index"));
    assert_eq!(code, 401);

    // Encoder mit Spool
    let spool = root.join("spool");
    std::fs::create_dir_all(&spool).unwrap();
    let mut ec = EncoderConfig::default();
    ec.input.kind = InputKind::Test;
    ec.video.width = 640;
    ec.video.height = 360;
    ec.video.fps = 25;
    ec.video.bitrate_kbps = 1500;
    ec.srt.mode = SrtMode::Listener;
    ec.srt.port = free_port();
    ec.rsp = SendConfig {
        enabled: true,
        url: url.clone(),
        token: enc_token,
        segment_s: 1,
        spool_dir: spool.to_string_lossy().to_string(),
        session: "test1".into(),
        title: "Gottesdienst 10 Uhr".into(),
        ..Default::default()
    };
    let mut enc = Channel::start_encoder(&ec, &[], &ChannelOptions::default()).unwrap();
    let up_cfg = Arc::new(Mutex::new(ec.rsp.clone()));
    let up = Uploader::spawn(up_cfg.clone());

    // Standort-Decoder holt von der Empfangsstelle
    let mut dc = DecoderConfig { source: DecoderSource::Relay, output: OutputKind::None, ..Default::default() };
    dc.relay = rsp::PullConfig { url: url.clone(), token: dec_token, delay_s: delay, cache_dir: root.join("cache").to_string_lossy().to_string(), ..Default::default() };
    let mut dec = Channel::start_decoder(&dc, &ChannelOptions::default()).unwrap();

    // Ausspielung der Empfangsstelle an ein SRT-Ziel, dort hört ein zweiter Decoder zu
    let out_port = free_port();
    let relay_cfg = RelayConfig {
        delay_s: delay,
        dir: relay_sup.relay_dir().to_string_lossy().to_string(),
        destinations: vec![Destination {
            kind: DestKind::Srt,
            srt: lz_streamer::config::SrtConfig { mode: SrtMode::Listener, port: out_port, latency_ms: 120, ..Default::default() },
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut playout = Playout::start(&relay_cfg).unwrap();
    let mut dc2 = DecoderConfig { output: OutputKind::None, ..Default::default() };
    dc2.srt.mode = SrtMode::Caller;
    dc2.srt.host = "127.0.0.1".into();
    dc2.srt.port = out_port;
    dc2.srt.latency_ms = 120;
    let mut dec2 = Channel::start_decoder(&dc2, &ChannelOptions::default()).unwrap();

    let t0 = Instant::now();
    let mut first_frames: Option<Duration> = None;
    let mut outage: Option<(Instant, Instant)> = None;
    let mut frames_after_outage = 0u64;
    let mut frames_at_outage_end = 0u64;
    let mut dec2_frames = 0u64;
    let mut live_list = String::new();
    while t0.elapsed() < Duration::from_secs(40) {
        let _ = status(&mut enc);
        playout.poll();
        if let Some(s) = status(&mut dec2) {
            dec2_frames = s.frames;
        }
        if let Some(s) = status(&mut dec) {
            if s.frames > 0 && first_frames.is_none() {
                first_frames = Some(t0.elapsed());
                // Netzausfall: 5 s keine Empfangsstelle (kürzer als die Verzögerung)
                up_cfg.lock().unwrap().url = format!("http://127.0.0.1:{}", free_port());
                outage = Some((Instant::now(), Instant::now() + Duration::from_secs(5)));
            }
            if let Some((_, end)) = outage {
                if Instant::now() >= end && up_cfg.lock().unwrap().url != url {
                    up_cfg.lock().unwrap().url = url.clone();
                    frames_at_outage_end = s.frames;
                }
                if up_cfg.lock().unwrap().url == url && frames_at_outage_end > 0 {
                    frames_after_outage = s.frames - frames_at_outage_end;
                }
            }
            if frames_after_outage > 25 * 12 {
                let p = s.playout.clone().unwrap();
                assert_eq!(p.feed.stalls, 0, "Standort stand still: {p:?}");
                assert!((p.feed.current_delay_s - delay as f64).abs() < 1.0, "Verzögerung {:?}", p.feed.current_delay_s);
                break;
            }
        }
        if live_list.is_empty() {
            let (c, b) = http("GET", &format!("{url}/live/index.m3u8"));
            if c == 200 && b.contains(".ts") {
                live_list = b;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let up_status = up.status.lock().unwrap().clone();
    eprintln!("erstes Bild am Standort nach {first_frames:?}, Upload {up_status:?}, Ziel-Decoder {dec2_frames} Bilder");
    if let Ok(k) = std::env::var("LZS_KEEP") { let _ = std::fs::write(k, root.to_string_lossy().as_bytes()); }
    let first = first_frames.expect("Standort-Decoder zeigt kein Bild");
    assert!(first >= Duration::from_secs(delay as u64 - 1), "Bild vor Ablauf der Verzögerung: {first:?}");
    assert!(frames_after_outage > 25 * 12, "nach dem Ausfall kamen nur {frames_after_outage} Bilder");
    assert!(dec2_frames > 25 * 5, "Ziel über die Ausspielung: nur {dec2_frames} Bilder");
    assert!(up_status.sent > 10 && up_status.last_error.is_none(), "{up_status:?}");

    // keine Lücke in der Ablage: fortlaufende Segmentnummern
    let dir = rsp::latest_session(&relay_sup.relay_dir()).unwrap();
    let segs = rsp::read_index(&dir);
    let nums: Vec<u32> = segs.iter().filter_map(|s| rsp::parse_name(&s.name).map(|(_, _, i)| i)).collect();
    assert!(nums.windows(2).all(|w| w[1] == w[0] + 1), "Lücke: {nums:?}");
    assert!(live_list.contains("#EXT-X-PROGRAM-DATE-TIME"), "{live_list}");

    // Mediathek: Titel, Marke und VOD-Playlist, die ffprobe lesen kann
    let (_, list) = http("GET", &format!("{url}/api/rsp/sessions"));
    assert!(list.contains("Gottesdienst 10 Uhr"), "{list}");
    let id = dir.file_name().unwrap().to_string_lossy().to_string();
    let probe = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream=codec_type", "-of", "csv=p=0"])
        .arg(format!("{url}/vod/{id}/index.m3u8"))
        .output()
        .unwrap();
    let p = String::from_utf8_lossy(&probe.stdout);
    assert!(p.contains("video") && p.contains("audio"), "VOD: {p} {}", String::from_utf8_lossy(&probe.stderr));
    // Segmente, die noch nicht ausgestrahlt sind, sind öffentlich nicht abrufbar
    let newest = segs.last().unwrap();
    let (c, _) = http("GET", &format!("{url}/live/{id}/{}", newest.name));
    assert_eq!(c, 404, "nicht ausgestrahltes Segment ausgeliefert");

    enc.stop();
    dec.stop();
    dec2.stop();
    playout.stop();
    drop(up);
    let _ = std::fs::remove_dir_all(&root);
}
