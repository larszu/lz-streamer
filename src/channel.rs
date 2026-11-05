//! Ein laufender Kanal (Encoder oder Decoder). Läuft im Normalbetrieb in
//! einem eigenen Kindprozess (siehe `supervisor.rs`), lässt sich für Tests
//! und Messungen aber auch direkt im selben Prozess betreiben.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;
use serde::{Deserialize, Serialize};

use crate::caps::{self, EncoderInfo};
use crate::config::{Codec, Config, DecoderConfig, EncoderConfig, OutputKind, Role, StandbyPattern};
use crate::devices::{self, CaptureDevice};
use crate::pipeline::{self, EncoderParts, ResolvedInput};
use crate::stats::{self, SrtSummary};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ChannelStatus {
    pub role: Role,
    pub input: String,
    pub encoder: Option<String>,
    pub encoder_label: Option<String>,
    pub encoder_hw: bool,
    pub codec: Option<Codec>,
    /// "ok", "lost" oder "unknown"
    pub signal: String,
    pub standby: bool,
    pub frames: u64,
    pub fps: f64,
    pub bytes: u64,
    pub bitrate_kbps: f64,
    pub resolution: Option<String>,
    /// Vom Eingang erkanntes Format (DeckLink) bzw. Erzeugungsformat (Testbild)
    pub input_format: Option<String>,
    pub srt: SrtSummary,
    pub rtmp: Option<String>,
    /// weitere Ausgänge
    pub outputs: Vec<OutputStatus>,
    /// laufende Aufnahmedatei
    pub recording: Option<String>,
    pub audio_tracks: Vec<TrackInfo>,
    /// Untertitel: CEA-608/708 im Strom
    pub captions: CaptionStatus,
    pub luma: Option<f64>,
    pub uptime_s: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CaptionStatus {
    /// Spracherkennung, falls Quelle „speech“
    pub asr: Option<crate::asr::AsrStatus>,
    /// Bilder mit Untertitel-Daten seit Start
    pub frames: u64,
    /// in den letzten 2 s Untertitel-Daten gesehen
    pub present: bool,
    /// zuletzt gesendeter bzw. empfangener Text
    pub text: Option<String>,
}

#[derive(Default)]
struct CaptionCounter {
    frames: AtomicU64,
    last: Mutex<Option<Instant>>,
    text: Mutex<Option<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OutputStatus {
    pub name: String,
    pub target: String,
    pub connected: Option<bool>,
    pub rtt_ms: Option<f64>,
    pub lost: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TrackInfo {
    pub index: u32,
    pub language: Option<String>,
    pub name: Option<String>,
    pub selected: bool,
    /// Lautheit nach EBU R 128 (Encoder: je Spur hinter Matrix und Pegel; Decoder: ausgegebene Spur)
    pub loudness: Option<crate::loudness::Loudness>,
    #[serde(skip)]
    pub demux_pad: String,
    #[serde(skip)]
    pub sel_pad: String,
}

/// Tonspur wählen: gewünschte Sprache, sonst die n-te Spur mit Sprachkennung,
/// sonst die n-te überhaupt.
fn select_audio(asel: &gst::Element, list: &mut [TrackInfo], language: &str, index: u32) {
    if list.is_empty() {
        return;
    }
    let want = crate::audio::language_key(language);
    let by_lang = (!language.trim().is_empty())
        .then(|| list.iter().position(|t| t.language.as_deref().map(crate::audio::language_key).as_deref() == Some(want.as_str())))
        .flatten();
    let tagged: Vec<usize> = (0..list.len()).filter(|&i| list[i].language.is_some()).collect();
    let pick = by_lang
        .or_else(|| tagged.get(index as usize).copied())
        .unwrap_or_else(|| (index as usize).min(list.len() - 1));
    for (i, t) in list.iter_mut().enumerate() {
        t.selected = i == pick;
    }
    if let Some(p) = asel.static_pad(&list[pick].sel_pad) {
        if asel.property::<Option<gst::Pad>>("active-pad").as_ref() != Some(&p) {
            asel.set_property("active-pad", &p);
        }
    }
}

fn output_target(o: &crate::config::ExtraOutput) -> String {
    match o.kind {
        crate::config::OutputType::Srt => o.srt.uri(),
        crate::config::OutputType::Udp => format!("udp://{}:{}", o.host, o.port),
        crate::config::OutputType::Rtp => format!("rtp://{}:{}", o.host, o.port),
    }
}

/// Sprachkennung und Titel je Tonspur setzen (vor PLAYING).
fn configure_audio(pipeline: &gst::Pipeline, cfg: &EncoderConfig) {
    for (i, track) in crate::audio::effective_tracks(&cfg.audio.tracks).iter().enumerate() {
        if let Some(t) = pipeline.by_name(&format!("atag{i}")) {
            let title = track.name.replace('"', "'");
            t.set_property("tags", format!("language-code={},title=\"{title}\"", track.language));
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ChildMsg {
    Status(ChannelStatus),
    Event { level: String, text: String },
    Fatal { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "cmd", rename_all = "lowercase")]
pub enum ChildCmd {
    /// Signalverlust nachstellen (sperrt den Eingang vor dem Umschalter).
    Signal { lost: bool },
    Keyframe,
    /// Untertiteltext live einspeisen (Quelle „live“)
    Caption { text: String },
    Stop,
}

#[derive(Default)]
pub struct Counters {
    pub frames: AtomicU64,
    pub bytes: AtomicU64,
    luma_bits: AtomicU64,
    has_luma: AtomicBool,
    luma_rise: Mutex<Option<Instant>>,
    resolution: Mutex<Option<(i32, i32)>>,
}

impl Counters {
    pub fn luma(&self) -> Option<f64> {
        self.has_luma.load(Ordering::Relaxed).then(|| f64::from_bits(self.luma_bits.load(Ordering::Relaxed)))
    }
    /// Zeitpunkt, zu dem die mittlere Helligkeit zuletzt über 50 % stieg.
    pub fn luma_rise(&self) -> Option<Instant> {
        *self.luma_rise.lock().unwrap()
    }
    fn set_luma(&self, v: f64) {
        let prev = self.luma();
        self.luma_bits.store(v.to_bits(), Ordering::Relaxed);
        self.has_luma.store(true, Ordering::Relaxed);
        if v > 128.0 && prev.is_none_or(|p| p <= 128.0) {
            *self.luma_rise.lock().unwrap() = Some(Instant::now());
        }
    }
}

type Events = Arc<Mutex<Vec<ChildMsg>>>;

fn event(events: &Events, level: &str, text: impl Into<String>) {
    events.lock().unwrap().push(ChildMsg::Event { level: level.into(), text: text.into() });
}

pub struct Channel {
    role: Role,
    pipeline: gst::Pipeline,
    counters: Arc<Counters>,
    events: Events,
    started: Instant,
    last_tick: (Instant, u64, u64),
    rates: (f64, f64),
    input: String,
    encoder: Option<EncoderInfo>,
    codec: Arc<Mutex<Option<Codec>>>,
    prev_standby: Option<bool>,
    prev_connected: bool,
    rtmp: Option<RtmpSender>,
    stopped: bool,
    recording: Option<String>,
    outputs: Vec<(String, String, String)>,
    tracks: Arc<Mutex<Vec<TrackInfo>>>,
    cc: Arc<CaptionCounter>,
    asr: Option<crate::asr::AsrRunner>,
}

/// Text in die Live-Untertitelquelle `ccin` schieben (etwas in die Zukunft
/// gestempelt: cccombiner hängt Untertitel nur an Bilder, die noch nicht
/// ausgegeben sind).
fn push_caption_text(pipeline: &gst::Pipeline, cc: &CaptionCounter, text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return true;
    }
    let Some(src) = pipeline.by_name("ccin").and_then(|s| s.downcast::<gst_app::AppSrc>().ok()) else { return false };
    let mut buf = gst::Buffer::from_slice(text.as_bytes().to_vec());
    let ms = (text.chars().count() as u64 * 70).clamp(2000, 6000);
    let rt = pipeline.current_running_time().unwrap_or(gst::ClockTime::ZERO);
    let b = buf.get_mut().unwrap();
    b.set_pts(rt + gst::ClockTime::from_mseconds(300));
    b.set_duration(gst::ClockTime::from_mseconds(ms));
    let _ = src.push_buffer(buf);
    *cc.text.lock().unwrap() = Some(text.to_string());
    true
}

/// Bilder mit Untertitel-Meta zählen (Encoder: vor dem Encoder).
fn count_captions(pad: &gst::Pad, cc: &Arc<CaptionCounter>) {
    let cc = cc.clone();
    pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
        if let Some(gst::PadProbeData::Buffer(b)) = &info.data {
            if b.meta::<gstreamer_video::VideoCaptionMeta>().is_some() {
                cc.frames.fetch_add(1, Ordering::Relaxed);
                *cc.last.lock().unwrap() = Some(Instant::now());
            }
        }
        gst::PadProbeReturn::Ok
    });
}

/// Text aus `cea608tott` (Decoder) einsammeln.
fn hook_caption_text(bin: &gst::Bin, cc: &Arc<CaptionCounter>) {
    let Some(sink) = bin.by_name("ccsink").and_then(|s| s.downcast::<gst_app::AppSink>().ok()) else { return };
    let cc = cc.clone();
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |s| {
                let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                if let Some(buf) = sample.buffer() {
                    if let Ok(map) = buf.map_readable() {
                        let t = String::from_utf8_lossy(map.as_slice()).trim().to_string();
                        if !t.is_empty() {
                            cc.frames.fetch_add(1, Ordering::Relaxed);
                            *cc.last.lock().unwrap() = Some(Instant::now());
                            *cc.text.lock().unwrap() = Some(t);
                        }
                    }
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
}

#[derive(Debug, Clone, Default)]
pub struct ChannelOptions {
    pub preview_path: Option<PathBuf>,
}

fn count_bytes(pad: &gst::Pad, counters: &Arc<Counters>) {
    let c = counters.clone();
    pad.add_probe(gst::PadProbeType::BUFFER | gst::PadProbeType::BUFFER_LIST, move |_, info| {
        let n = match &info.data {
            Some(gst::PadProbeData::Buffer(b)) => b.size(),
            Some(gst::PadProbeData::BufferList(l)) => l.calculate_size(),
            _ => 0,
        };
        c.bytes.fetch_add(n as u64, Ordering::Relaxed);
        gst::PadProbeReturn::Ok
    });
}

fn count_frames(pad: &gst::Pad, counters: &Arc<Counters>) {
    let c = counters.clone();
    pad.add_probe(gst::PadProbeType::BUFFER, move |pad, _| {
        if c.frames.fetch_add(1, Ordering::Relaxed) % 25 == 0 {
            if let Some(caps) = pad.current_caps() {
                if let Some(s) = caps.structure(0) {
                    if let (Ok(w), Ok(h)) = (s.get::<i32>("width"), s.get::<i32>("height")) {
                        *c.resolution.lock().unwrap() = Some((w, h));
                    }
                }
            }
        }
        gst::PadProbeReturn::Ok
    });
}

fn measure_luma(pad: &gst::Pad, counters: &Arc<Counters>) {
    let c = counters.clone();
    pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
        if let Some(gst::PadProbeData::Buffer(b)) = &info.data {
            if let Ok(map) = b.map_readable() {
                let s = map.as_slice();
                if !s.is_empty() {
                    let sum: u64 = s.iter().map(|&x| x as u64).sum();
                    c.set_luma(sum as f64 / s.len() as f64);
                }
            }
        }
        gst::PadProbeReturn::Ok
    });
}

fn hook_preview(pipeline: &impl IsA<gst::Bin>, path: Option<PathBuf>) {
    let (Some(path), Some(sink)) = (path, pipeline.as_ref().by_name("preview")) else { return };
    let Ok(sink) = sink.downcast::<gst_app::AppSink>() else { return };
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |s| {
                let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                if let Some(buf) = sample.buffer() {
                    if let Ok(map) = buf.map_readable() {
                        let tmp = path.with_extension("tmp");
                        if std::fs::write(&tmp, map.as_slice()).is_ok() {
                            let _ = std::fs::rename(&tmp, &path);
                        }
                    }
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
}

fn set_props(el: &gst::Element, props: &[(&str, String)], events: &Events) {
    for (k, v) in props {
        if el.find_property(k).is_none() {
            tracing::debug!("{} kennt {k} nicht – übersprungen", el.name());
            continue;
        }
        if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| el.set_property_from_str(k, v))) {
            event(events, "warn", format!("{}: {k}={v} nicht gesetzt ({e:?})", el.name()));
        }
    }
}

struct StartError {
    source: String,
    text: String,
}

/// PLAYING abwarten und – beim Encoder – das erste codierte Bild. So fallen
/// Hardware-Encoder, die sich zwar laden, aber nicht öffnen lassen, sofort auf.
fn play_and_wait(pipeline: &gst::Pipeline, ready: impl Fn() -> bool, timeout: Duration) -> Result<(), StartError> {
    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| StartError { source: String::new(), text: format!("{e}") })?;
    let bus = pipeline.bus().expect("Pipeline ohne Bus");
    let deadline = Instant::now() + timeout;
    let mut playing = false;
    while Instant::now() < deadline {
        if let Some(msg) = bus.timed_pop(gst::ClockTime::from_mseconds(50)) {
            match msg.view() {
                gst::MessageView::Error(err) => {
                    return Err(StartError {
                        source: msg.src().map(|s| s.name().to_string()).unwrap_or_default(),
                        text: format!("{} ({})", err.error(), err.debug().unwrap_or_default()),
                    });
                }
                gst::MessageView::StateChanged(sc) if msg.src() == Some(pipeline.upcast_ref()) => {
                    playing |= sc.current() == gst::State::Playing;
                }
                _ => {}
            }
        }
        if playing && ready() {
            return Ok(());
        }
    }
    if playing {
        Ok(())
    } else {
        Err(StartError { source: String::new(), text: "Pipeline erreicht PLAYING nicht".into() })
    }
}

impl Channel {
    pub fn start(cfg: &Config, opts: &ChannelOptions) -> Result<Self> {
        match cfg.role {
            Role::Encoder => {
                let devices = if cfg.encoder.input.kind == crate::config::InputKind::Test {
                    Vec::new()
                } else {
                    devices::decklink_devices()
                };
                Self::start_encoder(&cfg.encoder, &devices, opts)
            }
            Role::Decoder => Self::start_decoder(&cfg.decoder, opts),
        }
    }

    pub fn start_encoder(cfg: &EncoderConfig, devices: &[CaptureDevice], opts: &ChannelOptions) -> Result<Self> {
        let events: Events = Arc::default();
        let input = devices::resolve_input(&cfg.input, devices);
        if !caps::element_available("fallbackswitch") {
            bail!("fallbackswitch fehlt (gst-plugins-rs) – bitte nachinstallieren");
        }
        let candidates = caps::encoder_candidates(cfg.video.codec, &cfg.video.encoder);
        if candidates.is_empty() {
            bail!("Kein Encoder für {:?} gefunden", cfg.video.codec);
        }
        let aac = caps::aac_encoder();
        if cfg.audio.enabled && aac.is_none() {
            event(&events, "warn", "Kein AAC-Encoder gefunden – Ton bleibt aus.");
        }
        let image_ok = cfg.standby.pattern == StandbyPattern::Image && Path::new(&cfg.standby.image_path).is_file();
        if cfg.standby.pattern == StandbyPattern::Image && !image_ok && !cfg.standby.image_path.is_empty() {
            event(&events, "warn", format!("Standbild {} nicht lesbar – nehme Farbbalken.", cfg.standby.image_path));
        }
        let rtmp = pipeline::rtmp_active(cfg) && caps::element_available("rtmp2sink");
        let record_location = if cfg.record.enabled {
            let dir = if cfg.record.dir.trim().is_empty() {
                std::env::temp_dir().join("lz-streamer-aufnahmen")
            } else {
                PathBuf::from(cfg.record.dir.trim())
            };
            std::fs::create_dir_all(&dir).with_context(|| format!("Aufnahmeordner {} nicht anlegbar", dir.display()))?;
            let ext = match cfg.record.container {
                crate::config::RecordContainer::Ts => "ts",
                crate::config::RecordContainer::Mp4 => "mp4",
            };
            let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
            Some(dir.join(format!("lz-{stamp}_%03d.{ext}")).to_string_lossy().to_string())
        } else {
            None
        };
        if matches!(input, ResolvedInput::Test) && cfg.input.kind != crate::config::InputKind::Test {
            event(&events, "info", "Keine DeckLink/UltraStudio gefunden – sende Testbild.");
        }

        let mut last_err = anyhow!("kein Encoder versucht");
        for (i, enc) in candidates.iter().enumerate() {
            let parts = EncoderParts { input: &input, encoder: enc.factory, aac, image_ok, rtmp, record: record_location.as_deref() };
            let desc = pipeline::encoder_description(cfg, &parts);
            tracing::debug!("Encoder-Pipeline: {desc}");
            let pipeline = gst::parse::launch(&desc)
                .with_context(|| format!("Pipeline mit {} nicht baubar", enc.factory))?
                .downcast::<gst::Pipeline>()
                .map_err(|_| anyhow!("keine Pipeline"))?;
            let counters: Arc<Counters> = Arc::default();
            let encoded = Arc::new(AtomicU64::new(0));

            let venc = pipeline.by_name("venc").unwrap();
            set_props(&venc, &caps::encoder_props(enc.factory, cfg.video.bitrate_kbps, cfg.video.gop_frames()), &events);
            count_frames(&venc.static_pad("sink").unwrap(), &counters);
            {
                let e = encoded.clone();
                venc.static_pad("src").unwrap().add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                    e.fetch_add(1, Ordering::Relaxed);
                    gst::PadProbeReturn::Ok
                });
            }
            let srt = pipeline.by_name("srtout").unwrap();
            if !cfg.srt.passphrase.is_empty() {
                srt.set_property("passphrase", &cfg.srt.passphrase);
            }
            if !cfg.srt.streamid.is_empty() {
                srt.set_property("streamid", &cfg.srt.streamid);
            }
            count_bytes(&srt.static_pad("sink").unwrap(), &counters);
            for sw in ["vsw", "asw"] {
                if let Some(pad) = pipeline.by_name(sw).and_then(|s| s.static_pad("sink_1")) {
                    pad.set_property("priority", 1u32);
                }
                if let Some(pad) = pipeline.by_name(sw).and_then(|s| s.static_pad("sink_0")) {
                    pad.set_property("priority", 0u32);
                }
            }
            hook_preview(&pipeline, opts.preview_path.clone());
            configure_audio(&pipeline, cfg);
            let cc: Arc<CaptionCounter> = Arc::default();
            count_captions(&venc.static_pad("sink").unwrap(), &cc);
            if let Some(rec) = pipeline.by_name("rec") {
                if cfg.record.container == crate::config::RecordContainer::Mp4 {
                    // fragmentiert: Datei bleibt lesbar, auch wenn der Prozess hart endet
                    rec.set_property("muxer-properties", gst::Structure::builder("p").field("fragment-duration", 1000u32).build());
                }
            }
            let rtmp_sender = rtmp.then(|| RtmpSender::new(&pipeline, &cfg.rtmp.url, cfg.audio.enabled && aac.is_some()));

            let e2 = encoded.clone();
            match play_and_wait(&pipeline, move || e2.load(Ordering::Relaxed) > 0, Duration::from_secs(6)) {
                Ok(()) => {
                    if encoded.load(Ordering::Relaxed) == 0 {
                        event(&events, "warn", "Noch kein codiertes Bild – Eingang liefert nichts?");
                    }
                    event(
                        &events,
                        "ok",
                        format!("Encoder läuft: {} · {} · SRT {}", enc.label, input.label(), cfg.srt.uri()),
                    );
                    let asr = (cfg.captions.source == crate::config::CaptionSource::Speech).then(|| {
                            let (w, c) = (pipeline.downgrade(), cc.clone());
                            crate::asr::AsrRunner::new(
                                &pipeline,
                                "asr_tap",
                                cfg.captions.asr.clone(),
                                Arc::new(move |t: String| {
                                    if let Some(p) = w.upgrade() {
                                        push_caption_text(&p, &c, &t);
                                    }
                                }),
                            )
                        });
                    let mut ch = Channel {
                        role: Role::Encoder,
                        pipeline,
                        counters,
                        events,
                        started: Instant::now(),
                        last_tick: (Instant::now(), 0, 0),
                        rates: (0.0, 0.0),
                        input: input.label(),
                        encoder: Some(*enc),
                        codec: Arc::new(Mutex::new(Some(cfg.video.codec))),
                        prev_standby: None,
                        prev_connected: false,
                        rtmp: rtmp_sender,
                        stopped: false,
                        recording: None,
                        asr,
                        cc,
                        outputs: cfg
                            .outputs
                            .iter()
                            .enumerate()
                            .filter(|(_, o)| o.enabled)
                            .map(|(j, o)| (format!("out{j}"), o.name.clone(), output_target(o)))
                            .collect(),
                        tracks: Arc::new(Mutex::new(
                            crate::audio::effective_tracks(&cfg.audio.tracks)
                                .iter()
                                .enumerate()
                                .map(|(i, t)| TrackInfo {
                                    index: i as u32,
                                    language: Some(t.language.clone()),
                                    name: Some(t.name.clone()),
                                    selected: i == 0,
                                    ..Default::default()
                                })
                                .collect(),
                        )),
                    };
                    if let Some(r) = ch.rtmp.as_mut() {
                        r.start(&ch.events);
                    }
                    return Ok(ch);
                }
                Err(e) => {
                    let _ = pipeline.set_state(gst::State::Null);
                    let encoder_fault = e.source == "venc" || e.source.is_empty() || e.text.contains("enc");
                    if encoder_fault && i + 1 < candidates.len() {
                        event(
                            &events,
                            "warn",
                            format!("{} nicht nutzbar ({}) – weiter mit {}", enc.label, e.text, candidates[i + 1].label),
                        );
                        last_err = anyhow!(e.text);
                        continue;
                    }
                    bail!("Start fehlgeschlagen ({}): {}", e.source, e.text);
                }
            }
        }
        Err(last_err)
    }

    pub fn start_decoder(cfg: &DecoderConfig, opts: &ChannelOptions) -> Result<Self> {
        let events: Events = Arc::default();
        let counters: Arc<Counters> = Arc::default();
        let codec: Arc<Mutex<Option<Codec>>> = Arc::default();
        let tracks: Arc<Mutex<Vec<TrackInfo>>> = Arc::default();
        let desc = format!(
            "srtsrc name=srtin uri={} latency={} ! queue max-size-buffers=0 max-size-bytes=0 max-size-time=2000000000 \
             ! tsdemux name=demux latency={}",
            pipeline::quote(&cfg.srt.uri()),
            cfg.srt.latency_ms,
            cfg.demux_latency_ms
        );
        let pipeline = gst::parse::launch(&desc)?.downcast::<gst::Pipeline>().map_err(|_| anyhow!("keine Pipeline"))?;
        let srt = pipeline.by_name("srtin").unwrap();
        if !cfg.srt.passphrase.is_empty() {
            srt.set_property("passphrase", &cfg.srt.passphrase);
        }
        if !cfg.srt.streamid.is_empty() {
            srt.set_property("streamid", &cfg.srt.streamid);
        }
        count_bytes(&srt.static_pad("src").unwrap(), &counters);

        let demux = pipeline.by_name("demux").unwrap();
        let weak = pipeline.downgrade();
        let (c2, e2, k2, cfg2, prev) = (counters.clone(), events.clone(), codec.clone(), cfg.clone(), opts.preview_path.clone());
        let t2 = tracks.clone();
        let cc: Arc<CaptionCounter> = Arc::default();
        let cc2 = cc.clone();
        let ts_off = -(cfg.srt.latency_ms as i64) * 1_000_000;
        demux.connect_pad_added(move |_, pad| {
            let Some(pipeline) = weak.upgrade() else { return };
            let name = pad
                .current_caps()
                .or_else(|| Some(pad.query_caps(None)))
                .and_then(|c| c.structure(0).map(|s| s.name().to_string()))
                .unwrap_or_default();
            let branch = match name.as_str() {
                "video/x-h264" | "video/x-h265" => {
                    let codec = if name == "video/x-h264" { Codec::H264 } else { Codec::H265 };
                    let mut list = caps::decoder_candidates(codec);
                    if cfg2.decoder != "auto" && caps::element_available(&cfg2.decoder) {
                        list.insert(0, Box::leak(cfg2.decoder.clone().into_boxed_str()));
                    }
                    let Some(dec) = list.into_iter().next() else {
                        event(&e2, "error", format!("Kein Decoder für {name}"));
                        return;
                    };
                    *k2.lock().unwrap() = Some(codec);
                    event(&e2, "info", format!("Videostrom {name}, Decoder {dec}"));
                    pipeline::decoder_video_branch(codec, dec, cfg2.output, cfg2.device, &cfg2.deck_mode, ts_off, (cfg2.show_captions, cfg2.cc_line))
                }
                "audio/mpeg" => {
                    // Alle Tonspuren auf einen Umschalter, ausgegeben wird eine.
                    // So bleibt die Wahl live änderbar, und ein vom Demuxer
                    // verworfenes Pad (Programmwechsel beim Einstieg) stört nicht.
                    let asel = match pipeline.by_name("asel") {
                        Some(a) => a,
                        None => {
                            let Ok(asel) = gst::ElementFactory::make("input-selector").name("asel").property("sync-streams", false).build() else { return };
                            let branch = pipeline::decoder_audio_branch(cfg2.output, cfg2.audio, cfg2.device, ts_off);
                            let Ok(bin) = gst::parse::bin_from_description(&branch, true) else { return };
                            if pipeline.add_many([&asel, bin.upcast_ref()]).is_err() || asel.link(&bin).is_err() {
                                event(&e2, "error", "Tonausgabe nicht baubar");
                                return;
                            }
                            let _ = bin.sync_state_with_parent();
                            let _ = asel.sync_state_with_parent();
                            asel
                        }
                    };
                    let Some(sel_pad) = asel.request_pad_simple("sink_%u") else { return };
                    {
                        let mut list = t2.lock().unwrap();
                        let index = list.len() as u32;
                        list.push(TrackInfo {
                            index,
                            demux_pad: pad.name().to_string(),
                            sel_pad: sel_pad.name().to_string(),
                            ..Default::default()
                        });
                    }
                    // Sprachkennung kommt als Tag-Ereignis aus dem PMT-Deskriptor
                    let (t3, a3, c3, dp) = (t2.clone(), asel.clone(), cfg2.clone(), pad.name().to_string());
                    pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
                        if let Some(gst::PadProbeData::Event(ev)) = &info.data {
                            if let gst::EventView::Tag(tag) = ev.view() {
                                let tags = tag.tag();
                                let mut list = t3.lock().unwrap();
                                if let Some(t) = list.iter_mut().find(|t| t.demux_pad == dp) {
                                    if let Some(l) = tags.get::<gst::tags::LanguageCode>() {
                                        t.language = Some(l.get().to_string());
                                    }
                                    if let Some(n) = tags.get::<gst::tags::Title>() {
                                        t.name = Some(n.get().to_string());
                                    }
                                }
                                select_audio(&a3, &mut list, &c3.audio_language, c3.audio_track);
                            }
                        }
                        gst::PadProbeReturn::Ok
                    });
                    pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, |_, info| match &info.data {
                        Some(gst::PadProbeData::Event(ev)) if ev.type_() == gst::EventType::Eos => gst::PadProbeReturn::Drop,
                        _ => gst::PadProbeReturn::Ok,
                    });
                    if let Err(e) = pad.link(&sel_pad) {
                        event(&e2, "error", format!("Tonspur nicht verknüpfbar: {e:?}"));
                    }
                    select_audio(&asel, &mut t2.lock().unwrap(), &cfg2.audio_language, cfg2.audio_track);
                    return;
                }
                _ => "fakesink sync=false".to_string(),
            };
            let bin = match gst::parse::bin_from_description(&branch, true) {
                Ok(b) => b,
                Err(e) => {
                    event(&e2, "error", format!("Zweig für {name} nicht baubar: {e}"));
                    return;
                }
            };
            if let Some(vc) = bin.by_name("vconv") {
                count_frames(&vc.static_pad("sink").unwrap(), &c2);
            }
            if cfg2.output == OutputKind::None {
                if let Some(vs) = bin.by_name("vsink") {
                    measure_luma(&vs.static_pad("sink").unwrap(), &c2);
                }
            }
            hook_preview(&bin, prev.clone());
            hook_caption_text(&bin, &cc2);
            if pipeline.add(&bin).is_err() {
                return;
            }
            let _ = bin.sync_state_with_parent();
            // tsdemux schickt beim Einstieg mitten in einen Strom gelegentlich
            // EOS über ein Pad, bevor dessen Zweig Caps hatte (osxaudiosink:
            // „not negotiated before eos“ → Kanal-Neustart). Hier schlucken;
            // ein echtes Streamende meldet srtsrc selbst.
            pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, |_, info| match &info.data {
                Some(gst::PadProbeData::Event(ev)) if ev.type_() == gst::EventType::Eos => gst::PadProbeReturn::Drop,
                _ => gst::PadProbeReturn::Ok,
            });
            if let Some(sink) = bin.static_pad("sink") {
                if let Err(e) = pad.link(&sink) {
                    event(&e2, "error", format!("Verknüpfen von {name} fehlgeschlagen: {e:?}"));
                }
            }
        });

        let (t4, c4, w4) = (tracks.clone(), cfg.clone(), pipeline.downgrade());
        demux.connect_pad_removed(move |_, pad| {
            let Some(pipeline) = w4.upgrade() else { return };
            let Some(asel) = pipeline.by_name("asel") else { return };
            let mut list = t4.lock().unwrap();
            if let Some(pos) = list.iter().position(|t| t.demux_pad == pad.name().as_str()) {
                let t = list.remove(pos);
                if let Some(p) = asel.static_pad(&t.sel_pad) {
                    asel.release_request_pad(&p);
                }
                for (i, t) in list.iter_mut().enumerate() {
                    t.index = i as u32;
                }
                select_audio(&asel, &mut list, &c4.audio_language, c4.audio_track);
            }
        });

        // Ausgabepuffer: Demux-Puffer plus Reserve. Die SRT-Latenz ist hier
        // bewusst nicht enthalten (siehe ts-offset oben).
        pipeline.set_latency(gst::ClockTime::from_mseconds((cfg.demux_latency_ms + cfg.output_buffer_ms) as u64));
        pipeline.set_state(gst::State::Playing).context("Decoder-Pipeline startet nicht")?;
        event(&events, "ok", format!("Decoder wartet auf {}", cfg.srt.uri()));
        Ok(Channel {
            role: Role::Decoder,
            pipeline,
            counters,
            events,
            started: Instant::now(),
            last_tick: (Instant::now(), 0, 0),
            rates: (0.0, 0.0),
            input: format!("SRT {}", cfg.srt.uri()),
            encoder: None,
            codec,
            prev_standby: None,
            prev_connected: false,
            rtmp: None,
            stopped: false,
            recording: None,
            outputs: Vec::new(),
            tracks,
            cc,
            asr: None,
        })
    }

    fn update_loudness(&mut self, i: usize, mut l: crate::loudness::Loudness) {
        let mut list = self.tracks.lock().unwrap();
        // Decoder: lvl0 misst die ausgegebene Spur
        let idx = if self.role == Role::Decoder { list.iter().position(|t| t.selected) } else { Some(i) };
        let Some(t) = idx.and_then(|i| list.get_mut(i)) else { return };
        let silent = l.momentary.is_none_or(|m| m < crate::loudness::SILENCE_LUFS);
        let prev = t.loudness.as_ref().map(|p| p.silent_s).unwrap_or(0.0);
        l.silent_s = if silent { prev + 0.5 } else { 0.0 };
        t.loudness = Some(l);
    }

    /// Neueste Datei zum Aufnahme-Muster (falls die Startmeldung beim
    /// Hochfahren schon verbraucht wurde).
    fn newest_recording(&self) -> Option<String> {
        let pattern = self.pipeline.by_name("rec")?.property::<Option<String>>("location")?;
        let path = PathBuf::from(&pattern);
        let dir = path.parent()?;
        let prefix = path.file_name()?.to_string_lossy().split('%').next()?.to_string();
        std::fs::read_dir(dir)
            .ok()?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
            .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
            .map(|e| e.path().to_string_lossy().to_string())
    }

    pub fn counters(&self) -> &Arc<Counters> {
        &self.counters
    }

    pub fn pipeline(&self) -> &gst::Pipeline {
        &self.pipeline
    }

    pub fn command(&mut self, cmd: &ChildCmd) {
        match cmd {
            ChildCmd::Signal { lost } => {
                for v in ["sigv", "siga"] {
                    if let Some(el) = self.pipeline.by_name(v) {
                        el.set_property("drop", *lost);
                    }
                }
                event(&self.events, "info", if *lost { "Signalverlust nachgestellt" } else { "Eingang wieder freigegeben" });
            }
            ChildCmd::Keyframe => self.force_keyframe(),
            ChildCmd::Caption { text } => self.push_caption(text),
            ChildCmd::Stop => self.stop(),
        }
    }

    /// Text in die Live-Untertitelquelle schieben. Anzeigedauer nach Länge
    /// (Pop-on braucht eine Dauer, Roll-up ignoriert sie).
    pub fn push_caption(&self, text: &str) {
        if !push_caption_text(&self.pipeline, &self.cc, text) {
            event(&self.events, "warn", "Untertitelquelle steht nicht auf „live“");
        }
    }

    /// Spracherkennung abstürzen lassen (Test „Kanal läuft weiter“).
    pub fn crash_asr(&mut self) {
        if let Some(a) = self.asr.as_mut() {
            a.crash();
        }
    }

    pub fn force_keyframe(&self) {
        force_keyframe(&self.pipeline);
    }

    pub fn stop(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        if let Some(r) = self.rtmp.as_mut() {
            r.stop();
        }
        if let Some(a) = self.asr.as_mut() {
            a.stop();
        }
        if self.pipeline.by_name("rec").is_some() {
            // Aufnahme sauber abschließen: EOS durch die Pipeline, dann erst stoppen
            self.pipeline.send_event(gst::event::Eos::new());
            if let Some(bus) = self.pipeline.bus() {
                let _ = bus.timed_pop_filtered(gst::ClockTime::from_seconds(3), &[gst::MessageType::Eos, gst::MessageType::Error]);
            }
        }
        let _ = self.pipeline.set_state(gst::State::Null);
    }

    /// Bus leeren, Zustände vergleichen, Status bauen. Etwa zweimal pro Sekunde aufrufen.
    pub fn poll(&mut self) -> Vec<ChildMsg> {
        let mut out = Vec::new();
        let bus = self.pipeline.bus().unwrap();
        while let Some(msg) = bus.pop() {
            match msg.view() {
                gst::MessageView::Error(err) => {
                    let src = msg.src().map(|s| s.name().to_string()).unwrap_or_default();
                    out.push(ChildMsg::Fatal {
                        text: format!("{src}: {} ({})", err.error(), err.debug().unwrap_or_default()),
                    });
                }
                gst::MessageView::Warning(w) => {
                    let src = msg.src().map(|s| s.name().to_string()).unwrap_or_default();
                    event(&self.events, "warn", format!("{src}: {}", w.error()));
                }
                gst::MessageView::Eos(_) => out.push(ChildMsg::Fatal { text: "Strom zu Ende (EOS)".into() }),
                gst::MessageView::Element(e) => {
                    let src = msg.src().map(|s| s.name().to_string()).unwrap_or_default();
                    if let (Some(i), Some(l)) = (
                        src.strip_prefix("lvl").and_then(|n| n.parse::<usize>().ok()),
                        e.structure().and_then(crate::loudness::from_message),
                    ) {
                        self.update_loudness(i, l);
                    }
                    if let Some(st) = e.structure() {
                        if st.name() == "splitmuxsink-fragment-opened" {
                            if let Ok(loc) = st.get::<String>("location") {
                                event(&self.events, "info", format!("Aufnahme: {loc}"));
                                self.recording = Some(loc);
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        if let Some(r) = self.rtmp.as_mut() {
            r.poll(&self.events);
        }
        if let Some(a) = self.asr.as_mut() {
            for t in a.poll() {
                event(&self.events, if t.contains("läuft") { "ok" } else { "warn" }, t);
            }
        }

        let standby = self
            .pipeline
            .by_name("vsw")
            .and_then(|s| s.property::<Option<gst::Pad>>("active-pad"))
            .map(|p| p.name() == "sink_1");
        if let Some(sb) = standby {
            if self.prev_standby.is_some_and(|p| p != sb) || (self.prev_standby.is_none() && sb && self.counters.frames.load(Ordering::Relaxed) > 0) {
                if sb {
                    event(&self.events, "warn", "Signal weg – Standbild läuft, Ausgang bleibt stehen");
                } else {
                    event(&self.events, "ok", "Signal zurück – Eingang wieder auf Sendung");
                }
            }
            self.prev_standby = Some(sb);
        }
        let deck_signal = self.pipeline.by_name("capsrc").map(|c| c.property::<bool>("signal"));
        let input_format = self
            .pipeline
            .by_name("capsrc")
            .or_else(|| self.pipeline.by_name("testsrc"))
            .and_then(|c| c.static_pad("src"))
            .and_then(|p| p.current_caps())
            .and_then(|c| c.structure(0).map(describe_video))
            .map(|f| if self.pipeline.by_name("testsrc").is_some() { format!("{f} (Testbild)") } else { f });

        let srt_el = self.pipeline.by_name(if self.role == Role::Encoder { "srtout" } else { "srtin" });
        let srt = srt_el.as_ref().map(stats::read).unwrap_or_default();
        let connected = srt.connected || (self.role == Role::Decoder && self.counters.bytes.load(Ordering::Relaxed) > 0);
        if connected != self.prev_connected {
            if connected {
                event(&self.events, "ok", "Gegenstelle verbunden");
            } else {
                event(&self.events, "warn", "Gegenstelle getrennt");
            }
            self.prev_connected = connected;
        }

        let now = Instant::now();
        let frames = self.counters.frames.load(Ordering::Relaxed);
        let bytes = self.counters.bytes.load(Ordering::Relaxed);
        let dt = now.duration_since(self.last_tick.0).as_secs_f64();
        if dt >= 0.9 {
            self.rates = (
                (frames - self.last_tick.2) as f64 / dt,
                (bytes - self.last_tick.1) as f64 * 8.0 / 1000.0 / dt,
            );
            self.last_tick = (now, bytes, frames);
        }
        let signal = match (deck_signal, standby, self.role) {
            (Some(true), Some(false), _) => "ok",
            (Some(false), _, _) | (_, Some(true), _) => "lost",
            (None, Some(false), _) => "ok",
            (_, _, Role::Decoder) if self.rates.0 > 0.0 => "ok",
            (_, _, Role::Decoder) if frames > 0 => "lost",
            _ => "unknown",
        };
        let status = ChannelStatus {
            role: self.role,
            input: self.input.clone(),
            encoder: self.encoder.map(|e| e.factory.to_string()),
            encoder_label: self.encoder.map(|e| e.label.to_string()),
            encoder_hw: self.encoder.is_some_and(|e| e.hardware),
            codec: *self.codec.lock().unwrap(),
            signal: signal.into(),
            standby: standby.unwrap_or(false),
            frames,
            fps: (self.rates.0 * 10.0).round() / 10.0,
            bytes,
            bitrate_kbps: self.rates.1.round(),
            resolution: self.counters.resolution.lock().unwrap().map(|(w, h)| format!("{w}×{h}")),
            input_format,
            srt,
            rtmp: self.rtmp.as_ref().map(|r| r.state.clone()),
            outputs: self
                .outputs
                .iter()
                .map(|(el, name, target)| {
                    let st = target
                        .starts_with("srt://")
                        .then(|| self.pipeline.by_name(el))
                        .flatten()
                        .map(|e| stats::read(&e));
                    OutputStatus {
                        name: name.clone(),
                        target: target.clone(),
                        connected: st.as_ref().map(|s| s.connected),
                        rtt_ms: st.as_ref().and_then(|s| s.rtt_ms),
                        lost: st.as_ref().map(|s| s.lost).unwrap_or(0),
                    }
                })
                .collect(),
            recording: self.recording.clone().or_else(|| self.newest_recording()),
            audio_tracks: self.tracks.lock().unwrap().clone(),
            captions: CaptionStatus {
                asr: self.asr.as_ref().map(|a| a.status()),
                frames: self.cc.frames.load(Ordering::Relaxed),
                present: self.cc.last.lock().unwrap().is_some_and(|t| t.elapsed() < Duration::from_secs(2)),
                text: self.cc.text.lock().unwrap().clone(),
            },
            luma: self.counters.luma(),
            uptime_s: self.started.elapsed().as_secs_f64().round(),
        };
        out.splice(0..0, self.events.lock().unwrap().drain(..));
        out.push(ChildMsg::Status(status));
        out
    }
}

/// „1920×1080p50“ bzw. „1920×1080i25“ aus Video-Caps.
pub fn describe_video(s: &gst::StructureRef) -> String {
    let w = s.get::<i32>("width").unwrap_or(0);
    let h = s.get::<i32>("height").unwrap_or(0);
    let fps = s.get::<gst::Fraction>("framerate").ok().map(|f| f.numer() as f64 / f.denom().max(1) as f64);
    let interlaced = s.get::<&str>("interlace-mode").is_ok_and(|m| m != "progressive");
    let fps = fps.map(|f| if (f - f.round()).abs() < 0.01 { format!("{}", f.round()) } else { format!("{f:.2}") });
    format!("{w}×{h}{}{}", if interlaced { "i" } else { "p" }, fps.unwrap_or_default())
}

impl Drop for Channel {
    fn drop(&mut self) {
        self.stop();
    }
}

fn force_keyframe(pipeline: &gst::Pipeline) {
    if let Some(src) = pipeline.by_name("venc").and_then(|e| e.static_pad("src")) {
        let ev = gstreamer_video::UpstreamForceKeyUnitEvent::builder().all_headers(true).build();
        src.send_event(ev);
    }
}

// ---------------------------------------------------------------- RTMP

#[derive(Default)]
struct RtmpTaps {
    v: Option<gst_app::AppSrc>,
    a: Option<gst_app::AppSrc>,
    need_key: bool,
}

/// RTMP läuft als eigene kleine Pipeline. Bricht sie ab, versucht sie es
/// nach einer Pause erneut – SRT läuft davon unberührt weiter.
struct RtmpSender {
    url: String,
    audio: bool,
    main: gst::Pipeline,
    taps: Arc<Mutex<RtmpTaps>>,
    pipeline: Option<gst::Pipeline>,
    retry_at: Option<Instant>,
    bytes: Arc<AtomicU64>,
    last_bytes: u64,
    state: String,
}

impl RtmpSender {
    fn new(main: &gst::Pipeline, url: &str, audio: bool) -> Self {
        let taps: Arc<Mutex<RtmpTaps>> = Arc::default();
        for (name, is_video) in [("rtmp_v", true), ("rtmp_a", false)] {
            let Some(sink) = main.by_name(name).and_then(|s| s.downcast::<gst_app::AppSink>().ok()) else { continue };
            let t = taps.clone();
            sink.set_callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |s| {
                        let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        let mut taps = t.lock().unwrap();
                        let Some(buf) = sample.buffer_owned() else { return Ok(gst::FlowSuccess::Ok) };
                        if is_video {
                            if taps.need_key && buf.flags().contains(gst::BufferFlags::DELTA_UNIT) {
                                return Ok(gst::FlowSuccess::Ok);
                            }
                            taps.need_key = false;
                        } else if taps.need_key {
                            return Ok(gst::FlowSuccess::Ok);
                        }
                        let src = if is_video { taps.v.clone() } else { taps.a.clone() };
                        drop(taps);
                        if let Some(src) = src {
                            if src.caps().is_none() {
                                src.set_caps(sample.caps().map(|c| c.to_owned()).as_ref());
                            }
                            let _ = src.push_buffer(buf);
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );
        }
        RtmpSender {
            url: url.to_string(),
            audio,
            main: main.clone(),
            taps,
            pipeline: None,
            retry_at: None,
            bytes: Arc::default(),
            last_bytes: 0,
            state: "startet".into(),
        }
    }

    fn start(&mut self, events: &Events) {
        let desc = pipeline::rtmp_description(&self.url, self.audio);
        let p = match gst::parse::launch(&desc).map(|e| e.downcast::<gst::Pipeline>()) {
            Ok(Ok(p)) => p,
            _ => {
                self.state = "Fehler: RTMP-Pipeline nicht baubar".into();
                return;
            }
        };
        if let Some(clock) = self.main.clock() {
            p.use_clock(Some(&clock));
        }
        if let Some(bt) = self.main.base_time() {
            p.set_base_time(bt);
        }
        p.set_start_time(gst::ClockTime::NONE);
        if let Some(out) = p.by_name("rtmpout").and_then(|o| o.static_pad("sink")) {
            let b = self.bytes.clone();
            out.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                if let Some(gst::PadProbeData::Buffer(buf)) = &info.data {
                    b.fetch_add(buf.size() as u64, Ordering::Relaxed);
                }
                gst::PadProbeReturn::Ok
            });
        }
        let get = |n: &str| p.by_name(n).and_then(|e| e.downcast::<gst_app::AppSrc>().ok());
        let (v, a) = (get("v"), get("a"));
        for src in v.iter().chain(a.iter()) {
            src.set_max_bytes(4_000_000);
            if src.find_property("leaky-type").is_some() {
                src.set_property_from_str("leaky-type", "downstream");
            }
        }
        {
            let mut t = self.taps.lock().unwrap();
            t.v = v;
            t.a = a;
            t.need_key = true;
        }
        if p.set_state(gst::State::Playing).is_err() {
            self.fail(events, "RTMP startet nicht".into());
            return;
        }
        self.pipeline = Some(p);
        self.state = "verbindet".into();
        force_keyframe(&self.main);
        event(events, "info", "RTMP verbindet …");
    }

    fn fail(&mut self, events: &Events, why: String) {
        {
            let mut t = self.taps.lock().unwrap();
            t.v = None;
            t.a = None;
        }
        if let Some(p) = self.pipeline.take() {
            let _ = p.set_state(gst::State::Null);
        }
        self.retry_at = Some(Instant::now() + Duration::from_secs(5));
        self.state = format!("Fehler, neuer Versuch in 5 s: {why}");
        event(events, "warn", format!("RTMP: {why} – SRT läuft weiter"));
    }

    fn poll(&mut self, events: &Events) {
        if let Some(p) = self.pipeline.clone() {
            let bus = p.bus().unwrap();
            while let Some(msg) = bus.pop() {
                if let gst::MessageView::Error(err) = msg.view() {
                    self.fail(events, err.error().to_string());
                    return;
                }
            }
            let b = self.bytes.load(Ordering::Relaxed);
            if b > self.last_bytes {
                if self.state != "sendet" {
                    event(events, "ok", "RTMP sendet");
                }
                self.state = "sendet".into();
            }
            self.last_bytes = b;
        } else if self.retry_at.is_some_and(|t| Instant::now() >= t) {
            self.retry_at = None;
            self.start(events);
        }
    }

    fn stop(&mut self) {
        if let Some(p) = self.pipeline.take() {
            let _ = p.set_state(gst::State::Null);
        }
    }
}

// ---------------------------------------------------------------- Kindprozess

fn emit(msg: &ChildMsg) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = serde_json::to_writer(&mut out, msg);
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

/// Hauptschleife des Kindprozesses: Status als JSON-Zeilen auf stdout,
/// Befehle als JSON-Zeilen von stdin. Endet stdin (Elternprozess weg),
/// beendet sich der Kanal selbst – keine verwaisten Prozesse.
pub fn run_child(cfg: Config, opts: ChannelOptions) -> i32 {
    let mut ch = match Channel::start(&cfg, &opts) {
        Ok(c) => c,
        Err(e) => {
            emit(&ChildMsg::Fatal { text: format!("{e:#}") });
            return 2;
        }
    };
    let (tx, rx) = std::sync::mpsc::channel::<Option<ChildCmd>>();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if let Ok(cmd) = serde_json::from_str::<ChildCmd>(&line) {
                let _ = tx.send(Some(cmd));
            }
        }
        let _ = tx.send(None);
    });
    loop {
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                None | Some(ChildCmd::Stop) => {
                    ch.stop();
                    return 0;
                }
                Some(c) => ch.command(&c),
            }
        }
        for m in ch.poll() {
            emit(&m);
            if matches!(m, ChildMsg::Fatal { .. }) {
                ch.stop();
                return 2;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_protocol_roundtrip() {
        let m = ChildMsg::Event { level: "ok".into(), text: "x".into() };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("\"type\":\"event\""));
        assert_eq!(serde_json::from_str::<ChildMsg>(&s).unwrap(), m);
        let c: ChildCmd = serde_json::from_str(r#"{"cmd":"signal","lost":true}"#).unwrap();
        assert_eq!(c, ChildCmd::Signal { lost: true });
    }

    #[test]
    fn luma_rise_detected_once() {
        let c = Counters::default();
        c.set_luma(10.0);
        assert!(c.luma_rise().is_none());
        c.set_luma(200.0);
        let t = c.luma_rise().unwrap();
        c.set_luma(210.0);
        assert_eq!(c.luma_rise(), Some(t));
    }
}
