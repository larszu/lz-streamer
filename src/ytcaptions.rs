//! YouTube-Untertitel per HTTP POST (U8): für Fälle, in denen 608 im Strom
//! nicht ankommt (fremder Encoder, Plattform ignoriert SEI).
//!
//! YouTube nennt im Live-Dashboard eine Ingest-URL (`…/closedcaption?cid=…`).
//! Jede Zeile geht als eigener POST mit fortlaufender `seq`; der Körper ist
//! ein Zeitstempel (UTC, Millisekunden) in einer Zeile, darunter der Text.
//! Ohne neue Zeile kommt alle `HEARTBEAT` ein POST nur mit Zeitstempel, damit
//! YouTube die Verbindung als lebendig sieht. YouTube zeigt die Zeilen erst,
//! wenn der Stream mit 30 oder 60 s Verzögerung läuft (Einstellung dort).

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

const HEARTBEAT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct YoutubeCaptions {
    pub enabled: bool,
    /// Ingest-URL aus dem YouTube-Studio (enthält die cid; wird nicht angezeigt)
    pub url: String,
    /// Zeitversatz der Zeilen in ms (positiv = später), etwa wenn der Strom
    /// vor YouTube noch durch einen Mischer läuft
    pub offset_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct YoutubeStatus {
    /// Zeilen und Herzschläge, die YouTube angenommen hat
    pub sent: u64,
    pub failed: u64,
    pub last_error: Option<String>,
    /// Zeitstempel, den YouTube zuletzt zurückgab (Uhrabgleich)
    pub server_time: Option<String>,
}

/// Zeitstempel im Format, das YouTube erwartet: `2026-11-15T10:00:00.000`.
pub fn timestamp(t: SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Utc> = t.into();
    dt.format("%Y-%m-%dT%H:%M:%S%.3f").to_string()
}

/// Körper eines POST: Zeitstempel-Zeile, darunter der Text (leer = Herzschlag).
pub fn body(t: SystemTime, text: Option<&str>) -> String {
    match text.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => format!("{}\n{}\n", timestamp(t), s.replace('\r', "")),
        None => format!("{}\n", timestamp(t)),
    }
}

/// Ingest-URL mit Folgenummer.
pub fn post_url(base: &str, seq: u64) -> String {
    let base = base.trim();
    let sep = if base.contains('?') { '&' } else { '?' };
    format!("{base}{sep}seq={seq}")
}

/// Sendet Zeilen in einem eigenen Faden, damit ein langsamer Server die
/// Pipeline nicht aufhält. Endet, wenn der Sender fallen gelassen wird.
pub struct YtSender {
    tx: Mutex<Sender<(SystemTime, String)>>,
    pub status: Arc<Mutex<YoutubeStatus>>,
    offset_ms: i64,
}

impl YtSender {
    pub fn spawn(cfg: &YoutubeCaptions) -> Option<Self> {
        if !cfg.enabled || cfg.url.trim().is_empty() {
            return None;
        }
        let (tx, rx) = channel();
        let status: Arc<Mutex<YoutubeStatus>> = Arc::default();
        let (url, st) = (cfg.url.trim().to_string(), status.clone());
        std::thread::Builder::new().name("yt-captions".into()).spawn(move || run(&url, rx, &st)).ok()?;
        Some(Self { tx: Mutex::new(tx), status, offset_ms: cfg.offset_ms })
    }

    /// Zeile, die `delay` nach jetzt im Bild erscheint.
    pub fn send(&self, text: &str, delay: Duration) {
        let mut at = SystemTime::now() + delay;
        if self.offset_ms >= 0 {
            at += Duration::from_millis(self.offset_ms as u64);
        } else {
            at -= Duration::from_millis(self.offset_ms.unsigned_abs());
        }
        let _ = self.tx.lock().unwrap().send((at, text.to_string()));
    }
}

fn run(url: &str, rx: Receiver<(SystemTime, String)>, status: &Mutex<YoutubeStatus>) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut seq = 1u64;
    loop {
        let (at, text) = match rx.recv_timeout(HEARTBEAT) {
            Ok((at, t)) => (at, Some(t)),
            Err(RecvTimeoutError::Timeout) => (SystemTime::now(), None),
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let res = agent
            .post(&post_url(url, seq))
            .header("Content-Type", "text/plain; charset=utf-8")
            .send(body(at, text.as_deref()));
        seq += 1;
        let mut st = status.lock().unwrap();
        match res {
            Ok(mut r) if r.status().is_success() => {
                st.sent += 1;
                st.last_error = None;
                st.server_time = r.body_mut().read_to_string().ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
            }
            Ok(r) => {
                st.failed += 1;
                st.last_error = Some(format!("HTTP {}", r.status().as_u16()));
            }
            Err(e) => {
                st.failed += 1;
                st.last_error = Some(e.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_and_url() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_millis(1_794_700_800_123);
        assert_eq!(timestamp(t), "2026-11-15T00:00:00.123");
        assert_eq!(body(t, Some(" Hallo\r\nWelt ")), "2026-11-15T00:00:00.123\nHallo\nWelt\n");
        assert_eq!(body(t, Some("  ")), "2026-11-15T00:00:00.123\n");
        assert_eq!(post_url("http://upload.youtube.com/closedcaption?cid=abc", 7), "http://upload.youtube.com/closedcaption?cid=abc&seq=7");
        assert_eq!(post_url("http://h/cc", 1), "http://h/cc?seq=1");
        assert!(YtSender::spawn(&YoutubeCaptions { enabled: true, ..Default::default() }).is_none(), "ohne URL kein Sender");
    }
}
