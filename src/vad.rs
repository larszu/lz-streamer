//! Sprache/Musik/Pause-Erkennung (A3) je 1-s-Fenster.
//!
//! Klassisches Merkmalsverfahren statt Netz, damit es ohne Modelldatei und
//! mit < 1 ms je 10 ms Audio läuft:
//! - Pegel (RMS) je 10-ms-Rahmen → Stille
//! - spektrale Flachheit (FFT 512) → Applaus/Rauschen
//! - Stimmhaftigkeit und Grundton über Autokorrelation (70–800 Hz)
//! - Pegelmodulation über das Fenster (Silbenrhythmus) → Sprache
//! - Tonhöhen-Stabilität (gehaltene Töne) → Gesang/Orgel
//! - Mehrstimmigkeit (Spektralspitzen) → Orgel statt Gesang
//!
//! Eingang: 16 kHz mono f32 (wie der Abgriff der Spracherkennung).

use rustfft::{FftPlanner, num_complex::Complex};
use serde::{Deserialize, Serialize};

pub const RATE: usize = 16_000;
const HOP: usize = 160; // 10 ms
const WIN: usize = 512; // 32 ms
const WINDOW_FRAMES: usize = 100; // 1 s

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Class {
    Stille,
    Sprache,
    Gesang,
    Orgel,
    Applaus,
}

impl Class {
    /// grobe Gruppe für A2/K1: Sprache, Musik (Gesang/Orgel), Pause
    pub fn is_music(self) -> bool {
        matches!(self, Class::Gesang | Class::Orgel)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Frame {
    db: f64,
    flat: f64,
    voicing: f64,
    f0: f64,
    peaks: usize,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Features {
    /// Anteil der Rahmen über −50 dBFS
    pub active: f64,
    pub db: f64,
    pub db_std: f64,
    pub flat: f64,
    pub voiced: f64,
    pub f0_stable: f64,
    pub peaks: f64,
}

pub struct Classifier {
    buf: Vec<f32>,
    frames: Vec<Frame>,
    fft: std::sync::Arc<dyn rustfft::Fft<f32>>,
    hann: Vec<f32>,
}

impl Default for Classifier {
    fn default() -> Self {
        let mut p = FftPlanner::new();
        let hann = (0..WIN).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / WIN as f32).cos()).collect();
        Self { buf: Vec::new(), frames: Vec::new(), fft: p.plan_fft_forward(WIN), hann }
    }
}

impl Classifier {
    fn analyse(&self, x: &[f32]) -> Frame {
        let rms = (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt();
        let db = 20.0 * rms.max(1e-9).log10();
        // Spektrum
        let mut c: Vec<Complex<f32>> = x.iter().zip(&self.hann).map(|(v, w)| Complex::new(v * w, 0.0)).collect();
        self.fft.process(&mut c);
        let p: Vec<f64> = c[1..WIN / 2].iter().map(|z| (z.norm_sqr() as f64).max(1e-12)).collect();
        let geo = (p.iter().map(|v| v.ln()).sum::<f64>() / p.len() as f64).exp();
        let ari = p.iter().sum::<f64>() / p.len() as f64;
        let flat = geo / ari;
        // Spektralspitzen (deutlich über der Umgebung) zwischen 100 Hz und 4 kHz
        let lo = 100 * WIN / RATE;
        let hi = 4000 * WIN / RATE;
        let max = p[lo..hi].iter().copied().fold(0.0, f64::max);
        let peaks = (lo + 2..hi - 2)
            .filter(|&k| p[k] > max * 0.05 && p[k] > p[k - 1] && p[k] > p[k + 1] && p[k] > 4.0 * (p[k - 2] + p[k + 2]) / 2.0)
            .count();
        // Autokorrelation für Grundton 70–800 Hz
        let (min_lag, max_lag) = (RATE / 800, RATE / 70);
        let energy: f64 = x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>().max(1e-12);
        let mut best = (0.0, 0usize);
        for lag in min_lag..max_lag.min(x.len() - 1) {
            let r: f64 = x[..x.len() - lag].iter().zip(&x[lag..]).map(|(a, b)| *a as f64 * *b as f64).sum::<f64>() / energy;
            if r > best.0 {
                best = (r, lag);
            }
        }
        let f0 = if best.1 > 0 { RATE as f64 / best.1 as f64 } else { 0.0 };
        Frame { db, flat, voicing: best.0, f0, peaks }
    }

    /// Samples anhängen; liefert für jedes volle 1-s-Fenster eine Klasse.
    pub fn push(&mut self, samples: &[f32]) -> Vec<(Class, Features)> {
        self.buf.extend_from_slice(samples);
        while self.buf.len() >= WIN {
            let f = self.analyse(&self.buf[..WIN]);
            self.frames.push(f);
            self.buf.drain(..HOP);
        }
        let mut out = Vec::new();
        while self.frames.len() >= WINDOW_FRAMES {
            let w: Vec<Frame> = self.frames.drain(..WINDOW_FRAMES).collect();
            let feat = features(&w);
            out.push((classify(&feat), feat));
        }
        out
    }
}

pub fn features(w: &[Frame]) -> Features {
    let n = w.len() as f64;
    let active: Vec<&Frame> = w.iter().filter(|f| f.db > -55.0).collect();
    let na = active.len().max(1) as f64;
    let db = w.iter().map(|f| f.db).sum::<f64>() / n;
    let mean_active = active.iter().map(|f| f.db).sum::<f64>() / na;
    let db_std = (w.iter().map(|f| (f.db.max(-70.0) - mean_active).powi(2)).sum::<f64>() / n).sqrt();
    let voiced: Vec<&&Frame> = active.iter().filter(|f| f.voicing > 0.5).collect();
    let stable = voiced
        .windows(2)
        .filter(|p| p[0].f0 > 0.0 && ((p[1].f0 - p[0].f0) / p[0].f0).abs() < 0.03)
        .count() as f64
        / (voiced.len().max(2) - 1) as f64;
    Features {
        active: w.iter().filter(|f| f.db > -50.0).count() as f64 / n,
        db,
        db_std,
        flat: active.iter().map(|f| f.flat).sum::<f64>() / na,
        voiced: voiced.len() as f64 / na,
        f0_stable: stable,
        peaks: active.iter().map(|f| f.peaks as f64).sum::<f64>() / na,
    }
}

/// Entscheidungsregeln (Schwellen an einem gemischten Korpus abgestimmt,
/// siehe docs/sprache-musik-pause.md).
pub fn classify(f: &Features) -> Class {
    if f.active < 0.15 {
        return Class::Stille;
    }
    // rauschartig, breitbandig: Applaus
    if f.flat > 0.25 {
        return Class::Applaus;
    }
    // gehaltene Töne einer Stimme: wenig Pegelschwankung, stimmhaft
    if f.voiced > 0.6 && f.db_std < 5.5 {
        return Class::Gesang;
    }
    // mehrstimmig gehalten (Autokorrelation findet keinen einzelnen Grundton), sehr gleichmäßig
    if f.voiced < 0.3 && f.db_std < 3.0 && f.flat < 0.05 {
        return Class::Orgel;
    }
    Class::Sprache
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct CorpusResult {
    /// je Klasse: (richtig, gesamt)
    pub per_class: Vec<(Class, usize, usize)>,
    pub confusion: Vec<(Class, Class, usize)>,
    /// Rechenzeit je 10 ms Audio (µs)
    pub us_per_10ms: f64,
}

/// WAV (16 kHz mono, 16 bit) lesen.
pub fn read_wav(path: &std::path::Path) -> anyhow::Result<Vec<f32>> {
    let b = std::fs::read(path)?;
    let mut i = 12;
    while i + 8 <= b.len() {
        let id = &b[i..i + 4];
        let len = u32::from_le_bytes(b[i + 4..i + 8].try_into()?) as usize;
        if id == b"data" {
            let d = &b[i + 8..(i + 8 + len).min(b.len())];
            return Ok(d.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0).collect());
        }
        i += 8 + len + (len & 1);
    }
    anyhow::bail!("keine Daten in {}", path.display())
}

/// Korpus `<ordner>/<klasse>/*.wav` klassifizieren (1-s-Fenster).
pub fn evaluate_corpus(dir: &std::path::Path) -> anyhow::Result<CorpusResult> {
    use std::collections::HashMap;
    let classes = [("stille", Class::Stille), ("sprache", Class::Sprache), ("gesang", Class::Gesang), ("orgel", Class::Orgel), ("applaus", Class::Applaus)];
    let mut conf: HashMap<(Class, Class), usize> = HashMap::new();
    let mut samples = 0usize;
    let mut elapsed = std::time::Duration::ZERO;
    for (name, truth) in classes {
        let Ok(rd) = std::fs::read_dir(dir.join(name)) else { continue };
        for e in rd.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "wav")) {
            let x = read_wav(&e.path())?;
            let mut c = Classifier::default();
            let t = std::time::Instant::now();
            let r = c.push(&x);
            elapsed += t.elapsed();
            samples += x.len();
            for (k, f) in r {
                *conf.entry((truth, k)).or_default() += 1;
                if std::env::var("LZS_VAD_DEBUG").is_ok() {
                    eprintln!("{name}\t{k:?}\t{:.1}\t{:.1}\t{:.3}\t{:.2}\t{:.2}\t{:.1}\t{:.2}", f.db, f.db_std, f.flat, f.voiced, f.f0_stable, f.peaks, f.active);
                }
            }
        }
    }
    let per_class = classes
        .iter()
        .map(|(_, t)| {
            let total: usize = conf.iter().filter(|((a, _), _)| a == t).map(|(_, n)| n).sum();
            (*t, conf.get(&(*t, *t)).copied().unwrap_or(0), total)
        })
        .collect();
    let mut confusion: Vec<_> = conf.into_iter().map(|((a, b), n)| (a, b, n)).collect();
    confusion.sort_by_key(|(a, b, _)| (format!("{a:?}"), format!("{b:?}")));
    Ok(CorpusResult { per_class, confusion, us_per_10ms: elapsed.as_secs_f64() * 1e6 / (samples as f64 / HOP as f64).max(1.0) })
}

/// Rechenzeit-Hilfe: 10 ms Audio analysieren (für den Leistungstest).
pub fn bench_frame(c: &Classifier, x: &[f32]) -> f64 {
    c.analyse(x).db
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(f: f64, secs: f64, a: f32) -> Vec<f32> {
        (0..(secs * RATE as f64) as usize).map(|i| a * (2.0 * std::f64::consts::PI * f * i as f64 / RATE as f64).sin() as f32).collect()
    }

    #[test]
    fn silence_and_tone() {
        let mut c = Classifier::default();
        let quiet: Vec<f32> = vec![0.0001; RATE * 2];
        assert!(c.push(&quiet).iter().all(|(k, _)| *k == Class::Stille));
        let r = c.push(&sine(220.0, 3.0, 0.3));
        assert!(r.last().unwrap().0.is_music(), "{r:?}");
    }

    #[test]
    fn white_noise_is_applause_like() {
        let mut c = Classifier::default();
        let mut s = 12345u32;
        let noise: Vec<f32> = (0..RATE * 2)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                (s as f32 / u32::MAX as f32 - 0.5) * 0.4
            })
            .collect();
        assert!(c.push(&noise).iter().all(|(k, _)| *k == Class::Applaus));
    }
}
