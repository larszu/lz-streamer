//! Gesicherte Übertragung über eine Empfangsstelle (nach dem Vorbild von Resis RSP).
//!
//! - **Encoder:** Ein zweiter Mitschnitt (`splitmuxsink name=rsp`) schreibt kurze TS-Segmente in
//!   einen Spool-Ordner. Der Uploader (im Aufpasser-Prozess, überlebt also Kanal-Neustarts) schickt
//!   sie per HTTP(S) PUT mit Prüfsumme an die Empfangsstelle, mehrere parallel, und löscht ein Segment
//!   erst nach der Quittung. Fällt das Netz aus, wächst der Spool und wird danach nachgeschickt.
//! - **Empfangsstelle:** speichert die Segmente je Sendung, liefert sie mit fester Verzögerung
//!   (Vorgabe 90 s) als HLS aus und spielt sie an Ziele (RTMP/RTMPS, SRT, UDP) aus. Solange ein
//!   Ausfall kürzer als die Verzögerung ist, sieht niemand eine Lücke.
//! - **Standort-Decoder:** holen die Segmente von der Empfangsstelle und spielen sie zur Aufnahmezeit
//!   plus Verzögerung aus. Alle Standorte mit derselben Verzögerung laufen damit bildgleich
//!   (Voraussetzung: Rechneruhr per NTP).
//!
//! Geräte werden über einen Kopplungscode verbunden, nicht über Stream-Schlüssel.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use serde::{Deserialize, Serialize};

use crate::config::SrtConfig;

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// ------------------------------------------------------------------ Konfiguration

/// Encoder: an eine Empfangsstelle senden
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SendConfig {
    pub enabled: bool,
    /// Adresse der Empfangsstelle, z. B. `https://empfang.example.org:4200`
    pub url: String,
    /// Gerätezugang aus der Kopplung (wird nie angezeigt)
    pub token: String,
    /// Name, unter dem die Empfangsstelle dieses Gerät führt
    pub device_name: String,
    /// Segmentdauer in Sekunden
    pub segment_s: u32,
    /// gleichzeitige Uploads
    pub parallel: u32,
    /// so viele Minuten hält der Spool höchstens vor, danach fallen die ältesten Segmente weg
    pub spool_minutes: u32,
    /// Titel der Sendung (Player, Mediathek)
    pub title: String,
    /// vom Aufpasser gesetzt: Spool-Ordner und Sendungskennung
    pub spool_dir: String,
    pub session: String,
}

impl Default for SendConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            token: String::new(),
            device_name: String::new(),
            segment_s: 2,
            parallel: 3,
            spool_minutes: 30,
            title: String::new(),
            spool_dir: String::new(),
            session: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum DestKind {
    #[default]
    Rtmp,
    Srt,
    Udp,
}

/// Plattform-Vorlage: füllt die Ingest-Adresse, es fehlt nur der Schlüssel
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    #[default]
    Custom,
    Youtube,
    Facebook,
    Twitch,
    Vimeo,
}

impl Platform {
    pub fn ingest(self) -> Option<&'static str> {
        match self {
            Platform::Custom => None,
            Platform::Youtube => Some("rtmp://a.rtmp.youtube.com/live2"),
            Platform::Facebook => Some("rtmps://live-api-s.facebook.com:443/rtmp"),
            Platform::Twitch => Some("rtmp://live.twitch.tv/app"),
            Platform::Vimeo => Some("rtmps://rtmp-global.cloud.vimeo.com:443/live"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Destination {
    pub enabled: bool,
    pub name: String,
    pub kind: DestKind,
    pub platform: Platform,
    /// RTMP: vollständige Adresse oder (mit Vorlage) leer
    pub url: String,
    /// RTMP-Stream-Schlüssel (wird nie angezeigt)
    pub key: String,
    pub srt: SrtConfig,
    pub host: String,
    pub port: u16,
    /// eigene Verzögerung zusätzlich zur Grundverzögerung (z. B. Social Media später)
    pub extra_delay_s: u32,
}

impl Default for Destination {
    fn default() -> Self {
        Self {
            enabled: true,
            name: String::new(),
            kind: DestKind::Rtmp,
            platform: Platform::Custom,
            url: String::new(),
            key: String::new(),
            srt: SrtConfig::default(),
            host: String::new(),
            port: 5000,
            extra_delay_s: 0,
        }
    }
}

impl Destination {
    pub fn rtmp_url(&self) -> String {
        let base = self.platform.ingest().map(str::to_string).unwrap_or_else(|| self.url.trim().to_string());
        if self.key.trim().is_empty() { base } else { format!("{}/{}", base.trim_end_matches('/'), self.key.trim()) }
    }

    pub fn label(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.trim().to_string();
        }
        match (self.kind, self.platform) {
            (DestKind::Rtmp, Platform::Youtube) => "YouTube".into(),
            (DestKind::Rtmp, Platform::Facebook) => "Facebook".into(),
            (DestKind::Rtmp, Platform::Twitch) => "Twitch".into(),
            (DestKind::Rtmp, Platform::Vimeo) => "Vimeo".into(),
            (DestKind::Rtmp, Platform::Custom) => "RTMP".into(),
            (DestKind::Srt, _) => format!("SRT {}", self.srt.uri()),
            (DestKind::Udp, _) => format!("UDP {}:{}", self.host, self.port),
        }
    }
}

/// Rolle „Empfangsstelle“
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RelayConfig {
    /// Grundverzögerung in Sekunden: so lange darf ein Ausfall am Encoder dauern, ohne dass es jemand sieht
    pub delay_s: u32,
    pub destinations: Vec<Destination>,
    /// Sendungen so viele Tage aufheben (Mediathek); 0 = für immer
    pub retention_days: u32,
    pub player: PlayerConfig,
    /// vom Aufpasser gesetzt: Ablage der Sendungen
    pub dir: String,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self { delay_s: 90, destinations: Vec::new(), retention_days: 30, player: PlayerConfig::default(), dir: String::new() }
    }
}

/// Öffentlicher Web-Player (`/player`)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PlayerConfig {
    pub title: String,
    /// Knopf unter dem Bild, z. B. „Spenden“ oder „Anmelden“
    pub action_label: String,
    pub action_url: String,
    /// Mediathek mit vergangenen Sendungen zeigen
    pub library: bool,
    /// Text, solange nichts läuft
    pub offline_text: String,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self { title: String::new(), action_label: String::new(), action_url: String::new(), library: true, offline_text: "Gerade läuft keine Sendung.".into() }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum DecoderSource {
    #[default]
    Srt,
    /// von einer Empfangsstelle holen (gepuffert, standortsynchron)
    Relay,
}

/// Decoder: von einer Empfangsstelle holen
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PullConfig {
    pub url: String,
    pub token: String,
    pub device_name: String,
    /// Ausgabe zur Aufnahmezeit plus so viele Sekunden; gleiche Zahl an allen Standorten = bildgleich
    pub delay_s: u32,
    /// leer = jeweils die neueste Sendung, sonst eine bestimmte (zeitversetzte Wiedergabe)
    pub session: String,
    /// vom Aufpasser gesetzt: lokaler Zwischenspeicher
    pub cache_dir: String,
}

impl Default for PullConfig {
    fn default() -> Self {
        Self { url: String::new(), token: String::new(), device_name: String::new(), delay_s: 90, session: String::new(), cache_dir: String::new() }
    }
}

// ------------------------------------------------------------------ Segmente

/// Ein Segment im Index einer Sendung
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SegMeta {
    pub name: String,
    /// Kanal-Lauf (neuer Lauf = neue Zeitachse, Diskontinuität)
    pub run: String,
    /// Wanduhr beim Segmentbeginn am Encoder (ms seit 1970)
    pub wall_ms: u64,
    pub dur_ms: u64,
    pub size: u64,
    pub hash: String,
}

/// Spool-Dateiname: `<sitzung>_<lauf>_<nr>.ts`
pub fn spool_pattern(cfg: &SendConfig, run: &str) -> String {
    format!("{}/{}_{run}_%06d.ts", cfg.spool_dir.trim_end_matches('/'), cfg.session)
}

pub fn parse_name(name: &str) -> Option<(&str, &str, u32)> {
    let stem = name.strip_suffix(".ts")?;
    let mut it = stem.rsplitn(3, '_');
    let idx = it.next()?.parse().ok()?;
    let run = it.next()?;
    let session = it.next()?;
    Some((session, run, idx))
}

/// Sicherer Ordner-/Dateiname (nur Buchstaben, Ziffern, - und _)
pub fn safe_id(s: &str) -> Option<String> {
    let ok = !s.is_empty() && s.len() <= 120 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') && !s.contains("..");
    ok.then(|| s.to_string())
}

pub fn hash_hex(data: &[u8]) -> String {
    use blake2::Digest;
    let h = blake2::Blake2s256::digest(data);
    h.iter().map(|b| format!("{b:02x}")).collect()
}

/// Encoder-Zweig: derselbe TS-Strom, der per SRT hinausgeht, geht an einen Segmentierer.
/// Geschnitten wird an Keyframes; aneinandergereiht ergeben die Segmente wieder genau den Strom
/// (ein eigener Mux je Segment würde den Demuxer an jeder Grenze neu beginnen lassen).
pub fn encoder_branches(cfg: &crate::config::EncoderConfig) -> Vec<String> {
    let s = &cfg.rsp;
    if !s.enabled || s.spool_dir.trim().is_empty() || s.session.is_empty() {
        return vec![];
    }
    vec!["tsout. ! queue max-size-buffers=0 max-size-bytes=0 max-size-time=3000000000 ! appsink name=rsp_tap sync=false async=false max-buffers=0 drop=false".into()]
}

/// Beginnt im Paket ein Keyframe (Random-Access-Kennung und PES-Anfang)?
fn random_access(p: &[u8]) -> bool {
    p.len() >= 6 && p[0] == 0x47 && p[1] & 0x40 != 0 && (p[3] >> 4) & 0x2 != 0 && p[4] >= 1 && p[5] & 0x40 != 0
}

fn pid(p: &[u8]) -> u16 {
    (((p[1] & 0x1f) as u16) << 8) | p[2] as u16
}

/// Beginn der PSI-Tabelle in einem Paket (nach Anpassungsfeld und Pointer-Feld)
fn psi_start(p: &[u8]) -> Option<usize> {
    let mut o = 4;
    if (p[3] >> 4) & 0x2 != 0 {
        o += 1 + p[4] as usize;
    }
    let pointer = *p.get(o)? as usize;
    let t = o + 1 + pointer;
    (t + 3 <= p.len()).then_some(t)
}

/// PMT-PID des ersten Programms aus einer PAT
pub fn pat_pmt_pid(p: &[u8]) -> Option<u16> {
    let t = psi_start(p)?;
    let len = (((p[t + 1] & 0x0f) as usize) << 8) | p[t + 2] as usize;
    let mut i = t + 8;
    while i + 4 <= (t + 3 + len).saturating_sub(4).min(p.len()) {
        let prog = ((p[i] as u16) << 8) | p[i + 1] as u16;
        let pm = (((p[i + 2] & 0x1f) as u16) << 8) | p[i + 3] as u16;
        if prog != 0 {
            return Some(pm);
        }
        i += 4;
    }
    None
}

/// Bild-PID (H.264/H.265) aus einer PMT
pub fn pmt_video_pid(p: &[u8]) -> Option<u16> {
    let t = psi_start(p)?;
    let len = (((p[t + 1] & 0x0f) as usize) << 8) | p[t + 2] as usize;
    let end = (t + 3 + len).saturating_sub(4).min(p.len());
    let pil = (((p.get(t + 10)? & 0x0f) as usize) << 8) | *p.get(t + 11)? as usize;
    let mut i = t + 12 + pil;
    while i + 5 <= end {
        let st = p[i];
        let es = (((p[i + 1] & 0x1f) as u16) << 8) | p[i + 2] as u16;
        if matches!(st, 0x1b | 0x24 | 0x02) {
            return Some(es);
        }
        i += 5 + ((((p[i + 3] & 0x0f) as usize) << 8) | p[i + 4] as usize);
    }
    None
}

/// Schneidet einen TS-Bytestrom in Spool-Dateien
pub struct Segmenter {
    pattern_dir: PathBuf,
    prefix: String,
    seg_ms: u64,
    index: u32,
    file: Option<(std::fs::File, Instant)>,
    pat: Option<[u8; 188]>,
    pmt: Option<[u8; 188]>,
    pmt_pid: Option<u16>,
    video_pid: Option<u16>,
    rest: Vec<u8>,
}

impl Segmenter {
    pub fn new(cfg: &SendConfig, run: &str) -> Self {
        Segmenter {
            pattern_dir: PathBuf::from(&cfg.spool_dir),
            prefix: format!("{}_{run}", cfg.session),
            seg_ms: cfg.segment_s.clamp(1, 10) as u64 * 1000,
            index: 0,
            file: None,
            pat: None,
            pmt: None,
            pmt_pid: None,
            video_pid: None,
            rest: Vec::new(),
        }
    }

    fn open(&mut self) {
        use std::io::Write;
        let path = self.pattern_dir.join(format!("{}_{:06}.ts", self.prefix, self.index));
        self.index += 1;
        if let Ok(mut f) = std::fs::File::create(&path) {
            // Tabellen voran, damit jedes Segment für sich abspielbar ist
            for t in [self.pat, self.pmt].into_iter().flatten() {
                let _ = f.write_all(&t);
            }
            self.file = Some((f, Instant::now()));
        }
    }

    pub fn push(&mut self, data: &[u8]) {
        use std::io::Write;
        self.rest.extend_from_slice(data);
        let mut i = 0;
        // auf Paketanfang ausrichten
        while i < self.rest.len() && self.rest[i] != 0x47 {
            i += 1;
        }
        let mut out_from = i;
        while i + 188 <= self.rest.len() {
            let p: [u8; 188] = self.rest[i..i + 188].try_into().unwrap();
            let id = pid(&p);
            if id == 0 && p[1] & 0x40 != 0 {
                self.pat = Some(p);
                if let Some(pm) = pat_pmt_pid(&p) {
                    self.pmt_pid = Some(pm);
                }
            } else if Some(id) == self.pmt_pid && p[1] & 0x40 != 0 {
                self.pmt = Some(p);
                if let Some(v) = pmt_video_pid(&p) {
                    self.video_pid = Some(v);
                }
            }
            // ohne Bild (nur Ton) darf an jedem Ton-Frame geschnitten werden
            let key = random_access(&p) && self.video_pid.is_none_or(|v| v == id) && (self.video_pid.is_some() || self.pmt.is_some());
            let cut = key && self.file.as_ref().is_none_or(|(_, t)| t.elapsed().as_millis() as u64 >= self.seg_ms.saturating_sub(100));
            let long = self.file.as_ref().is_some_and(|(_, t)| t.elapsed().as_millis() as u64 >= self.seg_ms * 3);
            // vor dem ersten Keyframe nichts schreiben: ein Segment ohne PAT/PMT und Keyframe
            // am Anfang bringt den Demuxer beim Aneinanderreihen aus dem Tritt
            if self.file.is_none() && !key {
                out_from = i + 188;
            } else if cut || (long && key) || (long && self.file.as_ref().is_some_and(|(_, t)| t.elapsed().as_millis() as u64 >= self.seg_ms * 10)) {
                if let Some((f, _)) = self.file.as_mut() {
                    let _ = f.write_all(&self.rest[out_from..i]);
                }
                out_from = i;
                self.open();
            }
            i += 188;
        }
        if let Some((f, _)) = self.file.as_mut() {
            let _ = f.write_all(&self.rest[out_from..i]);
        }
        self.rest.drain(..i);
    }
}

/// Segmentierer an das appsink `rsp_tap` hängen
pub fn attach_spool(pipeline: &gst::Pipeline, cfg: &SendConfig) {
    let Some(sink) = pipeline.by_name("rsp_tap").and_then(|s| s.downcast::<gst_app::AppSink>().ok()) else { return };
    let _ = std::fs::create_dir_all(&cfg.spool_dir);
    let seg = Arc::new(Mutex::new(Segmenter::new(cfg, &now_ms().to_string())));
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |s| {
                let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                if let Some(buf) = sample.buffer() {
                    if let Ok(map) = buf.map_readable() {
                        seg.lock().unwrap().push(&map);
                    }
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
}

// ------------------------------------------------------------------ Uploader (Encoder)

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SendStatus {
    pub active: bool,
    pub connected: bool,
    /// Segmente im Spool, noch nicht quittiert
    pub pending: usize,
    /// Rückstand in Sekunden (Alter des ältesten offenen Segments)
    pub backlog_s: f64,
    pub sent: u64,
    pub sent_bytes: u64,
    /// verworfen, weil der Spool voll war
    pub dropped: u64,
    pub rate_kbps: f64,
    pub last_error: Option<String>,
    pub last_ok_ms: u64,
}

struct SpoolFile {
    path: PathBuf,
    name: String,
    run: String,
    wall_ms: u64,
    mtime_ms: u64,
}

fn file_times(meta: &std::fs::Metadata) -> (u64, u64) {
    let ms = |t: std::io::Result<SystemTime>| t.ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64);
    let m = ms(meta.modified()).unwrap_or_else(now_ms);
    (ms(meta.created()).unwrap_or(m), m)
}

fn list_spool(dir: &Path) -> Vec<SpoolFile> {
    let mut v: Vec<SpoolFile> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let (_, run, _) = parse_name(&name)?;
            let run = run.to_string();
            let (c, m) = file_times(&e.metadata().ok()?);
            Some(SpoolFile { path: e.path(), name, run, wall_ms: c, mtime_ms: m })
        })
        .collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

/// Fertige Segmente mit Dauer: fertig ist ein Segment, wenn im selben Lauf ein neueres existiert
/// oder es seit `idle_ms` nicht mehr wächst (Kanal gestoppt).
fn complete_segments(files: &[SpoolFile], idle_ms: u64, now: u64) -> Vec<(usize, u64)> {
    let mut out = Vec::new();
    for (i, f) in files.iter().enumerate() {
        match files.get(i + 1).filter(|n| n.run == f.run) {
            Some(next) => out.push((i, next.wall_ms.saturating_sub(f.wall_ms).max(1))),
            None if now.saturating_sub(f.mtime_ms) > idle_ms => out.push((i, f.mtime_ms.saturating_sub(f.wall_ms).max(1))),
            None => {}
        }
    }
    out
}

/// Läuft im Aufpasser-Prozess. Holt die aktuelle Konfiguration bei jeder Runde neu.
pub struct Uploader {
    pub status: Arc<Mutex<SendStatus>>,
    stop: Arc<AtomicBool>,
}

impl Uploader {
    pub fn spawn(cfg: Arc<Mutex<SendConfig>>) -> Self {
        let status: Arc<Mutex<SendStatus>> = Arc::default();
        let stop: Arc<AtomicBool> = Arc::default();
        let (st, sp) = (status.clone(), stop.clone());
        std::thread::Builder::new().name("rsp-upload".into()).spawn(move || upload_loop(cfg, st, sp)).ok();
        Uploader { status, stop }
    }
}

impl Drop for Uploader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .build()
        .into()
}

fn upload_loop(cfg: Arc<Mutex<SendConfig>>, status: Arc<Mutex<SendStatus>>, stop: Arc<AtomicBool>) {
    let agent = agent();
    let in_flight: Arc<Mutex<std::collections::HashSet<String>>> = Arc::default();
    let mut retry_at: std::collections::HashMap<String, (Instant, u32)> = Default::default();
    let mut rate = (Instant::now(), 0u64);
    let sent_bytes = Arc::new(AtomicU64::new(0));
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(250));
        let c = cfg.lock().unwrap().clone();
        let dir = PathBuf::from(&c.spool_dir);
        let active = c.enabled && !c.url.trim().is_empty() && !c.spool_dir.is_empty();
        let files = if c.spool_dir.is_empty() { vec![] } else { list_spool(&dir) };
        let now = now_ms();
        // Spool begrenzen: älteste offene Segmente verwerfen
        let limit_ms = c.spool_minutes.max(1) as u64 * 60_000;
        let mut dropped = 0;
        for f in files.iter().filter(|f| now.saturating_sub(f.wall_ms) > limit_ms) {
            if !in_flight.lock().unwrap().contains(&f.name) && std::fs::remove_file(&f.path).is_ok() {
                dropped += 1;
            }
        }
        let files = if dropped > 0 { list_spool(&dir) } else { files };
        // Cue-Punkte (kleine JSON-Dateien)
        if active {
            for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "cue") {
                    if let Ok(body) = std::fs::read(&p) {
                        let r = agent
                            .post(&format!("{}/api/rsp/cue", c.url.trim_end_matches('/')))
                            .header("Authorization", &format!("Bearer {}", c.token))
                            .header("Content-Type", "application/json")
                            .send(&body[..]);
                        if r.is_ok_and(|r| r.status().is_success()) {
                            let _ = std::fs::remove_file(&p);
                        }
                    }
                }
            }
        }
        let complete = complete_segments(&files, (c.segment_s.max(1) as u64 * 2000).max(4000), now);
        {
            let mut s = status.lock().unwrap();
            s.active = active;
            s.pending = files.len();
            s.backlog_s = files.first().map(|f| now.saturating_sub(f.wall_ms) as f64 / 1000.0).unwrap_or(0.0);
            s.dropped += dropped;
            if rate.0.elapsed() >= Duration::from_secs(2) {
                let b = sent_bytes.load(Ordering::Relaxed);
                s.rate_kbps = (b - rate.1) as f64 * 8.0 / rate.0.elapsed().as_secs_f64() / 1000.0;
                rate = (Instant::now(), b);
                s.sent_bytes = b;
            }
            if !active {
                s.connected = false;
            }
        }
        if !active {
            continue;
        }
        for (i, dur) in complete {
            let f = &files[i];
            if in_flight.lock().unwrap().len() >= c.parallel.clamp(1, 8) as usize {
                break;
            }
            if in_flight.lock().unwrap().contains(&f.name) {
                continue;
            }
            if retry_at.get(&f.name).is_some_and(|(t, _)| Instant::now() < *t) {
                continue;
            }
            in_flight.lock().unwrap().insert(f.name.clone());
            let (agent, c, st, fl, sb) = (agent.clone(), c.clone(), status.clone(), in_flight.clone(), sent_bytes.clone());
            let (path, name, run, wall) = (f.path.clone(), f.name.clone(), f.run.clone(), f.wall_ms);
            let fails = retry_at.get(&f.name).map(|(_, n)| *n).unwrap_or(0);
            // Wiederholung mit wachsender Pause (vor dem Start eingetragen, nach Erfolg entfernt)
            retry_at.insert(name.clone(), (Instant::now() + Duration::from_millis((500u64 << fails.min(5)).min(10_000)), fails + 1));
            std::thread::spawn(move || {
                let result = (|| -> Result<()> {
                    let body = std::fs::read(&path)?;
                    let r = agent
                        .put(&format!("{}/api/rsp/segment", c.url.trim_end_matches('/')))
                        .header("Authorization", &format!("Bearer {}", c.token))
                        .header("X-Name", &name)
                        .header("X-Run", &run)
                        .header("X-Wall-Ms", &wall.to_string())
                        .header("X-Dur-Ms", &dur.to_string())
                        .header("X-Hash", &hash_hex(&body))
                        .header("X-Title", &urlencode(&c.title))
                        .header("Content-Type", "video/mp2t")
                        .send(&body[..])?;
                    if !r.status().is_success() {
                        let code = r.status().as_u16();
                        let text = r.into_body().read_to_string().unwrap_or_default();
                        return Err(anyhow!("Empfangsstelle antwortet {code}: {}", text.chars().take(160).collect::<String>()));
                    }
                    sb.fetch_add(body.len() as u64, Ordering::Relaxed);
                    let _ = std::fs::remove_file(&path);
                    Ok(())
                })();
                let mut s = st.lock().unwrap();
                match result {
                    Ok(()) => {
                        s.connected = true;
                        s.sent += 1;
                        s.last_ok_ms = now_ms();
                        s.last_error = None;
                    }
                    Err(e) => {
                        s.connected = false;
                        s.last_error = Some(format!("{e:#}"));
                    }
                }
                drop(s);
                fl.lock().unwrap().remove(&name);
            });
        }
        let present: std::collections::HashSet<&str> = files.iter().map(|f| f.name.as_str()).collect();
        retry_at.retain(|k, _| present.contains(k.as_str()));
    }
}

pub fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~ ".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect::<String>()
        .replace(' ', "%20")
}

pub fn urldecode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Kopplung von dieser Seite aus: Code an die Empfangsstelle, Gerätezugang zurück
pub fn pair(url: &str, code: &str, name: &str, kind: &str) -> Result<(String, String)> {
    let r = agent()
        .post(&format!("{}/api/rsp/pair", url.trim_end_matches('/')))
        .header("Content-Type", "application/json")
        .send(serde_json::json!({"code": code, "name": name, "kind": kind}).to_string().as_bytes())
        .with_context(|| format!("Empfangsstelle {url} nicht erreichbar"))?;
    let ok = r.status().is_success();
    let v: serde_json::Value = serde_json::from_str(&r.into_body().read_to_string().unwrap_or_default()).unwrap_or_default();
    if !ok {
        return Err(anyhow!("{}", v["error"].as_str().unwrap_or("Kopplung abgelehnt")));
    }
    let token = v["token"].as_str().ok_or_else(|| anyhow!("Antwort ohne Zugang"))?.to_string();
    Ok((token, v["relay"].as_str().unwrap_or_default().to_string()))
}

// ------------------------------------------------------------------ Ablage (Empfangsstelle, Standort-Cache)

/// Index einer Sendung: eine JSON-Zeile je Segment
pub fn read_index(dir: &Path) -> Vec<SegMeta> {
    let mut v: Vec<SegMeta> = std::fs::read_to_string(dir.join("index.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v.dedup_by(|a, b| a.name == b.name);
    v
}

/// Segment ablegen (atomar) und in den Index eintragen; doppelt gesendete Segmente sind kein Fehler
pub fn store_segment(dir: &Path, meta: &SegMeta, data: &[u8]) -> std::io::Result<bool> {
    use std::io::Write;
    std::fs::create_dir_all(dir)?;
    let target = dir.join(&meta.name);
    if target.exists() {
        return Ok(false);
    }
    let tmp = dir.join(format!(".{}.tmp", meta.name));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, &target)?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("index.jsonl"))?;
    writeln!(f, "{}", serde_json::to_string(meta).unwrap_or_default())?;
    Ok(true)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub device: String,
    pub started_ms: u64,
}

pub fn session_dirs(root: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(root).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.join("index.jsonl").exists()).collect();
    v.sort();
    v
}

/// Neueste Sendung (nach letztem Segment)
pub fn latest_session(root: &Path) -> Option<PathBuf> {
    session_dirs(root).into_iter().max_by_key(|d| read_index(d).last().map(|s| s.wall_ms).unwrap_or(0))
}

/// HLS-Playlist einer Sendung. `release_ms`: nur Segmente bis zu dieser Aufnahmezeit
/// (live = jetzt minus Verzögerung), `window`: so viele Segmente (0 = alle, VOD).
pub fn playlist(segs: &[SegMeta], release_ms: Option<u64>, window: usize, prefix: &str) -> String {
    let visible: Vec<(usize, &SegMeta)> = segs.iter().enumerate().filter(|(_, s)| release_ms.is_none_or(|r| s.wall_ms + s.dur_ms <= r)).collect();
    let shown = if window > 0 && visible.len() > window { &visible[visible.len() - window..] } else { &visible[..] };
    let target = shown.iter().map(|(_, s)| s.dur_ms.div_ceil(1000)).max().unwrap_or(2).max(1);
    let mut p = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{target}\n#EXT-X-MEDIA-SEQUENCE:{}\n",
        shown.first().map(|(i, _)| *i).unwrap_or(visible.len())
    );
    if release_ms.is_none() {
        p += "#EXT-X-PLAYLIST-TYPE:VOD\n";
    }
    let mut prev_run: Option<&str> = None;
    for (i, s) in shown {
        let run_change = prev_run.is_some_and(|r| r != s.run) || (prev_run.is_none() && *i > 0 && segs[*i - 1].run != s.run);
        if run_change {
            p += "#EXT-X-DISCONTINUITY\n";
        }
        prev_run = Some(&s.run);
        let t = chrono::DateTime::from_timestamp_millis(s.wall_ms as i64).unwrap_or_default();
        p += &format!("#EXT-X-PROGRAM-DATE-TIME:{}\n#EXTINF:{:.3},\n{prefix}{}\n", t.format("%Y-%m-%dT%H:%M:%S%.3fZ"), s.dur_ms as f64 / 1000.0, s.name);
    }
    if release_ms.is_none() {
        p += "#EXT-X-ENDLIST\n";
    }
    p
}

// ------------------------------------------------------------------ Zuspieler mit fester Verzögerung

/// Woher die Segmente kommen: Ablage der Empfangsstelle oder Cache am Standort
pub trait SegmentSource: Send + 'static {
    /// aktuelle Sendung und ihre Segmente
    fn list(&mut self) -> Option<(String, Vec<SegMeta>)>;
    fn read(&mut self, session: &str, name: &str) -> Option<Vec<u8>>;
}

/// Ablage auf der Platte (Empfangsstelle bzw. Standort-Cache)
pub struct DirSource {
    pub root: PathBuf,
    /// leer = neueste Sendung
    pub session: String,
}

impl SegmentSource for DirSource {
    fn list(&mut self) -> Option<(String, Vec<SegMeta>)> {
        let dir = if self.session.is_empty() { latest_session(&self.root)? } else { self.root.join(&self.session) };
        let id = dir.file_name()?.to_string_lossy().to_string();
        Some((id, read_index(&dir)))
    }
    fn read(&mut self, session: &str, name: &str) -> Option<Vec<u8>> {
        std::fs::read(self.root.join(session).join(name)).ok()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FeedStatus {
    pub session: Option<String>,
    pub segment: Option<String>,
    /// Soll-Verzögerung
    pub delay_s: f64,
    /// tatsächliche Verzögerung (wächst, wenn Segmente zu spät kommen)
    pub current_delay_s: f64,
    /// so viel liegt nach dem laufenden Segment schon bereit
    pub buffered_s: f64,
    /// wartet auf das nächste Segment
    pub stalled: bool,
    pub stalls: u64,
    pub bytes: u64,
}

/// Steuerung von außen
#[derive(Default)]
pub struct FeedControl {
    pub stop: AtomicBool,
    /// auf die Soll-Verzögerung zurückspringen
    pub catch_up: AtomicBool,
}

/// Empfänger der Bytes: (Daten, beginnt ein Segment, beginnt eine neue Zeitachse)
pub type Sink = Box<dyn FnMut(&[u8], bool, bool) + Send>;

/// Spielt Segmente zur Aufnahmezeit plus `delay_ms` aus, in 20-ms-Häppchen gleichmäßig über die
/// Segmentdauer verteilt. Fehlt das nächste Segment, wartet er; die Verzögerung wächst dann um die
/// Wartezeit (wie bei Resi: es geht dort weiter, wo es abriss). `catch_up` springt zurück auf die Soll-Verzögerung.
pub fn spawn_feeder(mut src: impl SegmentSource, delay_ms: u64, ctl: Arc<FeedControl>, status: Arc<Mutex<FeedStatus>>, mut sink: Sink) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("rsp-feed".into())
        .spawn(move || {
            let mut session: Option<String> = None;
            let mut next: Option<String> = None; // Name des nächsten Segments
            let mut prev_run: Option<String> = None;
            let mut stall_since: Option<Instant> = None;
            status.lock().unwrap().delay_s = delay_ms as f64 / 1000.0;
            // Abstand Wanduhr → Ausgabe; wächst nach Wartezeiten
            let mut offset = delay_ms;
            // Taktanker: (Zeitpunkt, PCR in 27 MHz)
            let mut anchor: Option<(Instant, u64)> = None;
            let mut seg_t0: Option<Instant> = None;
            while !ctl.stop.load(Ordering::Relaxed) {
                let Some((sid, segs)) = src.list() else {
                    std::thread::sleep(Duration::from_millis(250));
                    continue;
                };
                let now = now_ms();
                if session.as_deref() != Some(&sid) || ctl.catch_up.swap(false, Ordering::Relaxed) {
                    // Einstieg: das Segment, das jetzt (Aufnahmezeit + Verzögerung) dran wäre
                    let target = now.saturating_sub(delay_ms);
                    let pick = segs.iter().rev().find(|s| s.wall_ms <= target).or(segs.first());
                    next = pick.map(|s| s.name.clone());
                    offset = delay_ms;
                    anchor = None;
                    if session.as_deref() != Some(&sid) {
                        prev_run = None;
                    } else {
                        prev_run = Some(String::new()); // Sprung = neue Zeitachse
                    }
                    session = Some(sid.clone());
                }
                let Some(pos) = next.as_ref().and_then(|n| segs.iter().position(|s| &s.name == n)) else {
                    // nächstes Segment fehlt noch: steht es schon im Index (später), sonst warten
                    let after = next.as_ref().and_then(|n| segs.iter().find(|s| s.name.as_str() > n.as_str()));
                    if let Some(a) = after {
                        // ein Segment ging ganz verloren (Spool-Grenze): weiter mit dem nächsten
                        next = Some(a.name.clone());
                        continue;
                    }
                    if stall_since.is_none() {
                        stall_since = Some(Instant::now());
                        let mut s = status.lock().unwrap();
                        s.stalled = true;
                        s.stalls += 1;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                };
                let seg = segs[pos].clone();
                let release = seg.wall_ms + offset;
                if now + 5 < release {
                    std::thread::sleep(Duration::from_millis((release - now).min(100)));
                    continue;
                }
                if now > release + 300 {
                    // zu spät (gewartet): Verzögerung wächst
                    offset = now - seg.wall_ms;
                }
                stall_since = None;
                let Some(data) = src.read(&sid, &seg.name) else {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                };
                let new_run = prev_run.as_deref() != Some(seg.run.as_str());
                prev_run = Some(seg.run.clone());
                {
                    let mut s = status.lock().unwrap();
                    s.session = Some(sid.clone());
                    s.segment = Some(seg.name.clone());
                    s.stalled = false;
                    s.current_delay_s = offset as f64 / 1000.0;
                    s.buffered_s = segs[pos + 1..].iter().map(|x| x.dur_ms).sum::<u64>() as f64 / 1000.0;
                }
                // im Takt der PCR senden, wie der Mux es live täte (gleichmäßig verteilt käme der
                // große Keyframe am Segmentanfang zu spät und alle Bilder bis zum nächsten wären verloren)
                let groups = pcr_groups(&data);
                if new_run {
                    anchor = None;
                }
                for (k, (pcr, range)) in groups.iter().enumerate() {
                    if ctl.stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let due = match (pcr, anchor) {
                        (Some(p), Some((t, p0))) if *p >= p0 => t + Duration::from_nanos((*p - p0) * 1000 / 27),
                        // ohne PCR: gleichmäßig über die Segmentdauer
                        (None, _) => seg_t0.get_or_insert_with(Instant::now).to_owned() + Duration::from_millis(seg.dur_ms * k as u64 / groups.len().max(1) as u64),
                        (Some(p), _) => {
                            anchor = Some((Instant::now(), *p));
                            Instant::now()
                        }
                    };
                    let now_i = Instant::now();
                    if due + Duration::from_millis(250) < now_i {
                        // zu spät (z. B. nach dem Warten auf ein Segment): neu verankern, Verzögerung wächst
                        if let Some(p) = pcr {
                            anchor = Some((now_i, *p));
                        }
                    } else if let Some(w) = due.checked_duration_since(now_i) {
                        std::thread::sleep(w.min(Duration::from_secs(2)));
                    }
                    let part = &data[range.clone()];
                    sink(part, k == 0, k == 0 && new_run);
                    status.lock().unwrap().bytes += part.len() as u64;
                }
                seg_t0 = None;
                next = segs.get(pos + 1).map(|s| s.name.clone()).or_else(|| {
                    // Nachfolger noch unbekannt: aus dem Namen erraten (gleicher Lauf, Nummer + 1)
                    parse_name(&seg.name).map(|(s, r, i)| format!("{s}_{r}_{:06}.ts", i + 1))
                });
            }
        })
        .expect("Thread")
}

/// PCR eines TS-Pakets (27 MHz), falls es eine trägt
pub fn packet_pcr(p: &[u8]) -> Option<u64> {
    if p.len() < 12 || p[0] != 0x47 || (p[3] >> 4) & 0x2 == 0 || p[4] < 7 || p[5] & 0x10 == 0 {
        return None;
    }
    let base = ((p[6] as u64) << 25) | ((p[7] as u64) << 17) | ((p[8] as u64) << 9) | ((p[9] as u64) << 1) | ((p[10] as u64) >> 7);
    let ext = (((p[10] & 1) as u64) << 8) | p[11] as u64;
    Some(base * 300 + ext)
}

/// Segment in Gruppen teilen, die jeweils mit einem PCR-Paket beginnen (erste Gruppe: alles davor dazu)
pub fn pcr_groups(data: &[u8]) -> Vec<(Option<u64>, std::ops::Range<usize>)> {
    let mut out: Vec<(Option<u64>, std::ops::Range<usize>)> = Vec::new();
    let mut start = 0;
    let mut cur: Option<u64> = None;
    let mut i = 0;
    while i + 188 <= data.len() {
        if let Some(p) = packet_pcr(&data[i..i + 188]) {
            if i > start && cur.is_some() {
                out.push((cur, start..i));
                start = i;
            }
            cur = Some(p);
        }
        i += 188;
    }
    if start < data.len() {
        out.push((cur, start..data.len()));
    }
    if cur.is_none() {
        // keine PCR: in 20-ms-Stücke teilen (Zeit kommt dann aus der Segmentdauer)
        let n = (data.len() / (188 * 20)).max(1);
        let size = data.len().div_ceil(n).next_multiple_of(188);
        return (0..data.len()).step_by(size).map(|a| (None, a..(a + size).min(data.len()))).collect();
    }
    out
}

// ------------------------------------------------------------------ Abruf am Standort

/// Holt die Segmente einer Empfangsstelle in einen lokalen Cache (Standort-Decoder).
pub struct Fetcher {
    stop: Arc<AtomicBool>,
    pub status: Arc<Mutex<FetchStatus>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FetchStatus {
    pub connected: bool,
    pub session: Option<String>,
    pub fetched: u64,
    pub last_error: Option<String>,
}

impl Drop for Fetcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Fetcher {
    pub fn spawn(cfg: PullConfig) -> Self {
        let stop: Arc<AtomicBool> = Arc::default();
        let status: Arc<Mutex<FetchStatus>> = Arc::default();
        let (sp, st) = (stop.clone(), status.clone());
        std::thread::Builder::new()
            .name("rsp-fetch".into())
            .spawn(move || {
                let agent = agent();
                let base = cfg.url.trim_end_matches('/').to_string();
                let auth = format!("Bearer {}", cfg.token);
                let root = PathBuf::from(&cfg.cache_dir);
                while !sp.load(Ordering::Relaxed) {
                    let r = (|| -> Result<usize> {
                        let q = if cfg.session.is_empty() { String::new() } else { format!("?session={}", urlencode(&cfg.session)) };
                        let resp = agent.get(&format!("{base}/api/rsp/index{q}")).header("Authorization", &auth).call()?;
                        if !resp.status().is_success() {
                            return Err(anyhow!("Empfangsstelle antwortet {}", resp.status().as_u16()));
                        }
                        let v: serde_json::Value = serde_json::from_str(&resp.into_body().read_to_string()?)?;
                        let sid = v["session"].as_str().ok_or_else(|| anyhow!("keine Sendung"))?.to_string();
                        let safe = safe_id(&sid).ok_or_else(|| anyhow!("ungültige Sendung"))?;
                        let segs: Vec<SegMeta> = serde_json::from_value(v["segments"].clone())?;
                        st.lock().unwrap().session = Some(sid.clone());
                        let dir = root.join(&safe);
                        let have: std::collections::HashSet<String> = read_index(&dir).into_iter().map(|s| s.name).collect();
                        // nur, was in den nächsten Minuten gebraucht wird: ab „jetzt minus Verzögerung minus Reserve“
                        let from = now_ms().saturating_sub(cfg.delay_s as u64 * 1000 + 30_000);
                        let mut n = 0;
                        for s in segs.iter().filter(|s| s.wall_ms + s.dur_ms >= from && !have.contains(&s.name)) {
                            if sp.load(Ordering::Relaxed) {
                                break;
                            }
                            let resp = agent.get(&format!("{base}/api/rsp/seg/{}/{}", urlencode(&sid), urlencode(&s.name))).header("Authorization", &auth).call()?;
                            if !resp.status().is_success() {
                                return Err(anyhow!("Segment {}: {}", s.name, resp.status().as_u16()));
                            }
                            let data = resp.into_body().with_config().limit(256 * 1024 * 1024).read_to_vec()?;
                            if hash_hex(&data) != s.hash {
                                return Err(anyhow!("Prüfsumme falsch: {}", s.name));
                            }
                            store_segment(&dir, s, &data)?;
                            n += 1;
                            st.lock().unwrap().fetched += 1;
                        }
                        Ok(n)
                    })();
                    let mut s = st.lock().unwrap();
                    match r {
                        Ok(_) => {
                            s.connected = true;
                            s.last_error = None;
                        }
                        Err(e) => {
                            s.connected = false;
                            s.last_error = Some(format!("{e:#}"));
                        }
                    }
                    drop(s);
                    std::thread::sleep(Duration::from_millis(500));
                }
            })
            .ok();
        Fetcher { stop, status }
    }
}

/// Cache am Standort aufräumen: Sendungen, die älter als `keep_h` Stunden sind
pub fn prune_cache(root: &Path, keep_h: u64) {
    let limit = now_ms().saturating_sub(keep_h * 3_600_000);
    for d in session_dirs(root) {
        if read_index(&d).last().is_some_and(|s| s.wall_ms < limit) {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

// ------------------------------------------------------------------ Ausspielung (Empfangsstelle)

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DestStatus {
    pub name: String,
    pub target: String,
    /// "verbunden", "verbindet", "wartet", "Fehler: …"
    pub state: String,
    pub bytes: u64,
    pub delay_s: f64,
}

struct DestPipe {
    cfg: Destination,
    pipeline: Option<gst::Pipeline>,
    src: Option<gst_app::AppSrc>,
    /// wartet auf einen Segmentanfang (Keyframe), bevor Daten fließen
    wait_start: bool,
    retry_at: Option<Instant>,
    state: String,
    bytes: Arc<AtomicU64>,
    connected_at: Option<Instant>,
}

impl DestPipe {
    fn target(&self) -> String {
        match self.cfg.kind {
            DestKind::Rtmp => {
                // Schlüssel nicht anzeigen
                let u = self.cfg.platform.ingest().map(str::to_string).unwrap_or_else(|| self.cfg.url.clone());
                if self.cfg.key.is_empty() { u } else { format!("{u}/•••") }
            }
            DestKind::Srt => self.cfg.srt.uri(),
            DestKind::Udp => format!("udp://{}:{}", self.cfg.host, self.cfg.port),
        }
    }

    fn description(&self) -> String {
        let src = "appsrc name=src is-live=true format=bytes do-timestamp=true block=false max-bytes=16000000";
        match self.cfg.kind {
            DestKind::Rtmp => format!(
                "{src} ! tsparse set-timestamps=true ! tsdemux name=d  flvmux name=fm streamable=true latency=200000000 ! rtmp2sink name=out async=false sync=false location={}",
                crate::pipeline::quote(&self.cfg.rtmp_url())
            ),
            DestKind::Srt => format!(
                "{src} ! srtsink name=out uri={} latency={} wait-for-connection=false async=false sync=false",
                crate::pipeline::quote(&self.cfg.srt.uri()),
                self.cfg.srt.latency_ms
            ),
            DestKind::Udp => format!("{src} ! udpsink name=out host={} port={} sync=false async=false", crate::pipeline::quote(&self.cfg.host), self.cfg.port),
        }
    }

    fn start(&mut self) {
        let p = match gst::parse::launch(&self.description()).ok().and_then(|e| e.downcast::<gst::Pipeline>().ok()) {
            Some(p) => p,
            None => {
                self.state = "Fehler: nicht baubar".into();
                self.retry_at = Some(Instant::now() + Duration::from_secs(10));
                return;
            }
        };
        if self.cfg.kind == DestKind::Srt {
            if let Some(o) = p.by_name("out") {
                if !self.cfg.srt.passphrase.is_empty() {
                    o.set_property("passphrase", &self.cfg.srt.passphrase);
                }
                if !self.cfg.srt.streamid.is_empty() {
                    o.set_property("streamid", &self.cfg.srt.streamid);
                }
            }
        }
        if self.cfg.kind == DestKind::Rtmp {
            // Bild und erste Tonspur an den FLV-Muxer, weitere Tonspuren verwerfen
            if let (Some(d), Some(fm)) = (p.by_name("d"), p.by_name("fm")) {
                let pw = p.downgrade();
                let audio_done = Arc::new(AtomicBool::new(false));
                d.connect_pad_added(move |_, pad| {
                    let Some(p) = pw.upgrade() else { return };
                    let caps = pad.current_caps().or_else(|| Some(pad.query_caps(None)));
                    let name = caps.as_ref().and_then(|c| c.structure(0)).map(|s| s.name().to_string()).unwrap_or_default();
                    let desc = if name == "video/x-h264" {
                        "queue max-size-buffers=0 max-size-bytes=0 max-size-time=2000000000 ! h264parse ! video/x-h264,stream-format=avc,alignment=au"
                    } else if name == "audio/mpeg" && !audio_done.swap(true, Ordering::Relaxed) {
                        "queue max-size-buffers=0 max-size-bytes=0 max-size-time=2000000000 ! aacparse ! audio/mpeg,mpegversion=4,stream-format=raw"
                    } else {
                        "queue ! fakesink sync=false async=false"
                    };
                    let Ok(bin) = gst::parse::bin_from_description(desc, true) else { return };
                    if p.add(&bin).is_err() {
                        return;
                    }
                    let to_mux = !desc.contains("fakesink");
                    if to_mux {
                        let pad_name = if name.starts_with("video") { "video" } else { "audio" };
                        if let (Some(src), Some(sink)) = (bin.static_pad("src"), fm.request_pad_simple(pad_name)) {
                            let _ = src.link(&sink);
                        }
                    }
                    let _ = bin.sync_state_with_parent();
                    if let Some(sink) = bin.static_pad("sink") {
                        let _ = pad.link(&sink);
                    }
                });
            }
        }
        if let Some(out) = p.by_name("out").and_then(|o| o.static_pad("sink")) {
            let b = self.bytes.clone();
            out.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                if let Some(gst::PadProbeData::Buffer(buf)) = &info.data {
                    b.fetch_add(buf.size() as u64, Ordering::Relaxed);
                }
                gst::PadProbeReturn::Ok
            });
        }
        self.src = p.by_name("src").and_then(|e| e.downcast::<gst_app::AppSrc>().ok());
        if p.set_state(gst::State::Playing).is_err() {
            self.fail("startet nicht".into());
            return;
        }
        self.pipeline = Some(p);
        self.wait_start = true;
        self.state = "verbindet".into();
        self.connected_at = None;
    }

    fn fail(&mut self, why: String) {
        if let Some(p) = self.pipeline.take() {
            let _ = p.set_state(gst::State::Null);
        }
        self.src = None;
        self.state = format!("Fehler: {why}");
        self.retry_at = Some(Instant::now() + Duration::from_secs(5));
    }

    fn poll(&mut self, last_bytes: &mut u64) {
        if self.pipeline.is_none() {
            if self.retry_at.is_none_or(|t| Instant::now() >= t) {
                self.retry_at = None;
                self.start();
            }
            return;
        }
        let mut failed = None;
        if let Some(bus) = self.pipeline.as_ref().and_then(|p| p.bus()) {
            while let Some(msg) = bus.pop() {
                match msg.view() {
                    gst::MessageView::Error(e) => failed = Some(e.error().to_string()),
                    gst::MessageView::Eos(_) => failed = Some("Ende".into()),
                    _ => {}
                }
            }
        }
        if let Some(f) = failed {
            self.fail(f);
            return;
        }
        let b = self.bytes.load(Ordering::Relaxed);
        if b > *last_bytes {
            if self.connected_at.is_none() {
                self.connected_at = Some(Instant::now());
            }
            self.state = "sendet".into();
        } else if !self.wait_start {
            self.state = "wartet".into();
        }
        *last_bytes = b;
    }

    fn push(&mut self, data: &[u8], seg_start: bool, new_run: bool) {
        if new_run && !self.wait_start && self.pipeline.is_some() && self.cfg.kind == DestKind::Rtmp {
            // neue Zeitachse: RTMP neu aufbauen (FLV verträgt keinen Zeitsprung)
            self.fail("neue Zeitachse".into());
            self.retry_at = Some(Instant::now());
            return;
        }
        if self.wait_start && !seg_start {
            return;
        }
        if let Some(src) = &self.src {
            self.wait_start = false;
            let _ = src.push_buffer(gst::Buffer::from_slice(data.to_vec()));
        }
    }
}

/// Ausspielung an alle Ziele; je Ziel eine eigene kleine Pipeline (ein abgelehntes Ziel stört die anderen nicht).
/// Ziele mit eigener Zusatzverzögerung bekommen einen eigenen Zuspieler.
pub struct Playout {
    groups: Vec<(Arc<Mutex<Vec<DestPipe>>>, Arc<FeedControl>, Arc<Mutex<FeedStatus>>)>,
    last: Vec<Vec<u64>>,
    pub started: Instant,
}

impl Playout {
    pub fn start(cfg: &RelayConfig) -> Result<Self> {
        if cfg.dir.trim().is_empty() {
            return Err(anyhow!("Ablage der Empfangsstelle fehlt"));
        }
        let mut by_delay: std::collections::BTreeMap<u32, Vec<Destination>> = Default::default();
        for d in cfg.destinations.iter().filter(|d| d.enabled) {
            by_delay.entry(d.extra_delay_s).or_default().push(d.clone());
        }
        if by_delay.is_empty() {
            // ohne Ziel trotzdem mitlaufen (Status, Verzögerungsanzeige)
            by_delay.insert(0, vec![]);
        }
        let mut groups = Vec::new();
        let mut last = Vec::new();
        for (extra, dests) in by_delay {
            let pipes: Arc<Mutex<Vec<DestPipe>>> = Arc::new(Mutex::new(
                dests
                    .into_iter()
                    .map(|d| DestPipe { cfg: d, pipeline: None, src: None, wait_start: true, retry_at: None, state: "startet".into(), bytes: Arc::default(), connected_at: None })
                    .collect(),
            ));
            last.push(vec![0; pipes.lock().unwrap().len()]);
            let ctl: Arc<FeedControl> = Arc::default();
            let st: Arc<Mutex<FeedStatus>> = Arc::default();
            let p2 = pipes.clone();
            spawn_feeder(
                DirSource { root: PathBuf::from(&cfg.dir), session: String::new() },
                (cfg.delay_s + extra) as u64 * 1000,
                ctl.clone(),
                st.clone(),
                Box::new(move |data, start, run| {
                    for p in p2.lock().unwrap().iter_mut() {
                        p.push(data, start, run);
                    }
                }),
            );
            groups.push((pipes, ctl, st));
        }
        Ok(Playout { groups, last, started: Instant::now() })
    }

    pub fn poll(&mut self) {
        for (g, (pipes, _, _)) in self.groups.iter().enumerate() {
            for (i, p) in pipes.lock().unwrap().iter_mut().enumerate() {
                p.poll(&mut self.last[g][i]);
            }
        }
    }

    pub fn catch_up(&self) {
        for (_, c, _) in &self.groups {
            c.catch_up.store(true, Ordering::Relaxed);
        }
    }

    pub fn status(&self) -> PlayoutStatus {
        let feed = self.groups.first().map(|(_, _, s)| s.lock().unwrap().clone()).unwrap_or_default();
        let mut dests = Vec::new();
        for (pipes, _, st) in &self.groups {
            let d = st.lock().unwrap().current_delay_s;
            for p in pipes.lock().unwrap().iter() {
                dests.push(DestStatus { name: p.cfg.label(), target: p.target(), state: p.state.clone(), bytes: p.bytes.load(Ordering::Relaxed), delay_s: d });
            }
        }
        PlayoutStatus { feed, destinations: dests }
    }

    pub fn stop(&mut self) {
        for (pipes, c, _) in &self.groups {
            c.stop.store(true, Ordering::Relaxed);
            for p in pipes.lock().unwrap().iter_mut() {
                if let Some(pl) = p.pipeline.take() {
                    let _ = pl.set_state(gst::State::Null);
                }
            }
        }
    }
}

impl Drop for Playout {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PlayoutStatus {
    pub feed: FeedStatus,
    pub destinations: Vec<DestStatus>,
}

/// Kindprozess der Rolle „Empfangsstelle“: Ausspielung, Status alle 500 ms
pub fn run_relay_child(cfg: &RelayConfig) -> i32 {
    use crate::channel::{ChildCmd, ChildMsg, emit};
    let mut p = match Playout::start(cfg) {
        Ok(p) => p,
        Err(e) => {
            emit(&ChildMsg::Fatal { text: format!("{e:#}") });
            return 2;
        }
    };
    emit(&ChildMsg::Event { level: "ok".into(), text: format!("Empfangsstelle bereit, Verzögerung {} s", cfg.delay_s) });
    let (tx, rx) = std::sync::mpsc::channel::<Option<ChildCmd>>();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if let Ok(cmd) = serde_json::from_str::<ChildCmd>(&line) {
                let _ = tx.send(Some(cmd));
            }
        }
        let _ = tx.send(None);
    });
    let mut last = (Instant::now(), 0u64);
    let mut rate = 0.0;
    loop {
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                None | Some(ChildCmd::Stop) => {
                    p.stop();
                    return 0;
                }
                Some(ChildCmd::CatchUp) => p.catch_up(),
                _ => {}
            }
        }
        p.poll();
        let st = p.status();
        let el = last.0.elapsed().as_secs_f64();
        if el >= 1.0 {
            rate = st.feed.bytes.saturating_sub(last.1) as f64 * 8.0 / el / 1000.0;
            last = (Instant::now(), st.feed.bytes);
        }
        let status = crate::channel::ChannelStatus {
            role: crate::config::Role::Relay,
            input: st.feed.session.clone().map(|s| format!("Sendung {s}")).unwrap_or_else(|| "wartet auf Sendung".into()),
            signal: if st.feed.session.is_none() || st.feed.stalled { "lost".into() } else { "ok".into() },
            bytes: st.feed.bytes,
            bitrate_kbps: rate,
            uptime_s: p.started.elapsed().as_secs_f64(),
            outputs: st
                .destinations
                .iter()
                .map(|d| crate::channel::OutputStatus { name: d.name.clone(), target: format!("{} · {}", d.target, d.state), connected: Some(d.state == "sendet"), rtt_ms: None, lost: 0 })
                .collect(),
            playout: Some(st),
            ..Default::default()
        };
        emit(&ChildMsg::Status(status));
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(name: &str, run: &str, wall: u64, dur: u64) -> SegMeta {
        SegMeta { name: name.into(), run: run.into(), wall_ms: wall, dur_ms: dur, size: 0, hash: String::new() }
    }

    #[test]
    fn names_and_ids() {
        assert_eq!(parse_name("20261114-101500_1731580000000_000012.ts"), Some(("20261114-101500", "1731580000000", 12)));
        assert_eq!(parse_name("x.ts"), None);
        assert!(safe_id("../etc").is_none() && safe_id("a/b").is_none());
        assert_eq!(safe_id("dev1-2026_11").as_deref(), Some("dev1-2026_11"));
        assert_eq!(urldecode(&urlencode("Gottesdienst – 10 Uhr")), "Gottesdienst – 10 Uhr");
    }

    #[test]
    fn pcr_from_packets() {
        let mut p = [0xffu8; 188];
        p[0] = 0x47;
        p[3] = 0x30; // Anpassungsfeld + Nutzdaten
        p[4] = 7;
        p[5] = 0x10;
        // Basis 90000 (1 s) → 27 000 000
        let base: u64 = 90_000;
        p[6] = (base >> 25) as u8;
        p[7] = (base >> 17) as u8;
        p[8] = (base >> 9) as u8;
        p[9] = (base >> 1) as u8;
        p[10] = ((base & 1) << 7) as u8 | 0x7e;
        p[11] = 0;
        assert_eq!(packet_pcr(&p), Some(27_000_000));
        let mut data = vec![0u8; 188 * 3];
        data[188..376].copy_from_slice(&p);
        let g = pcr_groups(&data);
        assert_eq!(g.len(), 1, "alles vor der ersten PCR gehört zur ersten Gruppe");
    }

    #[test]
    fn pat_and_pmt() {
        // PAT mit Anpassungsfeld-Füllung wie von mpegtsmux: Programm 1 → PMT-PID 0x20
        let mut pat = [0xffu8; 188];
        pat[..4].copy_from_slice(&[0x47, 0x40, 0x00, 0x30]);
        pat[4] = 166; // Anpassungsfeld bis Byte 170
        pat[5] = 0;
        let t = 5 + 166;
        pat[t..t + 17].copy_from_slice(&[0x00, 0x00, 0xb0, 0x0d, 0x00, 0x01, 0xc1, 0x00, 0x00, 0x00, 0x01, 0xe0, 0x20, 0, 0, 0, 0]);
        assert_eq!(pat_pmt_pid(&pat), Some(0x20));
        let mut pmt = [0xffu8; 188];
        pmt[..4].copy_from_slice(&[0x47, 0x40, 0x20, 0x10]);
        // pointer, table 0x02, len, prog 1, ver, sec, last, PCR-PID 0x41, pil 0, AAC 0x0f/0x42, H.264 0x1b/0x41
        let body = [0x00, 0x02, 0xb0, 0x17, 0x00, 0x01, 0xc1, 0x00, 0x00, 0xe0, 0x41, 0xf0, 0x00, 0x0f, 0xe0, 0x42, 0xf0, 0x00, 0x1b, 0xe0, 0x41, 0xf0, 0x00, 0, 0, 0, 0];
        pmt[4..4 + body.len()].copy_from_slice(&body);
        assert_eq!(pmt_video_pid(&pmt), Some(0x41));
    }

    #[test]
    fn rtmp_presets() {
        let d = Destination { platform: Platform::Youtube, key: "abcd-1234".into(), ..Default::default() };
        assert_eq!(d.rtmp_url(), "rtmp://a.rtmp.youtube.com/live2/abcd-1234");
        assert_eq!(d.label(), "YouTube");
        let d = Destination { url: "rtmp://host/app/".into(), key: "k".into(), ..Default::default() };
        assert_eq!(d.rtmp_url(), "rtmp://host/app/k");
    }

    #[test]
    fn delayed_playlist_and_discontinuity() {
        let s = vec![seg("s_1_000000.ts", "1", 10_000, 2000), seg("s_1_000001.ts", "1", 12_000, 2000), seg("s_2_000000.ts", "2", 20_000, 2000)];
        // Verzögerung: bei Aufnahmezeit 14 s sind nur die ersten zwei fertig
        let p = playlist(&s, Some(14_000), 6, "");
        assert!(p.contains("s_1_000001.ts") && !p.contains("s_2_000000.ts") && !p.contains("ENDLIST"), "{p}");
        let v = playlist(&s, None, 0, "seg/");
        assert!(v.contains("#EXT-X-DISCONTINUITY\n#EXT-X-PROGRAM-DATE-TIME") && v.contains("seg/s_2_000000.ts") && v.ends_with("#EXT-X-ENDLIST\n"), "{v}");
        assert!(v.contains("#EXT-X-PROGRAM-DATE-TIME:1970-01-01T00:00:10.000Z"));
        // Fenster: nur die letzten n, Media-Sequence passt
        let w = playlist(&s, None, 1, "");
        assert!(w.contains("MEDIA-SEQUENCE:2") && w.contains("#EXT-X-DISCONTINUITY"), "{w}");
    }

    #[test]
    fn spool_completion() {
        let f = |n: &str, run: &str, c: u64, m: u64| SpoolFile { path: PathBuf::new(), name: n.into(), run: run.into(), wall_ms: c, mtime_ms: m };
        let files = vec![f("a_1_000000.ts", "1", 0, 1990), f("a_1_000001.ts", "1", 2000, 3990), f("a_1_000002.ts", "1", 4000, 4500)];
        // das letzte wächst noch
        assert_eq!(complete_segments(&files, 4000, 5000), vec![(0, 2000), (1, 2000)]);
        // Kanal steht: auch das letzte ist fertig
        assert_eq!(complete_segments(&files, 4000, 9000), vec![(0, 2000), (1, 2000), (2, 500)]);
    }

    #[test]
    fn store_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("lzs-store-{}", now_ms()));
        let m = seg("s_1_000000.ts", "1", 1, 2);
        assert!(store_segment(&dir, &m, b"abc").unwrap());
        assert!(!store_segment(&dir, &m, b"abc").unwrap());
        assert_eq!(read_index(&dir).len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    struct Mem(Vec<(SegMeta, Vec<u8>)>, Arc<Mutex<usize>>);
    impl SegmentSource for Mem {
        fn list(&mut self) -> Option<(String, Vec<SegMeta>)> {
            let n = *self.1.lock().unwrap();
            Some(("s".into(), self.0.iter().take(n).map(|(m, _)| m.clone()).collect()))
        }
        fn read(&mut self, _: &str, name: &str) -> Option<Vec<u8>> {
            self.0.iter().find(|(m, _)| m.name == name).map(|(_, d)| d.clone())
        }
    }

    /// Verzögerung wird eingehalten; fehlt ein Segment, wartet der Zuspieler und macht danach lückenlos weiter
    #[test]
    fn feeder_waits_and_resumes() {
        let t0 = now_ms();
        let segs: Vec<(SegMeta, Vec<u8>)> = (0..4).map(|i| (seg(&format!("s_1_{i:06}.ts"), "1", t0 + i * 200, 200), vec![i as u8; 188 * 4])).collect();
        let avail = Arc::new(Mutex::new(2));
        let got: Arc<Mutex<Vec<(u8, Instant)>>> = Arc::default();
        let (g2, ctl, st) = (got.clone(), Arc::new(FeedControl::default()), Arc::new(Mutex::new(FeedStatus::default())));
        let start = Instant::now();
        let h = spawn_feeder(
            Mem(segs, avail.clone()),
            300,
            ctl.clone(),
            st.clone(),
            Box::new(move |d, s, _| {
                if s {
                    g2.lock().unwrap().push((d[0], Instant::now()));
                }
            }),
        );
        std::thread::sleep(Duration::from_millis(1100));
        // nach zwei Segmenten fehlt das dritte → steht
        assert_eq!(got.lock().unwrap().iter().map(|g| g.0).collect::<Vec<_>>(), vec![0, 1]);
        assert!(st.lock().unwrap().stalled);
        *avail.lock().unwrap() = 4;
        std::thread::sleep(Duration::from_millis(700));
        ctl.stop.store(true, Ordering::Relaxed);
        let _ = h.join();
        let g = got.lock().unwrap();
        assert_eq!(g.iter().map(|g| g.0).collect::<Vec<_>>(), vec![0, 1, 2, 3]);
        // erstes Segment nicht vor Aufnahmezeit + Verzögerung
        assert!(g[0].1.duration_since(start) >= Duration::from_millis(250), "{:?}", g[0].1.duration_since(start));
        assert!(st.lock().unwrap().current_delay_s > 0.5, "Verzögerung wächst nach dem Warten");
    }
}
