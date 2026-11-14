//! Upload-Ziele gegen lokale Ersatz-Server: WebDAV (PUT) mit gemessener
//! Drosselung und Neuversuch, S3 mit geprüfter Signatur, und ein Durchlauf,
//! bei dem SRT während des Uploads ohne Verluste weiterläuft.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::IntoResponse;
use futures_util::StreamExt;
use lz_streamer::upload::{self, SegmentTracker, TargetKind, UploadSettings, UploadTarget, Uploader};

#[derive(Default)]
struct Stub {
    files: Mutex<HashMap<String, Vec<u8>>>,
    /// erstes und letztes empfangenes Stück je Pfad
    timing: Mutex<HashMap<String, (Instant, Instant)>>,
    /// so viele PUTs nach 64 kB abbrechen
    fail: AtomicU32,
    requests: Mutex<Vec<(String, String)>>,
    auth: Option<String>,
    /// S3: Geheimnis zum Nachrechnen der Signatur
    s3_secret: Option<String>,
}

async fn handle(State(s): State<Arc<Stub>>, method: Method, uri: Uri, headers: HeaderMap, body: Body) -> impl IntoResponse {
    let path = uri.path().to_string();
    s.requests.lock().unwrap().push((method.to_string(), path.clone()));
    if let Some(a) = &s.auth
        && headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(a.as_str())
    {
        return StatusCode::UNAUTHORIZED;
    }
    if let Some(secret) = &s.s3_secret {
        let h = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let date = h("x-amz-date");
        let signed = [("host".to_string(), h("host")), ("x-amz-content-sha256".into(), h("x-amz-content-sha256")), ("x-amz-date".into(), date.clone())];
        let want = upload::sigv4(method.as_str(), &path, "", &signed, "UNSIGNED-PAYLOAD", "AKTEST", secret, "eu-central-1", "s3", &date);
        if h("authorization") != want || h("x-amz-content-sha256") != "UNSIGNED-PAYLOAD" {
            return StatusCode::FORBIDDEN;
        }
    }
    if method.as_str() == "MKCOL" {
        return StatusCode::CREATED;
    }
    if method != Method::PUT {
        return StatusCode::METHOD_NOT_ALLOWED;
    }
    let fail = s.fail.load(Ordering::SeqCst) > 0;
    let mut data = Vec::new();
    let mut stream = body.into_data_stream();
    let mut first = None;
    while let Some(c) = stream.next().await {
        let Ok(c) = c else { return StatusCode::BAD_REQUEST };
        first.get_or_insert_with(Instant::now);
        data.extend_from_slice(&c);
        if fail && data.len() > 64 * 1024 {
            s.fail.fetch_sub(1, Ordering::SeqCst);
            // Abbruch mitten in der Übertragung
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    }
    s.timing.lock().unwrap().insert(path.clone(), (first.unwrap_or_else(Instant::now), Instant::now()));
    s.files.lock().unwrap().insert(path, data);
    StatusCode::CREATED
}

async fn stub(s: Arc<Stub>) -> u16 {
    let app = axum::Router::new().fallback(handle).with_state(s);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    port
}

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lzs-upload-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn pattern(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 % 251) as u8).collect()
}

type Logs = Arc<Mutex<Vec<String>>>;

fn logger() -> (upload::Log, Logs) {
    let v: Logs = Arc::default();
    let w = v.clone();
    (Arc::new(move |level, text| w.lock().unwrap().push(format!("{level}: {text}"))), v)
}

async fn wait_for(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(Instant::now() < end, "Zeit abgelaufen: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn webdav(port: u16) -> UploadTarget {
    UploadTarget { name: "nas".into(), kind: TargetKind::Webdav, url: format!("http://127.0.0.1:{port}/dav"), user: "studio".into(), secret: "pa:ss wört".into(), dir: "kirche/2026".into(), ..Default::default() }
}

#[tokio::test]
async fn webdav_throttled_resumes_after_abort() {
    use base64::Engine;
    let s = Arc::new(Stub { auth: Some(format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("studio:pa:ss wört"))), ..Default::default() });
    s.fail.store(1, Ordering::SeqCst);
    let port = stub(s.clone()).await;
    let d = dir("dav");
    let file = d.join("lz-2026-11-14_10-00-00_000.ts");
    let data = pattern(1_500_000);
    std::fs::write(&file, &data).unwrap();
    std::fs::write(d.join("lz-2026-11-14_10-00-00_000.srt"), b"1\n00:00:00,000 --> 00:00:01,000\nHallo\n").unwrap();

    let up = Uploader::with_timing(d.clone(), Duration::from_millis(300), Duration::ZERO);
    up.save_settings(UploadSettings { enabled: true, limit_kbps: 4000, targets: vec![webdav(port)], ..Default::default() }).await.unwrap();
    assert!(!std::fs::read_to_string(d.join("upload.json")).unwrap().is_empty());
    assert_eq!(up.enqueue(&file, Duration::ZERO).await, 2, "Aufnahme und Untertitel");
    let (log, logs) = logger();
    tokio::spawn(up.clone().worker(log));

    let key = "/dav/kirche/2026/lz-2026-11-14_10-00-00_000.ts";
    // Status zeigt den laufenden Upload ohne Zugangsdaten
    wait_for("Upload läuft", 10, || s.fail.load(Ordering::SeqCst) == 0).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let st = serde_json::to_string(&up.status().await).unwrap();
    assert!(!st.contains("pa:ss"), "{st}");
    let s2 = s.clone();
    wait_for("Upload fertig", 30, move || s2.files.lock().unwrap().contains_key(key)).await;
    assert_eq!(s.files.lock().unwrap()[key], data, "Inhalt nach Neuversuch vollständig");
    let (a, b) = s.timing.lock().unwrap()[key];
    let kbps = data.len() as f64 * 8.0 / 1000.0 / (b - a).as_secs_f64();
    eprintln!("gedrosselt auf 4000 kbit/s, gemessen {kbps:.0} kbit/s");
    assert!((3400.0..4400.0).contains(&kbps), "Drosselung: {kbps:.0} kbit/s");
    let s3 = s.clone();
    wait_for("Untertitel", 10, move || s3.files.lock().unwrap().contains_key("/dav/kirche/2026/lz-2026-11-14_10-00-00_000.srt")).await;
    assert!(s.requests.lock().unwrap().iter().any(|(m, p)| m == "MKCOL" && p == "/dav/kirche/"), "Ordner angelegt");
    let l = logs.lock().unwrap().join("\n");
    assert!(l.contains("gescheitert") && l.contains("Hochgeladen"), "{l}");
    wait_for("Warteschlange leer", 5, || std::fs::read_to_string(d.join("upload-warteschlange.json")).is_ok_and(|t| t.trim() == "[]")).await;
    assert_eq!(up.status().await.done.len(), 2);
}

#[tokio::test]
async fn queue_survives_restart_and_wrong_password() {
    let s = Arc::new(Stub { auth: Some("Basic richtig".into()), ..Default::default() });
    let port = stub(s.clone()).await;
    let d = dir("queue");
    let file = d.join("a.ts");
    std::fs::write(&file, pattern(10_000)).unwrap();
    let up = Uploader::with_timing(d.clone(), Duration::from_secs(60), Duration::ZERO);
    up.save_settings(UploadSettings { enabled: true, targets: vec![webdav(port)], ..Default::default() }).await.unwrap();
    up.enqueue(&file, Duration::ZERO).await;
    let (log, _) = logger();
    tokio::spawn(up.clone().worker(log));
    let up2 = up.clone();
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let st = up2.status().await;
        if st.queue.first().is_some_and(|j| j.attempts > 0) {
            assert!(st.queue[0].error.as_deref().unwrap_or("").contains("401"), "{:?}", st.queue[0]);
            break;
        }
        assert!(Instant::now() < end);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // neuer Prozess: Auftrag samt Fehlversuch ist noch da
    let again = Uploader::new(d.clone());
    let st = again.status().await;
    assert_eq!(st.queue.len(), 1);
    assert_eq!(st.queue[0].attempts, 1);
    assert!(st.queue[0].next_ms > 0);
}

#[tokio::test]
async fn s3_put_signed_against_stub() {
    let s = Arc::new(Stub { s3_secret: Some("geheim/Schlüssel".into()), ..Default::default() });
    let port = stub(s.clone()).await;
    let d = dir("s3");
    let file = d.join("lz 1.ts");
    let data = pattern(300_000);
    std::fs::write(&file, &data).unwrap();
    let mut t = UploadTarget {
        name: "s3".into(),
        kind: TargetKind::S3,
        url: format!("http://127.0.0.1:{port}"),
        user: "AKTEST".into(),
        secret: "geheim/Schlüssel".into(),
        bucket: "aufnahmen".into(),
        region: "eu-central-1".into(),
        dir: "kanal1".into(),
        ..Default::default()
    };
    upload::upload(&t, &file, false, Arc::new(AtomicU32::new(0)), Arc::default()).await.unwrap();
    assert_eq!(s.files.lock().unwrap()["/aufnahmen/kanal1/lz%201.ts"], data);
    // falsches Geheimnis: Stub lehnt die Signatur ab
    t.secret = "falsch".into();
    let e = upload::upload(&t, &file, false, Arc::new(AtomicU32::new(0)), Arc::default()).await.unwrap_err();
    assert!(e.to_string().contains("403"), "{e}");
}

// ------------------------------------------------------------ neben dem Stream

use lz_streamer::channel::{Channel, ChannelOptions, ChannelStatus, ChildMsg};
use lz_streamer::config::{EncoderConfig, InputKind, OutputKind, SrtMode};

fn status(ch: &mut Channel) -> ChannelStatus {
    let mut st = None;
    for m in ch.poll() {
        match m {
            ChildMsg::Status(s) => st = Some(s),
            ChildMsg::Fatal { text } => panic!("Kanal gescheitert: {text}"),
            _ => {}
        }
    }
    st.unwrap()
}

struct StreamResult {
    recording: String,
    frames_during_upload: u64,
    lost: u64,
    dropped: u64,
}

/// Encoder (mit Aufnahme) → SRT → Decoder; meldet „bereit“, misst bis `upload_done`.
fn stream_while(rec_dir: &Path, ready: std::sync::mpsc::Sender<()>, upload_done: Arc<AtomicBool>) -> StreamResult {
    let _ = gstreamer::init();
    let port = lz_streamer::latency::free_port();
    let mut c = EncoderConfig::default();
    c.input.kind = InputKind::Test;
    c.video.width = 640;
    c.video.height = 360;
    c.video.fps = 25;
    c.video.bitrate_kbps = 1500;
    c.srt.mode = SrtMode::Listener;
    c.srt.host = "127.0.0.1".into();
    c.srt.port = port;
    c.srt.latency_ms = 120;
    c.record.enabled = true;
    c.record.dir = rec_dir.to_string_lossy().into();
    let mut dcfg = lz_streamer::config::DecoderConfig::default();
    dcfg.srt.mode = SrtMode::Caller;
    dcfg.srt.host = "127.0.0.1".into();
    dcfg.srt.port = port;
    dcfg.srt.latency_ms = 120;
    dcfg.output = OutputKind::None;
    let mut enc = Channel::start_encoder(&c, &[], &ChannelOptions::default()).unwrap();
    let mut dec = Channel::start_decoder(&dcfg, &ChannelOptions::default()).unwrap();
    let end = Instant::now() + Duration::from_secs(20);
    let (mut d, mut e);
    loop {
        e = status(&mut enc);
        d = status(&mut dec);
        if d.frames > 25 && e.recording.is_some() {
            break;
        }
        assert!(Instant::now() < end, "Stream kommt nicht an: {d:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
    let f0 = d.frames;
    ready.send(()).unwrap();
    let end = Instant::now() + Duration::from_secs(60);
    while !upload_done.load(Ordering::SeqCst) {
        assert!(Instant::now() < end, "Upload endet nicht");
        std::thread::sleep(Duration::from_millis(200));
        e = status(&mut enc);
        d = status(&mut dec);
    }
    let rec = e.recording.clone().unwrap();
    let res = StreamResult { recording: rec, frames_during_upload: d.frames - f0, lost: d.srt.lost + e.srt.lost, dropped: d.srt.dropped + e.srt.dropped };
    enc.stop();
    dec.stop();
    res
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upload_beside_srt_without_loss_then_segment_follows() {
    let s = Arc::new(Stub::default());
    let port = stub(s.clone()).await;
    let d = dir("srt");
    let rec_dir = d.join("aufnahmen");
    std::fs::create_dir_all(&rec_dir).unwrap();
    let big = rec_dir.join("frueher.ts");
    let big_data = pattern(4_000_000);
    std::fs::write(&big, &big_data).unwrap();
    let up = Uploader::with_timing(d.clone(), Duration::from_millis(300), Duration::from_millis(500));
    let mut t = webdav(port);
    t.user.clear();
    t.dir.clear();
    up.save_settings(UploadSettings { enabled: true, limit_kbps: 8000, targets: vec![t], ..Default::default() }).await.unwrap();
    let (log, _) = logger();
    tokio::spawn(up.clone().worker(log));

    let (tx, rx) = std::sync::mpsc::channel();
    let done = Arc::new(AtomicBool::new(false));
    let stream = {
        let (rec_dir, done) = (rec_dir.clone(), done.clone());
        tokio::task::spawn_blocking(move || stream_while(&rec_dir, tx, done))
    };
    tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(30)).expect("Stream bereit")).await.unwrap();
    let t0 = Instant::now();
    up.enqueue(&big, Duration::ZERO).await;
    let s2 = s.clone();
    wait_for("großer Upload", 30, move || s2.files.lock().unwrap().contains_key("/dav/frueher.ts")).await;
    let secs = t0.elapsed().as_secs_f64();
    done.store(true, Ordering::SeqCst);
    let r = stream.await.unwrap();
    eprintln!("4 MB mit 8000 kbit/s in {secs:.1} s; {} Bilder am Decoder, verloren {}, verworfen {}", r.frames_during_upload, r.lost, r.dropped);
    assert_eq!(s.files.lock().unwrap()["/dav/frueher.ts"], big_data);
    assert!(secs > 3.5, "Drosselung greift nicht: {secs:.1} s");
    assert_eq!(r.lost, 0, "SRT-Verluste während des Uploads");
    assert_eq!(r.dropped, 0, "SRT verworfen während des Uploads");
    assert!(r.frames_during_upload as f64 > secs * 25.0 * 0.8, "Decoder stockt: {} Bilder in {secs:.1} s", r.frames_during_upload);

    // Segment-Ende (Kanal gestoppt) → Aufnahme geht automatisch hinterher
    let mut tracker = SegmentTracker::default();
    tracker.observe(Some(&r.recording));
    let finished = tracker.observe(None).unwrap();
    up.enqueue(Path::new(&finished), up.settle).await;
    let name = format!("/dav/{}", Path::new(&finished).file_name().unwrap().to_string_lossy());
    let s3 = s.clone();
    let n2 = name.clone();
    wait_for("Segment hochgeladen", 30, move || s3.files.lock().unwrap().contains_key(&n2)).await;
    assert_eq!(s.files.lock().unwrap()[&name], std::fs::read(&finished).unwrap());
}

// ------------------------------------------------------------ SFTP gegen lokalen sshd

struct Sshd {
    child: std::process::Child,
    port: u16,
    dir: PathBuf,
}

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// eigener sshd als dieser Benutzer auf 127.0.0.1 (fehlt sshd, wird übersprungen)
fn sshd(tag: &str) -> Option<Sshd> {
    let sshd = ["/usr/sbin/sshd", "/usr/bin/sshd"].into_iter().find(|p| Path::new(p).is_file())?;
    let dir = dir(tag);
    for k in ["host", "id"] {
        let ok = std::process::Command::new("ssh-keygen").args(["-q", "-t", "ed25519", "-N", "", "-f"]).arg(dir.join(k)).status().ok()?.success();
        if !ok {
            return None;
        }
    }
    std::fs::copy(dir.join("id.pub"), dir.join("authorized_keys")).ok()?;
    let port = lz_streamer::latency::free_port();
    let cfg = format!(
        "Port {port}\nListenAddress 127.0.0.1\nHostKey {d}/host\nAuthorizedKeysFile {d}/authorized_keys\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\nStrictModes no\nPidFile {d}/pid\nSubsystem sftp internal-sftp\n",
        d = dir.display()
    );
    std::fs::write(dir.join("cfg"), cfg).ok()?;
    let child = std::process::Command::new(sshd).arg("-f").arg(dir.join("cfg")).args(["-D", "-e"]).stderr(std::process::Stdio::null()).spawn().ok()?;
    let end = Instant::now() + Duration::from_secs(5);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        if Instant::now() > end {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Some(Sshd { child, port, dir })
}

#[tokio::test]
async fn sftp_put_throttled_and_reput() {
    let Some(srv) = sshd("sshd") else {
        eprintln!("kein sshd – SFTP-Test übersprungen");
        return;
    };
    let remote = srv.dir.join("ziel");
    let t = UploadTarget {
        name: "sftp".into(),
        kind: TargetKind::Sftp,
        host: "127.0.0.1".into(),
        port: srv.port,
        user: std::env::var("USER").unwrap_or_default(),
        key_file: srv.dir.join("id").to_string_lossy().into(),
        known_hosts: srv.dir.join("known_hosts").to_string_lossy().into(),
        dir: format!("{}/2026", remote.display()),
        ..Default::default()
    };
    let file = srv.dir.join("lz 1.ts");
    let data = pattern(2_000_000);
    std::fs::write(&file, &data).unwrap();
    let t0 = Instant::now();
    upload::upload(&t, &file, false, Arc::new(AtomicU32::new(4000)), Arc::default()).await.unwrap();
    let secs = t0.elapsed().as_secs_f64();
    eprintln!("SFTP 2 MB mit 4000 kbit/s in {secs:.1} s");
    assert_eq!(std::fs::read(remote.join("2026/lz 1.ts")).unwrap(), data);
    assert!(secs > 2.5, "Drosselung greift nicht: {secs:.1} s");
    // Abbruch nachgestellt: halbe Datei am Ziel, Wiederaufnahme schreibt den Rest
    std::fs::write(remote.join("2026/lz 1.ts"), &data[..700_000]).unwrap();
    upload::upload(&t, &file, true, Arc::new(AtomicU32::new(0)), Arc::default()).await.unwrap();
    assert_eq!(std::fs::read(remote.join("2026/lz 1.ts")).unwrap(), data);
    // Wiederaufnahme ohne Teil am Ziel: fängt neu an
    let other = srv.dir.join("b.ts");
    std::fs::write(&other, pattern(5000)).unwrap();
    upload::upload(&t, &other, true, Arc::new(AtomicU32::new(0)), Arc::default()).await.unwrap();
    assert_eq!(std::fs::read(remote.join("2026/b.ts")).unwrap().len(), 5000);
}
