//! Verwaltung: Profile, Zeitplan, Alarme, Standbilder, Aufnahmen, Benutzer.
//! Alles als Dateien im Datenverzeichnis neben der Konfiguration.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use serde::{Deserialize, Serialize};

use crate::config::{Codec, Config};

// ------------------------------------------------------------ Dateien

/// Dateiname aus Benutzereingabe: nur Buchstaben, Ziffern, Punkt, Strich,
/// Unterstrich, Leerzeichen; kein Pfad.
pub fn safe_name(name: &str) -> Option<String> {
    let n = name.trim();
    if n.is_empty() || n.len() > 120 || n.starts_with('.') {
        return None;
    }
    n.chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ' | '(' | ')'))
        .then(|| n.to_string())
}

#[derive(Debug, Clone, Serialize)]
pub struct FileEntry {
    pub name: String,
    pub bytes: u64,
    pub modified_ms: u64,
}

pub fn list_files(dir: &Path, exts: &[&str]) -> Vec<FileEntry> {
    let mut v: Vec<FileEntry> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_lowercase();
            exts.iter().any(|x| n.ends_with(x))
        })
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some(FileEntry {
                name: e.file_name().to_string_lossy().to_string(),
                bytes: m.len(),
                modified_ms: m
                    .modified()
                    .ok()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?
                    .as_millis() as u64,
            })
        })
        .collect();
    v.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms));
    v
}

/// Bild prüfen: PNG oder JPEG an den ersten Bytes erkennen.
pub fn image_kind(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else {
        None
    }
}

#[derive(Debug, Clone)]
pub struct DataDir {
    pub root: PathBuf,
}

impl DataDir {
    pub fn new(root: PathBuf) -> Self {
        for sub in ["profile", "standbilder", "aufnahmen"] {
            let _ = std::fs::create_dir_all(root.join(sub));
        }
        Self { root }
    }
    pub fn presets(&self) -> PathBuf {
        self.root.join("profile")
    }
    pub fn standby(&self) -> PathBuf {
        self.root.join("standbilder")
    }
    pub fn recordings(&self) -> PathBuf {
        self.root.join("aufnahmen")
    }
    pub fn users(&self) -> PathBuf {
        self.root.join("benutzer.json")
    }
    pub fn settings(&self) -> PathBuf {
        self.root.join("verwaltung.json")
    }
}

// ------------------------------------------------------------ Profile

/// Mitgelieferte Vorlagen. Ein Profil ersetzt Bild-, Ton- und Zielwerte,
/// lässt Eingang und Rolle aber stehen.
pub fn builtin_presets() -> Vec<(&'static str, Config)> {
    let mk = |codec: Codec, w: u32, h: u32, fps: u32, kbps: u32| {
        let mut c = Config::default();
        c.encoder.video.codec = codec;
        c.encoder.video.width = w;
        c.encoder.video.height = h;
        c.encoder.video.fps = fps;
        c.encoder.video.bitrate_kbps = kbps;
        c
    };
    vec![
        ("H.264 1080p50 Contribution", mk(Codec::H264, 1920, 1080, 50, 10_000)),
        ("H.264 1080p25 Social", mk(Codec::H264, 1920, 1080, 25, 6_000)),
        ("H.264 720p50 schmal", mk(Codec::H264, 1280, 720, 50, 4_000)),
        ("H.265 1080p50 sparsam", mk(Codec::H265, 1920, 1080, 50, 6_000)),
        ("H.265 2160p50 UHD (HD wird hochskaliert)", mk(Codec::H265, 3840, 2160, 50, 25_000)),
    ]
}

/// Profil auf eine bestehende Konfiguration anwenden (Eingang, Rolle,
/// Decoder und Zugangsdaten bleiben).
pub fn apply_preset(current: &Config, preset: &Config) -> Config {
    let mut c = current.clone();
    c.encoder.video = preset.encoder.video.clone();
    c.encoder.audio = preset.encoder.audio.clone();
    c.encoder.standby = preset.encoder.standby.clone();
    c.encoder.srt.latency_ms = preset.encoder.srt.latency_ms;
    c.encoder.record = preset.encoder.record.clone();
    c
}

// ------------------------------------------------------------ Zeitplan

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ScheduleEntry {
    pub enabled: bool,
    pub name: String,
    /// einmalig an diesem Tag ("2026-11-20"); leer = wöchentlich nach `days`
    pub date: String,
    /// 1 = Montag … 7 = Sonntag
    pub days: Vec<u8>,
    pub start: String,
    pub stop: String,
    /// Profil, das vor dem Start geladen wird (leer = aktuelle Einstellungen)
    pub preset: String,
}

impl Default for ScheduleEntry {
    fn default() -> Self {
        Self {
            enabled: true,
            name: "Sendung".into(),
            date: String::new(),
            days: vec![1, 2, 3, 4, 5],
            start: "09:00".into(),
            stop: "10:00".into(),
            preset: String::new(),
        }
    }
}

fn hm(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s.trim(), "%H:%M").ok()
}

impl ScheduleEntry {
    /// Liegt `now` im Sendefenster? Fenster über Mitternacht sind erlaubt
    /// (Stopp vor Start → endet am Folgetag).
    pub fn active_at(&self, now: NaiveDateTime) -> bool {
        if !self.enabled {
            return false;
        }
        let (Some(start), Some(stop)) = (hm(&self.start), hm(&self.stop)) else { return false };
        let t = now.time();
        let today = now.date();
        let yesterday = today.pred_opt().unwrap_or(today);
        let day_ok = |d: NaiveDate| -> bool {
            if self.date.trim().is_empty() {
                self.days.contains(&(d.weekday().number_from_monday() as u8))
            } else {
                NaiveDate::parse_from_str(self.date.trim(), "%Y-%m-%d").is_ok_and(|x| x == d)
            }
        };
        if start <= stop {
            day_ok(today) && t >= start && t < stop
        } else {
            (day_ok(today) && t >= start) || (day_ok(yesterday) && t < stop)
        }
    }
}

/// Erster aktiver Eintrag oder keiner.
pub fn schedule_active(entries: &[ScheduleEntry], now: NaiveDateTime) -> Option<&ScheduleEntry> {
    entries.iter().find(|e| e.active_at(now))
}

// ------------------------------------------------------------ Alarme

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AlarmSettings {
    /// Standbild länger als n Sekunden
    pub signal_loss_s: u32,
    /// keine Gegenstelle länger als n Sekunden (0 = aus)
    pub no_peer_s: u32,
    /// Ausgangsbitrate unter n kbit/s (0 = aus)
    pub min_bitrate_kbps: u32,
    /// SRT-Verluste je Minute über n (0 = aus)
    pub max_lost_per_min: u32,
    /// Neustarts innerhalb von 10 Minuten
    pub max_restarts: u32,
    pub cpu_percent: u32,
    pub memory_percent: u32,
    pub disk_free_gb: u32,
    /// Ziel-Lautheit (Short-Term) in LUFS, z. B. −23 (R 128) oder −16 (Streaming)
    pub loudness_target: f64,
    /// erlaubte Abweichung in LU (0 = aus)
    pub loudness_tolerance_lu: f64,
    /// Spur still länger als n s (0 = aus)
    pub silence_s: u32,
    /// True Peak über n dBTP (z. B. −1)
    pub true_peak_max_dbtp: f64,
}

impl Default for AlarmSettings {
    fn default() -> Self {
        Self {
            signal_loss_s: 2,
            no_peer_s: 30,
            min_bitrate_kbps: 0,
            max_lost_per_min: 100,
            max_restarts: 3,
            cpu_percent: 90,
            memory_percent: 85,
            disk_free_gb: 10,
            loudness_target: -23.0,
            loudness_tolerance_lu: 5.0,
            silence_s: 10,
            true_peak_max_dbtp: -1.0,
        }
    }
}

/// Was die Alarm-Prüfung pro Durchlauf sieht.
#[derive(Debug, Clone, Default)]
pub struct AlarmInput {
    pub running: bool,
    pub encoder: bool,
    pub standby_for_s: f64,
    pub no_peer_for_s: f64,
    pub bitrate_kbps: Option<f64>,
    pub lost_per_min: f64,
    pub restarts_10min: u32,
    pub cpu_percent: f64,
    pub memory_percent: f64,
    pub disk_free_gb: f64,
    pub last_error: Option<String>,
    /// je Tonspur: Name, Short-Term-Lautheit, True Peak, Stille in s, eigenes Ziel
    pub tracks: Vec<(String, Option<f64>, Option<f64>, f64, Option<f64>)>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Alarm {
    pub id: String,
    pub level: &'static str,
    pub text: String,
}

pub fn evaluate_alarms(s: &AlarmSettings, i: &AlarmInput) -> Vec<Alarm> {
    let mut v = Vec::new();
    let mut add = |id: &str, level, text: String| v.push(Alarm { id: id.to_string(), level, text });
    if i.running {
        if i.encoder && s.signal_loss_s > 0 && i.standby_for_s >= s.signal_loss_s as f64 {
            add("signal", "error", format!("Kein Eingangssignal seit {:.0} s – Standbild läuft", i.standby_for_s));
        }
        if s.no_peer_s > 0 && i.no_peer_for_s >= s.no_peer_s as f64 {
            add("peer", "warn", format!("Keine Gegenstelle seit {:.0} s", i.no_peer_for_s));
        }
        if let Some(b) = i.bitrate_kbps {
            if s.min_bitrate_kbps > 0 && b < s.min_bitrate_kbps as f64 {
                add("bitrate", "warn", format!("Bitrate {b:.0} kbit/s unter {} kbit/s", s.min_bitrate_kbps));
            }
        }
        if s.max_lost_per_min > 0 && i.lost_per_min > s.max_lost_per_min as f64 {
            add("loss", "warn", format!("{:.0} verlorene SRT-Pakete je Minute", i.lost_per_min));
        }
        if s.max_restarts > 0 && i.restarts_10min >= s.max_restarts {
            add("restarts", "error", format!("{} Neustarts in 10 Minuten", i.restarts_10min));
        }
        for (n, (name, short, tp, silent, own)) in i.tracks.iter().enumerate() {
            let target = own.unwrap_or(s.loudness_target);
            if s.silence_s > 0 && *silent >= s.silence_s as f64 {
                add(&format!("silence:{n}"), "warn", format!("Tonspur „{name}“ still seit {silent:.0} s"));
            } else if let Some(st) = short {
                if s.loudness_tolerance_lu > 0.0 && (st - target).abs() > s.loudness_tolerance_lu {
                    add(
                        &format!("loudness:{n}"),
                        "warn",
                        format!("Tonspur „{name}“ {st:.1} LUFS, Ziel {target:.0} ±{:.0} LU", s.loudness_tolerance_lu),
                    );
                }
            }
            if let Some(p) = tp {
                if *p > s.true_peak_max_dbtp {
                    add(&format!("peak:{n}"), "warn", format!("Tonspur „{name}“ übersteuert: {p:.1} dBTP"));
                }
            }
        }
    }
    if s.cpu_percent > 0 && i.cpu_percent > s.cpu_percent as f64 {
        add("cpu", "warn", format!("CPU {:.0} %", i.cpu_percent));
    }
    if s.memory_percent > 0 && i.memory_percent > s.memory_percent as f64 {
        add("memory", "warn", format!("Arbeitsspeicher {:.0} %", i.memory_percent));
    }
    if s.disk_free_gb > 0 && i.disk_free_gb < s.disk_free_gb as f64 {
        add("disk", "warn", format!("Nur noch {:.1} GB frei für Aufnahmen", i.disk_free_gb));
    }
    v
}

/// Einstellungen, die nicht zur Kanal-Konfiguration gehören (ändern sie,
/// startet der Kanal nicht neu).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Settings {
    pub schedule: Vec<ScheduleEntry>,
    pub alarms: AlarmSettings,
    /// älteste Aufnahmen löschen, wenn weniger als `alarms.disk_free_gb` frei ist
    pub delete_oldest_recordings: bool,
}

/// Welche Aufnahmen weg müssen, damit wieder `need` Bytes frei sind
/// (älteste zuerst, die neueste – womöglich laufende – bleibt immer).
/// Untertiteldateien (.srt/.vtt), die neben einer Aufnahme liegen
pub fn subtitle_files(dir: &std::path::Path, recording: &str) -> Vec<String> {
    let stem = std::path::Path::new(recording).with_extension("");
    ["srt", "vtt"]
        .iter()
        .map(|e| stem.with_extension(e))
        .filter(|p| dir.join(p).is_file())
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect()
}

/// Aufnahme samt Untertiteldateien löschen
pub fn remove_recording(dir: &std::path::Path, name: &str) -> std::io::Result<()> {
    for s in subtitle_files(dir, name) {
        let _ = std::fs::remove_file(dir.join(s));
    }
    std::fs::remove_file(dir.join(name))
}

pub fn recordings_to_delete(files: &[FileEntry], need: u64) -> Vec<String> {
    let mut sorted: Vec<&FileEntry> = files.iter().collect();
    sorted.sort_by_key(|f| f.modified_ms);
    sorted.pop();
    let mut freed = 0;
    let mut out = Vec::new();
    for f in sorted {
        if freed >= need {
            break;
        }
        freed += f.bytes;
        out.push(f.name.clone());
    }
    out
}

pub fn load_json<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> T {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save_json<T: Serialize>(path: &Path, v: &T) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(v)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// ------------------------------------------------------------ Benutzer

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// nur ansehen
    Observer,
    /// starten, stoppen, Standbild testen
    Operator,
    /// alles, auch Einstellungen und Benutzer
    Admin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub name: String,
    pub role: Role,
    pub hash: String,
}

pub fn hash_password(pw: &str) -> Result<String> {
    use argon2::password_hash::{PasswordHasher, SaltString};
    let salt = SaltString::generate(&mut rand_core::OsRng);
    argon2::Argon2::default()
        .hash_password(pw.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| anyhow::anyhow!("{e}"))
}

pub fn verify_password(pw: &str, hash: &str) -> bool {
    use argon2::password_hash::{PasswordHash, PasswordVerifier};
    PasswordHash::new(hash).is_ok_and(|h| argon2::Argon2::default().verify_password(pw.as_bytes(), &h).is_ok())
}

pub fn check_new_password(pw: &str) -> Result<()> {
    if pw.chars().count() < 8 {
        bail!("Passwort braucht mindestens 8 Zeichen");
    }
    Ok(())
}

pub fn new_token() -> String {
    let mut b = [0u8; 24];
    getrandom::getrandom(&mut b).expect("Zufall");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Darf diese Rolle diesen Aufruf?
pub fn allowed(role: Role, method: &str, path: &str) -> bool {
    if method == "GET" {
        return true;
    }
    let operator_ok = ["/api/start", "/api/stop", "/api/signal", "/api/keyframe", "/api/caption", "/api/logout"];
    match role {
        Role::Admin => true,
        Role::Operator => operator_ok.contains(&path),
        Role::Observer => path == "/api/logout",
    }
}

pub fn local_now() -> NaiveDateTime {
    let n = chrono::Local::now().naive_local();
    n.with_nanosecond(0).unwrap_or(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    #[test]
    fn weekly_window() {
        // 2026-11-05 ist ein Donnerstag
        let e = ScheduleEntry { days: vec![4], start: "09:00".into(), stop: "10:30".into(), ..Default::default() };
        assert!(!e.active_at(at("2026-11-05 08:59")));
        assert!(e.active_at(at("2026-11-05 09:00")));
        assert!(e.active_at(at("2026-11-05 10:29")));
        assert!(!e.active_at(at("2026-11-05 10:30")));
        assert!(!e.active_at(at("2026-11-06 09:30")), "Freitag nicht gewählt");
    }

    #[test]
    fn overnight_window_belongs_to_start_day() {
        let e = ScheduleEntry { days: vec![5], start: "22:00".into(), stop: "02:00".into(), ..Default::default() };
        assert!(e.active_at(at("2026-11-06 23:00")), "Freitagabend");
        assert!(e.active_at(at("2026-11-07 01:59")), "Nacht auf Samstag");
        assert!(!e.active_at(at("2026-11-07 23:00")), "Samstagabend nicht");
    }

    #[test]
    fn one_off_date_and_disabled() {
        let mut e = ScheduleEntry { date: "2026-11-20".into(), start: "18:00".into(), stop: "20:00".into(), ..Default::default() };
        assert!(e.active_at(at("2026-11-20 19:00")));
        assert!(!e.active_at(at("2026-11-27 19:00")));
        e.enabled = false;
        assert!(!e.active_at(at("2026-11-20 19:00")));
        let bad = ScheduleEntry { start: "25:00".into(), ..Default::default() };
        assert!(!bad.active_at(at("2026-11-05 09:30")));
    }

    #[test]
    fn alarms_fire_by_threshold() {
        let s = AlarmSettings::default();
        let quiet = AlarmInput { running: true, encoder: true, disk_free_gb: 100.0, ..Default::default() };
        assert!(evaluate_alarms(&s, &quiet).is_empty());
        let bad = AlarmInput { standby_for_s: 3.0, no_peer_for_s: 31.0, restarts_10min: 3, cpu_percent: 95.0, ..quiet.clone() };
        let ids: Vec<_> = evaluate_alarms(&s, &bad).iter().map(|a| a.id.clone()).collect();
        assert_eq!(ids, vec!["signal", "peer", "restarts", "cpu"]);
        let stopped = AlarmInput { running: false, ..bad };
        assert_eq!(evaluate_alarms(&s, &stopped).iter().map(|a| a.id.clone()).collect::<Vec<_>>(), vec!["cpu"]);
    }

    #[test]
    fn loudness_alarms_per_track() {
        let s = AlarmSettings::default();
        let base = AlarmInput { running: true, encoder: true, disk_free_gb: 100.0, ..Default::default() };
        let ok = AlarmInput { tracks: vec![("DE".into(), Some(-23.5), Some(-3.0), 0.0, None)], ..base.clone() };
        assert!(evaluate_alarms(&s, &ok).is_empty());
        let bad = AlarmInput {
            tracks: vec![
                ("DE".into(), Some(-35.0), Some(-0.2), 0.0, None),
                ("EN".into(), None, None, 12.0, None),
                ("Stream".into(), Some(-16.0), Some(-3.0), 0.0, Some(-16.0)),
            ],
            ..base
        };
        let ids: Vec<_> = evaluate_alarms(&s, &bad).iter().map(|a| a.id.clone()).collect();
        assert_eq!(ids, vec!["loudness:0", "peak:0", "silence:1"]);
    }

    #[test]
    fn names_and_images() {
        assert_eq!(safe_name("Kein Signal (Tafel).png").as_deref(), Some("Kein Signal (Tafel).png"));
        assert!(safe_name("../etc/passwd").is_none());
        assert!(safe_name(".hidden").is_none());
        assert!(safe_name("a/b.png").is_none());
        assert_eq!(image_kind(&[0x89, b'P', b'N', b'G', 0]), Some("png"));
        assert_eq!(image_kind(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("jpg"));
        assert_eq!(image_kind(b"GIF89a"), None);
    }

    #[test]
    fn passwords_and_roles() {
        let h = hash_password("geheim123").unwrap();
        assert!(verify_password("geheim123", &h));
        assert!(!verify_password("falsch", &h));
        assert!(check_new_password("kurz").is_err());
        assert!(allowed(Role::Observer, "GET", "/api/state"));
        assert!(!allowed(Role::Observer, "POST", "/api/start"));
        assert!(allowed(Role::Operator, "POST", "/api/start"));
        assert!(!allowed(Role::Operator, "PUT", "/api/config"));
        assert!(allowed(Role::Admin, "PUT", "/api/config"));
        assert_ne!(new_token(), new_token());
    }

    #[test]
    fn oldest_recordings_go_first_newest_stays() {
        let f = |n: &str, t: u64, b: u64| FileEntry { name: n.into(), bytes: b, modified_ms: t };
        let files = vec![f("c", 3, 10), f("a", 1, 10), f("b", 2, 10)];
        assert_eq!(recordings_to_delete(&files, 15), vec!["a", "b"]);
        assert_eq!(recordings_to_delete(&files, 1000), vec!["a", "b"], "neueste bleibt");
        assert!(recordings_to_delete(&files[..1], 5).is_empty());
    }

    #[test]
    fn preset_keeps_input_and_role() {
        let mut cur = Config::default();
        cur.encoder.input.device = 2;
        cur.encoder.srt.host = "192.0.2.5".into();
        let (_, p) = &builtin_presets()[4];
        let c = apply_preset(&cur, p);
        assert_eq!(c.encoder.video.width, 3840);
        assert_eq!(c.encoder.input.device, 2);
        assert_eq!(c.encoder.srt.host, "192.0.2.5");
    }
}
