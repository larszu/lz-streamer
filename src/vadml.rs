//! Sprache/Musik/Pause (A3) mit gelernten Netzen über ONNX Runtime:
//! - Silero VAD (MIT) je 32 ms: Sprachwahrscheinlichkeit
//! - YAMNet (Apache-2.0, AudioSet-Klassen) je 1-s-Fenster: Log-Mel 96×64 →
//!   521 Klassenwerte und 1024er-Einbettung
//! - Entscheidung: kleiner linearer Kopf (`a3-kopf.json`, gelernt mit
//!   `scripts/vad/kopf-lernen.py`) über Einbettung + Silero + Aktivität;
//!   fehlt er, feste Regeln über AudioSet-Gruppen.
//!
//! Modelle liegen außerhalb des Repos in `<daten>/modelle/a3`
//! (`silero_vad.onnx`, `yamnet.onnx`, optional `a3-kopf.json`), siehe
//! docs/sprache-musik-pause.md. Eingang wie `vad`: 16 kHz mono f32.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;
use rustfft::{FftPlanner, num_complex::Complex};
use serde::{Deserialize, Serialize};

use crate::vad::{Class, RATE};

const WINDOW: usize = RATE; // 1 s
const MEL_WIN: usize = 400; // 25 ms
const MEL_HOP: usize = 160; // 10 ms
const FFT: usize = 512;
const MELS: usize = 64;
const PATCH: usize = 96; // 0,96 s
const SILERO_CHUNK: usize = 512;
const SILERO_CTX: usize = 64;
pub const EMB: usize = 1024;
pub const SCORES: usize = 521;

/// AudioSet-Gruppen (Zeilen in yamnet_class_map.csv)
const G_SPEECH: &[usize] = &[0, 1, 2, 3];
const G_SINGING: &[usize] = &[24, 25, 26, 27, 28, 29, 30, 31, 32];
const G_MUSIC: &[usize] = &[132, 133];
const G_APPLAUSE: &[usize] = &[58, 61, 62, 64];
const G_SILENCE: &[usize] = &[494];

/// Modellordner: `LZS_A3_MODELLE` oder `<Datenverzeichnis>/lz-streamer/modelle/a3`
pub fn default_dir() -> PathBuf {
    std::env::var_os("LZS_A3_MODELLE")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("lz-streamer").join("modelle").join("a3"))
}

/// Sind die Netze vorhanden?
pub fn available(dir: &Path) -> bool {
    dir.join("silero_vad.onnx").is_file() && dir.join("yamnet.onnx").is_file()
}

/// Linearer Kopf: Softmax(W·((x − mean)/std) + b)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Head {
    pub classes: Vec<Class>,
    pub mean: Vec<f32>,
    pub std: Vec<f32>,
    pub w: Vec<Vec<f32>>,
    pub b: Vec<f32>,
}

impl Head {
    fn decide(&self, x: &[f32]) -> Class {
        let z: Vec<f32> = x.iter().zip(&self.mean).zip(&self.std).map(|((v, m), s)| (v - m) / s.max(1e-6)).collect();
        let mut best = (f32::MIN, Class::Sprache);
        for (k, (w, b)) in self.w.iter().zip(&self.b).enumerate() {
            let y = b + w.iter().zip(&z).map(|(a, c)| a * c).sum::<f32>();
            if y > best.0 {
                best = (y, self.classes[k]);
            }
        }
        best.1
    }
}

/// Merkmale eines 1-s-Fensters
#[derive(Debug, Clone, Default, Serialize)]
pub struct Window {
    /// Anteil der 10-ms-Rahmen über −50 dBFS
    pub active: f32,
    /// Silero: mittlere Sprachwahrscheinlichkeit und Anteil > 0,5
    pub speech_p: f32,
    pub speech_frac: f32,
    /// YAMNet-Gruppenwerte (Maximum je Gruppe)
    pub g_speech: f32,
    pub g_singing: f32,
    pub g_music: f32,
    pub g_organ: f32,
    pub g_applause: f32,
    pub g_silence: f32,
    #[serde(skip)]
    pub emb: Vec<f32>,
}

impl Window {
    /// Eingang des Kopfs (Reihenfolge wie in kopf-lernen.py)
    pub fn head_input(&self) -> Vec<f32> {
        let mut x = self.emb.clone();
        x.extend([self.speech_p, self.speech_frac, self.active]);
        x
    }
}

/// Feste Regeln über die Gruppenwerte (ohne gelernten Kopf)
pub fn rules(w: &Window) -> Class {
    if w.active < 0.15 || w.g_silence > 0.5 {
        return Class::Stille;
    }
    if w.g_applause > 0.3 && w.g_applause > w.g_music {
        return Class::Applaus;
    }
    if w.g_music > 0.3 || w.g_singing > 0.3 {
        return if w.g_singing > 0.2 { Class::Gesang } else { Class::Orgel };
    }
    Class::Sprache
}

pub struct Models {
    silero: Session,
    yamnet: Session,
    pub head: Option<Head>,
}

impl Models {
    pub fn load(dir: &Path) -> Result<Self> {
        static ENV: std::sync::OnceLock<ort::environment::Environment> = std::sync::OnceLock::new();
        let env = match ENV.get() {
            Some(e) => e,
            None => {
                let e = ort::init().with_name("lz-streamer").build().map_err(|e| anyhow::anyhow!("ONNX Runtime: {e}"))?;
                ENV.get_or_init(|| e)
            }
        };
        let open = |name: &str| -> Result<Session> {
            Session::builder(env)
                .map_err(|e| anyhow::anyhow!("{e}"))?
                .with_optimization_level(GraphOptimizationLevel::Level3)
                .map_err(|e| anyhow::anyhow!("{e}"))?
                .with_intra_threads(1)
                .map_err(|e| anyhow::anyhow!("{e}"))?
                .commit_from_file(dir.join(name))
                .map_err(|e| anyhow::anyhow!("{name}: {e}"))
        };
        let head = match std::fs::read(dir.join("a3-kopf.json")) {
            Ok(b) => Some(serde_json::from_slice(&b).context("a3-kopf.json")?),
            Err(_) => None,
        };
        Ok(Self { silero: open("silero_vad.onnx")?, yamnet: open("yamnet.onnx")?, head })
    }
}

pub struct MlClassifier {
    m: Models,
    use_head: bool,
    fft: Arc<dyn rustfft::Fft<f32>>,
    hann: Vec<f32>,
    mel: Vec<Vec<f32>>,
    /// Samples für die Mel-Rahmen (Überlappung)
    mel_buf: Vec<f32>,
    frames: Vec<[f32; MELS]>,
    sil_buf: Vec<f32>,
    sil_ctx: Vec<f32>,
    sil_state: Vec<f32>,
    sil_p: Vec<f32>,
    rms_buf: Vec<f32>,
    rms_active: usize,
    rms_n: usize,
    in_window: usize,
}

fn hz2mel(f: f64) -> f64 {
    1127.0 * (1.0 + f / 700.0).ln()
}

/// Mel-Matrix wie tf.signal.linear_to_mel_weight_matrix (HTK, 125–7500 Hz, ohne DC)
fn mel_matrix() -> Vec<Vec<f32>> {
    let bins = FFT / 2 + 1;
    let edges: Vec<f64> = (0..MELS + 2).map(|i| hz2mel(125.0) + (hz2mel(7500.0) - hz2mel(125.0)) * i as f64 / (MELS + 1) as f64).collect();
    let mut m = vec![vec![0.0f32; MELS]; bins];
    for (k, row) in m.iter_mut().enumerate().skip(1) {
        let sm = hz2mel(8000.0 * k as f64 / (bins - 1) as f64);
        for (j, v) in row.iter_mut().enumerate() {
            let lo = (sm - edges[j]) / (edges[j + 1] - edges[j]);
            let up = (edges[j + 2] - sm) / (edges[j + 2] - edges[j + 1]);
            *v = lo.min(up).max(0.0) as f32;
        }
    }
    m
}

impl MlClassifier {
    pub fn new(m: Models) -> Self {
        let use_head = m.head.is_some();
        let hann = (0..MEL_WIN).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / MEL_WIN as f32).cos()).collect();
        Self {
            m,
            use_head,
            fft: FftPlanner::new().plan_fft_forward(FFT),
            hann,
            mel: mel_matrix(),
            mel_buf: Vec::new(),
            frames: Vec::new(),
            sil_buf: Vec::new(),
            sil_ctx: vec![0.0; SILERO_CTX],
            sil_state: vec![0.0; 2 * 128],
            sil_p: Vec::new(),
            rms_buf: Vec::new(),
            rms_active: 0,
            rms_n: 0,
            in_window: 0,
        }
    }

    pub fn load(dir: &Path) -> Result<Self> {
        Ok(Self::new(Models::load(dir)?))
    }

    /// Zustand (Puffer, Silero) zurücksetzen, z. B. je Datei
    pub fn reset(&mut self) {
        self.mel_buf.clear();
        self.frames.clear();
        self.sil_buf.clear();
        self.sil_ctx.fill(0.0);
        self.sil_state.fill(0.0);
        self.sil_p.clear();
        self.rms_buf.clear();
        self.rms_active = 0;
        self.rms_n = 0;
        self.in_window = 0;
    }

    /// Kopf ausschalten (Regeln prüfen, Merkmale für das Lernen ausgeben)
    pub fn set_use_head(&mut self, on: bool) {
        self.use_head = on && self.m.head.is_some();
    }

    pub fn uses_head(&self) -> bool {
        self.use_head
    }

    fn mel_frame(&self, x: &[f32]) -> [f32; MELS] {
        let mut c: Vec<Complex<f32>> = vec![Complex::new(0.0, 0.0); FFT];
        for (i, (v, w)) in x.iter().zip(&self.hann).enumerate() {
            c[i].re = v * w;
        }
        self.fft.process(&mut c);
        let mut out = [0.0f32; MELS];
        for (k, z) in c[..FFT / 2 + 1].iter().enumerate().skip(1) {
            let mag = z.norm();
            for (o, w) in out.iter_mut().zip(&self.mel[k]) {
                *o += mag * w;
            }
        }
        for o in out.iter_mut() {
            *o = (*o + 0.001).ln();
        }
        out
    }

    fn silero(&mut self, chunk: &[f32]) -> Result<f32> {
        let mut input = self.sil_ctx.clone();
        input.extend_from_slice(chunk);
        self.sil_ctx.copy_from_slice(&chunk[SILERO_CHUNK - SILERO_CTX..]);
        let out = self.m.silero.run(ort::inputs![
            "input" => Tensor::from_array(([1usize, SILERO_CHUNK + SILERO_CTX], input))?,
            "state" => Tensor::from_array(([2usize, 1, 128], self.sil_state.clone()))?,
            "sr" => Tensor::from_array(((), vec![RATE as i64]))?,
        ])?;
        let (_, p) = out["output"].try_extract_tensor::<f32>()?;
        let (_, st) = out["stateN"].try_extract_tensor::<f32>()?;
        let p = p[0];
        self.sil_state.copy_from_slice(st);
        Ok(p)
    }

    fn yamnet(&mut self) -> Result<(Vec<f32>, Vec<f32>)> {
        let n = self.frames.len();
        let mut patch = Vec::with_capacity(PATCH * MELS);
        for f in &self.frames[n.saturating_sub(PATCH)..] {
            patch.extend_from_slice(f);
        }
        // zu Beginn: vorne mit dem ersten Rahmen auffüllen
        while patch.len() < PATCH * MELS {
            let first: Vec<f32> = patch[..MELS].to_vec();
            patch.splice(0..0, first);
        }
        let out = self.m.yamnet.run(ort::inputs!["patch" => Tensor::from_array(([1usize, PATCH, MELS], patch))?])?;
        let (_, s) = out["scores"].try_extract_tensor::<f32>()?;
        let (_, e) = out["embedding"].try_extract_tensor::<f32>()?;
        Ok((s.to_vec(), e.to_vec()))
    }

    fn finish_window(&mut self) -> Result<(Class, Window)> {
        let (scores, emb) = self.yamnet()?;
        let g = |idx: &[usize]| idx.iter().map(|i| scores[*i]).fold(0.0f32, f32::max);
        let np = self.sil_p.len().max(1) as f32;
        let w = Window {
            active: self.rms_active as f32 / self.rms_n.max(1) as f32,
            speech_p: self.sil_p.iter().sum::<f32>() / np,
            speech_frac: self.sil_p.iter().filter(|p| **p > 0.5).count() as f32 / np,
            g_speech: g(G_SPEECH),
            g_singing: g(G_SINGING),
            g_music: g(G_MUSIC),
            g_organ: g(&[150, 151, 152]),
            g_applause: g(G_APPLAUSE),
            g_silence: g(G_SILENCE),
            emb,
        };
        self.sil_p.clear();
        self.rms_active = 0;
        self.rms_n = 0;
        // Stille ist eindeutig über den Pegel; sonst Kopf bzw. Regeln
        let class = if w.active < 0.15 {
            Class::Stille
        } else if self.use_head {
            self.m.head.as_ref().map(|h| h.decide(&w.head_input())).unwrap_or_else(|| rules(&w))
        } else {
            rules(&w)
        };
        // Mel-Rahmen nur so viele behalten, wie der nächste Ausschnitt braucht
        let n = self.frames.len();
        if n > PATCH {
            self.frames.drain(..n - PATCH);
        }
        Ok((class, w))
    }

    /// Samples anhängen; liefert für jedes volle 1-s-Fenster Klasse und Merkmale.
    pub fn push(&mut self, samples: &[f32]) -> Result<Vec<(Class, Window)>> {
        let mut out = Vec::new();
        let mut rest = samples;
        while !rest.is_empty() {
            let take = rest.len().min(WINDOW - self.in_window);
            let part = &rest[..take];
            rest = &rest[take..];
            self.in_window += take;
            // Pegel je 10 ms
            self.rms_buf.extend_from_slice(part);
            while self.rms_buf.len() >= MEL_HOP {
                let e = self.rms_buf[..MEL_HOP].iter().map(|v| v * v).sum::<f32>() / MEL_HOP as f32;
                self.rms_n += 1;
                if 10.0 * e.max(1e-12).log10() > -50.0 {
                    self.rms_active += 1;
                }
                self.rms_buf.drain(..MEL_HOP);
            }
            // Mel-Rahmen
            self.mel_buf.extend_from_slice(part);
            while self.mel_buf.len() >= MEL_WIN {
                let f = self.mel_frame(&self.mel_buf[..MEL_WIN]);
                self.frames.push(f);
                self.mel_buf.drain(..MEL_HOP);
            }
            // Silero je 32 ms
            self.sil_buf.extend_from_slice(part);
            while self.sil_buf.len() >= SILERO_CHUNK {
                let chunk: Vec<f32> = self.sil_buf.drain(..SILERO_CHUNK).collect();
                let p = self.silero(&chunk)?;
                self.sil_p.push(p);
            }
            if self.in_window == WINDOW {
                self.in_window = 0;
                out.push(self.finish_window()?);
            }
        }
        Ok(out)
    }
}

/// Erkennung im eigenen Thread, damit die Netze den Audiotakt nicht aufhalten.
/// `push` blockiert nie (bei Rückstau gehen Samples verloren, die Klasse bleibt
/// dann einen Takt länger stehen); `latest` liefert die zuletzt erkannte Klasse.
pub struct Worker {
    tx: std::sync::mpsc::SyncSender<Vec<f32>>,
    rx: std::sync::mpsc::Receiver<Class>,
    last: Option<Class>,
}

impl Worker {
    pub fn start(mut c: MlClassifier) -> Self {
        let (tx, in_rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(256);
        let (out_tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("a3-netze".into())
            .spawn(move || {
                while let Ok(s) = in_rx.recv() {
                    match c.push(&s) {
                        Ok(r) => {
                            for (k, _) in r {
                                if out_tx.send(k).is_err() {
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("A3-Netze: {e}");
                            return;
                        }
                    }
                }
            })
            .expect("Thread a3-netze");
        Self { tx, rx, last: None }
    }

    pub fn push(&mut self, s: Vec<f32>) {
        let _ = self.tx.try_send(s);
    }

    pub fn latest(&mut self) -> Option<Class> {
        while let Ok(k) = self.rx.try_recv() {
            self.last = Some(k);
        }
        self.last
    }
}

/// Merkmale aller Fenster eines Korpus `<ordner>/<klasse>/*.wav`, für Messung und Lernen
pub struct Labeled {
    pub truth: Class,
    pub file: String,
    pub class: Class,
    pub w: Window,
}

pub fn run_corpus(dir: &Path, models: &Path, use_head: bool) -> Result<(Vec<Labeled>, f64)> {
    let mut c = MlClassifier::load(models)?;
    c.set_use_head(use_head);
    let mut out = Vec::new();
    let mut samples = 0usize;
    let mut elapsed = std::time::Duration::ZERO;
    for (name, truth) in crate::vad::CLASS_DIRS {
        let Ok(rd) = std::fs::read_dir(dir.join(name)) else { continue };
        let mut files: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "wav")).collect();
        files.sort();
        for p in files {
            let x = crate::vad::read_wav(&p)?;
            c.reset();
            let t = std::time::Instant::now();
            let r = c.push(&x)?;
            elapsed += t.elapsed();
            samples += x.len();
            let file = p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
            for (k, w) in r {
                out.push(Labeled { truth: *truth, file: file.clone(), class: k, w });
            }
        }
    }
    Ok((out, elapsed.as_secs_f64() * 1e6 / (samples as f64 / 160.0).max(1.0)))
}
