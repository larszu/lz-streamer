//! Latenzmessung ohne Kamera: Encoder (Testbild) und Decoder laufen im
//! selben Prozess über eine echte SRT-Verbindung auf diesem Rechner. Das
//! Testbild springt von Schwarz auf Weiß; gemessen wird, bis die mittlere
//! Helligkeit am Decoder-Ausgang (vor der Darstellung, mit Taktsynchronisation)
//! über 50 % steigt. Nicht enthalten: Kamera, Capture-Karte, Bildschirm, Netz.

use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use gstreamer as gst;
use gst::prelude::*;
use serde::Serialize;

use crate::channel::{Channel, ChannelOptions, ChildMsg};
use crate::config::{Codec, DecoderConfig, EncoderConfig, InputKind, OutputKind, SrtMode};

#[derive(Debug, Clone, Serialize)]
pub struct LatencyReport {
    pub encoder: String,
    pub srt_latency_ms: u32,
    pub samples_ms: Vec<f64>,
    pub median_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    /// Von GStreamer gemeldete Pipeline-Latenz (Puffer, die bewusst eingeplant sind)
    pub encoder_pipeline_ms: Option<f64>,
    pub decoder_pipeline_ms: Option<f64>,
    /// Median je Station
    pub stages: StageBreakdown,
}

pub fn pipeline_latency_ms(p: &gst::Pipeline) -> Option<f64> {
    let mut q = gst::query::Latency::new();
    p.query(&mut q).then(|| q.result().1.mseconds() as f64)
}

pub struct MeasureOptions {
    pub codec: Codec,
    pub encoder: String,
    pub srt_latency_ms: u32,
    pub port: u16,
    pub rounds: usize,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub audio: bool,
    pub decoder: String,
    pub demux_latency_ms: u32,
    pub output_buffer_ms: u32,
}

impl Default for MeasureOptions {
    fn default() -> Self {
        Self { codec: Codec::H264, encoder: "auto".into(), srt_latency_ms: 120, port: 0, rounds: 7, width: 1280, height: 720, fps: 50, audio: true, decoder: "auto".into(), demux_latency_ms: 40, output_buffer_ms: 60 }
    }
}

pub fn free_port() -> u16 {
    // UDP-Port, den das System gerade frei meldet
    std::net::UdpSocket::bind("127.0.0.1:0").and_then(|s| s.local_addr()).map(|a| a.port()).unwrap_or(9876)
}

fn pump(ch: &mut Channel) -> Result<()> {
    for m in ch.poll() {
        if let ChildMsg::Fatal { text } = m {
            bail!("{text}");
        }
    }
    Ok(())
}

pub fn pair(opts: &MeasureOptions) -> Result<(Channel, Channel)> {
    let port = if opts.port == 0 { free_port() } else { opts.port };
    let mut enc = EncoderConfig::default();
    enc.input.kind = InputKind::Test;
    enc.video.codec = opts.codec;
    enc.video.encoder = opts.encoder.clone();
    enc.video.width = opts.width;
    enc.video.height = opts.height;
    enc.video.fps = opts.fps;
    enc.video.gop_ms = 500;
    enc.audio.enabled = opts.audio;
    enc.srt.mode = SrtMode::Listener;
    enc.srt.port = port;
    enc.srt.latency_ms = opts.srt_latency_ms;
    let encoder = Channel::start_encoder(&enc, &[], &ChannelOptions::default())?;

    let mut dec = DecoderConfig::default();
    dec.srt.mode = SrtMode::Caller;
    dec.srt.host = "127.0.0.1".into();
    dec.srt.port = port;
    dec.srt.latency_ms = opts.srt_latency_ms;
    dec.output = OutputKind::None;
    dec.decoder = opts.decoder.clone();
    dec.demux_latency_ms = opts.demux_latency_ms;
    dec.output_buffer_ms = opts.output_buffer_ms;
    let decoder = Channel::start_decoder(&dec, &ChannelOptions::default())?;
    Ok((encoder, decoder))
}

fn set_pattern(enc: &Channel, pattern: &str) {
    if let Some(src) = enc.pipeline().by_name("testsrc") {
        src.set_property_from_str("pattern", pattern);
    }
}

fn wait_until(enc: &mut Channel, dec: &mut Channel, timeout: Duration, mut f: impl FnMut(&Channel) -> bool) -> Result<bool> {
    let end = Instant::now() + timeout;
    let mut n = 0u32;
    while Instant::now() < end {
        if f(dec) {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(2));
        n += 1;
        // Busse regelmäßig leeren, damit Fehler sofort auffallen
        if n % 100 == 0 {
            pump(enc)?;
            pump(dec)?;
        }
    }
    Ok(false)
}


/// Zeitpunkte des Weißsprungs an Zwischenstationen (für die Aufschlüsselung).
#[derive(Default)]
struct Stamps {
    t0: Option<Instant>,
    enc_in: Option<(Instant, Option<gst::ClockTime>)>,
    enc_out: Option<Instant>,
    mux_out: Option<Instant>,
    dec_out: Option<Instant>,
}

type SharedStamps = std::sync::Arc<std::sync::Mutex<Stamps>>;

/// Mittlere Helligkeit der ersten Zeilen der Y-Ebene (NV12/I420).
fn y_mean(b: &gst::BufferRef) -> Option<f64> {
    let map = b.map_readable().ok()?;
    let s = map.as_slice();
    let n = s.len().min(64 * 1024);
    if n == 0 {
        return None;
    }
    Some(s[..n].iter().step_by(16).map(|&x| x as f64).sum::<f64>() / (n / 16).max(1) as f64)
}

fn probe_luma(pad: &gst::Pad, st: &SharedStamps, encoder_side: bool) {
    let st = st.clone();
    pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
        if let Some(gst::PadProbeData::Buffer(b)) = &info.data {
            let mut s = st.lock().unwrap();
            let pending = if encoder_side { s.enc_in.is_none() } else { s.dec_out.is_none() };
            if s.t0.is_some() && pending && y_mean(b).is_some_and(|y| y > 128.0) {
                if encoder_side {
                    s.enc_in = Some((Instant::now(), b.pts()));
                } else {
                    s.dec_out = Some(Instant::now());
                }
            }
        }
        gst::PadProbeReturn::Ok
    });
}

fn probe_pts(pad: &gst::Pad, st: &SharedStamps, mux: bool) {
    let st = st.clone();
    pad.add_probe(gst::PadProbeType::BUFFER | gst::PadProbeType::BUFFER_LIST, move |_, info| {
        let pts = match &info.data {
            Some(gst::PadProbeData::Buffer(b)) => b.pts(),
            Some(gst::PadProbeData::BufferList(l)) => l.get(0).and_then(|b| b.pts()),
            _ => None,
        };
        let mut s = st.lock().unwrap();
        let Some((_, Some(p))) = s.enc_in else { return gst::PadProbeReturn::Ok };
        let slot = if mux { &mut s.mux_out } else { &mut s.enc_out };
        if slot.is_none() && pts.is_some_and(|x| x >= p) {
            *slot = Some(Instant::now());
        }
        gst::PadProbeReturn::Ok
    });
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct StageBreakdown {
    /// Befehl „Weiß“ → Bild am Encoder-Eingang (Erzeugung, Bildtakt, Umschalter)
    pub source_ms: f64,
    /// Encoder-Eingang → codiertes Bild
    pub encode_ms: f64,
    /// codiertes Bild → TS-Paket am SRT-Sender (Parser, Mux)
    pub mux_ms: f64,
    /// SRT-Sender → decodiertes Bild (Netz, SRT-Puffer, Demux, Decoder)
    pub transport_decode_ms: f64,
    /// decodiertes Bild → Ausgabe (Taktwarten an der Senke)
    pub output_ms: f64,
}

pub fn measure(opts: &MeasureOptions) -> Result<LatencyReport> {
    let (mut enc, mut dec) = pair(opts)?;
    let encoder = enc.poll().into_iter().find_map(|m| match m {
        ChildMsg::Status(s) => s.encoder,
        _ => None,
    });
    set_pattern(&enc, "black");
    if !wait_until(&mut enc, &mut dec, Duration::from_secs(15), |d| d.counters().luma().is_some_and(|l| l < 40.0))? {
        bail!("Decoder zeigt nach 15 s kein schwarzes Bild – keine Verbindung?");
    }
    std::thread::sleep(Duration::from_millis(500));
    let st: SharedStamps = Default::default();
    if let Some(v) = enc.pipeline().by_name("venc") {
        probe_luma(&v.static_pad("sink").unwrap(), &st, true);
        probe_pts(&v.static_pad("src").unwrap(), &st, false);
    }
    if let Some(p) = enc.pipeline().by_name("srtout").and_then(|e| e.static_pad("sink")) {
        probe_pts(&p, &st, true);
    }
    if let Some(p) = dec.pipeline().by_name("vdec").and_then(|e| e.static_pad("src")) {
        probe_luma(&p, &st, false);
    }
    let mut samples = Vec::new();
    let mut parts: Vec<[f64; 5]> = Vec::new();
    for _ in 0..opts.rounds {
        let t0 = Instant::now();
        *st.lock().unwrap() = Stamps { t0: Some(t0), ..Default::default() };
        set_pattern(&enc, "white");
        if !wait_until(&mut enc, &mut dec, Duration::from_secs(5), |d| d.counters().luma_rise().is_some_and(|t| t > t0))? {
            bail!("Weißsprung kam nicht an");
        }
        let t1 = dec.counters().luma_rise().unwrap();
        samples.push((t1 - t0).as_secs_f64() * 1000.0);
        {
            let s = st.lock().unwrap();
            let ms = |a: Instant, b: Instant| b.saturating_duration_since(a).as_secs_f64() * 1000.0;
            if let (Some((ei, _)), Some(eo), Some(mo), Some(dd)) = (s.enc_in, s.enc_out, s.mux_out, s.dec_out) {
                parts.push([ms(t0, ei), ms(ei, eo), ms(eo, mo), ms(mo, dd), ms(dd, t1)]);
            }
        }
        st.lock().unwrap().t0 = None;
        set_pattern(&enc, "black");
        wait_until(&mut enc, &mut dec, Duration::from_secs(5), |d| d.counters().luma().is_some_and(|l| l < 40.0))?;
        std::thread::sleep(Duration::from_millis(300));
    }
    let (el, dl) = (pipeline_latency_ms(enc.pipeline()), pipeline_latency_ms(dec.pipeline()));
    enc.stop();
    dec.stop();
    let mut sorted = samples.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let r = |x: f64| (x * 10.0).round() / 10.0;
    let med = |i: usize| -> f64 {
        let mut v: Vec<f64> = parts.iter().map(|p| p[i]).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v.get(v.len() / 2).copied().map(r).unwrap_or(0.0)
    };
    let stages = StageBreakdown { source_ms: med(0), encode_ms: med(1), mux_ms: med(2), transport_decode_ms: med(3), output_ms: med(4) };
    Ok(LatencyReport {
        encoder: encoder.unwrap_or_default(),
        srt_latency_ms: opts.srt_latency_ms,
        median_ms: r(sorted[sorted.len() / 2]),
        min_ms: r(sorted[0]),
        max_ms: r(*sorted.last().unwrap()),
        samples_ms: samples.into_iter().map(r).collect(),
        encoder_pipeline_ms: el,
        decoder_pipeline_ms: dl,
        stages,
    })
}

pub fn ensure_init() {
    let _ = gst::init();
}
