//! Plugin-Host (P1/P2): VST3-Plugins und am Mac Audio Units laufen in einem eigenen Hilfsprozess
//! `lz-streamer plugin-host`. Das Element `lzplugin` schickt jeden Tonblock
//! über eine Pipe hin und zurück. Antwortet der Hilfsprozess nicht rechtzeitig
//! oder stirbt er, geht der Ton unverändert durch (Bypass), und das Element
//! startet den Hilfsprozess im Hintergrund neu. Der Kanal läuft weiter.
//!
//! Protokoll (binär, little endian):
//! - Hilfsprozess → Element beim Start: `u32 latenz_samples`
//! - Element → Hilfsprozess je Block: `u32 frames`, dann `frames × kanäle × f32` (interleaved)
//! - Hilfsprozess → Element: `u32 frames`, dann die verarbeiteten Samples

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{LazyLock, Mutex, Once};

use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::glib;
use gstreamer::prelude::*;
use gstreamer::subclass::prelude::*;
use gstreamer_base as gst_base;
use gstreamer_base::subclass::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct PluginConfig {
    pub enabled: bool,
    /// Pfad zum .vst3-Bündel oder `au:…`
    pub path: String,
    /// Ton unbearbeitet durchlassen (Plugin bleibt geladen)
    pub bypass: bool,
}

pub const RATE: f64 = 48_000.0;

/// Installierte VST3-Plugins in den üblichen Ordnern (macOS, Linux, Windows).
pub fn installed() -> Vec<(String, String)> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(h) = dirs::home_dir() {
        dirs.push(h.join("Library/Audio/Plug-Ins/VST3"));
        dirs.push(h.join(".vst3"));
    }
    dirs.extend(["/Library/Audio/Plug-Ins/VST3", "/usr/lib/vst3", "/usr/local/lib/vst3", "C:\\Program Files\\Common Files\\VST3"].map(PathBuf::from));
    let mut out: Vec<(String, String)> = dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|rd| rd.flatten())
        .filter(|e| e.path().extension().is_some_and(|x| x == "vst3"))
        .map(|e| {
            let p = e.path();
            (p.file_stem().unwrap_or_default().to_string_lossy().into_owned(), p.to_string_lossy().into_owned())
        })
        .collect();
    out.sort();
    out.dedup_by(|a, b| a.1 == b.1);
    out
}

/// Anzeigename aus dem Pfad
pub fn display_name(path: &str) -> String {
    std::path::Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string())
}
const MAX_BLOCK: usize = 8192;

// ---------------------------------------------------------------- Hilfsprozess

/// Ein geladenes Plugin im Hilfsprozess: verarbeitet interleaved in place.
type Engine = Box<dyn FnMut(&mut [f32]) -> bool>;

/// Plugin nach Kennung laden: `au:…` (Mac) oder Pfad zu einem `.vst3`-Bündel.
fn load_engine(id: &str, channels: usize) -> anyhow::Result<(u32, Engine)> {
    #[cfg(target_os = "macos")]
    if id.starts_with("au:") {
        let mut p = crate::au::AuPlugin::load(id, RATE, channels, MAX_BLOCK)?;
        let lat = p.latency_samples;
        return Ok((lat, Box::new(move |buf: &mut [f32]| p.process(buf).is_ok())));
    }
    let mut host = vst3_host::Vst3Host::builder()
        .sample_rate(RATE)
        .block_size(MAX_BLOCK)
        .input_channels(channels)
        .output_channels(channels)
        .build()?;
    let mut plugin = host.load_plugin(id)?;
    plugin.start_processing()?;
    let lat = plugin.latency_samples();
    Ok((
        lat,
        Box::new(move |buf: &mut [f32]| {
            let _keep = &host; // Host lebt so lange wie das Plugin
            let frames = buf.len() / channels;
            let mut bufs = vst3_host::AudioBuffers::new(channels, channels, frames, RATE);
            for (i, v) in buf.iter().enumerate() {
                bufs.inputs[i % channels][i / channels] = *v;
            }
            if plugin.process_audio(&mut bufs).is_err() {
                return false;
            }
            for (i, v) in buf.iter_mut().enumerate() {
                *v = bufs.outputs[i % channels][i / channels];
            }
            true
        }),
    ))
}

/// Prüflauf für den Scan: laden, eine halbe Sekunde Rauschen verarbeiten,
/// `ok <latenz>` ausgeben. Stürzt das Plugin dabei ab, endet nur dieser Prozess.
pub fn probe(id: &str, channels: usize) -> anyhow::Result<()> {
    let (lat, mut eng) = load_engine(id, channels)?;
    let mut s = 1u32;
    for _ in 0..50 {
        let mut buf: Vec<f32> = (0..480 * channels)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                (s as f32 / u32::MAX as f32 - 0.5) * 0.2
            })
            .collect();
        if !eng(&mut buf) {
            anyhow::bail!("verarbeitet nicht");
        }
    }
    println!("ok {lat}");
    Ok(())
}

/// Hauptschleife von `lz-streamer plugin-host`.
pub fn run_host(id: &str, channels: usize) -> anyhow::Result<()> {
    let (latency, mut eng) = load_engine(id, channels)?;
    let mut out = std::io::stdout().lock();
    let mut inp = std::io::stdin().lock();
    out.write_all(&latency.to_le_bytes())?;
    out.flush()?;
    let mut head = [0u8; 4];
    let mut raw = Vec::new();
    let mut samples: Vec<f32> = Vec::new();
    loop {
        if inp.read_exact(&mut head).is_err() {
            return Ok(()); // Element weg
        }
        let frames = u32::from_le_bytes(head) as usize;
        raw.resize(frames * channels * 4, 0);
        inp.read_exact(&mut raw)?;
        samples.clear();
        samples.extend(raw.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])));
        let dry = samples.clone();
        if !eng(&mut samples) {
            // durchreichen statt abbrechen
            samples = dry;
        }
        for (i, v) in samples.iter().enumerate() {
            raw[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        out.write_all(&head)?;
        out.write_all(&raw)?;
        out.flush()?;
    }
}

// ---------------------------------------------------------------- Scan mit Zwischenspeicher und Sperrliste

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScannedPlugin {
    /// Pfad (VST3) oder `au:typ:subtyp:hersteller`
    pub id: String,
    pub name: String,
    /// vst3 oder au
    pub format: String,
    pub latency_samples: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BlockedPlugin {
    pub id: String,
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ScanResult {
    pub plugins: Vec<ScannedPlugin>,
    pub blocked: Vec<BlockedPlugin>,
}

/// Kandidaten: VST3-Bündel in den üblichen Ordnern und am Mac die Effekt-AUs.
pub fn candidates() -> Vec<(String, String, &'static str)> {
    let mut c: Vec<(String, String, &'static str)> = installed().into_iter().map(|(name, path)| (path, name, "vst3")).collect();
    #[cfg(target_os = "macos")]
    c.extend(crate::au::list_effects().into_iter().map(|(id, name)| (id, name, "au")));
    c
}

/// Ein Plugin in einem eigenen Prozess prüfen (Zeitlimit 15 s).
fn probe_in_child(id: &str) -> Result<u32, String> {
    let exe = std::env::var_os("LZS_PLUGIN_HOST_EXE").map(PathBuf::from).or_else(|| std::env::current_exe().ok()).ok_or("kein Programmpfad")?;
    let mut child = Command::new(exe)
        .args(["plugin-host", "--probe", "--plugin", id, "--channels", "2"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                if let Some(mut o) = child.stdout.take() {
                    let _ = o.read_to_string(&mut out);
                }
                return match out.trim().strip_prefix("ok ") {
                    Some(l) if status.success() => Ok(l.parse().unwrap_or(0)),
                    _ => Err(match status.code() {
                        Some(c) => format!("Prüflauf endete mit Fehler {c}"),
                        None => "abgestürzt".into(),
                    }),
                };
            }
            Ok(None) if Instant::now() > until => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("hängt (15 s)".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Plugins prüfen. Bereits geprüfte und gesperrte werden aus `cache` übernommen,
/// außer bei `force`. Jeder Prüflauf läuft in einem eigenen Prozess, damit ein
/// abstürzendes Plugin nur auf der Sperrliste landet.
pub fn scan(cands: &[(String, String, &'static str)], cache: &ScanResult, force: bool) -> ScanResult {
    let mut out = ScanResult::default();
    for (id, name, format) in cands {
        if !force {
            if let Some(p) = cache.plugins.iter().find(|p| &p.id == id) {
                out.plugins.push(p.clone());
                continue;
            }
            if let Some(b) = cache.blocked.iter().find(|b| &b.id == id) {
                out.blocked.push(b.clone());
                continue;
            }
        }
        match probe_in_child(id) {
            Ok(latency_samples) => out.plugins.push(ScannedPlugin { id: id.clone(), name: name.clone(), format: format.to_string(), latency_samples }),
            Err(reason) => out.blocked.push(BlockedPlugin { id: id.clone(), name: name.clone(), reason }),
        }
    }
    out
}

/// Scan mit Zwischenspeicher in `file` (JSON).
pub fn scan_cached(file: &std::path::Path, force: bool) -> ScanResult {
    let cache: ScanResult = std::fs::read(file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let r = scan(&candidates(), &cache, force);
    if let Ok(j) = serde_json::to_vec_pretty(&r) {
        let _ = std::fs::write(file, j);
    }
    r
}

// ---------------------------------------------------------------- Verbindung zum Hilfsprozess

pub struct Helper {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Vec<u8>>,
    latency: u32,
    /// Antworten auf Blöcke, die schon im Bypass liefen (werden verworfen)
    stale: usize,
}

/// so viele verspätete Blöcke (≈ 0,5 s bei 10 ms) duldet das Element, bevor es neu startet
const MAX_STALE: usize = 50;

impl Helper {
    fn spawn(path: &str, channels: usize) -> Option<Self> {
        let exe = std::env::var_os("LZS_PLUGIN_HOST_EXE").map(PathBuf::from).or_else(|| std::env::current_exe().ok())?;
        let mut child = Command::new(exe)
            .args(["plugin-host", "--plugin", path, "--channels", &channels.to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let stdin = child.stdin.take()?;
        let mut stdout = child.stdout.take()?;
        let mut lat = [0u8; 4];
        // Laden kann bei großen Plugins dauern
        let (ltx, lrx) = channel();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            if stdout.read_exact(&mut lat).is_err() {
                return;
            }
            let _ = ltx.send(u32::from_le_bytes(lat));
            loop {
                let mut head = [0u8; 4];
                if stdout.read_exact(&mut head).is_err() {
                    return;
                }
                let mut buf = vec![0u8; u32::from_le_bytes(head) as usize * channels * 4];
                if stdout.read_exact(&mut buf).is_err() || tx.send(buf).is_err() {
                    return;
                }
            }
        });
        match lrx.recv_timeout(Duration::from_secs(20)) {
            Ok(latency) => Some(Self { child, stdin, rx, latency, stale: 0 }),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                None
            }
        }
    }
}

// ---------------------------------------------------------------- Element

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct State {
        pub helper: Option<Helper>,
        pub channels: usize,
        pub retry_at: Option<Instant>,
        pub spawning: Option<Receiver<Option<Helper>>>,
        pub bypassed_blocks: u64,
        pub processed_blocks: u64,
        pub restarts: u32,
    }

    #[derive(Default)]
    pub struct LzPlugin {
        pub path: Mutex<String>,
        pub bypass: std::sync::atomic::AtomicBool,
        pub state: Mutex<State>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LzPlugin {
        const NAME: &'static str = "LzStreamerPlugin";
        type Type = super::LzPlugin;
        type ParentType = gst_base::BaseTransform;
    }

    impl ObjectImpl for LzPlugin {}
    impl GstObjectImpl for LzPlugin {}

    impl ElementImpl for LzPlugin {
        fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
            static META: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
                gst::subclass::ElementMetadata::new(
                    "LZ plugin host",
                    "Filter/Effect/Audio",
                    "Runs a VST3 plugin in a helper process, bypass on crash",
                    "Lars Zumpe",
                )
            });
            Some(&*META)
        }
        fn pad_templates() -> &'static [gst::PadTemplate] {
            static T: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
                let caps = gst::Caps::builder("audio/x-raw")
                    .field("format", "F32LE")
                    .field("rate", RATE as i32)
                    .field("channels", gst::IntRange::new(1, 2))
                    .field("layout", "interleaved")
                    .build();
                vec![
                    gst::PadTemplate::new("src", gst::PadDirection::Src, gst::PadPresence::Always, &caps).unwrap(),
                    gst::PadTemplate::new("sink", gst::PadDirection::Sink, gst::PadPresence::Always, &caps).unwrap(),
                ]
            });
            T.as_ref()
        }
    }

    impl BaseTransformImpl for LzPlugin {
        const MODE: gst_base::subclass::BaseTransformMode = gst_base::subclass::BaseTransformMode::AlwaysInPlace;
        const PASSTHROUGH_ON_SAME_CAPS: bool = false;
        const TRANSFORM_IP_ON_PASSTHROUGH: bool = false;

        fn set_caps(&self, incaps: &gst::Caps, _o: &gst::Caps) -> Result<(), gst::LoggableError> {
            let ch = incaps.structure(0).and_then(|s| s.get::<i32>("channels").ok()).unwrap_or(2).max(1) as usize;
            let mut st = self.state.lock().unwrap();
            // Laden läuft im Hintergrund; bis dahin geht der Ton unbearbeitet durch
            st.channels = ch;
            st.retry_at = None;
            Ok(())
        }

        fn stop(&self) -> Result<(), gst::ErrorMessage> {
            if let Some(mut h) = self.state.lock().unwrap().helper.take() {
                let _ = h.child.kill();
            }
            Ok(())
        }

        fn query(&self, direction: gst::PadDirection, query: &mut gst::QueryRef) -> bool {
            if let gst::QueryViewMut::Latency(q) = query.view_mut() {
                let ok = BaseTransformImplExt::parent_query(self, direction, q.query_mut());
                if ok {
                    let lat = self.state.lock().unwrap().helper.as_ref().map(|h| h.latency).unwrap_or(0);
                    let (live, min, max) = q.result();
                    let l = gst::ClockTime::from_nseconds((lat as f64 / RATE * 1e9) as u64);
                    q.set(live, min + l, max.map(|m| m + l));
                }
                return ok;
            }
            BaseTransformImplExt::parent_query(self, direction, query)
        }

        fn transform_ip(&self, buf: &mut gst::BufferRef) -> Result<gst::FlowSuccess, gst::FlowError> {
            let mut st = self.state.lock().unwrap();
            if self.bypass.load(std::sync::atomic::Ordering::Relaxed) {
                st.bypassed_blocks += 1;
                return Ok(gst::FlowSuccess::Ok);
            }
            if st.helper.is_none() {
                // Aufpasser: im Hintergrund neu starten, bis dahin Bypass
                if let Some(rx) = &st.spawning {
                    match rx.try_recv() {
                        Ok(h) => {
                            st.spawning = None;
                            let latency = h.as_ref().map(|h| h.latency).unwrap_or(0);
                            st.helper = h;
                            if latency > 0 {
                                // Pipeline soll die Latenz neu abfragen
                                let _ = self.obj().post_message(gst::message::Latency::builder().src(&*self.obj()).build());
                            }
                            st.retry_at = Some(Instant::now() + Duration::from_secs(1));
                        }
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => st.spawning = None,
                        Err(_) => {}
                    }
                } else if st.retry_at.is_none_or(|t| Instant::now() >= t) {
                    let (tx, rx) = channel();
                    let (path, ch) = (self.path.lock().unwrap().clone(), st.channels);
                    std::thread::spawn(move || {
                        let _ = tx.send(Helper::spawn(&path, ch));
                    });
                    st.spawning = Some(rx);
                    if st.retry_at.is_some() {
                        st.restarts += 1;
                    }
                }
                if st.helper.is_none() {
                    st.bypassed_blocks += 1;
                    return Ok(gst::FlowSuccess::Ok);
                }
            }
            let mut map = buf.map_writable().map_err(|_| gst::FlowError::Error)?;
            let bytes = map.as_mut_slice();
            let ch = st.channels;
            let frames = bytes.len() / 4 / ch;
            // Ergebnis: Some(true) verarbeitet, Some(false) verspätet (Bypass), None tot
            let ok = {
                let h = st.helper.as_mut().unwrap();
                let sent = h.stdin.write_all(&(frames as u32).to_le_bytes()).and_then(|_| h.stdin.write_all(bytes)).and_then(|_| h.stdin.flush());
                // Zeitbudget: halbe Blockdauer, mindestens 5 ms
                let deadline = Instant::now() + Duration::from_secs_f64((frames as f64 / RATE * 0.5).max(0.005));
                if sent.is_err() {
                    None
                } else {
                    loop {
                        match h.rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                            // verspätete Antwort auf einen früheren Block: verwerfen
                            Ok(_) if h.stale > 0 => h.stale -= 1,
                            Ok(out) if out.len() == bytes.len() => {
                                bytes.copy_from_slice(&out);
                                break Some(true);
                            }
                            Ok(_) => break None,
                            Err(RecvTimeoutError::Timeout) => {
                                // Antwort kommt später; einmal langsam ist kein Absturz
                                h.stale += 1;
                                let exited = h.child.try_wait().ok().flatten().is_some();
                                break if exited || h.stale > MAX_STALE { None } else { Some(false) };
                            }
                            Err(RecvTimeoutError::Disconnected) => break None,
                        }
                    }
                }
            };
            let ok = match ok {
                Some(true) => true,
                Some(false) => {
                    st.bypassed_blocks += 1;
                    return Ok(gst::FlowSuccess::Ok);
                }
                None => false,
            };
            if ok {
                st.processed_blocks += 1;
            } else {
                // Hilfsprozess tot oder dauerhaft zu langsam: Ton unverändert durchlassen, neu starten
                st.bypassed_blocks += 1;
                if let Some(mut h) = st.helper.take() {
                    let _ = h.child.kill();
                    let _ = h.child.wait();
                }
                st.retry_at = Some(Instant::now() + Duration::from_millis(200));
            }
            Ok(gst::FlowSuccess::Ok)
        }
    }
}

glib::wrapper! {
    pub struct LzPlugin(ObjectSubclass<imp::LzPlugin>) @extends gst_base::BaseTransform, gst::Element, gst::Object;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PluginStatus {
    pub name: String,
    /// von Hand auf Bypass geschaltet
    #[serde(default)]
    pub bypass: bool,
    pub running: bool,
    pub latency_samples: u32,
    pub processed_blocks: u64,
    pub bypassed_blocks: u64,
    pub restarts: u32,
    pub pid: Option<u32>,
}

impl LzPlugin {
    pub fn set_bypass(&self, b: bool) {
        self.imp().bypass.store(b, std::sync::atomic::Ordering::Relaxed);
    }
    pub fn set_path(&self, p: &str) {
        *self.imp().path.lock().unwrap() = p.to_string();
    }
    pub fn status(&self) -> PluginStatus {
        let st = self.imp().state.lock().unwrap();
        PluginStatus {
            name: display_name(&self.imp().path.lock().unwrap()),
            bypass: self.imp().bypass.load(std::sync::atomic::Ordering::Relaxed),
            running: st.helper.is_some(),
            latency_samples: st.helper.as_ref().map(|h| h.latency).unwrap_or(0),
            processed_blocks: st.processed_blocks,
            bypassed_blocks: st.bypassed_blocks,
            restarts: st.restarts,
            pid: st.helper.as_ref().map(|h| h.child.id()),
        }
    }
}

pub fn register() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = gst::Element::register(None, "lzplugin", gst::Rank::NONE, LzPlugin::static_type());
    });
}
