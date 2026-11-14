//! HLS per HTTP PUT hochladen. `hlssink3`/`hlscmafsink` schreiben nur
//! Dateien; dieser Uploader beobachtet den Ordner und lädt je Medien-Playlist
//! zuerst alle darin genannten Segmente (und Init-Segmente) hoch, danach die
//! Playlist selbst. So nennt eine Playlist am Ziel nie ein Segment, das dort
//! noch fehlt. Fehlgeschlagenes wird im nächsten Durchgang wiederholt.
//! Übertragen wird mit `curl` (HTTPS, Proxys, Kopfzeilen ohne eigene TLS-Abhängigkeit).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UploadStatus {
    pub target: String,
    /// erfolgreich hochgeladene Dateien
    pub uploaded: u64,
    /// fehlgeschlagene Versuche (werden wiederholt)
    pub failed: u64,
    pub last_error: Option<String>,
}

pub struct Uploader {
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<UploadStatus>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// Segment- und Init-Dateien, die eine Medien-Playlist nennt
pub fn referenced(playlist: &str) -> Vec<String> {
    let mut out = Vec::new();
    for l in playlist.lines().map(str::trim) {
        if let Some(rest) = l.strip_prefix("#EXT-X-MAP:") {
            if let Some(uri) = rest.split("URI=\"").nth(1).and_then(|u| u.split('"').next()) {
                out.push(uri.to_string());
            }
        } else if !l.is_empty() && !l.starts_with('#') {
            out.push(l.to_string());
        }
    }
    out
}

fn content_type(name: &str) -> &'static str {
    match Path::new(name).extension().and_then(|e| e.to_str()) {
        Some("m3u8") => "application/vnd.apple.mpegurl",
        Some("ts") => "video/mp2t",
        Some("m4s") => "video/iso.segment",
        Some("mp4") => "video/mp4",
        Some("mpd") => "application/dash+xml",
        _ => "application/octet-stream",
    }
}

/// Nur einfache Dateinamen (keine Pfade, keine Adressen) werden übertragen
fn plain_name(n: &str) -> bool {
    !n.is_empty() && !n.contains('/') && !n.contains('\\') && !n.contains("..") && !n.contains(':')
}

struct Target {
    url: String,
    header: String,
}

impl Target {
    fn url_for(&self, name: &str) -> String {
        format!("{}/{name}", self.url.trim_end_matches('/'))
    }

    fn curl(&self, args: &[&str], name: &str) -> Result<(), String> {
        let mut c = std::process::Command::new("curl");
        c.args(["-sS", "-f", "--max-time", "15", "--connect-timeout", "5"]).args(args);
        if !self.header.trim().is_empty() {
            c.args(["-H", self.header.trim()]);
        }
        let o = c.arg(self.url_for(name)).output().map_err(|e| format!("curl: {e}"))?;
        if o.status.success() { Ok(()) } else { Err(format!("{name}: {}", String::from_utf8_lossy(&o.stderr).trim())) }
    }

    /// Hochladen mit bis zu drei Versuchen
    fn put(&self, dir: &Path, name: &str) -> Result<(), String> {
        let path = dir.join(name);
        let file = path.to_string_lossy().to_string();
        let ct = format!("Content-Type: {}", content_type(name));
        let mut err = String::new();
        for attempt in 0..3u32 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(300 << attempt));
            }
            match self.curl(&["-X", "PUT", "-H", &ct, "--data-binary", &format!("@{file}")], name) {
                Ok(()) => return Ok(()),
                Err(e) => err = e,
            }
        }
        Err(err)
    }

    fn delete(&self, name: &str) {
        let _ = self.curl(&["-X", "DELETE"], name);
    }
}

/// Ein Durchgang über den Ordner. Merkt sich Hochgeladenes in `done`
/// (Segmente) und `sent` (Playlist-Inhalt).
struct Sync {
    dir: PathBuf,
    target: Target,
    delete: bool,
    done: HashSet<String>,
    sent: HashMap<String, String>,
    refs: HashMap<String, Vec<String>>,
}

impl Sync {
    fn pass(&mut self, status: &Mutex<UploadStatus>) {
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return };
        let mut lists: Vec<String> =
            rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| (n.ends_with(".m3u8") && !n.ends_with(".raw.m3u8")) || n.ends_with(".mpd")).collect();
        // Medien-Playlists vor der Master-Playlist
        lists.sort_by_key(|n| (n == "index.m3u8", n.clone()));
        for list in lists {
            let Ok(text) = std::fs::read_to_string(self.dir.join(&list)) else { continue };
            if self.sent.get(&list) == Some(&text) {
                continue;
            }
            let mut ok = true;
            let names: Vec<String> = if list == "index.m3u8" || list.ends_with(".mpd") { Vec::new() } else { referenced(&text) };
            for n in &names {
                if !plain_name(n) || self.done.contains(n) || !self.dir.join(n).is_file() {
                    continue;
                }
                match self.target.put(&self.dir, n) {
                    Ok(()) => {
                        self.done.insert(n.clone());
                        status.lock().unwrap().uploaded += 1;
                    }
                    Err(e) => {
                        ok = false;
                        let mut s = status.lock().unwrap();
                        s.failed += 1;
                        s.last_error = Some(e);
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }
            match self.target.put(&self.dir, &list) {
                Ok(()) => {
                    self.sent.insert(list.clone(), text);
                    self.refs.insert(list, names);
                    let mut s = status.lock().unwrap();
                    s.uploaded += 1;
                    s.last_error = None;
                }
                Err(e) => {
                    let mut s = status.lock().unwrap();
                    s.failed += 1;
                    s.last_error = Some(e);
                }
            }
        }
        // Segmente, die keine Playlist mehr nennt, vergessen (und am Ziel löschen)
        let live: HashSet<&String> = self.refs.values().flatten().collect();
        let gone: Vec<String> = self.done.iter().filter(|n| !live.contains(n)).cloned().collect();
        for n in gone {
            if self.delete {
                self.target.delete(&n);
            }
            self.done.remove(&n);
        }
    }
}

impl Uploader {
    pub fn start(dir: PathBuf, url: String, header: String, delete: bool) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(UploadStatus { target: url.clone(), ..Default::default() }));
        let (s2, st2) = (stop.clone(), status.clone());
        let handle = std::thread::Builder::new()
            .name("hls-upload".into())
            .spawn(move || {
                let mut sync = Sync { dir, target: Target { url, header }, delete, done: HashSet::new(), sent: HashMap::new(), refs: HashMap::new() };
                while !s2.load(Ordering::Relaxed) {
                    sync.pass(&st2);
                    std::thread::sleep(Duration::from_millis(250));
                }
            })
            .ok();
        Self { stop, status, handle }
    }

    pub fn status(&self) -> UploadStatus {
        self.status.lock().unwrap().clone()
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Uploader {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_segments_and_init() {
        let p = "#EXTM3U\n#EXT-X-MAP:URI=\"v_init_000.mp4\"\n#EXTINF:2.0,\nv_00001.m4s\n#EXTINF:2.0,\nv_00002.m4s\n";
        assert_eq!(referenced(p), vec!["v_init_000.mp4", "v_00001.m4s", "v_00002.m4s"]);
        assert!(plain_name("v_00001.ts") && !plain_name("../x") && !plain_name("http://a/b"));
        assert_eq!(content_type("a.m3u8"), "application/vnd.apple.mpegurl");
    }

    /// Kleiner PUT-Empfänger: erster Versuch je Playlist schlägt fehl (Wiederholung)
    /// Methode, Pfad, Länge des Inhalts
    type Log = Arc<Mutex<Vec<(String, String, usize)>>>;

    fn server() -> (String, Log) {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let log: Log = Arc::default();
        let lg = log.clone();
        std::thread::spawn(move || {
            let mut failed = HashSet::new();
            for s in l.incoming().flatten() {
                let mut s = s;
                let mut buf = Vec::new();
                let mut tmp = [0u8; 65536];
                let (head, body_len) = loop {
                    let n = s.read(&mut tmp).unwrap_or(0);
                    if n == 0 {
                        break (String::new(), 0);
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..p]).to_string();
                        let len = head
                            .lines()
                            .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                            .unwrap_or(0);
                        while buf.len() < p + 4 + len {
                            let n = s.read(&mut tmp).unwrap_or(0);
                            if n == 0 {
                                break;
                            }
                            buf.extend_from_slice(&tmp[..n]);
                        }
                        break (head, len);
                    }
                };
                let mut it = head.split_whitespace();
                let (m, path) = (it.next().unwrap_or("").to_string(), it.next().unwrap_or("").to_string());
                let fail = path.ends_with(".m3u8") && failed.insert(path.clone());
                lg.lock().unwrap().push((m, path, body_len));
                let code = if fail { "500 Internal Server Error" } else { "201 Created" };
                let _ = s.write_all(format!("HTTP/1.1 {code}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes());
            }
        });
        (format!("http://{addr}/live"), log)
    }

    #[test]
    fn uploads_segments_before_playlist_with_retry() {
        let dir = std::env::temp_dir().join(format!("lzs-up-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("v_00001.ts"), vec![7u8; 1000]).unwrap();
        std::fs::write(dir.join("v_00002.ts"), vec![7u8; 500]).unwrap();
        std::fs::write(dir.join("v.m3u8"), "#EXTM3U\n#EXTINF:2,\nv_00001.ts\n#EXTINF:2,\nv_00002.ts\n").unwrap();
        std::fs::write(dir.join("index.m3u8"), "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nv.m3u8\n").unwrap();
        let (url, log) = server();
        let status = Mutex::new(UploadStatus::default());
        let mut sync = Sync {
            dir: dir.clone(),
            target: Target { url, header: "X-Test: 1".into() },
            delete: true,
            done: HashSet::new(),
            sent: HashMap::new(),
            refs: HashMap::new(),
        };
        sync.pass(&status);
        let l = log.lock().unwrap().clone();
        let paths: Vec<&str> = l.iter().map(|(_, p, _)| p.as_str()).collect();
        // Segmente zuerst, Playlist nach einem Fehlversuch wiederholt
        assert_eq!(paths[..2], ["/live/v_00001.ts", "/live/v_00002.ts"], "{paths:?}");
        assert_eq!(l[0].2, 1000);
        assert_eq!(paths.iter().filter(|p| **p == "/live/v.m3u8").count(), 2, "{paths:?}");
        assert_eq!(paths.last(), Some(&"/live/index.m3u8"));
        assert_eq!(status.lock().unwrap().uploaded, 4);
        // neue Playlist ohne Segment 1: nur Segment 3 und Playlist, Segment 1 wird gelöscht
        std::fs::write(dir.join("v_00003.ts"), vec![7u8; 10]).unwrap();
        std::fs::write(dir.join("v.m3u8"), "#EXTM3U\n#EXTINF:2,\nv_00002.ts\n#EXTINF:2,\nv_00003.ts\n").unwrap();
        sync.pass(&status);
        let l = log.lock().unwrap().clone();
        let tail: Vec<String> = l.iter().skip(paths.len()).map(|(m, p, _)| format!("{m} {p}")).collect();
        assert_eq!(tail, ["PUT /live/v_00003.ts", "PUT /live/v.m3u8", "DELETE /live/v_00001.ts"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
