//! Betrieb neben dem Kanal: Statistik-Verlauf, Systemlast, Alarme, Zeitplan.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::Mutex;

use crate::admin::{self, Alarm, AlarmInput, DataDir, Settings};
use crate::config::Role as ChannelRole;
use crate::supervisor::Supervisor;

/// Ein Messpunkt alle 5 s, 24 h Verlauf.
const STEP_S: u64 = 5;
const KEEP: usize = 24 * 3600 / STEP_S as usize;

#[derive(Debug, Clone, Serialize, Default)]
pub struct StatPoint {
    pub at_ms: u64,
    pub running: bool,
    pub bitrate_kbps: f64,
    pub rtt_ms: Option<f64>,
    pub lost: u64,
    pub retransmitted: u64,
    pub fps: f64,
    pub standby: bool,
    pub connected: bool,
    pub cpu: f64,
    /// Short-Term-Lautheit je Spur (LUFS)
    pub loudness: Vec<Option<f64>>,
    /// True Peak je Spur (dBTP)
    pub true_peak: Vec<Option<f64>>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct SystemInfo {
    pub cpu_percent: f64,
    pub memory_percent: f64,
    pub memory_total_gb: f64,
    pub disk_free_gb: f64,
    pub disk_total_gb: f64,
    pub hostname: String,
    pub uptime_s: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActiveAlarm {
    #[serde(flatten)]
    pub alarm: Alarm,
    pub since_ms: u64,
}

pub struct Ops {
    pub data: DataDir,
    pub settings: Mutex<Settings>,
    pub stats: Mutex<VecDeque<StatPoint>>,
    pub alarms: Mutex<Vec<ActiveAlarm>>,
    pub system: Mutex<SystemInfo>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl Ops {
    pub fn new(data: DataDir) -> Arc<Self> {
        let settings: Settings = admin::load_json(&data.settings());
        Arc::new(Self {
            data,
            settings: Mutex::new(settings),
            stats: Mutex::new(VecDeque::new()),
            alarms: Mutex::new(Vec::new()),
            system: Mutex::new(SystemInfo::default()),
        })
    }

    pub async fn save_settings(&self, s: Settings) -> anyhow::Result<()> {
        admin::save_json(&self.data.settings(), &s)?;
        *self.settings.lock().await = s;
        Ok(())
    }

    /// Statistik, Systemlast und Alarme – alle 5 s.
    pub async fn monitor(self: Arc<Self>, sup: Arc<Supervisor>) {
        let mut sys = sysinfo::System::new();
        let mut disks = sysinfo::Disks::new_with_refreshed_list();
        let mut tick = tokio::time::interval(Duration::from_secs(STEP_S));
        let (mut standby_for, mut no_peer_for) = (0.0f64, 0.0f64);
        let mut restarts: VecDeque<(u64, u32)> = VecDeque::new();
        let mut last_lost = 0u64;
        loop {
            tick.tick().await;
            sys.refresh_cpu_usage();
            sys.refresh_memory();
            disks.refresh(true);
            let rec_dir = self.data.recordings();
            let disk = disks
                .iter()
                .filter(|d| rec_dir.starts_with(d.mount_point()))
                .max_by_key(|d| d.mount_point().as_os_str().len());
            let info = SystemInfo {
                cpu_percent: sys.global_cpu_usage() as f64,
                memory_percent: if sys.total_memory() > 0 { sys.used_memory() as f64 * 100.0 / sys.total_memory() as f64 } else { 0.0 },
                memory_total_gb: sys.total_memory() as f64 / 1e9,
                disk_free_gb: disk.map(|d| d.available_space() as f64 / 1e9).unwrap_or(0.0),
                disk_total_gb: disk.map(|d| d.total_space() as f64 / 1e9).unwrap_or(0.0),
                hostname: sysinfo::System::host_name().unwrap_or_default(),
                uptime_s: sysinfo::System::uptime(),
            };

            // Platz schaffen: älteste Aufnahmen löschen
            let (auto_delete, min_free) = {
                let s = self.settings.lock().await;
                (s.delete_oldest_recordings, s.alarms.disk_free_gb as f64)
            };
            if auto_delete && min_free > 0.0 && info.disk_total_gb > 0.0 && info.disk_free_gb < min_free {
                let need = ((min_free - info.disk_free_gb) * 1e9) as u64;
                let files = admin::list_files(&rec_dir, &[".ts", ".mp4"]);
                for name in admin::recordings_to_delete(&files, need) {
                    if std::fs::remove_file(rec_dir.join(&name)).is_ok() {
                        sup.log("warn", format!("Platz knapp – älteste Aufnahme gelöscht: {name}")).await;
                    }
                }
            }
            let snap = sup.snapshot().await;
            let cfg = sup.config().await;
            let st = snap.status.as_ref();
            let running = snap.running;
            standby_for = if st.is_some_and(|s| s.standby) { standby_for + STEP_S as f64 } else { 0.0 };
            no_peer_for = if running && st.is_some_and(|s| !s.srt.connected) { no_peer_for + STEP_S as f64 } else { 0.0 };
            let now = now_ms();
            if restarts.back().is_none_or(|&(_, n)| n != snap.restarts) {
                restarts.push_back((now, snap.restarts));
            }
            while restarts.front().is_some_and(|&(t, _)| now - t > 600_000) && restarts.len() > 1 {
                restarts.pop_front();
            }
            let restarts_10min = match (restarts.front(), restarts.back()) {
                (Some(a), Some(b)) => b.1.saturating_sub(a.1),
                _ => 0,
            };
            let lost = st.map(|s| s.srt.lost).unwrap_or(0);
            let lost_per_min = lost.saturating_sub(last_lost) as f64 * 60.0 / STEP_S as f64;
            last_lost = lost;

            let input = AlarmInput {
                running,
                encoder: cfg.role == ChannelRole::Encoder,
                standby_for_s: standby_for,
                no_peer_for_s: no_peer_for,
                bitrate_kbps: st.map(|s| s.bitrate_kbps),
                lost_per_min,
                restarts_10min,
                cpu_percent: info.cpu_percent,
                memory_percent: info.memory_percent,
                disk_free_gb: if info.disk_total_gb > 0.0 { info.disk_free_gb } else { f64::MAX },
                last_error: snap.last_error.clone(),
                // Short-Term braucht 3 s Anlauf; vorher keine Lautheitsalarme
                tracks: st
                    .filter(|s| s.uptime_s >= 6.0)
                    .map(|s| {
                        s.audio_tracks
                            .iter()
                            .map(|t| {
                                let l = t.loudness.clone().unwrap_or_default();
                                (t.name.clone().unwrap_or_else(|| format!("Spur {}", t.index + 1)), l.short_term, l.true_peak, l.silent_s, t.target)
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            };
            let fresh = admin::evaluate_alarms(&self.settings.lock().await.alarms, &input);
            {
                let mut active = self.alarms.lock().await;
                for a in &fresh {
                    if !active.iter().any(|x| x.alarm.id == a.id) {
                        sup.log(a.level, format!("Alarm: {}", a.text)).await;
                        active.push(ActiveAlarm { alarm: a.clone(), since_ms: now });
                    }
                }
                let mut cleared = Vec::new();
                active.retain(|x| {
                    let keep = fresh.iter().any(|a| a.id == x.alarm.id);
                    if !keep {
                        cleared.push(x.alarm.text.clone());
                    }
                    keep
                });
                for x in active.iter_mut() {
                    if let Some(a) = fresh.iter().find(|a| a.id == x.alarm.id) {
                        x.alarm.text = a.text.clone();
                    }
                }
                for c in cleared {
                    sup.log("ok", format!("Alarm erledigt: {c}")).await;
                }
            }

            let point = StatPoint {
                at_ms: now,
                running,
                bitrate_kbps: st.map(|s| s.bitrate_kbps).unwrap_or(0.0),
                rtt_ms: st.and_then(|s| s.srt.rtt_ms),
                lost,
                retransmitted: st.map(|s| s.srt.retransmitted).unwrap_or(0),
                fps: st.map(|s| s.fps).unwrap_or(0.0),
                standby: st.is_some_and(|s| s.standby),
                connected: st.is_some_and(|s| s.srt.connected),
                cpu: info.cpu_percent,
                loudness: st.map(|s| s.audio_tracks.iter().map(|t| t.loudness.as_ref().and_then(|l| l.short_term)).collect()).unwrap_or_default(),
                true_peak: st.map(|s| s.audio_tracks.iter().map(|t| t.loudness.as_ref().and_then(|l| l.true_peak)).collect()).unwrap_or_default(),
            };
            *self.system.lock().await = info;
            let mut stats = self.stats.lock().await;
            stats.push_back(point);
            while stats.len() > KEEP {
                stats.pop_front();
            }
        }
    }

    /// Zeitplan: schaltet nur bei Wechsel des Sollzustands, damit Handbedienung
    /// zwischen zwei Terminen nicht überstimmt wird.
    pub async fn scheduler(self: Arc<Self>, sup: Arc<Supervisor>) {
        let mut tick = tokio::time::interval(Duration::from_secs(10));
        let mut was: Option<String> = None;
        loop {
            tick.tick().await;
            let entries = self.settings.lock().await.schedule.clone();
            let now_entry = admin::schedule_active(&entries, admin::local_now()).cloned();
            let key = now_entry.as_ref().map(|e| format!("{}|{}|{}", e.name, e.start, e.stop));
            if key == was {
                continue;
            }
            match &now_entry {
                Some(e) => {
                    if !e.preset.is_empty() {
                        if let Some(p) = load_preset(&self.data, &e.preset) {
                            let cfg = admin::apply_preset(&sup.config().await, &p);
                            let _ = sup.set_config(cfg).await;
                        }
                    }
                    sup.log("info", format!("Zeitplan: „{}“ beginnt", e.name)).await;
                    sup.start().await;
                }
                None => {
                    if was.is_some() {
                        sup.log("info", "Zeitplan: Sendefenster vorbei").await;
                        sup.stop().await;
                    }
                }
            }
            was = key;
        }
    }
}

pub fn load_preset(data: &DataDir, name: &str) -> Option<crate::config::Config> {
    if let Some((_, c)) = admin::builtin_presets().into_iter().find(|(n, _)| *n == name) {
        return Some(c);
    }
    let file = admin::safe_name(name)?;
    let p = data.presets().join(format!("{file}.json"));
    std::fs::read(p).ok().and_then(|b| serde_json::from_slice(&b).ok())
}

pub fn list_presets(data: &DataDir) -> Vec<serde_json::Value> {
    let mut v: Vec<serde_json::Value> =
        admin::builtin_presets().into_iter().map(|(n, _)| serde_json::json!({"name": n, "builtin": true})).collect();
    for f in admin::list_files(&data.presets(), &[".json"]) {
        v.push(serde_json::json!({"name": f.name.trim_end_matches(".json"), "builtin": false, "modified_ms": f.modified_ms}));
    }
    v
}

/// Bericht als CSV: Verlauf und Ereignisse.
pub fn report_csv(stats: &[StatPoint], events: &[crate::supervisor::Event]) -> String {
    let ts = |ms: u64| {
        chrono::DateTime::from_timestamp_millis(ms as i64)
            .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default()
    };
    let mut s = String::from("zeit;laeuft;bitrate_kbps;rtt_ms;verloren;wiederholt;fps;standbild;verbunden;cpu;lautheit_s_lufs_je_spur;true_peak_dbtp_je_spur\n");
    let join = |v: &[Option<f64>]| v.iter().map(|x| x.map(|x| format!("{x:.1}")).unwrap_or_default()).collect::<Vec<_>>().join("/");
    for p in stats {
        s.push_str(&format!(
            "{};{};{:.0};{};{};{};{:.1};{};{};{:.0};{};{}\n",
            ts(p.at_ms),
            p.running as u8,
            p.bitrate_kbps,
            p.rtt_ms.map(|r| format!("{r:.2}")).unwrap_or_default(),
            p.lost,
            p.retransmitted,
            p.fps,
            p.standby as u8,
            p.connected as u8,
            p.cpu,
            join(&p.loudness),
            join(&p.true_peak)
        ));
    }
    s.push_str("\nzeit;stufe;ereignis\n");
    for e in events {
        s.push_str(&format!("{};{};{}\n", ts(e.at_ms), e.level, e.text.replace(';', ",")));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_has_header_and_rows() {
        let p = StatPoint {
            at_ms: 1_762_300_000_000,
            running: true,
            bitrate_kbps: 8000.4,
            rtt_ms: Some(1.234),
            loudness: vec![Some(-23.04), None, Some(-18.0)],
            ..Default::default()
        };
        let e = crate::supervisor::Event { at_ms: 1_762_300_000_000, level: "ok".into(), text: "a;b".into() };
        let csv = report_csv(&[p], &[e]);
        assert!(csv.starts_with("zeit;laeuft;bitrate_kbps"));
        assert!(csv.contains(";1;8000;1.23;"));
        assert!(csv.contains(";-23.0//-18.0;"), "{csv}");
        assert!(csv.contains(";ok;a,b"));
    }

    #[test]
    fn builtin_preset_lookup() {
        let d = DataDir::new(std::env::temp_dir().join("lzs-preset-test"));
        assert!(load_preset(&d, "H.264 720p50 schmal").is_some());
        assert!(load_preset(&d, "../x").is_none());
        assert!(list_presets(&d).len() >= 5);
    }
}
