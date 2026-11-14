//! Der Aufpasser: startet den Kanal als eigenen Prozess, liest dessen
//! Status, startet ihn bei Absturz oder Stillstand neu (mit wachsender
//! Pause) und hält eine kurze Ereignisliste für die Oberfläche.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, Notify};

use crate::channel::{ChannelStatus, ChildCmd, ChildMsg};
use crate::config::{Config, Role};

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub at_ms: u64,
    pub level: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Snapshot {
    pub running: bool,
    pub phase: String,
    pub restarts: u32,
    pub status: Option<ChannelStatus>,
    pub last_error: Option<String>,
    pub events: Vec<Event>,
    pub ever_connected: bool,
}

struct State {
    config: Config,
    desired: bool,
    generation: u64,
    phase: String,
    restarts: u32,
    status: Option<ChannelStatus>,
    last_error: Option<String>,
    events: VecDeque<Event>,
    stdin: Option<ChildStdin>,
    ever_connected: bool,
}

pub struct Supervisor {
    state: Mutex<State>,
    wake: Notify,
    exe: PathBuf,
    pub config_path: PathBuf,
    pub preview_path: PathBuf,
    run_dir: PathBuf,
    /// Aufnahmeordner, wenn in der Konfiguration keiner steht
    pub record_dir: PathBuf,
    /// neue Untertiteltexte aus dem Kanal (für den Grafik-Renderer)
    pub captions: tokio::sync::broadcast::Sender<String>,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Nach so vielen Sekunden ohne neue Bytes gilt ein Encoder als hängend.
const STALL_SECS: u64 = 8;
/// Ohne Statuszeile so lange → Kindprozess hängt.
const SILENT_SECS: u64 = 6;

impl Supervisor {
    pub fn new(config: Config, config_path: PathBuf, run_dir: PathBuf, record_dir: PathBuf) -> Arc<Self> {
        let _ = std::fs::create_dir_all(&run_dir);
        let exe = std::env::current_exe().expect("eigener Programmpfad");
        Arc::new(Self {
            state: Mutex::new(State {
                config,
                desired: false,
                generation: 0,
                phase: "gestoppt".into(),
                restarts: 0,
                status: None,
                last_error: None,
                events: VecDeque::new(),
                stdin: None,
                ever_connected: false,
            }),
            wake: Notify::new(),
            exe,
            config_path,
            preview_path: run_dir.join("preview.jpg"),
            run_dir,
            record_dir,
            captions: tokio::sync::broadcast::channel(32).0,
        })
    }

    /// Whisper-Modelle liegen neben den Aufnahmen im Datenverzeichnis.
    pub fn models_dir(&self) -> PathBuf {
        self.record_dir.parent().map(|p| p.join("modelle")).unwrap_or_else(|| PathBuf::from("modelle"))
    }

    /// Ordner der HLS-Ausgabe (neben den Aufnahmen)
    pub fn hls_dir(&self) -> PathBuf {
        self.record_dir.parent().map(|p| p.join("hls")).unwrap_or_else(|| PathBuf::from("hls"))
    }

    pub async fn config(&self) -> Config {
        self.state.lock().await.config.clone()
    }

    fn push(st: &mut State, level: &str, text: impl Into<String>) {
        st.events.push_back(Event { at_ms: now_ms(), level: level.into(), text: text.into() });
        while st.events.len() > 60 {
            st.events.pop_front();
        }
    }

    pub async fn log(&self, level: &str, text: impl Into<String>) {
        Self::push(&mut *self.state.lock().await, level, text);
    }

    pub async fn set_config(&self, mut cfg: Config) -> anyhow::Result<()> {
        cfg.normalize();
        let json = serde_json::to_vec_pretty(&cfg)?;
        if let Some(dir) = self.config_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&self.config_path, json)?;
        let mut st = self.state.lock().await;
        let changed = st.config != cfg;
        st.config = cfg;
        if changed && st.desired {
            st.generation += 1;
            Self::push(&mut st, "info", "Einstellungen geändert – Kanal startet neu");
            if let Some(mut stdin) = st.stdin.take() {
                let _ = stdin.write_all(b"{\"cmd\":\"stop\"}\n").await;
            }
        }
        drop(st);
        self.wake.notify_one();
        Ok(())
    }

    /// Konfiguration speichern, ohne den Kanal neu zu starten (Live-Umstellung).
    pub async fn set_config_live(&self, mut cfg: Config) -> anyhow::Result<()> {
        cfg.normalize();
        std::fs::write(&self.config_path, serde_json::to_vec_pretty(&cfg)?)?;
        self.state.lock().await.config = cfg;
        Ok(())
    }

    pub async fn start(&self) {
        let mut st = self.state.lock().await;
        if !st.desired {
            st.desired = true;
            st.restarts = 0;
            st.last_error = None;
            st.ever_connected = false;
            st.generation += 1;
            Self::push(&mut st, "info", "Start");
        }
        drop(st);
        self.wake.notify_one();
    }

    pub async fn stop(&self) {
        let mut st = self.state.lock().await;
        if st.desired {
            st.desired = false;
            st.generation += 1;
            Self::push(&mut st, "info", "Stopp");
            if let Some(mut stdin) = st.stdin.take() {
                let _ = stdin.write_all(b"{\"cmd\":\"stop\"}\n").await;
            }
        }
        drop(st);
        self.wake.notify_one();
    }

    pub async fn command(&self, cmd: &ChildCmd) -> bool {
        let mut st = self.state.lock().await;
        let line = serde_json::to_string(cmd).unwrap() + "\n";
        match st.stdin.as_mut() {
            Some(stdin) => stdin.write_all(line.as_bytes()).await.is_ok(),
            None => false,
        }
    }

    pub async fn snapshot(&self) -> Snapshot {
        let st = self.state.lock().await;
        Snapshot {
            running: st.desired,
            phase: st.phase.clone(),
            restarts: st.restarts,
            status: st.status.clone(),
            last_error: st.last_error.clone(),
            events: st.events.iter().cloned().collect(),
            ever_connected: st.ever_connected,
        }
    }

    fn spawn_child(&self, cfg: &Config) -> std::io::Result<Child> {
        let cfg_file = self.run_dir.join("channel.json");
        let mut cfg = cfg.clone();
        if cfg.encoder.record.dir.trim().is_empty() {
            cfg.encoder.record.dir = self.record_dir.to_string_lossy().to_string();
        }
        if cfg.encoder.hls.dir.trim().is_empty() {
            cfg.encoder.hls.dir = self.hls_dir().to_string_lossy().to_string();
        }
        let asr = &mut cfg.encoder.captions.asr;
        if asr.model_path.trim().is_empty() {
            asr.model_path = self.models_dir().join(format!("ggml-{}.bin", asr.model)).to_string_lossy().to_string();
        }
        std::fs::write(&cfg_file, serde_json::to_vec(&cfg).unwrap())?;
        let _ = std::fs::remove_file(&self.preview_path);
        Command::new(&self.exe)
            .arg("channel")
            .arg("--config")
            .arg(&cfg_file)
            .arg("--preview")
            .arg(&self.preview_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
    }

    /// Endlosschleife des Aufpassers. Einmal als Task starten.
    pub async fn run(self: Arc<Self>) {
        let mut backoff = Duration::from_secs(1);
        loop {
            let (cfg, generation) = {
                let mut st = self.state.lock().await;
                if !st.desired {
                    st.phase = "gestoppt".into();
                    st.status = None;
                    drop(st);
                    self.wake.notified().await;
                    continue;
                }
                st.phase = "startet".into();
                (st.config.clone(), st.generation)
            };

            let mut child = match self.spawn_child(&cfg) {
                Ok(c) => c,
                Err(e) => {
                    let mut st = self.state.lock().await;
                    st.last_error = Some(format!("Kanalprozess startet nicht: {e}"));
                    Self::push(&mut st, "error", format!("Kanalprozess startet nicht: {e}"));
                    drop(st);
                    tokio::time::sleep(backoff).await;
                    continue;
                }
            };
            let started = Instant::now();
            let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
            self.state.lock().await.stdin = child.stdin.take();

            let mut last_line = Instant::now();
            let mut last_bytes = (0u64, Instant::now());
            let mut fatal: Option<String> = None;
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            let exit = loop {
                tokio::select! {
                    line = lines.next_line() => match line {
                        Ok(Some(l)) => {
                            last_line = Instant::now();
                            let Ok(msg) = serde_json::from_str::<ChildMsg>(&l) else { continue };
                            let mut st = self.state.lock().await;
                            match msg {
                                ChildMsg::Status(s) => {
                                    st.phase = "läuft".into();
                                    if s.bytes > last_bytes.0 { last_bytes = (s.bytes, Instant::now()); }
                                    st.ever_connected |= s.srt.connected;
                                    st.status = Some(s);
                                }
                                ChildMsg::Event { level, text } => Self::push(&mut st, &level, text),
                                ChildMsg::Caption { text } => {
                                    let _ = self.captions.send(text);
                                }
                                ChildMsg::Fatal { text } => {
                                    Self::push(&mut st, "error", text.clone());
                                    fatal = Some(text);
                                }
                            }
                        }
                        _ => break child.wait().await.ok(),
                    },
                    status = child.wait() => break status.ok(),
                    _ = tick.tick() => {
                        let st = self.state.lock().await;
                        let gone = st.generation != generation || !st.desired;
                        let encoder = cfg.role == Role::Encoder;
                        drop(st);
                        let reason = if gone && started.elapsed() > Duration::from_secs(3) {
                            Some("Stopp angefordert, Kanal antwortet nicht")
                        } else if last_line.elapsed() > Duration::from_secs(SILENT_SECS) {
                            Some("Kanal meldet sich nicht mehr")
                        } else if encoder && started.elapsed() > Duration::from_secs(STALL_SECS)
                            && last_bytes.1.elapsed() > Duration::from_secs(STALL_SECS) {
                            Some("Ausgang steht – keine Daten mehr")
                        } else { None };
                        if let Some(r) = reason {
                            if !gone { self.log("error", format!("Aufpasser: {r} – starte neu")).await; }
                            let _ = child.start_kill();
                        }
                    }
                }
            };

            let mut st = self.state.lock().await;
            st.stdin = None;
            st.status = None;
            let code = exit.and_then(|s| s.code());
            if st.generation != generation || !st.desired {
                backoff = Duration::from_secs(1);
                continue;
            }
            st.restarts += 1;
            if started.elapsed() > Duration::from_secs(30) {
                backoff = Duration::from_secs(1);
            }
            let why = fatal.unwrap_or_else(|| format!("Kanal beendet (Code {code:?})"));
            st.last_error = Some(why);
            st.phase = format!("Neustart in {} s", backoff.as_secs());
            let n = st.restarts;
            Self::push(&mut st, "warn", format!("Neustart Nr. {n} in {} s", backoff.as_secs()));
            drop(st);
            tokio::select! {
                _ = tokio::time::sleep(backoff) => {},
                _ = self.wake.notified() => {},
            }
            backoff = (backoff * 2).min(Duration::from_secs(10));
        }
    }
}
