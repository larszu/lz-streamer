//! Aufnahmen nach Ende eines Segments hochladen: WebDAV (PUT), S3 (SigV4)
//! und SFTP (System-`sftp`). Eine Warteschlange auf Platte überdauert
//! Abbrüche und Neustarts; Neuversuch mit wachsender Pause. Das
//! Bandbreitenlimit (Token-Bucket) hält den Upload neben dem Stream klein.
//!
//! Zugangsdaten stehen nur in `upload.json` (Rechte 0600) und kommen nie
//! über API, Oberfläche, Diagnose oder Kanal-Export zurück.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use axum::body::Bytes;
use axum::http::{Method, Request, Uri};
use http_body_util::BodyExt;
use http_body_util::combinators::BoxBody;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::sync::Mutex;

/// S3 nimmt in einem PUT höchstens 5 GB
const S3_MAX_PUT: u64 = 5 * 1024 * 1024 * 1024;
const CHUNK: usize = 16 * 1024;
/// ohne Fortschritt so lange → Abbruch und Neuversuch
const STALL: Duration = Duration::from_secs(60);

// ------------------------------------------------------------ Einstellungen

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum TargetKind {
    #[default]
    Webdav,
    S3,
    Sftp,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct UploadTarget {
    pub enabled: bool,
    pub name: String,
    pub kind: TargetKind,
    /// WebDAV: Ordner-URL; S3: Endpunkt (z. B. https://s3.eu-central-1.amazonaws.com)
    pub url: String,
    /// WebDAV-/SFTP-Benutzer bzw. S3-Access-Key
    pub user: String,
    /// WebDAV-Passwort bzw. S3-Secret-Key – wird nie ausgegeben
    pub secret: String,
    pub bucket: String,
    /// leer = us-east-1
    pub region: String,
    /// S3: Bucket als Teil des Rechnernamens statt im Pfad
    pub virtual_host: bool,
    /// SFTP
    pub host: String,
    pub port: u16,
    /// SFTP: privater Schlüssel (leer = Standard von ssh)
    pub key_file: String,
    /// SFTP: eigene known_hosts-Datei (leer = Standard von ssh)
    pub known_hosts: String,
    /// Unterordner bzw. Präfix am Ziel
    pub dir: String,
}

impl Default for UploadTarget {
    fn default() -> Self {
        Self {
            enabled: true,
            name: "Ziel".into(),
            kind: TargetKind::Webdav,
            url: String::new(),
            user: String::new(),
            secret: String::new(),
            bucket: String::new(),
            region: String::new(),
            virtual_host: false,
            host: String::new(),
            port: 22,
            key_file: String::new(),
            known_hosts: String::new(),
            dir: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct UploadSettings {
    /// fertige Segmente automatisch hochladen
    pub enabled: bool,
    /// kbit/s für alle Uploads zusammen (0 = unbegrenzt)
    pub limit_kbps: u32,
    /// nach erfolgreichem Upload zu allen Zielen lokal löschen
    pub delete_after: bool,
    pub targets: Vec<UploadTarget>,
}

impl UploadSettings {
    /// Sicht für API und Oberfläche: Geheimnisse leer, dafür `has_secret`
    pub fn redacted(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        if let Some(list) = v["targets"].as_array_mut() {
            for (t, orig) in list.iter_mut().zip(&self.targets) {
                t["secret"] = serde_json::Value::String(String::new());
                t["has_secret"] = serde_json::Value::Bool(!orig.secret.is_empty());
            }
        }
        v
    }

    /// Leeres Geheimnis heißt „unverändert“: vom gleichnamigen bisherigen Ziel übernehmen.
    /// `clear_secret` im JSON löscht es ausdrücklich.
    pub fn merge_secrets(&mut self, old: &UploadSettings, clear: &[bool]) {
        for (i, t) in self.targets.iter_mut().enumerate() {
            if !t.secret.is_empty() || clear.get(i).copied().unwrap_or(false) {
                continue;
            }
            if let Some(o) = old.targets.iter().find(|o| o.name == t.name) {
                t.secret = o.secret.clone();
            }
        }
    }
}

// ------------------------------------------------------------ Warteschlange

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Job {
    pub file: PathBuf,
    pub target: String,
    pub attempts: u32,
    /// frühester nächster Versuch (ms seit 1970)
    pub next_ms: u64,
    pub error: Option<String>,
    pub added_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Active {
    pub file: String,
    pub target: String,
    pub sent: u64,
    pub total: u64,
    pub rate_kbps: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Done {
    pub file: String,
    pub target: String,
    pub bytes: u64,
    pub seconds: f64,
    pub at_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueueView {
    pub file: String,
    pub target: String,
    pub attempts: u32,
    pub next_ms: u64,
    pub error: Option<String>,
}

/// Für `/api/state` – ohne Zugangsdaten, Ziele nur mit Namen
#[derive(Debug, Clone, Serialize)]
pub struct UploadStatus {
    pub enabled: bool,
    pub limit_kbps: u32,
    pub active: Option<Active>,
    pub queue: Vec<QueueView>,
    pub done: Vec<Done>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Pause vor Versuch n+1: 5 s, 10 s, 20 s … höchstens 10 min
pub fn backoff(base: Duration, attempts: u32) -> Duration {
    let f = 1u64 << attempts.saturating_sub(1).min(16);
    (base * f as u32).min(Duration::from_secs(600))
}

/// Erkennt Segment-Enden aus der jeweils laufenden Aufnahmedatei.
#[derive(Debug, Default)]
pub struct SegmentTracker {
    current: Option<String>,
}

impl SegmentTracker {
    /// `now` = Datei, in die gerade geschrieben wird. Wechselt sie oder endet
    /// die Aufnahme, ist die vorige fertig.
    pub fn observe(&mut self, now: Option<&str>) -> Option<String> {
        if self.current.as_deref() == now {
            return None;
        }
        std::mem::replace(&mut self.current, now.map(str::to_string))
    }
}

pub type Log = Arc<dyn Fn(&'static str, String) + Send + Sync>;

pub struct Uploader {
    dir: PathBuf,
    pub settings: Mutex<UploadSettings>,
    queue: Mutex<Vec<Job>>,
    active: Mutex<Option<(String, String, u64, Instant)>>,
    sent: Arc<AtomicU64>,
    limit: Arc<AtomicU32>,
    done: Mutex<Vec<Done>>,
    wake: tokio::sync::Notify,
    /// Grundpause der Neuversuche
    pub retry_base: Duration,
    /// Wartezeit nach Segment-Ende, bis die Datei sicher geschlossen ist
    pub settle: Duration,
}

impl Uploader {
    pub fn new(dir: PathBuf) -> Arc<Self> {
        let settings: UploadSettings = crate::admin::load_json(&dir.join("upload.json"));
        let queue: Vec<Job> = crate::admin::load_json(&dir.join("upload-warteschlange.json"));
        Arc::new(Self {
            limit: Arc::new(AtomicU32::new(settings.limit_kbps)),
            dir,
            settings: Mutex::new(settings),
            queue: Mutex::new(queue),
            active: Mutex::new(None),
            sent: Arc::default(),
            done: Mutex::new(Vec::new()),
            wake: tokio::sync::Notify::new(),
            retry_base: Duration::from_secs(5),
            settle: Duration::from_secs(3),
        })
    }

    pub fn with_timing(dir: PathBuf, retry_base: Duration, settle: Duration) -> Arc<Self> {
        let mut u = Arc::into_inner(Self::new(dir)).expect("neu");
        u.retry_base = retry_base;
        u.settle = settle;
        Arc::new(u)
    }

    pub async fn save_settings(&self, s: UploadSettings) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&s)?;
        crate::tls::write_private(&self.dir.join("upload.json"), &bytes)?;
        self.limit.store(s.limit_kbps, Ordering::Relaxed);
        *self.settings.lock().await = s;
        self.wake.notify_one();
        Ok(())
    }

    async fn persist(&self, q: &[Job]) {
        if let Err(e) = crate::admin::save_json(&self.dir.join("upload-warteschlange.json"), &q) {
            tracing::warn!("Upload-Warteschlange nicht speicherbar: {e}");
        }
    }

    /// Datei (samt .srt/.vtt daneben) für alle eingeschalteten Ziele einreihen.
    pub async fn enqueue(&self, file: &Path, delay: Duration) -> usize {
        let targets: Vec<String> = self.settings.lock().await.targets.iter().filter(|t| t.enabled).map(|t| t.name.clone()).collect();
        let mut files = vec![file.to_path_buf()];
        if let Some(dir) = file.parent() {
            files.extend(crate::admin::subtitle_files(dir, &file_name(file)).into_iter().map(|s| dir.join(s)));
        }
        let at = now_ms() + delay.as_millis() as u64;
        let mut q = self.queue.lock().await;
        let mut n = 0;
        for t in &targets {
            for f in &files {
                if !q.iter().any(|j| &j.file == f && &j.target == t) {
                    q.push(Job { file: f.clone(), target: t.clone(), attempts: 0, next_ms: at, error: None, added_ms: now_ms() });
                    n += 1;
                }
            }
        }
        self.persist(&q).await;
        drop(q);
        self.wake.notify_one();
        n
    }

    pub async fn clear_queue(&self) {
        let mut q = self.queue.lock().await;
        q.clear();
        self.persist(&q).await;
    }

    /// alle wartenden Aufträge sofort versuchen
    pub async fn retry_now(&self) {
        let mut q = self.queue.lock().await;
        q.iter_mut().for_each(|j| j.next_ms = 0);
        self.persist(&q).await;
        drop(q);
        self.wake.notify_one();
    }

    pub async fn status(&self) -> UploadStatus {
        let s = self.settings.lock().await;
        let active = self.active.lock().await.as_ref().map(|(f, t, total, started)| {
            let sent = self.sent.load(Ordering::Relaxed);
            let secs = started.elapsed().as_secs_f64().max(0.001);
            Active { file: f.clone(), target: t.clone(), sent, total: *total, rate_kbps: sent as f64 * 8.0 / 1000.0 / secs }
        });
        UploadStatus {
            enabled: s.enabled,
            limit_kbps: s.limit_kbps,
            active,
            queue: self
                .queue
                .lock()
                .await
                .iter()
                .map(|j| QueueView { file: file_name(&j.file), target: j.target.clone(), attempts: j.attempts, next_ms: j.next_ms, error: j.error.clone() })
                .collect(),
            done: self.done.lock().await.iter().rev().cloned().collect(),
        }
    }

    /// Segment-Ende aus dem Kanalstatus erkennen und abarbeiten
    pub async fn run(self: Arc<Self>, sup: Arc<crate::supervisor::Supervisor>) {
        let log: Log = {
            let sup = sup.clone();
            Arc::new(move |level, text| {
                let sup = sup.clone();
                tokio::spawn(async move { sup.log(level, text).await });
            })
        };
        tokio::spawn(self.clone().worker(log));
        let mut tracker = SegmentTracker::default();
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            let rec = sup.snapshot().await.status.and_then(|s| s.recording);
            if let Some(done) = tracker.observe(rec.as_deref())
                && self.settings.lock().await.enabled
            {
                self.enqueue(Path::new(&done), self.settle).await;
            }
        }
    }

    /// Arbeitet die Warteschlange ab, ein Auftrag nach dem anderen.
    pub async fn worker(self: Arc<Self>, log: Log) {
        loop {
            let next = {
                let q = self.queue.lock().await;
                let now = now_ms();
                let due = q.iter().find(|j| j.next_ms <= now).cloned();
                let wait = q.iter().map(|j| j.next_ms.saturating_sub(now)).min().unwrap_or(60_000);
                (due, wait)
            };
            let (Some(job), _) = next else {
                let _ = tokio::time::timeout(Duration::from_millis(next.1.clamp(50, 60_000)), self.wake.notified()).await;
                continue;
            };
            self.process(job, &log).await;
        }
    }

    async fn process(&self, job: Job, log: &Log) {
        let settings = self.settings.lock().await.clone();
        let remove = |q: &mut Vec<Job>| q.retain(|j| !(j.file == job.file && j.target == job.target));
        let Some(target) = settings.targets.iter().find(|t| t.name == job.target).cloned() else {
            let mut q = self.queue.lock().await;
            remove(&mut q);
            self.persist(&q).await;
            return;
        };
        let total = match std::fs::metadata(&job.file) {
            Ok(m) => m.len(),
            Err(_) => {
                log("warn", format!("Upload: {} fehlt – aus der Warteschlange genommen", file_name(&job.file)));
                let mut q = self.queue.lock().await;
                remove(&mut q);
                self.persist(&q).await;
                return;
            }
        };
        self.sent.store(0, Ordering::Relaxed);
        let started = Instant::now();
        *self.active.lock().await = Some((file_name(&job.file), target.name.clone(), total, started));
        let r = upload(&target, &job.file, job.attempts > 0, self.limit.clone(), self.sent.clone()).await;
        *self.active.lock().await = None;
        let mut q = self.queue.lock().await;
        match r {
            Ok(()) => {
                remove(&mut q);
                let secs = started.elapsed().as_secs_f64();
                log("ok", format!("Hochgeladen: {} → {} ({:.1} MB in {secs:.0} s)", file_name(&job.file), target.name, total as f64 / 1e6));
                let mut d = self.done.lock().await;
                d.push(Done { file: file_name(&job.file), target: target.name.clone(), bytes: total, seconds: secs, at_ms: now_ms() });
                let excess = d.len().saturating_sub(20);
                d.drain(..excess);
                if settings.delete_after && !q.iter().any(|j| j.file == job.file) && std::fs::remove_file(&job.file).is_ok() {
                    log("info", format!("Nach dem Upload lokal gelöscht: {}", file_name(&job.file)));
                }
            }
            Err(e) => {
                if let Some(j) = q.iter_mut().find(|j| j.file == job.file && j.target == job.target) {
                    j.attempts += 1;
                    let pause = backoff(self.retry_base, j.attempts);
                    j.next_ms = now_ms() + pause.as_millis() as u64;
                    j.error = Some(format!("{e:#}"));
                    log("warn", format!("Upload {} → {} gescheitert ({e:#}), neuer Versuch in {} s", file_name(&job.file), target.name, pause.as_secs()));
                }
            }
        }
        self.persist(&q).await;
    }
}

// ------------------------------------------------------------ Drosselung

/// Token-Bucket in Bytes; Rate kommt laufend aus `limit` (kbit/s, 0 = frei).
pub struct TokenBucket {
    limit: Arc<AtomicU32>,
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    pub fn new(limit: Arc<AtomicU32>) -> Self {
        Self { limit, tokens: 0.0, last: Instant::now() }
    }

    pub async fn take(&mut self, n: usize) {
        loop {
            let kbps = self.limit.load(Ordering::Relaxed);
            if kbps == 0 {
                self.tokens = 0.0;
                self.last = Instant::now();
                return;
            }
            let rate = kbps as f64 * 1000.0 / 8.0;
            // höchstens 100 ms Vorrat, damit kein Stoß entsteht
            let cap = (rate * 0.1).max(n as f64);
            let now = Instant::now();
            self.tokens = (self.tokens + now.duration_since(self.last).as_secs_f64() * rate).min(cap);
            self.last = now;
            if self.tokens >= n as f64 {
                self.tokens -= n as f64;
                return;
            }
            let wait = (n as f64 - self.tokens) / rate;
            tokio::time::sleep(Duration::from_secs_f64(wait.min(0.5))).await;
        }
    }
}

type UpBody = BoxBody<Bytes, std::io::Error>;

fn empty() -> UpBody {
    http_body_util::Empty::new().map_err(|never| match never {}).boxed()
}

fn file_body(file: tokio::fs::File, limit: Arc<AtomicU32>, sent: Arc<AtomicU64>) -> UpBody {
    let stream = futures_util::stream::unfold((file, TokenBucket::new(limit), sent), |(mut f, mut bucket, sent)| async move {
        let mut buf = vec![0u8; CHUNK];
        match f.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                bucket.take(n).await;
                sent.fetch_add(n as u64, Ordering::Relaxed);
                Some((Ok(hyper::body::Frame::data(Bytes::from(buf))), (f, bucket, sent)))
            }
            Err(e) => Some((Err(e), (f, bucket, sent))),
        }
    });
    http_body_util::StreamBody::new(stream).boxed()
}

// ------------------------------------------------------------ HTTP

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// Zertifikate der Systemablage (PEM-Bündel) für https-Ziele
fn client_tls() -> Result<Arc<tokio_rustls::rustls::ClientConfig>> {
    use pki_types::pem::PemObject;
    use tokio_rustls::rustls;
    static CFG: std::sync::OnceLock<Option<Arc<rustls::ClientConfig>>> = std::sync::OnceLock::new();
    CFG.get_or_init(|| {
        let mut roots = rustls::RootCertStore::empty();
        let files = std::env::var("SSL_CERT_FILE").into_iter().chain(
            ["/etc/ssl/cert.pem", "/etc/ssl/certs/ca-certificates.crt", "/etc/pki/tls/certs/ca-bundle.crt", "/etc/ssl/ca-bundle.pem"].map(String::from),
        );
        for f in files {
            if let Ok(b) = std::fs::read(&f) {
                for c in pki_types::CertificateDer::pem_slice_iter(&b).flatten() {
                    let _ = roots.add(c);
                }
            }
            if !roots.is_empty() {
                break;
            }
        }
        if roots.is_empty() {
            return None;
        }
        let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .ok()?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Some(Arc::new(cfg))
    })
    .clone()
    .ok_or_else(|| anyhow!("keine Systemzertifikate gefunden (SSL_CERT_FILE setzen)"))
}

async fn connect(uri: &Uri) -> Result<Box<dyn Io>> {
    let host = uri.host().ok_or_else(|| anyhow!("Adresse ohne Rechner"))?.trim_matches(['[', ']']).to_string();
    let tls = match uri.scheme_str() {
        Some("https") => true,
        Some("http") => false,
        _ => bail!("Adresse muss mit http:// oder https:// beginnen"),
    };
    let port = uri.port_u16().unwrap_or(if tls { 443 } else { 80 });
    let tcp = tokio::time::timeout(Duration::from_secs(10), tokio::net::TcpStream::connect((host.as_str(), port)))
        .await
        .map_err(|_| anyhow!("{host}:{port} antwortet nicht"))?
        .with_context(|| format!("Verbindung zu {host}:{port}"))?;
    let _ = tcp.set_nodelay(true);
    if !tls {
        return Ok(Box::new(tcp));
    }
    let name = pki_types::ServerName::try_from(host.clone()).map_err(|_| anyhow!("ungültiger Rechnername {host}"))?;
    let s = tokio_rustls::TlsConnector::from(client_tls()?).connect(name, tcp).await.context("TLS")?;
    Ok(Box::new(s))
}

/// Eine Anfrage auf eigener Verbindung; hängt sie ohne Fortschritt, Abbruch.
async fn send(mut req: Request<UpBody>, sent: Arc<AtomicU64>) -> Result<(u16, String)> {
    let io = connect(req.uri()).await?;
    // Host-Kopf setzen und nur Pfad senden (origin-form)
    if !req.headers().contains_key("host") {
        let h = host_header(req.uri());
        req.headers_mut().insert("host", h.parse()?);
    }
    let path = req.uri().path_and_query().map(|p| p.as_str().to_string()).unwrap_or_else(|| "/".into());
    *req.uri_mut() = path.parse()?;
    let (mut tx, conn) = hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(io)).await?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let watch = async {
        let (mut last, mut since) = (sent.load(Ordering::Relaxed), Instant::now());
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let now = sent.load(Ordering::Relaxed);
            if now != last {
                (last, since) = (now, Instant::now());
            } else if since.elapsed() > STALL {
                return;
            }
        }
    };
    let resp = tokio::select! {
        r = tx.send_request(req) => r?,
        _ = watch => bail!("keine Antwort seit {} s", STALL.as_secs()),
    };
    let code = resp.status().as_u16();
    let body = tokio::time::timeout(Duration::from_secs(30), resp.into_body().collect()).await.ok().and_then(|r| r.ok()).map(|b| b.to_bytes()).unwrap_or_default();
    Ok((code, String::from_utf8_lossy(&body[..body.len().min(500)]).into_owned()))
}

/// Pfadteil prozentkodieren (RFC 3986, unreservierte Zeichen bleiben)
pub fn uri_encode(s: &str, keep_slash: bool) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') || (keep_slash && b == b'/') {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

fn join_path(parts: &[&str]) -> String {
    parts.iter().flat_map(|p| p.split('/')).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("/")
}

fn basic_auth(user: &str, pw: &str) -> String {
    use base64::Engine;
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pw}")))
}

fn check(code: u16, body: &str) -> Result<()> {
    if (200..300).contains(&code) {
        return Ok(());
    }
    let hint = match code {
        401 | 403 => " (Zugangsdaten?)",
        404 => " (Ordner oder Bucket fehlt?)",
        507 => " (Ziel voll)",
        _ => "",
    };
    let msg: String = body.chars().filter(|c| !c.is_control()).take(160).collect();
    bail!("HTTP {code}{hint}{}", if msg.is_empty() { String::new() } else { format!(": {msg}") })
}

async fn webdav(t: &UploadTarget, file: &Path, limit: Arc<AtomicU32>, sent: Arc<AtomicU64>) -> Result<()> {
    let base = t.url.trim().trim_end_matches('/');
    let auth = (!t.user.is_empty()).then(|| basic_auth(&t.user, &t.secret));
    // Unterordner anlegen; 405 = gibt es schon
    let mut acc = String::new();
    for seg in t.dir.split('/').filter(|s| !s.is_empty()) {
        acc = format!("{acc}/{}", uri_encode(seg, false));
        let mut b = Request::builder().method(Method::from_bytes(b"MKCOL")?).uri(format!("{base}{acc}/"));
        if let Some(a) = &auth {
            b = b.header("authorization", a);
        }
        let (code, body) = send(b.body(empty())?, Arc::default()).await?;
        if !(200..300).contains(&code) && code != 405 {
            check(code, &body).context("Ordner anlegen")?;
        }
    }
    let path = join_path(&[&t.dir, &file_name(file)]);
    let uri = format!("{base}/{}", uri_encode(&path, true));
    let f = tokio::fs::File::open(file).await?;
    let len = f.metadata().await?.len();
    let mut b = Request::builder().method(Method::PUT).uri(&uri).header("content-length", len).header("content-type", "application/octet-stream");
    if let Some(a) = &auth {
        b = b.header("authorization", a);
    }
    let (code, body) = send(b.body(file_body(f, limit, sent.clone()))?, sent).await?;
    check(code, &body)
}

fn host_header(uri: &Uri) -> String {
    uri.authority().map(|a| a.as_str().rsplit('@').next().unwrap_or("").to_string()).unwrap_or_default()
}

// ------------------------------------------------------------ S3 (SigV4)

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn sha256_hex(b: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, b).as_ref())
}

fn hmac(key: &[u8], msg: &[u8]) -> Vec<u8> {
    ring::hmac::sign(&ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key), msg).as_ref().to_vec()
}

/// AWS Signature Version 4. `headers` in Kleinbuchstaben, `uri` schon kodiert.
#[allow(clippy::too_many_arguments)]
pub fn sigv4(method: &str, uri: &str, query: &str, headers: &[(String, String)], payload_hash: &str, access: &str, secret: &str, region: &str, service: &str, amz_date: &str) -> String {
    let mut h: Vec<(String, String)> = headers.iter().map(|(k, v)| (k.to_lowercase(), v.split_whitespace().collect::<Vec<_>>().join(" "))).collect();
    h.sort();
    let canon_headers: String = h.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed: String = h.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
    let creq = format!("{method}\n{uri}\n{query}\n{canon_headers}\n{signed}\n{payload_hash}");
    let date = &amz_date[..8];
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let sts = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", sha256_hex(creq.as_bytes()));
    let k = hmac(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k = hmac(&k, region.as_bytes());
    let k = hmac(&k, service.as_bytes());
    let k = hmac(&k, b"aws4_request");
    let sig = hex(&hmac(&k, sts.as_bytes()));
    format!("AWS4-HMAC-SHA256 Credential={access}/{scope}, SignedHeaders={signed}, Signature={sig}")
}

/// Adresse und Kopfzeilen für einen S3-PUT (Nutzlast unsigniert, wie bei großen Dateien üblich)
pub fn s3_put_request(t: &UploadTarget, key: &str, len: u64, amz_date: &str) -> Result<(String, Vec<(String, String)>)> {
    let ep: Uri = t.url.trim().trim_end_matches('/').parse().context("S3-Endpunkt")?;
    let scheme = ep.scheme_str().unwrap_or("https");
    let auth = ep.authority().ok_or_else(|| anyhow!("S3-Endpunkt ohne Rechner"))?.as_str();
    if t.bucket.trim().is_empty() {
        bail!("Bucket fehlt");
    }
    let bucket = t.bucket.trim();
    let base = ep.path().trim_end_matches('/');
    let (host, path) = if t.virtual_host {
        (format!("{bucket}.{auth}"), format!("{base}/{}", uri_encode(key, true)))
    } else {
        (auth.to_string(), format!("{base}/{}/{}", uri_encode(bucket, false), uri_encode(key, true)))
    };
    let region = if t.region.trim().is_empty() { "us-east-1" } else { t.region.trim() };
    let mut headers = vec![
        ("host".to_string(), host.clone()),
        ("x-amz-content-sha256".to_string(), "UNSIGNED-PAYLOAD".to_string()),
        ("x-amz-date".to_string(), amz_date.to_string()),
    ];
    let auth_header = sigv4("PUT", &path, "", &headers, "UNSIGNED-PAYLOAD", &t.user, &t.secret, region, "s3", amz_date);
    headers.push(("authorization".into(), auth_header));
    headers.push(("content-length".into(), len.to_string()));
    Ok((format!("{scheme}://{host}{path}"), headers))
}

async fn s3(t: &UploadTarget, file: &Path, limit: Arc<AtomicU32>, sent: Arc<AtomicU64>) -> Result<()> {
    let f = tokio::fs::File::open(file).await?;
    let len = f.metadata().await?.len();
    if len > S3_MAX_PUT {
        bail!("Datei über 5 GB – für S3 kürzere Segmente einstellen");
    }
    let key = join_path(&[&t.dir, &file_name(file)]);
    let amz_date = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let (url, headers) = s3_put_request(t, &key, len, &amz_date)?;
    let mut b = Request::builder().method(Method::PUT).uri(url);
    for (k, v) in headers {
        b = b.header(k, v);
    }
    let (code, body) = send(b.body(file_body(f, limit, sent.clone()))?, sent).await?;
    check(code, &body)
}

// ------------------------------------------------------------ SFTP

/// Befehlszeile und Batch für das System-`sftp`
pub fn sftp_command(t: &UploadTarget, file: &Path, resume: bool, kbps: u32) -> (Vec<String>, String) {
    let mut args: Vec<String> = ["-b", "-", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=accept-new", "-o", "ConnectTimeout=10", "-o", "ServerAliveInterval=15"]
        .map(String::from)
        .to_vec();
    args.extend(["-P".into(), t.port.max(1).to_string()]);
    if !t.key_file.trim().is_empty() {
        args.extend(["-i".into(), t.key_file.trim().into()]);
    }
    if !t.known_hosts.trim().is_empty() {
        args.extend(["-o".into(), format!("UserKnownHostsFile={}", t.known_hosts.trim())]);
    }
    if kbps > 0 {
        args.extend(["-l".into(), kbps.to_string()]);
    }
    let host = t.host.trim();
    let host = if host.contains(':') && !host.starts_with('[') { format!("[{host}]") } else { host.to_string() };
    args.push(if t.user.trim().is_empty() { host } else { format!("{}@{host}", t.user.trim()) });
    let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let mut batch = String::new();
    let mut acc = String::new();
    for seg in t.dir.split('/').filter(|s| !s.is_empty()) {
        acc = if acc.is_empty() && t.dir.starts_with('/') { format!("/{seg}") } else if acc.is_empty() { seg.to_string() } else { format!("{acc}/{seg}") };
        // „-“ davor: Fehler (Ordner gibt es schon) nicht abbrechen
        batch.push_str(&format!("-mkdir {}\n", q(&acc)));
    }
    let remote = if acc.is_empty() { file_name(file) } else { format!("{acc}/{}", file_name(file)) };
    batch.push_str(&format!("{} {} {}\n", if resume { "reput" } else { "put" }, q(&file.to_string_lossy()), q(&remote)));
    (args, batch)
}

async fn sftp(t: &UploadTarget, file: &Path, resume: bool, limit: Arc<AtomicU32>, sent: Arc<AtomicU64>) -> Result<()> {
    if t.host.trim().is_empty() {
        bail!("Rechner fehlt");
    }
    let run = |resume: bool| {
        let (args, batch) = sftp_command(t, file, resume, limit.load(Ordering::Relaxed));
        async move {
            use tokio::io::AsyncWriteExt;
            let mut child = tokio::process::Command::new("sftp")
                .args(&args)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .context("sftp nicht startbar")?;
            if let Some(mut i) = child.stdin.take() {
                i.write_all(batch.as_bytes()).await?;
            }
            let out = child.wait_with_output().await?;
            if out.status.success() {
                return Ok(());
            }
            let err = String::from_utf8_lossy(&out.stderr).lines().rfind(|l| !l.trim().is_empty()).unwrap_or("").to_string();
            Err(anyhow!("sftp: {}", if err.is_empty() { format!("Fehler {}", out.status) } else { err }))
        }
    };
    match run(resume).await {
        Ok(()) => {}
        // Wiederaufnahme scheitert, wenn noch nichts am Ziel liegt: neu von vorn
        Err(_) if resume => run(false).await?,
        Err(e) => return Err(e),
    }
    sent.store(std::fs::metadata(file).map(|m| m.len()).unwrap_or(0), Ordering::Relaxed);
    Ok(())
}

/// Eine Datei zu einem Ziel hochladen
pub async fn upload(t: &UploadTarget, file: &Path, resume: bool, limit: Arc<AtomicU32>, sent: Arc<AtomicU64>) -> Result<()> {
    match t.kind {
        TargetKind::Webdav => webdav(t, file, limit, sent).await,
        TargetKind::S3 => s3(t, file, limit, sent).await,
        TargetKind::Sftp => sftp(t, file, resume, limit, sent).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(k: &str, v: &str) -> (String, String) {
        (k.into(), v.into())
    }

    /// Beispiel „GET Object“ aus der S3-Dokumentation (Signature Version 4)
    #[test]
    fn sigv4_s3_get_object_example() {
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let a = sigv4(
            "GET",
            "/test.txt",
            "",
            &[h("host", "examplebucket.s3.amazonaws.com"), h("range", "bytes=0-9"), h("x-amz-content-sha256", empty), h("x-amz-date", "20130524T000000Z")],
            empty,
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            "us-east-1",
            "s3",
            "20130524T000000Z",
        );
        assert_eq!(
            a,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    /// Beispiel „ListUsers“ (IAM) aus der allgemeinen SigV4-Dokumentation
    #[test]
    fn sigv4_iam_example() {
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let a = sigv4(
            "GET",
            "/",
            "Action=ListUsers&Version=2010-05-08",
            &[h("content-type", "application/x-www-form-urlencoded; charset=utf-8"), h("host", "iam.amazonaws.com"), h("x-amz-date", "20150830T123600Z")],
            empty,
            "AKIDEXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "us-east-1",
            "iam",
            "20150830T123600Z",
        );
        assert!(a.ends_with("Signature=5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7"), "{a}");
    }

    #[test]
    fn s3_path_and_virtual_host() {
        let mut t = UploadTarget { kind: TargetKind::S3, url: "http://127.0.0.1:9000".into(), bucket: "aufnahmen".into(), user: "AK".into(), secret: "SK".into(), ..Default::default() };
        let (url, hs) = s3_put_request(&t, "kirche/lz 1.ts", 10, "20261114T100000Z").unwrap();
        assert_eq!(url, "http://127.0.0.1:9000/aufnahmen/kirche/lz%201.ts");
        assert!(hs.iter().any(|(k, v)| k == "authorization" && v.contains("/20261114/us-east-1/s3/aws4_request")));
        assert!(!hs.iter().any(|(_, v)| v.contains("SK")), "Geheimnis nie im Klartext");
        t.virtual_host = true;
        t.url = "https://s3.eu-central-1.amazonaws.com".into();
        t.region = "eu-central-1".into();
        let (url, _) = s3_put_request(&t, "a.ts", 1, "20261114T100000Z").unwrap();
        assert_eq!(url, "https://aufnahmen.s3.eu-central-1.amazonaws.com/a.ts");
        t.bucket.clear();
        assert!(s3_put_request(&t, "a.ts", 1, "20261114T100000Z").is_err());
    }

    #[test]
    fn segment_end_detected_on_change_and_stop() {
        let mut s = SegmentTracker::default();
        assert_eq!(s.observe(None), None);
        assert_eq!(s.observe(Some("a.ts")), None);
        assert_eq!(s.observe(Some("a.ts")), None);
        assert_eq!(s.observe(Some("b.ts")).as_deref(), Some("a.ts"));
        assert_eq!(s.observe(None).as_deref(), Some("b.ts"), "Aufnahme beendet");
        assert_eq!(s.observe(None), None);
    }

    #[test]
    fn backoff_grows_and_caps() {
        let b = Duration::from_secs(5);
        assert_eq!(backoff(b, 1), Duration::from_secs(5));
        assert_eq!(backoff(b, 2), Duration::from_secs(10));
        assert_eq!(backoff(b, 4), Duration::from_secs(40));
        assert_eq!(backoff(b, 30), Duration::from_secs(600));
    }

    #[test]
    fn secrets_never_leave_and_survive_edit() {
        let old = UploadSettings { targets: vec![UploadTarget { name: "nas".into(), secret: "geheim".into(), ..Default::default() }], ..Default::default() };
        let v = old.redacted();
        assert!(!v.to_string().contains("geheim"));
        assert_eq!(v["targets"][0]["has_secret"], true);
        // Oberfläche schickt das Ziel ohne Geheimnis zurück → bleibt
        let mut new: UploadSettings = serde_json::from_value(v).unwrap();
        new.targets[0].dir = "neu".into();
        new.merge_secrets(&old, &[]);
        assert_eq!(new.targets[0].secret, "geheim");
        let mut cleared = new.clone();
        cleared.targets[0].secret.clear();
        cleared.merge_secrets(&old, &[true]);
        assert!(cleared.targets[0].secret.is_empty());
    }

    #[test]
    fn sftp_batch_and_args() {
        let t = UploadTarget { kind: TargetKind::Sftp, host: "nas.local".into(), port: 2222, user: "studio".into(), key_file: "/k/id".into(), dir: "aufnahmen/2026".into(), ..Default::default() };
        let (args, batch) = sftp_command(&t, Path::new("/rec/lz \"1\".ts"), false, 4000);
        assert_eq!(args.last().unwrap(), "studio@nas.local");
        assert!(args.windows(2).any(|w| w == ["-P", "2222"]) && args.windows(2).any(|w| w == ["-l", "4000"]) && args.windows(2).any(|w| w == ["-i", "/k/id"]));
        assert_eq!(batch, "-mkdir \"aufnahmen\"\n-mkdir \"aufnahmen/2026\"\nput \"/rec/lz \\\"1\\\".ts\" \"aufnahmen/2026/lz \\\"1\\\".ts\"\n");
        let (_, b2) = sftp_command(&t, Path::new("/rec/a.ts"), true, 0);
        assert!(b2.ends_with("reput \"/rec/a.ts\" \"aufnahmen/2026/a.ts\"\n"));
    }

    #[tokio::test]
    async fn token_bucket_holds_rate() {
        let limit = Arc::new(AtomicU32::new(800)); // 100 kB/s
        let mut b = TokenBucket::new(limit.clone());
        let t0 = Instant::now();
        for _ in 0..20 {
            b.take(CHUNK).await; // 20 × 16 KiB = 327 kB → ≈ 3,3 s
        }
        let s = t0.elapsed().as_secs_f64();
        assert!((3.0..3.9).contains(&s), "{s}");
        limit.store(0, Ordering::Relaxed);
        let t1 = Instant::now();
        for _ in 0..100 {
            b.take(CHUNK).await;
        }
        assert!(t1.elapsed() < Duration::from_millis(50), "unbegrenzt");
    }
}
