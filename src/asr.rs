//! Live-Untertitel aus Spracherkennung (lokal Whisper über `whispertranscriber`).
//!
//! Die Erkennung läuft in einer eigenen kleinen Pipeline, gespeist aus einem
//! Abgriff der gewählten Tonspur (16 kHz mono). Stürzt sie ab oder fehlt das
//! Modell, versucht sie es nach einer Pause erneut – Bild, Ton und SRT laufen
//! davon unberührt weiter. Der erkannte Text geht nach der Ersetzungsliste in
//! die Live-Untertitelquelle (`ccin`, siehe `pipeline::caption_branch`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Replacement {
    pub from: String,
    pub to: String,
}

impl Default for Replacement {
    fn default() -> Self {
        Self { from: String::new(), to: String::new() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AsrConfig {
    /// Modellname wie bei whisper.cpp: tiny, base, small, medium, large-v3-turbo …
    pub model: String,
    /// vom Aufpasser gesetzt: Pfad zur ggml-Datei
    pub model_path: String,
    /// Sprache der Aufnahme (ISO 639-1, z. B. "de")
    pub language: String,
    /// Tonspur als Quelle (Index in `audio.tracks`)
    pub track: u32,
    /// Stückgröße für die Erkennung in ms (kleiner = schneller, ungenauer)
    pub chunk_ms: u32,
    /// Namen, Fachbegriffe: erkannt → geschrieben
    pub replacements: Vec<Replacement>,
    /// ins Englische übersetzen (Whisper-eigene Übersetzung)
    pub translate: bool,
}

impl Default for AsrConfig {
    fn default() -> Self {
        Self {
            model: "small".into(),
            model_path: String::new(),
            language: "de".into(),
            track: 0,
            // gemessen am M1 Pro: 2 s schafft Whisper „small“ live nicht dauerhaft (Rückstau), 3 s schon
            chunk_ms: 3000,
            replacements: Vec::new(),
            translate: false,
        }
    }
}

/// Bekannte Modelle (Größe laut whisper.cpp, gerundet).
pub const MODELS: &[(&str, &str, u64)] = &[
    ("tiny", "tiny", 77_691_713),
    ("base", "base", 147_951_465),
    ("small", "small", 487_601_967),
    ("medium", "medium", 1_533_763_059),
    ("large-v3-turbo", "large-v3-turbo", 1_624_555_275),
];

pub fn model_url(name: &str) -> String {
    format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{name}.bin")
}

/// `model-preset` des Elements passend zum Modellnamen.
pub fn model_preset(name: &str) -> &'static str {
    match name {
        n if n.starts_with("tiny") => "tiny",
        n if n.starts_with("base") => "base",
        n if n.starts_with("small") => "small",
        n if n.starts_with("medium") => "medium",
        n if n.contains("turbo") => "large-v3-turbo",
        n if n.starts_with("large-v3") => "large-v3",
        n if n.starts_with("large-v2") => "large-v2",
        _ => "small",
    }
}

/// Ersetzungsliste anwenden: ganze Wörter, Groß-/Kleinschreibung egal.
pub fn apply_replacements(text: &str, list: &[Replacement]) -> String {
    let mut out = text.to_string();
    for r in list.iter().filter(|r| !r.from.trim().is_empty()) {
        let from = r.from.trim();
        let lower = out.to_lowercase();
        let needle = from.to_lowercase();
        let mut result = String::new();
        let mut last = 0;
        let mut i = 0;
        while let Some(pos) = lower[i..].find(&needle) {
            let s = i + pos;
            let e = s + needle.len();
            let before_ok = s == 0 || !lower[..s].chars().last().is_some_and(|c| c.is_alphanumeric());
            let after_ok = e >= lower.len() || !lower[e..].chars().next().is_some_and(|c| c.is_alphanumeric());
            if before_ok && after_ok && out.is_char_boundary(s) && out.is_char_boundary(e) {
                result.push_str(&out[last..s]);
                result.push_str(&r.to);
                last = e;
            }
            i = e;
            if i >= lower.len() {
                break;
            }
        }
        result.push_str(&out[last..]);
        out = result;
    }
    out
}

fn words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_string())
        .collect()
}

/// Wortfehlerrate (Levenshtein auf Wörtern, ohne Satzzeichen, kleingeschrieben).
pub fn wer(reference: &str, hypothesis: &str) -> f64 {
    let r = words(reference);
    let h = words(hypothesis);
    if r.is_empty() {
        return if h.is_empty() { 0.0 } else { 1.0 };
    }
    let mut prev: Vec<usize> = (0..=h.len()).collect();
    for i in 1..=r.len() {
        let mut cur = vec![i; h.len() + 1];
        for j in 1..=h.len() {
            let sub = prev[j - 1] + usize::from(r[i - 1] != h[j - 1]);
            cur[j] = sub.min(prev[j] + 1).min(cur[j - 1] + 1);
        }
        prev = cur;
    }
    prev[h.len()] as f64 / r.len() as f64
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AsrStatus {
    pub state: String,
    pub model: String,
    /// erkannte Textstücke seit Start
    pub segments: u64,
    pub last_text: Option<String>,
    /// Abstand zwischen gesprochener Stelle (Zeitstempel) und Ausgabe, ms
    pub last_delay_ms: Option<f64>,
}

/// Erkannter Text mit Verzug, an den Kanal weitergereicht.
pub type TextSink = Arc<dyn Fn(String) + Send + Sync>;

/// Whisper liefert Wort für Wort. Für CEA-608 (32 Zeichen je Zeile) werden
/// daraus Zeilen: Schluss bei Satzende, bei 32 Zeichen oder nach einer Pause.
#[derive(Default)]
pub struct Phraser {
    /// innerhalb von [ … ] oder ( … ): Geräuschbeschreibung, nicht anzeigen
    bracket: i32,
    words: Vec<String>,
    first_pts: Option<u64>,
    last_end: Option<u64>,
    last_word_at: Option<Instant>,
}

/// Fertige Zeile: Text, Zeitstempel des ersten Wortes (ns, Laufzeit der Pipeline), Ausgabezeitpunkt.
#[derive(Debug, Clone)]
pub struct Phrase {
    pub text: String,
    pub first_word_ns: Option<u64>,
    pub at: Instant,
}

pub const LINE_CHARS: usize = 32;

impl Phraser {
    fn take(&mut self) -> Option<Phrase> {
        if self.words.is_empty() {
            return None;
        }
        let text = self.words.join(" ").trim().to_string();
        self.words.clear();
        let first = self.first_pts.take();
        self.last_end = None;
        Some(Phrase { text, first_word_ns: first, at: Instant::now() })
    }

    /// Wort hinzufügen; liefert ggf. abgeschlossene Zeilen (vorher und/oder jetzt).
    pub fn push(&mut self, word: &str, pts: Option<u64>, end: Option<u64>) -> Vec<Phrase> {
        let w = word.trim();
        let mut out = Vec::new();
        // Whisper schreibt Geräusche als [Musik], (Applaus), [BLANK_AUDIO] – weglassen
        let opens = w.matches(['[', '(']).count() as i32;
        let closes = w.matches([']', ')']).count() as i32;
        let inside = self.bracket > 0 || opens > 0;
        self.bracket = (self.bracket + opens - closes).max(0);
        if w.is_empty() || inside || w.starts_with('*') {
            return out;
        }
        let gap = matches!((self.last_end, pts), (Some(e), Some(p)) if p > e + 800_000_000);
        let len = self.words.iter().map(|x| x.chars().count() + 1).sum::<usize>() + w.chars().count();
        if gap || (len > LINE_CHARS && !self.words.is_empty()) {
            out.extend(self.take());
        }
        if self.words.is_empty() {
            self.first_pts = pts;
        }
        self.words.push(w.to_string());
        self.last_end = end.or(pts);
        self.last_word_at = Some(Instant::now());
        if w.ends_with(['.', '?', '!', ':']) {
            out.extend(self.take());
        }
        out
    }

    /// Ohne neue Wörter seit `idle`: offene Zeile ausgeben.
    pub fn flush_idle(&mut self, idle: Duration) -> Option<Phrase> {
        if self.last_word_at.is_some_and(|t| t.elapsed() >= idle) {
            self.last_word_at = None;
            return self.take();
        }
        None
    }
}

fn emit_phrase(
    mut ph: Phrase,
    repl: &[Replacement],
    main: &gst::Pipeline,
    seg: &AtomicU64,
    last: &Mutex<(Option<String>, Option<f64>)>,
    on_text: &TextSink,
    phrases: &Mutex<Vec<Phrase>>,
) {
    ph.text = apply_replacements(&ph.text, repl);
    let delay = match (main.current_running_time(), ph.first_word_ns) {
        (Some(now), Some(p)) => Some(now.nseconds().saturating_sub(p) as f64 / 1e6),
        _ => None,
    };
    seg.fetch_add(1, Ordering::Relaxed);
    *last.lock().unwrap() = (Some(ph.text.clone()), delay);
    on_text(ph.text.clone());
    phrases.lock().unwrap().push(ph);
}

pub struct AsrRunner {
    cfg: AsrConfig,
    main: gst::Pipeline,
    tap: Arc<Mutex<Option<gst_app::AppSrc>>>,
    pipeline: Option<gst::Pipeline>,
    retry_at: Option<Instant>,
    segments: Arc<AtomicU64>,
    last: Arc<Mutex<(Option<String>, Option<f64>)>>,
    on_text: TextSink,
    phraser: Arc<Mutex<Phraser>>,
    /// alle ausgegebenen Zeilen (für Messungen)
    pub phrases: Arc<Mutex<Vec<Phrase>>>,
    pub state: String,
}

impl AsrRunner {
    /// `tap_name`: appsink der Haupt-Pipeline mit 16-kHz-Mono-Ton.
    pub fn new(main: &gst::Pipeline, tap_name: &str, cfg: AsrConfig, on_text: TextSink) -> Self {
        let tap: Arc<Mutex<Option<gst_app::AppSrc>>> = Arc::default();
        if let Some(sink) = main.by_name(tap_name).and_then(|s| s.downcast::<gst_app::AppSink>().ok()) {
            let t = tap.clone();
            sink.set_callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |s| {
                        let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        let src = t.lock().unwrap().clone();
                        if let (Some(src), Some(buf)) = (src, sample.buffer_owned()) {
                            let _ = src.push_buffer(buf);
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );
        }
        Self {
            cfg,
            main: main.clone(),
            tap,
            pipeline: None,
            retry_at: None,
            segments: Arc::default(),
            last: Arc::default(),
            on_text,
            phraser: Arc::default(),
            phrases: Arc::default(),
            state: "startet".into(),
        }
    }

    pub fn description(cfg: &AsrConfig) -> String {
        format!(
            "appsrc name=a is-live=true format=time do-timestamp=false \
             caps=audio/x-raw,format=F32LE,layout=interleaved,rate=16000,channels=1 \
             ! queue max-size-buffers=0 max-size-bytes=0 max-size-time=10000000000 \
             ! whispertranscriber name=asr model-path={} model-preset={} chunk-duration={c} live-edge-offset=500 latency={c} \
             ! appsink name=t sync=false async=false",
            crate::pipeline::quote(&cfg.model_path),
            model_preset(&cfg.model),
            c = cfg.chunk_ms.clamp(500, 10_000)
        )
    }

    pub fn start(&mut self) -> Result<(), String> {
        if !std::path::Path::new(&self.cfg.model_path).is_file() {
            return Err(format!("Modell {} fehlt – unter Verwaltung → Spracherkennung laden", self.cfg.model));
        }
        let p = gst::parse::launch(&Self::description(&self.cfg))
            .map_err(|e| e.to_string())?
            .downcast::<gst::Pipeline>()
            .map_err(|_| "keine Pipeline".to_string())?;
        if let Some(asr) = p.by_name("asr") {
            if !self.cfg.language.trim().is_empty() && asr.find_property("language").is_some() {
                asr.set_property("language", self.cfg.language.trim());
            }
            if self.cfg.translate {
                asr.set_property("translate", true);
            }
            for p in ["suppress-nst", "suppress-blank"] {
                if asr.find_property(p).is_some() {
                    asr.set_property(p, true);
                }
            }
            if asr.find_property("n-threads").is_some() {
                let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8) as i32;
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| asr.set_property_from_str("n-threads", &n.to_string())));
            }
        }
        if let Some(clock) = self.main.clock() {
            p.use_clock(Some(&clock));
        }
        if let Some(bt) = self.main.base_time() {
            p.set_base_time(bt);
        }
        p.set_start_time(gst::ClockTime::NONE);
        let src = p.by_name("a").and_then(|e| e.downcast::<gst_app::AppSrc>().ok());
        if let Some(s) = &src {
            s.set_max_bytes(16_000 * 4 * 20);
        }
        if let Some(sink) = p.by_name("t").and_then(|s| s.downcast::<gst_app::AppSink>().ok()) {
            let (seg, last, on_text, repl, main, phraser, phrases) = (
                self.segments.clone(),
                self.last.clone(),
                self.on_text.clone(),
                self.cfg.replacements.clone(),
                self.main.clone(),
                self.phraser.clone(),
                self.phrases.clone(),
            );
            sink.set_callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |s| {
                        let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        let Some(buf) = sample.buffer() else { return Ok(gst::FlowSuccess::Ok) };
                        let Ok(map) = buf.map_readable() else { return Ok(gst::FlowSuccess::Ok) };
                        let raw = String::from_utf8_lossy(map.as_slice()).trim().to_string();
                        if raw.is_empty() {
                            return Ok(gst::FlowSuccess::Ok);
                        }
                        let pts = buf.pts().map(|p| p.nseconds());
                        let end = buf.pts().zip(buf.duration()).map(|(p, d)| (p + d).nseconds());
                        let done = phraser.lock().unwrap().push(&raw, pts, end);
                        for ph in done {
                            emit_phrase(ph, &repl, &main, &seg, &last, &on_text, &phrases);
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );
        }
        p.set_state(gst::State::Playing).map_err(|e| e.to_string())?;
        *self.tap.lock().unwrap() = src;
        self.pipeline = Some(p);
        self.state = "läuft".into();
        Ok(())
    }

    fn fail(&mut self, why: String) {
        *self.tap.lock().unwrap() = None;
        if let Some(p) = self.pipeline.take() {
            let _ = p.set_state(gst::State::Null);
        }
        self.retry_at = Some(Instant::now() + Duration::from_secs(10));
        self.state = format!("Fehler, neuer Versuch in 10 s: {why}");
    }

    /// Bus prüfen, ggf. neu starten. Gibt Ereignistexte zurück.
    pub fn poll(&mut self) -> Vec<String> {
        let mut ev = Vec::new();
        let idle = self.phraser.lock().unwrap().flush_idle(Duration::from_millis(1200));
        if let Some(ph) = idle {
            emit_phrase(ph, &self.cfg.replacements, &self.main, &self.segments, &self.last, &self.on_text, &self.phrases);
        }
        if let Some(p) = self.pipeline.clone() {
            let bus = p.bus().unwrap();
            while let Some(msg) = bus.pop() {
                if let gst::MessageView::Error(err) = msg.view() {
                    let why = format!(
                        "{} ({}: {})",
                        err.error(),
                        msg.src().map(|s| s.name().to_string()).unwrap_or_default(),
                        err.debug().unwrap_or_default().lines().last().unwrap_or_default()
                    );
                    ev.push(format!("Spracherkennung abgebrochen: {why} – Kanal läuft weiter"));
                    self.fail(why);
                    return ev;
                }
            }
        } else if self.retry_at.is_none_or(|t| Instant::now() >= t) {
            self.retry_at = None;
            match self.start() {
                Ok(()) => ev.push(format!("Spracherkennung läuft ({}, {})", self.cfg.model, self.cfg.language)),
                Err(e) => {
                    ev.push(format!("Spracherkennung: {e}"));
                    self.fail(e);
                }
            }
        }
        ev
    }

    pub fn status(&self) -> AsrStatus {
        let (t, d) = self.last.lock().unwrap().clone();
        AsrStatus {
            state: self.state.clone(),
            model: self.cfg.model.clone(),
            segments: self.segments.load(Ordering::Relaxed),
            last_text: t,
            last_delay_ms: d,
        }
    }

    /// Absturz nachstellen (für den Test „Kanal läuft weiter“).
    pub fn crash(&mut self) {
        if let Some(p) = &self.pipeline {
            if let Some(asr) = p.by_name("asr") {
                let _ = asr.post_message(gst::message::Error::new(gst::CoreError::Failed, "Absturz nachgestellt"));
            }
        }
    }

    pub fn stop(&mut self) {
        *self.tap.lock().unwrap() = None;
        if let Some(p) = self.pipeline.take() {
            let _ = p.set_state(gst::State::Null);
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FileMeasurement {
    pub text: String,
    /// je Textstück: Abstand zwischen Ende der gesprochenen Stelle und Ausgabe (ms)
    pub delays_ms: Vec<f64>,
    pub median_delay_ms: Option<f64>,
    pub wer: Option<f64>,
}

/// Datei in Echtzeit (clocksync) durch dieselbe Whisper-Einstellung schicken
/// wie im Kanal, Text sammeln, Verzug messen, optional WER gegen Referenz.
pub fn measure_file(audio: &std::path::Path, cfg: &AsrConfig, reference: Option<&str>) -> anyhow::Result<FileMeasurement> {
    let _ = gst::init();
    let desc = format!(
        "filesrc location={} ! decodebin ! audioconvert ! audioresample \
         ! audio/x-raw,format=F32LE,layout=interleaved,rate=16000,channels=1 ! clocksync \
         ! whispertranscriber name=asr model-path={} model-preset={} chunk-duration={} live-edge-offset=500 latency=3000 \
         ! appsink name=t sync=false",
        crate::pipeline::quote(&audio.to_string_lossy()),
        crate::pipeline::quote(&cfg.model_path),
        model_preset(&cfg.model),
        cfg.chunk_ms.clamp(500, 10_000)
    );
    let p = gst::parse::launch(&desc)?.downcast::<gst::Pipeline>().map_err(|_| anyhow::anyhow!("keine Pipeline"))?;
    if let Some(asr) = p.by_name("asr") {
        if !cfg.language.is_empty() {
            asr.set_property("language", cfg.language.as_str());
        }
        let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8) as i32;
        asr.set_property("n-threads", n);
    }
    let sink = p.by_name("t").unwrap().downcast::<gst_app::AppSink>().unwrap();
    p.set_state(gst::State::Playing)?;
    let mut text = Vec::new();
    let mut delays = Vec::new();
    let bus = p.bus().unwrap();
    loop {
        if let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_mseconds(100)) {
            if let Some(buf) = sample.buffer() {
                let t = buf.map_readable().map(|m| String::from_utf8_lossy(m.as_slice()).trim().to_string()).unwrap_or_default();
                if !t.is_empty() {
                    if let (Some(now), Some(pts)) = (p.current_running_time(), buf.pts()) {
                        let end = pts + buf.duration().unwrap_or(gst::ClockTime::ZERO);
                        delays.push(now.nseconds().saturating_sub(end.nseconds()) as f64 / 1e6);
                    }
                    text.push(apply_replacements(&t, &cfg.replacements));
                }
            }
        }
        if let Some(msg) = bus.pop() {
            match msg.view() {
                gst::MessageView::Eos(_) => break,
                gst::MessageView::Error(e) => anyhow::bail!("{}", e.error()),
                _ => {}
            }
        }
        if sink.is_eos() {
            break;
        }
    }
    p.set_state(gst::State::Null)?;
    let text = text.join(" ");
    let mut sorted = delays.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Ok(FileMeasurement {
        wer: reference.map(|r| wer(r, &text)),
        median_delay_ms: sorted.get(sorted.len() / 2).copied(),
        delays_ms: delays,
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacements_whole_words_case_insensitive() {
        let list = vec![
            Replacement { from: "lars zumpe".into(), to: "Lars Zumpe".into() },
            Replacement { from: "srt".into(), to: "SRT".into() },
        ];
        assert_eq!(apply_replacements("Willkommen bei LARS ZUMPE und srt-Technik", &list), "Willkommen bei Lars Zumpe und SRT-Technik");
        assert_eq!(apply_replacements("Ein Sortiment", &list), "Ein Sortiment", "nur ganze Wörter");
        assert_eq!(apply_replacements("Größe srt", &list), "Größe SRT");
    }

    #[test]
    fn phraser_builds_608_lines() {
        let mut p = Phraser::default();
        let s = 1_000_000_000u64;
        let mut lines = Vec::new();
        for (i, w) in ["Guten", "Abend", "und", "herzlich", "willkommen", "zum", "Gottesdienst."].iter().enumerate() {
            lines.extend(p.push(w, Some(i as u64 * s / 3), Some(i as u64 * s / 3 + s / 4)));
        }
        let texts: Vec<_> = lines.iter().map(|l| l.text.clone()).collect();
        assert_eq!(texts, vec!["Guten Abend und herzlich", "willkommen zum Gottesdienst."]);
        assert!(texts.iter().all(|t| t.chars().count() <= LINE_CHARS));
        assert_eq!(lines[1].first_word_ns, Some(4 * s / 3));
        // Pause > 0,8 s trennt
        let l = p.push("Heute", Some(10 * s), Some(10 * s + s / 4));
        assert!(l.is_empty());
        let l = p.push("hören", Some(12 * s), None);
        assert_eq!(l[0].text, "Heute");
        // Geräusche in Klammern fallen weg, auch über mehrere Wörter
        for w in ["[Musik]", "[", "Aufregende", "Musik", "]", "(Applaus)"] {
            assert!(p.push(w, Some(13 * s), None).is_empty());
        }
        let l = p.push("Amen.", Some(13 * s), None);
        let t: Vec<_> = l.iter().map(|x| x.text.as_str()).collect();
        assert_eq!(t, vec!["hören", "Amen."], "Pause trennt, Klammern verschwinden");
    }

    #[test]
    fn word_error_rate() {
        assert_eq!(wer("Guten Abend, liebe Gemeinde.", "guten abend liebe gemeinde"), 0.0);
        assert!((wer("eins zwei drei vier", "eins zwo drei") - 0.5).abs() < 1e-9);
        assert_eq!(wer("", ""), 0.0);
    }

    #[test]
    fn presets_and_urls() {
        assert_eq!(model_preset("small"), "small");
        assert_eq!(model_preset("large-v3-turbo"), "large-v3-turbo");
        assert!(model_url("small").ends_with("ggml-small.bin"));
        assert!(AsrRunner::description(&AsrConfig { model_path: "/m/ggml-small.bin".into(), ..Default::default() }).contains("model-preset=small"));
    }
}
