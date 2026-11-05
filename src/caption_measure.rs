//! Live-Messung der Untertitel: eine Videodatei läuft in Echtzeit wie ein
//! Live-Eingang durch Encoder (Spracherkennung, 608 in der SEI, Aufnahme mit
//! Untertiteldatei, Einbrennen) über SRT zum Decoder. Gemessen werden
//! Wortfehlerrate und Verzug je Ausgabeweg gegen den Sprachbeginn.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::asr::wer;
use crate::channel::{Channel, ChannelOptions, ChildMsg};
use crate::config::{BurnTarget, CaptionMode, CaptionSource, DecoderConfig, EncoderConfig, InputKind, OutputKind, SrtMode};

pub struct LiveMeasure {
    pub video: PathBuf,
    /// Sprachbeginn je Äußerung (s im Video) und Text, TAB-getrennt
    pub onsets: PathBuf,
    pub reference: PathBuf,
    pub language: String,
    pub model_path: String,
    pub translate: bool,
    pub mode: CaptionMode,
    pub chunk_ms: u32,
    pub word_mode: bool,
    pub paced: bool,
    /// zweite Sprache (Übersetzung auf CC3) zusätzlich; Referenz dafür
    pub second: Option<PathBuf>,
    /// Modell für die Übersetzung (sonst Vorgabe)
    pub second_model: Option<String>,
    pub work_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Stats {
    pub n: usize,
    pub median_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub min_ms: Option<f64>,
    pub max_ms: Option<f64>,
}

pub fn stats(mut v: Vec<f64>) -> Stats {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |p: f64| v.get(((v.len() as f64 - 1.0) * p).round() as usize).copied().map(|x| x.round());
    Stats { n: v.len(), median_ms: q(0.5), p95_ms: q(0.95), min_ms: v.first().copied().map(f64::round), max_ms: v.last().copied().map(f64::round) }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Report {
    pub video: String,
    pub language: String,
    pub translate: bool,
    pub mode: String,
    pub chunk_ms: u32,
    pub word_mode: bool,
    pub paced: bool,
    pub model: String,
    pub utterances: usize,
    pub lines: usize,
    pub wer_asr: f64,
    pub wer_decoder: f64,
    /// Zeile erkannt (am Encoder, vor dem Video-Encoder) – gegen das erste Wort der Zeile
    pub delay_asr_line: Stats,
    /// Zeile am Decoder angezeigt (608 aus der SEI) – gegen das erste Wort der Zeile
    pub delay_decoder_line: Stats,
    /// erstes Wort einer Äußerung (Sprachbeginn laut Stille-Erkennung) bis zur Anzeige am Decoder
    pub delay_decoder_first_word: Stats,
    /// Untertiteldatei neben der Aufnahme: Anfangszeit gegen das erste Wort der Zeile
    pub delay_sidecar_line: Stats,
    /// 608 in der Aufnahme (ffmpeg liest sie aus dem TS): Anfangszeit gegen das erste Wort
    pub delay_recording_608_line: Stats,
    pub recording: String,
    pub sidecar_entries: usize,
    pub recording_608_entries: usize,
    pub asr_text: String,
    /// Übersetzung (CC3): Zeile übersetzt bzw. am CC3-Decoder angezeigt – gegen das erste Wort
    pub delay_cc3_asr_line: Stats,
    pub delay_cc3_decoder_line: Stats,
    /// Zusatzverzug der Übersetzung: Median CC3 − Median CC1 (Erkennung bzw. Anzeige)
    pub extra_delay_cc3_asr_ms: Option<f64>,
    pub extra_delay_cc3_decoder_ms: Option<f64>,
    pub wer_cc3: Option<f64>,
    /// WER der übersetzten Zeilen (am Encoder) gegen die englische Referenz
    pub wer_cc3_asr: Option<f64>,
    pub cc3_text: String,
}

fn words(s: &str) -> Vec<String> {
    s.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect()
}

/// Anteil der Wörter von `a`, die in `b` vorkommen.
fn overlap(a: &str, b: &str) -> f64 {
    let (wa, wb) = (words(a), words(b));
    if wa.is_empty() {
        return 0.0;
    }
    wa.iter().filter(|w| wb.contains(w)).count() as f64 / wa.len() as f64
}

fn parse_srt_times(srt: &str) -> Vec<(f64, String)> {
    let mut out = Vec::new();
    for block in srt.split("\n\n") {
        let lines: Vec<&str> = block.lines().collect();
        let Some(i) = lines.iter().position(|l| l.contains("-->")) else { continue };
        let start = lines[i].split("-->").next().unwrap_or("").trim().replace(',', ".");
        let p: Vec<f64> = start.split(':').filter_map(|x| x.parse().ok()).collect();
        if p.len() != 3 {
            continue;
        }
        let text = strip_markup(&lines[i + 1..].join(" "));
        out.push((p[0] * 3600.0 + p[1] * 60.0 + p[2], text.trim().to_string()));
    }
    out
}

/// `<font …>` und `{\an7}` aus ffmpeg-SRT entfernen.
fn strip_markup(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '<' | '{' => depth += 1,
            '>' | '}' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn duration_s(video: &Path) -> Result<f64> {
    let o = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(video)
        .output()
        .context("ffprobe fehlt")?;
    Ok(String::from_utf8_lossy(&o.stdout).trim().parse()?)
}

pub fn run(m: &LiveMeasure) -> Result<Report> {
    let _ = gstreamer::init();
    let total = duration_s(&m.video)?;
    let onsets: Vec<(f64, String)> = std::fs::read_to_string(&m.onsets)?
        .lines()
        .filter_map(|l| l.split_once('\t').map(|(a, b)| (a.parse().unwrap_or(0.0), b.to_string())))
        .collect();
    let reference = std::fs::read_to_string(&m.reference)?;
    let port = crate::latency::free_port();
    let rec_dir = m.work_dir.join(format!("aufnahme-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&rec_dir);

    let mut enc = EncoderConfig::default();
    enc.input.kind = InputKind::File;
    enc.input.file = m.video.to_string_lossy().to_string();
    enc.video.width = 1280;
    enc.video.height = 720;
    enc.video.fps = 25;
    enc.video.bitrate_kbps = 4000;
    enc.srt.mode = SrtMode::Listener;
    enc.srt.port = port;
    enc.captions.source = CaptionSource::Speech;
    enc.captions.mode = m.mode;
    enc.captions.asr.chunk_ms = m.chunk_ms;
    enc.captions.asr.word_mode = m.word_mode;
    enc.captions.asr.paced = m.paced;
    enc.captions.asr.model_path = m.model_path.clone();
    enc.captions.asr.model = Path::new(&m.model_path).file_stem().map(|s| s.to_string_lossy().trim_start_matches("ggml-").to_string()).unwrap_or_default();
    enc.captions.asr.language = m.language.clone();
    enc.captions.asr.translate = m.translate;
    enc.record.enabled = true;
    enc.record.dir = rec_dir.to_string_lossy().to_string();
    enc.graphics.burn = BurnTarget::All;
    enc.graphics.frame_port = 0;
    enc.captions.second.enabled = m.second.is_some();
    if let Some(sm) = &m.second_model {
        enc.captions.second.model = sm.clone();
    }
    let mut e = Channel::start_encoder(&enc, &[], &ChannelOptions::default())?;
    let mut dec = DecoderConfig::default();
    dec.srt.host = "127.0.0.1".into();
    dec.srt.port = port;
    dec.output = OutputKind::None;
    let mut d = Channel::start_decoder(&dec, &ChannelOptions::default())?;
    // zweiter Empfänger auf CC3
    let mut d3 = if m.second.is_some() {
        let mut dec3 = dec.clone();
        dec3.caption_channel = 3;
        Some(Channel::start_decoder(&dec3, &ChannelOptions::default())?)
    } else {
        None
    };

    let end = Instant::now() + Duration::from_secs_f64(total + 8.0);
    let mut recording = None;
    while Instant::now() < end {
        if let Some(d3) = d3.as_mut() {
            let _ = d3.poll();
        }
        for ch in [&mut e, &mut d] {
            for msg in ch.poll() {
                match msg {
                    ChildMsg::Fatal { text } => anyhow::bail!("Kanal: {text}"),
                    ChildMsg::Status(s) if s.recording.is_some() => recording = s.recording,
                    _ => {}
                }
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let phrases = e.asr_phrases();
    let phrases2 = e.asr2_phrases();
    let shown: Vec<(u64, String)> = d.captions_shown().into_iter().filter_map(|(at, t)| e.running_ns_at(at).map(|rt| (rt, t))).collect();
    let pushed: Vec<(u64, &crate::asr::Phrase)> = phrases.iter().filter_map(|p| e.running_ns_at(p.at).map(|rt| (rt, p))).collect();
    let pushed2: Vec<(u64, &crate::asr::Phrase)> = phrases2.iter().filter_map(|p| e.running_ns_at(p.at).map(|rt| (rt, p))).collect();
    let shown3: Vec<(u64, String)> = d3
        .as_ref()
        .map(|d3| d3.captions_shown().into_iter().filter_map(|(at, t)| e.running_ns_at(at).map(|rt| (rt, t))).collect())
        .unwrap_or_default();
    e.stop();
    d.stop();
    if let Some(d3) = d3.as_mut() {
        d3.stop();
    }

    let ms = |ns: i128| ns as f64 / 1e6;
    let mut asr_line = Vec::new();
    let mut dec_line = Vec::new();
    let mut matched: Vec<(u64, u64)> = Vec::new(); // (erstes Wort ns, Anzeige ns)
    for (at, p) in &pushed {
        let Some(fw) = p.first_word_ns else { continue };
        asr_line.push(ms(*at as i128 - fw as i128));
        if let Some((rt, _)) = shown.iter().find(|(rt, t)| *rt + 500_000_000 >= *at && overlap(&p.text, t) >= 0.5) {
            dec_line.push(ms(*rt as i128 - fw as i128));
            matched.push((fw, *rt));
        }
    }
    // erstes Wort je Äußerung: früheste Anzeige einer Zeile, deren erstes Wort in die Äußerung fällt
    let mut first_word = Vec::new();
    for (i, (on, _)) in onsets.iter().enumerate() {
        let lo = ((on - 0.5) * 1e9) as u64;
        let hi = onsets.get(i + 1).map(|(n, _)| ((n - 0.5) * 1e9) as u64).unwrap_or(u64::MAX);
        if let Some(t) = matched.iter().filter(|(fw, _)| *fw >= lo && *fw < hi).map(|(_, rt)| *rt).min() {
            first_word.push(ms(t as i128 - (on * 1e9) as i128));
        }
    }
    // Aufnahme: Untertiteldatei daneben und 608 im TS
    let rec = recording.unwrap_or_default();
    let sidecar = parse_srt_times(&std::fs::read_to_string(Path::new(&rec).with_extension("srt")).unwrap_or_default());
    let start_pts = {
        let o = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=start_time", "-of", "csv=p=0", &rec])
            .output()
            .ok();
        o.and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<f64>().ok()).unwrap_or(0.0)
    };
    let cc608 = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("movie={rec}[out0+subcc]"))
        .args(["-map", "0:1", "-f", "srt", "-"])
        .output()
        .map(|o| parse_srt_times(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default();
    let line_delay = |entries: &[(f64, String)], offset_s: f64| -> Vec<f64> {
        pushed
            .iter()
            .filter_map(|(_, p)| {
                let fw = p.first_word_ns? as f64 / 1e9;
                entries.iter().find(|(t, x)| *t + offset_s + 0.5 >= fw && overlap(&p.text, x) >= 0.5).map(|(t, _)| (t + offset_s - fw) * 1000.0)
            })
            .collect()
    };
    let asr_text = phrases.iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join(" ");
    // Roll-up zeigt frühere Zeilen erneut: nur die neu hinzugekommenen Wörter zählen
    let mut dec_words: Vec<String> = Vec::new();
    let mut prev: Vec<String> = Vec::new();
    for (_, t) in &shown {
        let cur = words(t);
        let mut keep = 0;
        for k in (1..=prev.len().min(cur.len())).rev() {
            if prev[prev.len() - k..] == cur[..k] {
                keep = k;
                break;
            }
        }
        if cur != prev {
            dec_words.extend(cur[keep..].iter().cloned());
        }
        prev = cur;
    }
    let dec_texts = vec![dec_words.join(" ")];
    let (delay_asr, delay_dec) = (stats(asr_line.clone()), stats(dec_line.clone()));
    // Übersetzung: dieselben Zeilenmaße wie für CC1
    let mut cc3_asr = Vec::new();
    let mut cc3_dec = Vec::new();
    for (at, p) in &pushed2 {
        let Some(fw) = p.first_word_ns else { continue };
        cc3_asr.push(ms(*at as i128 - fw as i128));
        if let Some((rt, _)) = shown3.iter().find(|(rt, t)| *rt + 500_000_000 >= *at && overlap(&p.text, t) >= 0.5) {
            cc3_dec.push(ms(*rt as i128 - fw as i128));
        }
    }
    let (c3a, c3d) = (stats(cc3_asr), stats(cc3_dec));
    // neu hinzugekommene Wörter wie beim Decoder-Text oben
    let mut cc3_words: Vec<String> = Vec::new();
    let mut prev: Vec<String> = Vec::new();
    for (_, t) in &shown3 {
        let cur = words(t);
        if cur.len() >= prev.len() && cur[..prev.len()] == prev[..] {
            // dieselbe Zeile wächst: erst beim Abschluss zählen
            prev = cur;
            continue;
        }
        cc3_words.extend(prev.iter().cloned());
        prev = cur;
    }
    cc3_words.extend(prev);
    let cc3_text = cc3_words.join(" ");
    let wer_cc3 = m.second.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).map(|r| wer(&r, &cc3_text));
    Ok(Report {
        video: m.video.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        language: m.language.clone(),
        translate: m.translate,
        mode: m.mode.gst_nick().into(),
        chunk_ms: m.chunk_ms,
        word_mode: m.word_mode,
        paced: m.paced,
        model: enc.captions.asr.model.clone(),
        utterances: onsets.len(),
        lines: phrases.len(),
        wer_asr: wer(&reference, &asr_text),
        wer_decoder: wer(&reference, &dec_texts.join(" ")),
        delay_asr_line: delay_asr.clone(),
        delay_decoder_line: delay_dec.clone(),
        delay_decoder_first_word: stats(first_word),
        delay_sidecar_line: stats(line_delay(&sidecar, 0.0)),
        delay_recording_608_line: stats(line_delay(&cc608, start_pts)),
        sidecar_entries: sidecar.len(),
        recording_608_entries: cc608.len(),
        recording: rec,
        asr_text,
        extra_delay_cc3_asr_ms: c3a.median_ms.zip(delay_asr.median_ms).map(|(a, b)| a - b),
        extra_delay_cc3_decoder_ms: c3d.median_ms.zip(delay_dec.median_ms).map(|(a, b)| a - b),
        delay_cc3_asr_line: c3a,
        delay_cc3_decoder_line: c3d,
        wer_cc3,
        wer_cc3_asr: m.second.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).map(|r| wer(&r, &phrases2.iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join(" "))),
        cc3_text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        let s = stats((1..=100).map(|x| x as f64).collect());
        assert_eq!((s.median_ms, s.p95_ms, s.n), (Some(51.0), Some(95.0), 100));
        assert_eq!(stats(vec![]).median_ms, None);
    }

    #[test]
    fn srt_parsing_and_overlap() {
        let srt = "1\n00:00:02,790 --> 00:00:04,790\n<font face=\"Monospace\">{\\an7}Zeile Nummer 0</font>\n\n2\n00:01:04,000 --> 00:01:06,000\nHallo Welt\n\n";
        let e = parse_srt_times(srt);
        assert_eq!(e.len(), 2);
        assert!((e[0].0 - 2.79).abs() < 1e-9 && e[0].1 == "Zeile Nummer 0");
        assert!((e[1].0 - 64.0).abs() < 1e-9 && e[1].1 == "Hallo Welt");
        assert_eq!(overlap("Guten Abend zusammen", "guten abend"), 2.0 / 3.0);
    }
}
