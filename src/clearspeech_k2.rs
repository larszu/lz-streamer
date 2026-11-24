//! Klare Sprache K2: Qualitätsmodus mit fester Verzögerung.
//!
//! HTDemucs läuft im Hilfsprozess `scripts/k2/trenner.py` (Python, PyTorch,
//! am Mac über MPS) und trennt den Ton segmentweise mit Vor- und Nachlauf. Die
//! Spur kommt hier mit genau `delay` Samples Verzögerung heraus, unabhängig davon,
//! wann der Hilfsprozess liefert:
//! - Ausgang zur Zeit n ist Eingang n − delay.
//! - Liegt die getrennte Stimme für diesen Index vor, wird gemischt, sonst geht
//!   das Original durch (Anlauf, Hilfsprozess zu langsam oder abgestürzt).
//!
//! Damit Bild und alle Spuren gleich verzögert sind, meldet `lzclearspeech` die
//! Verzögerung als Latenz und setzt die Zeitstempel zurück; die Warteschlangen
//! vor Mux und Aufnahme werden um diese Zeit vergrößert (`pipeline`).

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, channel, sync_channel};
use std::time::{Duration, Instant};

const SCRIPT: &str = include_str!("../scripts/k2/trenner.py");
/// höchstens so viele Blöcke auf dem Weg zum Hilfsprozess (≈ 30 s), sonst Neustart
const MAX_QUEUED: usize = 3000;

/// Python mit demucs: `LZS_K2_PYTHON`, sonst `python3`
pub fn python(cfg: &str) -> String {
    if !cfg.is_empty() {
        return cfg.to_string();
    }
    std::env::var("LZS_K2_PYTHON").unwrap_or_else(|_| "python3".into())
}

/// Ablage der torch-hub-Gewichte
pub fn models_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("lz-streamer").join("modelle").join("k2")
}

fn script_path() -> std::io::Result<PathBuf> {
    let p = std::env::temp_dir().join(format!("lz-streamer-k2-{}.py", env!("CARGO_PKG_VERSION")));
    if std::fs::read_to_string(&p).ok().as_deref() != Some(SCRIPT) {
        std::fs::write(&p, SCRIPT)?;
    }
    Ok(p)
}

struct Helper {
    child: Child,
    tx: SyncSender<Vec<f32>>,
    rx: Receiver<Msg>,
    /// absoluter Eingangs-Index, ab dem dieser Hilfsprozess zählt
    origin: u64,
}

enum Msg {
    Ready,
    Segment(u64, Vec<f32>),
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct K2Stats {
    pub ready: bool,
    /// Ausgangs-Frames mit getrennter Stimme bzw. mit Original-Ersatz (seit Bereitschaft)
    pub separated: u64,
    pub fallback: u64,
    pub restarts: u32,
}

pub struct K2 {
    ch: usize,
    delay: usize,
    python: String,
    model: String,
    segment_s: f64,
    context_s: f64,
    helper: Option<Helper>,
    next_start: Instant,
    /// Original, um `delay` Frames verzögert (interleaved)
    hist: VecDeque<f32>,
    /// getrennte Stimme ab Frame `sep_base` (NaN = fehlt)
    sep: VecDeque<f32>,
    sep_base: u64,
    n_in: u64,
    pub stats: K2Stats,
}

impl K2 {
    pub fn new(ch: usize, delay_frames: usize, python: String, model: String) -> Self {
        // Segment + Vorlauf + Rechenzeit müssen in die Verzögerung passen
        let d = delay_frames as f64 / 48_000.0;
        let context_s = 1.0f64.min(d / 6.0);
        let segment_s = (d - context_s - 1.0).clamp(1.0, 8.0);
        let mut k = Self {
            ch,
            delay: delay_frames,
            python,
            model,
            segment_s,
            context_s,
            helper: None,
            next_start: Instant::now(),
            hist: VecDeque::from(vec![0.0; delay_frames * ch]),
            sep: VecDeque::new(),
            sep_base: 0,
            n_in: 0,
            stats: K2Stats::default(),
        };
        k.start();
        k
    }

    pub fn delay_frames(&self) -> usize {
        self.delay
    }

    fn start(&mut self) {
        self.next_start = Instant::now() + Duration::from_secs(5);
        match self.spawn() {
            Ok(h) => self.helper = Some(h),
            Err(e) => tracing::warn!("K2-Hilfsprozess startet nicht: {e}"),
        }
    }

    fn spawn(&self) -> anyhow::Result<Helper> {
        let mut child = Command::new(&self.python)
            .arg(script_path()?)
            .args([self.ch.to_string(), format!("{:.3}", self.segment_s), format!("{:.3}", self.context_s), self.model.clone()])
            .env("TORCH_HOME", models_dir())
            .env("HF_HOME", models_dir())
            .env("PYTORCH_ENABLE_MPS_FALLBACK", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut stdin = child.stdin.take().ok_or_else(|| anyhow::anyhow!("stdin"))?;
        let mut stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("stdout"))?;
        let (tx, wrx) = sync_channel::<Vec<f32>>(MAX_QUEUED);
        let ch = self.ch;
        std::thread::Builder::new().name("k2-schreiben".into()).spawn(move || {
            while let Ok(b) = wrx.recv() {
                let mut out = Vec::with_capacity(4 + b.len() * 4);
                out.extend_from_slice(&((b.len() / ch) as u32).to_le_bytes());
                for v in &b {
                    out.extend_from_slice(&v.to_le_bytes());
                }
                if stdin.write_all(&out).is_err() {
                    return;
                }
            }
        })?;
        let (mtx, rx) = channel();
        std::thread::Builder::new().name("k2-lesen".into()).spawn(move || {
            let mut hdr = [0u8; 12];
            if stdout.read_exact(&mut hdr).is_err() || &hdr[..4] != b"K2OK" {
                return;
            }
            let _ = mtx.send(Msg::Ready);
            loop {
                let mut h = [0u8; 12];
                if stdout.read_exact(&mut h).is_err() {
                    return;
                }
                let start = u64::from_le_bytes(h[..8].try_into().unwrap());
                let frames = u32::from_le_bytes(h[8..].try_into().unwrap()) as usize;
                let mut b = vec![0u8; frames * ch * 4];
                if stdout.read_exact(&mut b).is_err() {
                    return;
                }
                let v = b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
                if mtx.send(Msg::Segment(start, v)).is_err() {
                    return;
                }
            }
        })?;
        Ok(Helper { child, tx, rx, origin: self.n_in })
    }

    fn receive(&mut self) {
        let Some(h) = self.helper.as_mut() else { return };
        let mut dead = false;
        loop {
            match h.rx.try_recv() {
                Ok(Msg::Ready) => self.stats.ready = true,
                Ok(Msg::Segment(start, v)) => {
                    let abs = h.origin + start;
                    let end = self.sep_base + (self.sep.len() / self.ch) as u64;
                    if self.sep.is_empty() && abs > self.sep_base {
                        self.sep_base = abs;
                    } else if abs > end {
                        self.sep.extend(std::iter::repeat_n(f32::NAN, (abs - end) as usize * self.ch));
                    }
                    let end = self.sep_base + (self.sep.len() / self.ch) as u64;
                    let skip = end.saturating_sub(abs) as usize * self.ch;
                    if skip < v.len() {
                        self.sep.extend(&v[skip..]);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    dead = true;
                    break;
                }
            }
        }
        if dead {
            tracing::warn!("K2-Hilfsprozess beendet, Original geht durch; Neustart in 5 s");
            self.helper = None;
            self.stats.ready = false;
            self.stats.restarts += 1;
            self.next_start = Instant::now() + Duration::from_secs(5);
        }
    }

    /// Interleaved verarbeiten: liefert (Original, Stimme) je Sample, beide um `delay`
    /// verzögert; Stimme `None`, wo die Trennung (noch) fehlt.
    pub fn process(&mut self, samples: &[f32], out: &mut Vec<(f32, Option<f32>)>) {
        if self.helper.is_none() && Instant::now() >= self.next_start {
            self.start();
        }
        if let Some(h) = self.helper.as_mut() {
            if h.tx.try_send(samples.to_vec()).is_err() {
                tracing::warn!("K2-Hilfsprozess kommt nicht nach, Neustart");
                self.helper = None;
                self.stats.ready = false;
                self.stats.restarts += 1;
                self.next_start = Instant::now();
            }
        }
        self.receive();
        let frames = samples.len() / self.ch;
        out.clear();
        for f in 0..frames {
            let idx = self.n_in + f as u64; // Eingangs-Frame
            for c in 0..self.ch {
                self.hist.push_back(samples[f * self.ch + c]);
            }
            let Some(m) = idx.checked_sub(self.delay as u64) else {
                for _ in 0..self.ch {
                    out.push((self.hist.pop_front().unwrap_or(0.0), None));
                }
                continue;
            };
            // Stimme für Frame m
            while m > self.sep_base && !self.sep.is_empty() {
                for _ in 0..self.ch {
                    self.sep.pop_front();
                }
                self.sep_base += 1;
            }
            let have = m == self.sep_base && self.sep.len() >= self.ch && !self.sep[0].is_nan();
            if self.stats.ready || have {
                if have {
                    self.stats.separated += 1;
                } else {
                    self.stats.fallback += 1;
                }
            }
            for c in 0..self.ch {
                let x = self.hist.pop_front().unwrap_or(0.0);
                out.push((x, if have { Some(self.sep[c]) } else { None }));
            }
        }
        self.n_in += frames as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_delay_without_helper() {
        let mut k = K2::new(2, 4800, "/nicht/da".into(), "htdemucs".into());
        let x: Vec<f32> = (0..48_000 * 2).map(|i| (i % 977) as f32 / 977.0).collect();
        let mut y = Vec::new();
        let mut out = Vec::new();
        for b in x.chunks(960) {
            k.process(b, &mut out);
            y.extend(out.iter().map(|(a, s)| {
                assert!(s.is_none());
                *a
            }));
        }
        assert_eq!(y.len(), x.len());
        assert!(y[..4800 * 2].iter().all(|v| *v == 0.0));
        assert_eq!(&y[4800 * 2..], &x[..x.len() - 4800 * 2]);
    }
}
