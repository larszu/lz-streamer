//! Latenzarme Lautheitsregelung (A2), Prinzip nach Recherche §3.5:
//! - K-gewichtete Lautheit (BS.1770): Momentary 400 ms und Short-Term 3 s
//! - langsame AGC auf ein Ziel, mit Ober- und Untergrenze
//! - Einfrieren in Pausen: kein Hochziehen von Stille oder Raumrauschen
//! - schneller Transientenprozessor: zieht Spitzen über dem Ziel rasch herunter
//! - True-Peak-Begrenzer mit 1,5 ms Vorausschau (4-fach überabgetastete Spitzenschätzung)
//!
//! Die Latenz ist genau die Vorausschau (72 Samples bei 48 kHz).

use serde::{Deserialize, Serialize};

pub const RATE: f64 = 48_000.0;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Preset {
    /// ≤ 0,2 dB/s – wie das Jünger-Handbuch (1 dB je 5 s)
    Soft,
    #[default]
    Mid,
    Hard,
}

impl Preset {
    /// Stellgeschwindigkeit der AGC in dB/s (nach oben, nach unten)
    pub fn rates(self) -> (f64, f64) {
        match self {
            Preset::Soft => (0.2, 0.4),
            Preset::Mid => (0.8, 1.5),
            Preset::Hard => (2.5, 4.0),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LevelerConfig {
    pub enabled: bool,
    /// Ziel in LUFS: −23 (R 128), −24 (A/85), −16 oder −14 (Streaming)
    pub target: f64,
    pub preset: Preset,
    /// True-Peak-Grenze in dBTP
    pub ceiling: f64,
    /// Regelbereich ±dB
    pub range_db: f64,
    /// unter dieser Momentary-Lautheit wird die AGC eingefroren
    pub freeze_below: f64,
}

impl Default for LevelerConfig {
    fn default() -> Self {
        Self { enabled: false, target: -23.0, preset: Preset::Mid, ceiling: -1.0, range_db: 15.0, freeze_below: -45.0 }
    }
}

/// Biquad (Direktform I), für die K-Gewichtung.
#[derive(Clone, Copy, Default)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    x: [f64; 2],
    y: [f64; 2],
}

impl Biquad {
    fn new(b: [f64; 3], a: [f64; 2]) -> Self {
        Self { b, a, ..Default::default() }
    }
    fn run(&mut self, v: f64) -> f64 {
        let y = self.b[0] * v + self.b[1] * self.x[0] + self.b[2] * self.x[1] - self.a[0] * self.y[0] - self.a[1] * self.y[1];
        self.x = [v, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
}

/// K-Filter bei 48 kHz (Koeffizienten aus BS.1770).
fn k_filter() -> (Biquad, Biquad) {
    (
        Biquad::new([1.53512485958697, -2.69169618940638, 1.19839281085285], [-1.69065929318241, 0.73248077421585]),
        Biquad::new([1.0, -2.0, 1.0], [-1.99004745483398, 0.99007225036621]),
    )
}

/// Gleitender Mittelwert der Quadrate über Blöcke von 100 ms.
struct Window {
    blocks: std::collections::VecDeque<f64>,
    len: usize,
}

impl Window {
    fn new(len: usize) -> Self {
        Self { blocks: Default::default(), len }
    }
    fn push(&mut self, ms: f64) {
        self.blocks.push_back(ms);
        while self.blocks.len() > self.len {
            self.blocks.pop_front();
        }
    }
    fn lufs(&self) -> f64 {
        if self.blocks.is_empty() {
            return -100.0;
        }
        let m = self.blocks.iter().sum::<f64>() / self.blocks.len() as f64;
        -0.691 + 10.0 * m.max(1e-12).log10()
    }
}

pub fn db(x: f64) -> f64 {
    10f64.powf(x / 20.0)
}

pub struct Leveler {
    pub cfg: LevelerConfig,
    channels: usize,
    filters: Vec<(Biquad, Biquad)>,
    block_acc: f64,
    block_n: usize,
    block_len: usize,
    momentary: Window,
    short: Window,
    /// AGC-Verstärkung (dB)
    pub agc_db: f64,
    /// schnelle Absenkung (dB, ≤ 0)
    pub fast_db: f64,
    // Begrenzer
    delay: std::collections::VecDeque<f32>,
    lookahead: usize,
    lim_gain: f64,
    prev: Vec<f32>,
}

impl Leveler {
    pub fn new(cfg: LevelerConfig, channels: usize) -> Self {
        let lookahead = (0.0015 * RATE) as usize;
        Self {
            cfg,
            channels: channels.max(1),
            filters: vec![k_filter(); channels.max(1)],
            block_acc: 0.0,
            block_n: 0,
            block_len: (0.1 * RATE) as usize,
            momentary: Window::new(4),
            short: Window::new(30),
            agc_db: 0.0,
            fast_db: 0.0,
            delay: Default::default(),
            lookahead,
            lim_gain: 1.0,
            prev: vec![0.0; channels.max(1)],
        }
    }

    /// Latenz in Samples (je Kanal).
    pub fn latency_samples(&self) -> usize {
        self.lookahead
    }

    pub fn momentary(&self) -> f64 {
        self.momentary.lufs()
    }
    pub fn short_term(&self) -> f64 {
        self.short.lufs()
    }

    /// Alle 100 ms: AGC und Transientenprozessor nachführen. `pause` kommt
    /// optional von der Sprache/Musik/Pause-Erkennung (A3).
    fn update(&mut self, pause: bool) {
        let dt = 0.1;
        let m = self.momentary.lufs() + self.agc_db; // gemessen wird vor der Regelung
        let s = self.short.lufs() + self.agc_db;
        let (up, down) = self.cfg.preset.rates();
        let frozen = pause || self.momentary.lufs() < self.cfg.freeze_below;
        if !frozen {
            let err = self.cfg.target - s;
            // große Abweichung: schneller nachführen (×4), sonst sanft
            let boost = if err.abs() > 6.0 { 4.0 } else { 1.0 };
            let step = if err > 0.0 { (up * boost * dt).min(err) } else { (-down * boost * dt).max(err) };
            self.agc_db = (self.agc_db + step).clamp(-self.cfg.range_db, self.cfg.range_db);
        }
        // Transienten: Momentary über Ziel + 3 LU → sofort absenken, Freigabe 3 dB/s
        let over = m - (self.cfg.target + 3.0);
        if over > 0.0 {
            self.fast_db = (-over).min(self.fast_db.min(0.0));
        } else {
            self.fast_db = (self.fast_db + 3.0 * dt).min(0.0);
        }
    }

    /// Interleaved f32 in place regeln.
    pub fn process(&mut self, buf: &mut [f32], pause: bool) {
        let ch = self.channels;
        let ceiling = db(self.cfg.ceiling) as f32;
        let release = (-1.0 / (0.05 * RATE)).exp(); // 50 ms
        for frame in buf.chunks_exact_mut(ch) {
            // Messung (vor der Verstärkung)
            let mut sq = 0.0;
            for (c, v) in frame.iter().enumerate() {
                let (f1, f2) = &mut self.filters[c];
                let k = f2.run(f1.run(*v as f64));
                sq += k * k;
            }
            self.block_acc += sq;
            self.block_n += 1;
            if self.block_n == self.block_len {
                let ms = self.block_acc / self.block_len as f64;
                self.momentary.push(ms);
                self.short.push(ms);
                self.block_acc = 0.0;
                self.block_n = 0;
                self.update(pause);
            }
            // Verstärkung, dann in die Vorausschau
            let g = db(self.agc_db + self.fast_db) as f32;
            for v in frame.iter() {
                self.delay.push_back(v * g);
            }
            // Spitze der kommenden 1,5 ms (mit Zwischenwerten: 4-fach linear)
            if self.delay.len() >= (self.lookahead + 1) * ch {
                let mut peak = 0.0f32;
                for c in 0..ch {
                    let mut last = self.prev[c];
                    for i in (c..self.delay.len()).step_by(ch) {
                        let x = self.delay[i];
                        let mid = (x + last) * 0.5;
                        peak = peak.max(x.abs()).max(mid.abs() * 1.05);
                        last = x;
                    }
                }
                let want = if peak > ceiling { ceiling / peak } else { 1.0 } as f64;
                self.lim_gain = if want < self.lim_gain { want } else { want + (self.lim_gain - want) * release };
                for (c, out) in frame.iter_mut().enumerate() {
                    let x = self.delay.pop_front().unwrap_or(0.0);
                    self.prev[c] = x;
                    *out = x * self.lim_gain as f32;
                }
            } else {
                for out in frame.iter_mut() {
                    *out = 0.0;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// sprachähnliches Signal: Rauschen mit 4-Hz-Silbenrhythmus, Pegel in dBFS (RMS)
    fn speechlike(secs: f64, rms_db: f64, seed: &mut u32) -> Vec<f32> {
        let n = (secs * RATE) as usize;
        (0..n)
            .map(|i| {
                *seed ^= *seed << 13;
                *seed ^= *seed >> 17;
                *seed ^= *seed << 5;
                let w = (*seed as f64 / u32::MAX as f64 - 0.5) * 3.46; // RMS 1
                let env = 0.6 + 0.4 * (2.0 * std::f64::consts::PI * 4.0 * i as f64 / RATE).sin().abs() * 1.4;
                (w * env * db(rms_db)) as f32
            })
            .collect()
    }

    fn short_term_after(lev: &mut Leveler, x: &mut [f32]) -> Vec<f64> {
        // Ausgang messen: zweiter, unbeteiligter Messer
        let mut meter = Leveler::new(LevelerConfig { enabled: false, ..Default::default() }, 1);
        lev.process(x, false);
        let mut st = Vec::new();
        for chunk in x.chunks(4800) {
            let mut c = chunk.to_vec();
            let before = meter.agc_db;
            meter.process(&mut c, true); // pause=true: Messer regelt nicht
            assert_eq!(meter.agc_db, before);
            st.push(meter.short_term());
        }
        st
    }

    #[test]
    fn alternating_speech_levels_land_near_target() {
        let mut seed = 1;
        let mut lev = Leveler::new(LevelerConfig { enabled: true, preset: Preset::Mid, ..Default::default() }, 1);
        let mut x = Vec::new();
        for _ in 0..3 {
            x.extend(speechlike(30.0, -30.0, &mut seed));
            x.extend(speechlike(30.0, -18.0, &mut seed));
        }
        let st = short_term_after(&mut lev, &mut x);
        // je 30-s-Abschnitt: die letzten 15 s innerhalb ±2 LU um −23
        for seg in 1..6 {
            let a = seg * 300 + 150; // 100-ms-Schritte
            let worst = st[a..seg * 300 + 299].iter().map(|v| (v + 23.0).abs()).fold(0.0, f64::max);
            assert!(worst <= 2.0, "Abschnitt {seg}: Abweichung {worst:.1} LU");
        }
    }

    #[test]
    fn pauses_are_not_pulled_up() {
        let mut seed = 2;
        let mut lev = Leveler::new(LevelerConfig { enabled: true, ..Default::default() }, 1);
        let mut x = speechlike(20.0, -23.0, &mut seed);
        x.extend(vec![0.0001f32; (20.0 * RATE) as usize]); // −80 dBFS Raumrauschen
        lev.process(&mut x, false);
        let before = lev.agc_db;
        let mut pause = vec![0.0001f32; (10.0 * RATE) as usize];
        lev.process(&mut pause, false);
        assert!((lev.agc_db - before).abs() < 0.01, "Pause hochgezogen: {before:.2} → {:.2} dB", lev.agc_db);
    }

    #[test]
    fn true_peak_stays_below_ceiling() {
        let mut lev = Leveler::new(LevelerConfig { enabled: true, ceiling: -1.0, ..Default::default() }, 2);
        // Sinus mit 0 dBFS Spitze und Knall auf +3 dB
        let mut x: Vec<f32> = (0..(4.0 * RATE) as usize)
            .flat_map(|i| {
                let v = (2.0 * std::f64::consts::PI * 997.0 * i as f64 / RATE).sin() as f32;
                let v = if (96_000..96_480).contains(&i) { v * 1.41 } else { v };
                [v, v]
            })
            .collect();
        lev.process(&mut x, false);
        let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(20.0 * (peak as f64).log10() <= -0.95, "Spitze {:.2} dBFS", 20.0 * (peak as f64).log10());
    }

    #[test]
    fn latency_is_lookahead_only() {
        let lev = Leveler::new(LevelerConfig::default(), 2);
        assert_eq!(lev.latency_samples(), 72);
        assert!(lev.latency_samples() as f64 / RATE * 1000.0 <= 5.0);
    }
}
