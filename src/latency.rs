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
}

impl Default for MeasureOptions {
    fn default() -> Self {
        Self { codec: Codec::H264, encoder: "auto".into(), srt_latency_ms: 120, port: 0, rounds: 7, width: 1280, height: 720, fps: 50, audio: true, decoder: "auto".into() }
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
    let mut samples = Vec::new();
    for _ in 0..opts.rounds {
        let t0 = Instant::now();
        set_pattern(&enc, "white");
        if !wait_until(&mut enc, &mut dec, Duration::from_secs(5), |d| d.counters().luma_rise().is_some_and(|t| t > t0))? {
            bail!("Weißsprung kam nicht an");
        }
        let t1 = dec.counters().luma_rise().unwrap();
        samples.push((t1 - t0).as_secs_f64() * 1000.0);
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
    Ok(LatencyReport {
        encoder: encoder.unwrap_or_default(),
        srt_latency_ms: opts.srt_latency_ms,
        median_ms: r(sorted[sorted.len() / 2]),
        min_ms: r(sorted[0]),
        max_ms: r(*sorted.last().unwrap()),
        samples_ms: samples.into_iter().map(r).collect(),
        encoder_pipeline_ms: el,
        decoder_pipeline_ms: dl,
    })
}

pub fn ensure_init() {
    let _ = gst::init();
}
