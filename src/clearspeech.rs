//! Klare Sprache (K1): DeepFilterNet3 trennt Sprache `s` vom Rest `r = x − s`.
//! Ausgabe `s + g·r`, also `(1 − g)·s + g·x`, mit `x` um die Modell-Latenz
//! verzögert. Bei erkanntem Gesang (A3) geht `g` sanft auf 1 (keine Absenkung).
//!
//! Element `lzclearspeech`: F32, 48 kHz, 1–2 Kanäle, in place. Latenz 30 ms,
//! wenn die Puffer ein Vielfaches von 10 ms sind (davor `audiobuffersplit`).

use std::collections::VecDeque;
use std::sync::{LazyLock, Mutex, Once};

use deep_filter_rt::tract::{DfParams, DfTract, RuntimeParams};
use gstreamer as gst;
use gstreamer::glib;
use gstreamer::prelude::*;
use gstreamer::subclass::prelude::*;
use gstreamer_base as gst_base;
use gstreamer_base::subclass::prelude::*;
use ndarray::Array2;
use serde::{Deserialize, Serialize};

pub const RATE: usize = 48_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ClearSpeechConfig {
    pub enabled: bool,
    /// Pegel des Rests (Musik, Geräusch, Hall) gegenüber der Sprache, dB
    pub residual_db: f64,
    /// bei erkanntem Gesang nicht absenken
    pub keep_singing: bool,
}

impl Default for ClearSpeechConfig {
    fn default() -> Self {
        Self { enabled: false, residual_db: -9.0, keep_singing: true }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct ClearSpeechStatus {
    /// aktuelle Rest-Absenkung, dB (0 = keine)
    pub residual_db: f64,
    pub singing: bool,
    /// Sprach-Rausch-Schätzung des Modells, dB
    pub lsnr: f64,
}

pub struct ClearSpeech {
    pub cfg: ClearSpeechConfig,
    df: DfTract,
    ch: usize,
    hop: usize,
    delay: Vec<VecDeque<f32>>,
    fifo_in: Vec<f32>,
    fifo_out: VecDeque<f32>,
    g: f64,
    vad: crate::vad::Classifier,
    decim: Vec<f32>,
    prefill: usize,
    pub status: ClearSpeechStatus,
}

impl ClearSpeech {
    pub fn new(cfg: ClearSpeechConfig, ch: usize) -> anyhow::Result<Self> {
        let rp = RuntimeParams::default_with_ch(ch);
        let df = DfTract::new(DfParams::default(), &rp)?;
        let d = df.delay();
        let hop = df.hop_size;
        Ok(Self {
            cfg,
            df,
            ch,
            hop,
            delay: (0..ch).map(|_| VecDeque::from(vec![0.0; d])).collect(),
            fifo_in: Vec::new(),
            fifo_out: VecDeque::new(),
            g: 1.0,
            vad: Default::default(),
            decim: Vec::new(),
            prefill: 0,
            status: Default::default(),
        })
    }

    /// Modell-Latenz in Samples (DeepFilterNet3: 1440 = 30 ms)
    pub fn latency_samples(&self) -> usize {
        self.df.delay()
    }

    /// gesamte Verzögerung des Ausgangs je Kanal: Modell plus ggf. Vorfüllung
    pub fn output_delay_samples(&self) -> usize {
        self.df.delay() + self.prefill / self.ch
    }

    fn target_g(&self) -> f64 {
        if !self.cfg.enabled || self.cfg.keep_singing && self.status.singing { 1.0 } else { 10f64.powf(self.cfg.residual_db.min(0.0) / 20.0) }
    }

    fn hop_frame(&mut self, frame: &[f32]) -> Vec<f32> {
        let (ch, hop) = (self.ch, self.hop);
        let mut noisy = Array2::<f32>::zeros((ch, hop));
        for (i, v) in frame.iter().enumerate() {
            noisy[[i % ch, i / ch]] = *v;
        }
        // Gesang erkennen: Mono, 48 → 16 kHz durch Mitteln je 3 Samples
        for k in 0..hop / 3 {
            let mut acc = 0.0;
            for j in 0..3 {
                for c in 0..ch {
                    acc += noisy[[c, 3 * k + j]];
                }
            }
            self.decim.push(acc / (3 * ch) as f32);
        }
        if let Some((class, _)) = self.vad.push(&std::mem::take(&mut self.decim)).pop() {
            self.status.singing = class == crate::vad::Class::Gesang;
        }
        let mut enh = Array2::<f32>::zeros((ch, hop));
        let ok = self.df.process(noisy.view(), enh.view_mut());
        if let Ok(l) = ok {
            self.status.lsnr = l as f64;
        }
        // Absenkung je Block nachführen (Zeitkonstante ≈ 0,5 s)
        let tg = self.target_g();
        let step = 1.0 - (-(hop as f64) / (0.5 * RATE as f64)).exp();
        let g0 = self.g;
        self.g += (tg - self.g) * step;
        self.status.residual_db = 20.0 * self.g.max(1e-6).log10();
        let mut out = vec![0.0; ch * hop];
        for t in 0..hop {
            let g = (g0 + (self.g - g0) * t as f64 / hop as f64) as f32;
            for c in 0..ch {
                self.delay[c].push_back(noisy[[c, t]]);
                let x = self.delay[c].pop_front().unwrap_or(0.0);
                // Modellfehler: unverändert durchlassen
                let s = if ok.is_ok() { enh[[c, t]] } else { x };
                out[t * ch + c] = (1.0 - g) * s + g * x;
            }
        }
        out
    }

    /// Interleaved verarbeiten. Bei Blöcken aus ganzen 10-ms-Stücken entsteht keine
    /// Zusatzlatenz über die Modell-Latenz hinaus.
    pub fn process(&mut self, samples: &mut [f32]) {
        self.fifo_in.extend_from_slice(samples);
        let n = self.ch * self.hop;
        while self.fifo_in.len() >= n {
            let frame: Vec<f32> = self.fifo_in.drain(..n).collect();
            let out = self.hop_frame(&frame);
            self.fifo_out.extend(out);
        }
        if self.fifo_out.len() < samples.len() {
            // ungerade Blockgröße: einmalig mit Stille vorfüllen (ein Block mehr Latenz)
            let missing = samples.len() - self.fifo_out.len();
            for _ in 0..missing {
                self.fifo_out.push_front(0.0);
            }
            self.prefill += missing;
        }
        for s in samples.iter_mut() {
            *s = self.fifo_out.pop_front().unwrap_or(0.0);
        }
    }
}

mod imp {
    use super::*;

    pub struct LzClearSpeech {
        pub cfg: Mutex<ClearSpeechConfig>,
        pub dsp: Mutex<Option<ClearSpeech>>,
    }

    impl Default for LzClearSpeech {
        fn default() -> Self {
            Self { cfg: Mutex::new(ClearSpeechConfig { enabled: true, ..Default::default() }), dsp: Mutex::new(None) }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LzClearSpeech {
        const NAME: &'static str = "LzStreamerClearSpeech";
        type Type = super::LzClearSpeech;
        type ParentType = gst_base::BaseTransform;
    }

    impl ObjectImpl for LzClearSpeech {}
    impl GstObjectImpl for LzClearSpeech {}

    impl ElementImpl for LzClearSpeech {
        fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
            static META: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
                gst::subclass::ElementMetadata::new(
                    "LZ clear speech",
                    "Filter/Effect/Audio",
                    "DeepFilterNet3 speech/residual split, residual lowered by a set amount, not during singing",
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

    impl BaseTransformImpl for LzClearSpeech {
        const MODE: gst_base::subclass::BaseTransformMode = gst_base::subclass::BaseTransformMode::AlwaysInPlace;
        const PASSTHROUGH_ON_SAME_CAPS: bool = false;
        const TRANSFORM_IP_ON_PASSTHROUGH: bool = false;

        fn set_caps(&self, incaps: &gst::Caps, _o: &gst::Caps) -> Result<(), gst::LoggableError> {
            let ch = incaps.structure(0).and_then(|s| s.get::<i32>("channels").ok()).unwrap_or(1).max(1) as usize;
            let dsp = ClearSpeech::new(self.cfg.lock().unwrap().clone(), ch).map_err(|e| gst::loggable_error!(gst::CAT_RUST, "DeepFilterNet: {e}"))?;
            *self.dsp.lock().unwrap() = Some(dsp);
            Ok(())
        }

        fn query(&self, direction: gst::PadDirection, query: &mut gst::QueryRef) -> bool {
            if let gst::QueryViewMut::Latency(q) = query.view_mut() {
                let ok = BaseTransformImplExt::parent_query(self, direction, q.query_mut());
                if ok {
                    let n = self.dsp.lock().unwrap().as_ref().map(|d| d.latency_samples()).unwrap_or(1440);
                    let (live, min, max) = q.result();
                    let l = gst::ClockTime::from_nseconds((n as u64 * 1_000_000_000) / RATE as u64);
                    q.set(live, min + l, max.map(|m| m + l));
                }
                return ok;
            }
            BaseTransformImplExt::parent_query(self, direction, query)
        }

        fn transform_ip(&self, buf: &mut gst::BufferRef) -> Result<gst::FlowSuccess, gst::FlowError> {
            let mut map = buf.map_writable().map_err(|_| gst::FlowError::Error)?;
            // SAFETY: f32 hat keine ungültigen Bitmuster; align_to_mut gibt nur den passend ausgerichteten Teil
            let (head, samples, tail) = unsafe { map.as_mut_slice().align_to_mut::<f32>() };
            if !head.is_empty() || !tail.is_empty() {
                return Err(gst::FlowError::Error);
            }
            let shift = match self.dsp.lock().unwrap().as_mut() {
                Some(d) => {
                    d.process(samples);
                    d.output_delay_samples()
                }
                None => 0,
            };
            drop(map);
            // Inhalt kommt um die Modell-Latenz später: Zeitstempel zurücksetzen, damit die
            // Spur lippensynchron bleibt (die gemeldete Latenz deckt die Verspätung)
            if let Some(pts) = buf.pts() {
                let d = gst::ClockTime::from_nseconds(shift as u64 * 1_000_000_000 / RATE as u64);
                buf.set_pts(pts.saturating_sub(d));
            }
            Ok(gst::FlowSuccess::Ok)
        }
    }
}

glib::wrapper! {
    pub struct LzClearSpeech(ObjectSubclass<imp::LzClearSpeech>) @extends gst_base::BaseTransform, gst::Element, gst::Object;
}

impl LzClearSpeech {
    pub fn set_config(&self, c: ClearSpeechConfig) {
        *self.imp().cfg.lock().unwrap() = c.clone();
        if let Some(d) = self.imp().dsp.lock().unwrap().as_mut() {
            d.cfg = c;
        }
    }
    pub fn status(&self) -> Option<ClearSpeechStatus> {
        self.imp().dsp.lock().unwrap().as_ref().map(|d| d.status)
    }
}

pub fn register() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = gst::Element::register(None, "lzclearspeech", gst::Rank::NONE, LzClearSpeech::static_type());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, a: f32, seed: &mut u32) -> Vec<f32> {
        (0..n)
            .map(|_| {
                *seed ^= *seed << 13;
                *seed ^= *seed >> 17;
                *seed ^= *seed << 5;
                (*seed as f32 / u32::MAX as f32 - 0.5) * a
            })
            .collect()
    }

    #[test]
    fn lowers_noise_and_reports_30ms() {
        let mut cs = ClearSpeech::new(ClearSpeechConfig { enabled: true, residual_db: -9.0, keep_singing: false }, 1).unwrap();
        assert_eq!(cs.latency_samples(), 1440);
        let mut seed = 7;
        let x = noise(RATE * 3, 0.2, &mut seed);
        let rms = |v: &[f32]| (v.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / v.len() as f64).sqrt();
        let mut y = x.clone();
        for c in y.chunks_mut(480) {
            cs.process(c);
        }
        // reines Rauschen: Rest wird um ≈ 9 dB abgesenkt (nach dem Einschwingen)
        let drop = 20.0 * (rms(&y[RATE * 2..]) / rms(&x[RATE * 2..])).log10();
        assert!(drop < -6.0 && drop > -12.0, "{drop:.1} dB");
        assert!((cs.status.residual_db + 9.0).abs() < 0.5, "{:?}", cs.status);
    }

    #[test]
    fn zero_db_is_delayed_passthrough() {
        let mut cs = ClearSpeech::new(ClearSpeechConfig { enabled: true, residual_db: 0.0, keep_singing: false }, 2).unwrap();
        let mut seed = 3;
        let x = noise(2 * 4800, 0.3, &mut seed);
        let mut y = x.clone();
        for c in y.chunks_mut(960) {
            cs.process(c);
        }
        // g = 1: Ausgang = Eingang, um 1440 Frames verschoben
        let d = 1440 * 2;
        let err = y[d..].iter().zip(&x[..x.len() - d]).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(err < 1e-6, "{err}");
    }
}
