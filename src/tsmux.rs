//! `lztsmux`: eigener MPEG-TS-Multiplexer für H.264/H.265 und AAC, ohne Wartezeit.
//!
//! `mpegtsmux` ist ein Aggregator: Er gibt ein Bild erst ab, wenn auf jedem
//! Eingang ein Puffer liegt (beim Ton ≈ 55 ms nach dessen Zeitstempel), hält
//! mit `alignment=7` den Rest jedes Bilds bis zum nächsten zurück und setzt die
//! PCR fest 125 ms vor die DTS. `lztsmux` schreibt jeden Puffer sofort und
//! leitet die PCR aus der Pipeline-Uhr ab:
//!
//! - PTS/DTS = Laufzeit des Puffers + Verzug D,
//! - PCR = Laufzeit beim Senden.
//!
//! D ist damit der Abstand PCR → PTS, den der Empfänger als Puffer sieht. Er muss
//! nur so groß sein wie der späteste Eingang (in der Regel der Ton) braucht, um
//! am Mux anzukommen und gesendet zu sein. Mit `delay=0` lernt der Mux D in der
//! ersten halben Sekunde und hebt ihn danach nur an, wenn ein Puffer zu spät käme.
//!
//! Mit `bitrate` sendet ein eigener Takt-Thread konstant Paket für Paket
//! (Nullpakete als Füllung), die PCR-Werte entsprechen exakt der Sendeposition.
//! Der Takt beachtet das T-STD-Modell (Transportpuffer 512 Byte je PID mit der
//! Abflussrate nach ISO/IEC 13818-1, Ton 2 Mbit/s): ein Tonpaket geht erst,
//! wenn es den Transportpuffer nicht überlaufen lässt.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Once};

use gstreamer as gst;
use gstreamer::glib;
use gstreamer::prelude::*;
use gstreamer::subclass::prelude::*;

pub const PACKET: usize = 188;
/// Pakete je Ausgabepuffer (7 × 188 = 1316 Byte, eine SRT-/UDP-Nutzlast)
pub const GROUP: usize = 7;
pub const PMT_PID: u16 = 0x20;
pub const FIRST_ES_PID: u16 = 0x41;
/// Zeitbasis wie bei mpegtsmux: PTS beginnen bei 1 h
const BASE_90K: u64 = 90_000 * 3600;
const MASK_33: u64 = (1 << 33) - 1;
/// Transportpuffer TB_n im T-STD
pub const TB_SIZE: f64 = 512.0;
/// Abflussrate des Transportpuffers für Ton (ISO/IEC 13818-1 2.4.2.3)
pub const RX_AUDIO: f64 = 2_000_000.0;

/// Elementarstrom-Art
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    H264,
    H265,
    Aac,
}

impl Kind {
    pub fn stream_type(self) -> u8 {
        match self {
            Kind::H264 => 0x1b,
            Kind::H265 => 0x24,
            Kind::Aac => 0x0f,
        }
    }
    pub fn is_video(self) -> bool {
        self != Kind::Aac
    }
}

/// CRC-32/MPEG-2 der PSI-Tabellen
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= (b as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04c1_1db7 } else { crc << 1 };
        }
    }
    crc
}

fn section(table_id: u8, id: u16, version: u8, body: &[u8]) -> Vec<u8> {
    let len = 5 + body.len() + 4;
    let mut s = vec![table_id, 0xb0 | ((len >> 8) as u8 & 0x0f), len as u8, (id >> 8) as u8, id as u8, 0xc1 | ((version & 0x1f) << 1), 0, 0];
    s.extend_from_slice(body);
    let c = crc32(&s);
    s.extend_from_slice(&c.to_be_bytes());
    s
}

/// PAT mit einem Programm (Nummer 1 → PMT auf `PMT_PID`)
pub fn pat_section() -> Vec<u8> {
    section(0x00, 1, 0, &[0x00, 0x01, 0xe0 | (PMT_PID >> 8) as u8, PMT_PID as u8])
}

/// Eintrag in der PMT
#[derive(Debug, Clone, PartialEq)]
pub struct EsInfo {
    pub pid: u16,
    pub kind: Kind,
    pub language: Option<String>,
}

pub fn pmt_section(version: u8, pcr_pid: u16, streams: &[EsInfo]) -> Vec<u8> {
    let mut b = vec![0xe0 | (pcr_pid >> 8) as u8, pcr_pid as u8, 0xf0, 0x00];
    for s in streams {
        let mut desc = Vec::new();
        if let Some(l) = s.language.as_deref().filter(|l| l.len() == 3 && l.is_ascii()) {
            // ISO_639_language_descriptor
            desc.extend_from_slice(&[0x0a, 4]);
            desc.extend_from_slice(l.as_bytes());
            desc.push(0);
        }
        b.extend_from_slice(&[s.kind.stream_type(), 0xe0 | (s.pid >> 8) as u8, s.pid as u8, 0xf0 | (desc.len() >> 8) as u8, desc.len() as u8]);
        b.extend_from_slice(&desc);
    }
    section(0x02, 1, version, &b)
}

/// Eine Tabelle in ein Paket (Tabellen dieses Mux passen immer in 183 Byte)
pub fn psi_packet(pid: u16, cc: &mut u8, sec: &[u8]) -> [u8; PACKET] {
    let mut p = [0xffu8; PACKET];
    p[..4].copy_from_slice(&[0x47, 0x40 | (pid >> 8) as u8, pid as u8, 0x10 | *cc]);
    *cc = (*cc + 1) & 0x0f;
    p[4] = 0;
    let n = sec.len().min(PACKET - 5);
    p[5..5 + n].copy_from_slice(&sec[..n]);
    p
}

pub fn null_packet() -> [u8; PACKET] {
    let mut p = [0xffu8; PACKET];
    p[..4].copy_from_slice(&[0x47, 0x1f, 0xff, 0x10]);
    p
}

/// 27-MHz-PCR in 6 Byte (33 bit Basis, 6 reserviert, 9 bit Erweiterung)
fn pcr_bytes(pcr: u64) -> [u8; 6] {
    let base = (pcr / 300) & MASK_33;
    let ext = pcr % 300;
    [(base >> 25) as u8, (base >> 17) as u8, (base >> 9) as u8, (base >> 1) as u8, ((base & 1) << 7) as u8 | 0x7e | (ext >> 8) as u8, ext as u8]
}

/// Paket nur mit Anpassungsfeld und PCR (zählt den CC nicht weiter)
pub fn pcr_packet(pid: u16, cc: u8, pcr: u64, discontinuity: bool) -> [u8; PACKET] {
    let mut p = [0xffu8; PACKET];
    p[..4].copy_from_slice(&[0x47, (pid >> 8) as u8 & 0x1f, pid as u8, 0x20 | (cc.wrapping_sub(1) & 0x0f)]);
    p[4] = 183;
    p[5] = 0x10 | if discontinuity { 0x80 } else { 0 };
    p[6..12].copy_from_slice(&pcr_bytes(pcr));
    p
}

fn ts_bytes(prefix: u8, t: u64) -> [u8; 5] {
    let t = t & MASK_33;
    [(prefix << 4) | ((t >> 29) as u8 & 0x0e) | 1, (t >> 22) as u8, ((t >> 14) as u8 & 0xfe) | 1, (t >> 7) as u8, ((t << 1) as u8 & 0xfe) | 1]
}

/// PES-Kopf (Zeiten in 90 kHz)
pub fn pes_header(stream_id: u8, payload_len: usize, pts: u64, dts: Option<u64>) -> Vec<u8> {
    let dts = dts.filter(|d| *d != pts);
    let hl = if dts.is_some() { 10 } else { 5 };
    let total = 3 + hl + payload_len;
    // Bild darf unbegrenzt sein (Länge 0), Ton muss passen
    let len = if total > 0xffff { 0 } else { total };
    let mut h = vec![0, 0, 1, stream_id, (len >> 8) as u8, len as u8, 0x84, if dts.is_some() { 0xc0 } else { 0x80 }, hl as u8];
    match dts {
        Some(d) => {
            h.extend_from_slice(&ts_bytes(3, pts));
            h.extend_from_slice(&ts_bytes(1, d));
        }
        None => h.extend_from_slice(&ts_bytes(2, pts)),
    }
    h
}

/// PES in TS-Pakete zerlegen. `pcr` landet im Anpassungsfeld des ersten Pakets,
/// `random_access` setzt dort das Kennzeichen für Einstiegspunkte.
pub fn packetize(pid: u16, cc: &mut u8, pes: &[u8], pcr: Option<u64>, random_access: bool, out: &mut Vec<[u8; PACKET]>) {
    let mut off = 0;
    let mut first = true;
    while off < pes.len() {
        let mut p = [0xffu8; PACKET];
        let want_af = first && (pcr.is_some() || random_access);
        let af_fixed = if want_af { 2 + if pcr.is_some() { 6 } else { 0 } } else { 0 };
        let cap = PACKET - 4 - af_fixed;
        let take = (pes.len() - off).min(cap);
        let af_len = PACKET - 4 - take; // Anpassungsfeld samt Füllung (0 = keins)
        let afc = if af_len > 0 { 0x30 } else { 0x10 };
        p[..4].copy_from_slice(&[0x47, if first { 0x40 } else { 0 } | (pid >> 8) as u8 & 0x1f, pid as u8, afc | *cc]);
        *cc = (*cc + 1) & 0x0f;
        if af_len > 0 {
            p[4] = (af_len - 1) as u8;
            if af_len > 1 {
                let mut flags = 0u8;
                if want_af && random_access {
                    flags |= 0x40;
                }
                if let (true, Some(v)) = (want_af, pcr) {
                    flags |= 0x10;
                    p[6..12].copy_from_slice(&pcr_bytes(v));
                }
                p[5] = flags;
            }
        }
        p[4 + af_len..].copy_from_slice(&pes[off..off + take]);
        off += take;
        first = false;
        out.push(p);
    }
}

/// ADTS-Kopf aus der AudioSpecificConfig (`codec_data` bei `stream-format=raw`)
pub fn adts_header(asc: &[u8], frame_len: usize) -> Option<[u8; 7]> {
    if asc.len() < 2 {
        return None;
    }
    let object = asc[0] >> 3;
    let freq = ((asc[0] & 7) << 1) | (asc[1] >> 7);
    let ch = (asc[1] >> 3) & 0x0f;
    if object == 0 || object > 4 || freq > 12 {
        return None;
    }
    let len = frame_len + 7;
    Some([0xff, 0xf1, ((object - 1) << 6) | (freq << 2) | (ch >> 2), ((ch & 3) << 6) | (len >> 11) as u8, (len >> 3) as u8, ((len as u8 & 7) << 5) | 0x1f, 0xfc])
}

/// Steht am Anfang schon ein Zugriffseinheit-Begrenzer (AUD)?
pub fn starts_with_aud(kind: Kind, data: &[u8]) -> bool {
    let nal = if data.starts_with(&[0, 0, 0, 1]) {
        data.get(4)
    } else if data.starts_with(&[0, 0, 1]) {
        data.get(3)
    } else {
        None
    };
    match (kind, nal) {
        (Kind::H264, Some(b)) => b & 0x1f == 9,
        (Kind::H265, Some(b)) => (b >> 1) & 0x3f == 35,
        _ => true,
    }
}

fn aud(kind: Kind) -> &'static [u8] {
    match kind {
        Kind::H264 => &[0, 0, 0, 1, 0x09, 0xf0],
        Kind::H265 => &[0, 0, 0, 1, 0x46, 0x01, 0x50],
        Kind::Aac => &[],
    }
}

fn ns_to_90k(ns: i64) -> u64 {
    (BASE_90K as i64 + ns.div_euclid(100_000) * 9 + (ns.rem_euclid(100_000) * 9) / 100_000) as u64 & MASK_33
}

fn ns_to_27m(ns: u64) -> u64 {
    (BASE_90K * 300 + ns / 1000 * 27 + (ns % 1000) * 27 / 1000) % ((MASK_33 + 1) * 300)
}

const MS: u64 = 1_000_000;
/// so lange lernt der Mux D aus den Eingängen
const LEARN_MS: u64 = 1000;
/// so weit sinkt D je Bild, wenn er größer als nötig ist
const SLEW_NS: u64 = 200_000;

#[derive(Debug, Clone)]
pub struct Settings {
    /// PCR → PTS in ms, 0 = automatisch
    pub delay_ms: u32,
    /// Spielraum für den Empfänger über der gelernten Verspätung (ms)
    pub headroom_ms: u32,
    /// Gesamtrate in bit/s, 0 = variabel (sofort senden)
    pub bitrate: u64,
    pub pcr_interval_ms: u32,
    pub si_interval_ms: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { delay_ms: 0, headroom_ms: 25, bitrate: 0, pcr_interval_ms: 20, si_interval_ms: 100 }
    }
}

struct Stream {
    pad: gst::Pad,
    pid: u16,
    stream_id: u8,
    kind: Option<Kind>,
    /// AudioSpecificConfig, wenn der Ton ohne ADTS kommt
    asc: Option<Vec<u8>>,
    language: Option<String>,
    segment: gst::FormattedSegment<gst::ClockTime>,
    cc: u8,
    buffers: u64,
    eos: bool,
    /// nur im CBR-Betrieb: wartende Pakete
    queue: VecDeque<(u64, [u8; PACKET])>,
    /// CC des zuletzt gesendeten Pakets (für PCR-Pakete ohne Nutzinhalt)
    sent_cc: u8,
    tb: f64,
}

#[derive(Default)]
struct State {
    streams: Vec<Stream>,
    pat_cc: u8,
    pmt_cc: u8,
    pmt_version: u8,
    pmt_dirty: bool,
    delay_ns: u64,
    learn_until: Option<u64>,
    /// nach dem Lernen: Puffer, die über D lagen (Zeit, Verspätung)
    exceed: VecDeque<(u64, u64)>,
    /// Verspätungen der letzten 2 s (Zeit, Verspätung)
    window: VecDeque<(u64, u64)>,
    last_si: Option<u64>,
    last_pcr: Option<u64>,
    sent_events: bool,
    /// CBR: Startzeit und Index des nächsten Pakets
    t0: Option<u64>,
    slot: u64,
    tb_time: u64,
    discont: bool,
    eos_sent: bool,
    queued: usize,
    seq: u64,
    /// CBR: PMT nach der PAT im nächsten Paket (auch über das Ende eines Ausgabepuffers)
    pending_psi: Vec<[u8; PACKET]>,
    warned_overflow: bool,
}

impl State {
    fn es_infos(&self) -> Vec<EsInfo> {
        self.streams.iter().filter_map(|s| s.kind.map(|k| EsInfo { pid: s.pid, kind: k, language: s.language.clone() })).collect()
    }
    fn pcr_index(&self) -> Option<usize> {
        self.streams.iter().position(|s| s.kind.is_some_and(Kind::is_video)).or_else(|| self.streams.iter().position(|s| s.kind.is_some()))
    }
    fn pcr_pid(&self) -> u16 {
        self.pcr_index().map(|i| self.streams[i].pid).unwrap_or(0x1fff)
    }
    fn psi(&mut self, out: &mut Vec<[u8; PACKET]>) {
        out.push(psi_packet(0, &mut self.pat_cc, &pat_section()));
        if self.pmt_dirty {
            self.pmt_version = (self.pmt_version + 1) & 0x1f;
            self.pmt_dirty = false;
        }
        let pmt = pmt_section(self.pmt_version, self.pcr_pid(), &self.es_infos());
        out.push(psi_packet(PMT_PID, &mut self.pmt_cc, &pmt));
    }
}

mod imp {
    use super::*;

    pub struct LzTsMux {
        pub(super) srcpad: gst::Pad,
        pub(super) settings: Mutex<Settings>,
        pub(super) state: Mutex<State>,
        /// hält die Reihenfolge der Ausgabe über Threads hinweg
        pub(super) push_lock: Mutex<()>,
        pub(super) running: Arc<AtomicBool>,
        pub(super) worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LzTsMux {
        const NAME: &'static str = "LzStreamerTsMux";
        type Type = super::LzTsMux;
        type ParentType = gst::Element;

        fn with_class(klass: &Self::Class) -> Self {
            let templ = klass.pad_template("src").unwrap();
            let srcpad = gst::Pad::builder_from_template(&templ)
                .query_function(|pad, parent, query| LzTsMux::catch_panic_pad_function(parent, || false, |imp| imp.src_query(pad, query)))
                .event_function(|pad, parent, event| LzTsMux::catch_panic_pad_function(parent, || false, |imp| imp.src_event(pad, event)))
                .build();
            Self {
                srcpad,
                settings: Mutex::new(Settings::default()),
                state: Mutex::new(State::default()),
                push_lock: Mutex::new(()),
                running: Arc::new(AtomicBool::new(false)),
                worker: Mutex::new(None),
            }
        }
    }

    impl ObjectImpl for LzTsMux {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().add_pad(&self.srcpad).unwrap();
        }

        fn properties() -> &'static [glib::ParamSpec] {
            static P: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
                vec![
                    glib::ParamSpecUInt::builder("delay")
                        .nick("Verzug")
                        .blurb("PCR to PTS distance in ms (0 = learn from the inputs)")
                        .maximum(2000)
                        .build(),
                    glib::ParamSpecUInt::builder("headroom")
                        .blurb("Receiver headroom in ms on top of the learned input lateness (delay=0)")
                        .maximum(2000)
                        .default_value(25)
                        .build(),
                    glib::ParamSpecUInt64::builder("bitrate").nick("Rate").blurb("Constant TS rate in bit/s, null-packet padded and paced (0 = VBR)").build(),
                    glib::ParamSpecUInt::builder("pcr-interval").blurb("PCR interval in ms").minimum(5).maximum(40).default_value(20).build(),
                    glib::ParamSpecUInt::builder("si-interval").blurb("PAT/PMT interval in ms").minimum(20).maximum(500).default_value(100).build(),
                    glib::ParamSpecUInt::builder("current-delay").blurb("PCR to PTS distance in use (ms)").read_only().build(),
                ]
            });
            P.as_ref()
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            let mut s = self.settings.lock().unwrap();
            match pspec.name() {
                "delay" => s.delay_ms = value.get().unwrap(),
                "headroom" => s.headroom_ms = value.get().unwrap(),
                "bitrate" => s.bitrate = value.get().unwrap(),
                "pcr-interval" => s.pcr_interval_ms = value.get().unwrap(),
                "si-interval" => s.si_interval_ms = value.get().unwrap(),
                _ => {}
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            let s = self.settings.lock().unwrap();
            match pspec.name() {
                "delay" => s.delay_ms.to_value(),
                "headroom" => s.headroom_ms.to_value(),
                "bitrate" => s.bitrate.to_value(),
                "pcr-interval" => s.pcr_interval_ms.to_value(),
                "si-interval" => s.si_interval_ms.to_value(),
                "current-delay" => ((self.state.lock().unwrap().delay_ns / MS) as u32).to_value(),
                _ => unimplemented!(),
            }
        }
    }

    impl GstObjectImpl for LzTsMux {}

    impl ElementImpl for LzTsMux {
        fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
            static META: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
                gst::subclass::ElementMetadata::new(
                    "LZ MPEG-TS muxer",
                    "Codec/Muxer",
                    "Low-latency MPEG-TS muxer for H.264/H.265 and AAC: no waiting for other inputs, PCR from the pipeline clock, optional CBR pacing with T-STD check",
                    "Lars Zumpe",
                )
            });
            Some(&*META)
        }

        fn pad_templates() -> &'static [gst::PadTemplate] {
            static T: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
                let sink = gst::Caps::builder_full()
                    .structure(gst::Structure::builder("video/x-h264").field("stream-format", "byte-stream").field("alignment", "au").build())
                    .structure(gst::Structure::builder("video/x-h265").field("stream-format", "byte-stream").field("alignment", "au").build())
                    .structure(gst::Structure::builder("audio/mpeg").field("mpegversion", 4i32).field("stream-format", gst::List::new(["adts", "raw"])).build())
                    .build();
                let src = gst::Caps::builder("video/mpegts").field("systemstream", true).field("packetsize", PACKET as i32).build();
                vec![
                    gst::PadTemplate::new("src", gst::PadDirection::Src, gst::PadPresence::Always, &src).unwrap(),
                    gst::PadTemplate::new("sink_%u", gst::PadDirection::Sink, gst::PadPresence::Request, &sink).unwrap(),
                ]
            });
            T.as_ref()
        }

        fn request_new_pad(&self, templ: &gst::PadTemplate, name: Option<&str>, _caps: Option<&gst::Caps>) -> Option<gst::Pad> {
            let mut st = self.state.lock().unwrap();
            // sink_65 → PID 65 wie bei mpegtsmux, sonst fortlaufend ab 0x41
            let pid = match name.and_then(|n| n.strip_prefix("sink_")).and_then(|n| n.parse::<u16>().ok()) {
                Some(p) if (0x10..0x1fff).contains(&p) && st.streams.iter().all(|s| s.pid != p) => p,
                _ => (FIRST_ES_PID..0x1fff).find(|p| st.streams.iter().all(|s| s.pid != *p))?,
            };
            let pad = gst::Pad::builder_from_template(templ)
                .name(format!("sink_{pid}"))
                .chain_function(|pad, parent, buf| LzTsMux::catch_panic_pad_function(parent, || Err(gst::FlowError::Error), |imp| imp.chain(pad, buf)))
                .event_function(|pad, parent, ev| LzTsMux::catch_panic_pad_function(parent, || false, |imp| imp.sink_event(pad, ev)))
                .build();
            st.streams.push(Stream {
                pad: pad.clone(),
                pid,
                stream_id: 0,
                kind: None,
                asc: None,
                language: None,
                segment: gst::FormattedSegment::new(),
                cc: 0,
                buffers: 0,
                eos: false,
                queue: VecDeque::new(),
                sent_cc: 15,
                tb: 0.0,
            });
            drop(st);
            pad.set_active(true).ok()?;
            self.obj().add_pad(&pad).ok()?;
            Some(pad)
        }

        fn release_pad(&self, pad: &gst::Pad) {
            let mut st = self.state.lock().unwrap();
            st.streams.retain(|s| &s.pad != pad);
            st.pmt_dirty = true;
            drop(st);
            let _ = self.obj().remove_pad(pad);
        }

        fn change_state(&self, t: gst::StateChange) -> Result<gst::StateChangeSuccess, gst::StateChangeError> {
            match t {
                gst::StateChange::ReadyToPaused => {
                    let mut st = self.state.lock().unwrap();
                    let streams = std::mem::take(&mut st.streams);
                    *st = State { streams, ..Default::default() };
                    for s in st.streams.iter_mut() {
                        s.buffers = 0;
                        s.eos = false;
                        s.queue.clear();
                    }
                    st.delay_ns = self.settings.lock().unwrap().delay_ms as u64 * MS;
                    drop(st);
                    self.start_worker();
                }
                gst::StateChange::PausedToReady => self.stop_worker(),
                _ => {}
            }
            self.parent_change_state(t)
        }
    }

    impl LzTsMux {
        fn now(&self) -> Option<u64> {
            let obj = self.obj();
            let clock = obj.clock()?;
            let base = obj.base_time()?;
            Some(clock.time().nseconds().saturating_sub(base.nseconds()))
        }

        fn start_worker(&self) {
            self.stop_worker();
            self.running.store(true, Ordering::SeqCst);
            let running = self.running.clone();
            let weak = self.obj().downgrade();
            let h = std::thread::Builder::new()
                .name("lztsmux".into())
                .spawn(move || {
                    while running.load(Ordering::SeqCst) {
                        let Some(obj) = weak.upgrade() else { break };
                        let wait = obj.imp().tick();
                        drop(obj);
                        if let Some(w) = wait {
                            std::thread::sleep(w);
                        }
                    }
                })
                .ok();
            *self.worker.lock().unwrap() = h;
        }

        fn stop_worker(&self) {
            self.running.store(false, Ordering::SeqCst);
            if let Some(h) = self.worker.lock().unwrap().take() {
                let _ = h.join();
            }
        }

        fn ensure_events(&self, st: &mut State) -> Vec<gst::Event> {
            if st.sent_events {
                return Vec::new();
            }
            st.sent_events = true;
            let caps = gst::Caps::builder("video/mpegts").field("systemstream", true).field("packetsize", PACKET as i32).build();
            let mut seg = gst::FormattedSegment::<gst::ClockTime>::new();
            seg.set_start(gst::ClockTime::ZERO);
            vec![gst::event::StreamStart::new("lztsmux"), gst::event::Caps::new(&caps), gst::event::Segment::new(&seg)]
        }

        fn push(&self, events: Vec<gst::Event>, packets: &[[u8; PACKET]], pts: u64) -> Result<gst::FlowSuccess, gst::FlowError> {
            for e in events {
                self.srcpad.push_event(e);
            }
            if packets.is_empty() {
                return Ok(gst::FlowSuccess::Ok);
            }
            let mut list = gst::BufferList::new_sized(packets.len().div_ceil(GROUP));
            {
                let l = list.get_mut().unwrap();
                for g in packets.chunks(GROUP) {
                    let mut b = gst::Buffer::with_size(g.len() * PACKET).map_err(|_| gst::FlowError::Error)?;
                    {
                        let bm = b.get_mut().unwrap();
                        bm.set_pts(gst::ClockTime::from_nseconds(pts));
                        let mut m = bm.map_writable().map_err(|_| gst::FlowError::Error)?;
                        for (i, p) in g.iter().enumerate() {
                            m[i * PACKET..(i + 1) * PACKET].copy_from_slice(p);
                        }
                    }
                    l.add(b);
                }
            }
            self.srcpad.push_list(list)
        }

        /// Takt-Thread: CBR sendet Paket für Paket, VBR hält nur den PCR-Abstand ein.
        /// Rückgabe: wie lange bis zum nächsten Aufruf geschlafen werden soll.
        fn tick(&self) -> Option<std::time::Duration> {
            let settings = self.settings.lock().unwrap().clone();
            let idle = Some(std::time::Duration::from_millis(2));
            let Some(now) = self.now() else { return idle };
            let _g = self.push_lock.lock().unwrap();
            let mut st = self.state.lock().unwrap();
            if st.eos_sent || st.streams.iter().all(|s| s.kind.is_none()) {
                return idle;
            }
            if settings.bitrate == 0 {
                // VBR: fehlt die PCR zu lange (kein Bild), ein PCR-Paket nachschieben
                let iv = settings.pcr_interval_ms as u64 * MS;
                if st.last_pcr.is_some_and(|l| now.saturating_sub(l) >= iv) {
                    let mut out = Vec::new();
                    if let Some(i) = st.pcr_index() {
                        let (pid, cc) = (st.streams[i].pid, st.streams[i].cc);
                        out.push(pcr_packet(pid, cc, ns_to_27m(now), false));
                        st.last_pcr = Some(now);
                    }
                    let ev = self.ensure_events(&mut st);
                    drop(st);
                    let _ = self.push(ev, &out, now);
                }
                return Some(std::time::Duration::from_millis(5));
            }
            // CBR
            let dur = (PACKET as f64 * 8.0 * 1e9) / settings.bitrate as f64;
            let t0 = match st.t0 {
                Some(t) => t,
                None => {
                    if st.queued == 0 {
                        return idle;
                    }
                    st.t0 = Some(now);
                    st.slot = 0;
                    st.tb_time = now;
                    now
                }
            };
            let slot_t = |k: u64| t0 + (k as f64 * dur) as u64;
            let mut t = slot_t(st.slot);
            if t > now {
                let w = (t - now).min(5 * MS);
                return Some(std::time::Duration::from_nanos(w));
            }
            if now - t > 100 * MS {
                // Takt verloren (Rechner hing): neu aufsetzen, PCR mit Unstetigkeit
                st.t0 = Some(now);
                st.slot = 0;
                st.discont = true;
                t = now;
            }
            let first_t = t;
            let mut out: Vec<[u8; PACKET]> = Vec::with_capacity(GROUP);
            let pcr_iv = settings.pcr_interval_ms as u64 * MS;
            let si_iv = settings.si_interval_ms as u64 * MS;
            while out.len() < GROUP {
                let t0 = st.t0.unwrap();
                let tk = t0 + (st.slot as f64 * dur) as u64;
                st.slot += 1;
                // Transportpuffer leeren
                let dt = tk.saturating_sub(st.tb_time) as f64 / 1e9;
                st.tb_time = tk;
                let ts_rate = settings.bitrate as f64;
                for s in st.streams.iter_mut() {
                    let rx = if s.kind == Some(Kind::Aac) { RX_AUDIO } else { ts_rate * 1.2 };
                    s.tb = (s.tb - rx / 8.0 * dt).max(0.0);
                }
                if let Some(p) = st.pending_psi.pop() {
                    out.push(p);
                    continue;
                }
                if st.last_pcr.is_none_or(|l| tk.saturating_sub(l) >= pcr_iv) {
                    if let Some(i) = st.pcr_index() {
                        let (pid, cc) = (st.streams[i].pid, (st.streams[i].sent_cc + 1) & 0x0f);
                        let d = std::mem::take(&mut st.discont);
                        out.push(pcr_packet(pid, cc, ns_to_27m(tk), d));
                        st.last_pcr = Some(tk);
                        continue;
                    }
                }
                if st.last_si.is_none_or(|l| tk.saturating_sub(l) >= si_iv) || st.pmt_dirty {
                    let mut psi = Vec::new();
                    st.psi(&mut psi);
                    st.last_si = Some(tk);
                    psi.reverse();
                    out.push(psi.pop().unwrap());
                    st.pending_psi = psi;
                    continue;
                }
                // ES-Paket: ältester Strom zuerst, der in seinen Transportpuffer passt
                let pick = st
                    .streams
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| !s.queue.is_empty() && s.tb + PACKET as f64 <= TB_SIZE)
                    .min_by_key(|(_, s)| s.queue[0].0)
                    .map(|(i, _)| i);
                match pick {
                    Some(i) => {
                        let s = &mut st.streams[i];
                        let (_, p) = s.queue.pop_front().unwrap();
                        s.sent_cc = p[3] & 0x0f;
                        s.tb += PACKET as f64;
                        st.queued -= 1;
                        out.push(p);
                    }
                    None => out.push(null_packet()),
                }
            }
            let eos = st.streams.iter().all(|s| s.eos) && st.queued == 0;
            let ev = self.ensure_events(&mut st);
            if eos {
                st.eos_sent = true;
            }
            drop(st);
            let _ = self.push(ev, &out, first_t);
            if eos {
                self.srcpad.push_event(gst::event::Eos::new());
            }
            None
        }

        fn chain(&self, pad: &gst::Pad, buf: gst::Buffer) -> Result<gst::FlowSuccess, gst::FlowError> {
            let settings = self.settings.lock().unwrap().clone();
            let _g = if settings.bitrate == 0 { Some(self.push_lock.lock().unwrap()) } else { None };
            let mut st = self.state.lock().unwrap();
            let idx = st.streams.iter().position(|s| &s.pad == pad).ok_or(gst::FlowError::NotLinked)?;
            let kind = st.streams[idx].kind.ok_or(gst::FlowError::NotNegotiated)?;
            let seg = st.streams[idx].segment.clone();
            let rt = |t: Option<gst::ClockTime>| -> Option<i64> {
                let t = t?;
                match seg.to_running_time_full(t)? {
                    gst::Signed::Positive(v) => Some(v.nseconds() as i64),
                    gst::Signed::Negative(v) => Some(-(v.nseconds() as i64)),
                }
            };
            let Some(pts_rt) = rt(buf.pts()).or_else(|| rt(buf.dts())) else {
                gst::warning!(gst::CAT_DEFAULT, imp = self, "Puffer ohne Zeitstempel verworfen");
                return Ok(gst::FlowSuccess::Ok);
            };
            let dts_rt = rt(buf.dts()).unwrap_or(pts_rt);
            let now = self.now().unwrap_or(dts_rt.max(0) as u64);
            st.streams[idx].buffers += 1;
            let nbuf = st.streams[idx].buffers;
            // Verzug: wie spät ist der Puffer, bis er gesendet ist?
            let drain = if settings.bitrate > 0 { (st.queued * PACKET * 8) as u64 * 1_000_000_000 / settings.bitrate } else { 0 };
            let late = (now as i64 + drain as i64 - dts_rt).max(0) as u64;
            if settings.delay_ms == 0 && nbuf > 3 {
                // die ersten Puffer je Eingang laufen sich noch ein und zählen nicht
                let margin = settings.headroom_ms as u64 * MS;
                let learn_until = *st.learn_until.get_or_insert(now + LEARN_MS * MS);
                if late + margin > st.delay_ns {
                    let raise = if now <= learn_until {
                        Some(late)
                    } else {
                        // danach nur bei anhaltender Verspätung (≥ 3 Puffer in 1 s): ein einzelner
                        // Ausreißer (Rechner kurz ausgelastet) soll D nicht für immer anheben
                        st.exceed.push_back((now, late));
                        while st.exceed.front().is_some_and(|(t, _)| now.saturating_sub(*t) > 1000 * MS) {
                            st.exceed.pop_front();
                        }
                        (st.exceed.len() >= 3).then(|| st.exceed.iter().map(|(_, l)| *l).max().unwrap_or(late))
                    };
                    if let Some(l) = raise {
                        // 5 ms darüber, damit kleines Zittern D nicht ständig anhebt
                        let new = (l + margin + 5 * MS).div_ceil(MS) * MS;
                        if now > learn_until {
                            st.exceed.clear();
                            gst::info!(gst::CAT_DEFAULT, imp = self, "Verzug {} → {} ms (PID {:#x})", st.delay_ns / MS, new / MS, st.streams[idx].pid);
                            let _ = self.obj().post_message(
                                gst::message::Element::builder(gst::Structure::builder("lztsmux-delay").field("delay-ms", (new / MS) as u32).build())
                                    .src(&*self.obj())
                                    .build(),
                            );
                        }
                        st.delay_ns = new;
                    }
                }
                // Verspätungen der letzten 2 s; ist D größer als nötig (Ausreißer, Anlauf),
                // gleitet er langsam zurück: 0,2 ms je Bild, bei 50p 1 % – der Empfänger
                // spielt kurz minimal schneller, ohne Sprung in den Zeitstempeln
                st.window.push_back((now, late));
                while st.window.front().is_some_and(|(t, _)| now.saturating_sub(*t) > 2000 * MS) {
                    st.window.pop_front();
                }
                if now > learn_until && st.pcr_index() == Some(idx) {
                    let need = st.window.iter().map(|(_, l)| *l).max().unwrap_or(0) + margin + 5 * MS;
                    if st.delay_ns > need + MS {
                        st.delay_ns -= (st.delay_ns - need).min(SLEW_NS);
                    }
                }
            }
            let d = st.delay_ns as i64;
            let pts90 = ns_to_90k(pts_rt + d);
            let dts90 = ns_to_90k(dts_rt + d);
            let map = buf.map_readable().map_err(|_| gst::FlowError::Error)?;
            let mut payload: Vec<u8> = Vec::with_capacity(map.len() + 16);
            match kind {
                Kind::Aac => {
                    if let Some(asc) = st.streams[idx].asc.as_deref() {
                        payload.extend_from_slice(&adts_header(asc, map.len()).ok_or(gst::FlowError::NotNegotiated)?);
                    }
                }
                _ if !starts_with_aud(kind, &map) => payload.extend_from_slice(aud(kind)),
                _ => {}
            }
            payload.extend_from_slice(&map);
            drop(map);
            let mut pes = pes_header(st.streams[idx].stream_id, payload.len(), pts90, kind.is_video().then_some(dts90));
            pes.extend_from_slice(&payload);
            let key = !buf.flags().contains(gst::BufferFlags::DELTA_UNIT);
            let mut out = Vec::with_capacity(pes.len() / 184 + 4);
            let si_iv = settings.si_interval_ms as u64 * MS;
            let pcr_iv = settings.pcr_interval_ms as u64 * MS;
            if settings.bitrate == 0 {
                if st.last_si.is_none_or(|l| now.saturating_sub(l) >= si_iv) || st.pmt_dirty {
                    st.psi(&mut out);
                    st.last_si = Some(now);
                }
                let pcr_here = st.pcr_index() == Some(idx);
                let pcr_due = st.last_pcr.is_none_or(|l| now.saturating_sub(l) >= pcr_iv / 2) || (pcr_here && key);
                let mut pcr = None;
                if pcr_due {
                    if pcr_here {
                        pcr = Some(ns_to_27m(now));
                    } else if let Some(i) = st.pcr_index() {
                        let (pid, cc) = (st.streams[i].pid, st.streams[i].cc);
                        out.push(pcr_packet(pid, cc, ns_to_27m(now), false));
                    }
                    st.last_pcr = Some(now);
                }
                let s = &mut st.streams[idx];
                packetize(s.pid, &mut s.cc, &pes, pcr, key && kind.is_video(), &mut out);
                let ev = self.ensure_events(&mut st);
                drop(st);
                self.push(ev, &out, now)
            } else {
                let seq = st.seq;
                st.seq += 1;
                let s = &mut st.streams[idx];
                packetize(s.pid, &mut s.cc, &pes, None, key && kind.is_video(), &mut out);
                // Warteschlange begrenzen: mehr als 2 s Rückstand heißt, die Rate reicht nicht
                let limit = (settings.bitrate / 8 * 2) as usize / PACKET;
                let n = out.len();
                s.queue.extend(out.into_iter().map(|p| (seq, p)));
                st.queued += n;
                if st.queued > limit {
                    let drop_n = st.queued - limit;
                    let s = &mut st.streams[idx];
                    let k = drop_n.min(s.queue.len());
                    s.queue.drain(..k);
                    st.queued -= k;
                    if !st.warned_overflow {
                        st.warned_overflow = true;
                        drop(st);
                        gst::element_imp_warning!(self, gst::StreamError::Mux, ["TS-Rate {} kbit/s reicht nicht für Bild und Ton", settings.bitrate / 1000]);
                    }
                }
                Ok(gst::FlowSuccess::Ok)
            }
        }

        fn sink_event(&self, pad: &gst::Pad, event: gst::Event) -> bool {
            use gst::EventView;
            let mut st = self.state.lock().unwrap();
            let Some(idx) = st.streams.iter().position(|s| &s.pad == pad) else { return false };
            match event.view() {
                EventView::Caps(c) => {
                    let Some(s) = c.caps().structure(0) else { return false };
                    let (kind, sid) = match s.name().as_str() {
                        "video/x-h264" => (Kind::H264, 0xe0),
                        "video/x-h265" => (Kind::H265, 0xe0),
                        "audio/mpeg" => (Kind::Aac, 0xc0),
                        _ => return false,
                    };
                    let n_same = st.streams.iter().filter(|x| x.kind.is_some_and(|k| k.is_video() == kind.is_video()) && x.pad != *pad).count() as u8;
                    let raw = s.get::<&str>("stream-format").is_ok_and(|f| f == "raw");
                    let asc = if raw { s.get::<gst::Buffer>("codec_data").ok().and_then(|b| b.map_readable().ok().map(|m| m.to_vec())) } else { None };
                    if raw && asc.is_none() {
                        return false;
                    }
                    let x = &mut st.streams[idx];
                    if x.kind != Some(kind) {
                        x.stream_id = sid + n_same.min(15);
                    }
                    x.kind = Some(kind);
                    x.asc = asc;
                    st.pmt_dirty = true;
                    true
                }
                EventView::Segment(s) => {
                    if let Ok(seg) = s.segment().clone().downcast::<gst::ClockTime>() {
                        st.streams[idx].segment = seg;
                    }
                    true
                }
                EventView::Tag(t) => {
                    if let Some(l) = t.tag().get::<gst::tags::LanguageCode>() {
                        let code = l.get().to_string();
                        // zweistellige Codes (de) auf drei Buchstaben (deu) bringen
                        let code = match code.as_str() {
                            "de" => "deu".to_string(),
                            "en" => "eng".to_string(),
                            "fr" => "fra".to_string(),
                            "es" => "spa".to_string(),
                            "it" => "ita".to_string(),
                            c => c.to_string(),
                        };
                        if st.streams[idx].language.as_deref() != Some(code.as_str()) {
                            st.streams[idx].language = Some(code);
                            st.pmt_dirty = true;
                        }
                    }
                    true
                }
                EventView::Eos(_) => {
                    st.streams[idx].eos = true;
                    let all = st.streams.iter().all(|s| s.eos);
                    let cbr = self.settings.lock().unwrap().bitrate > 0;
                    if all && !cbr && !st.eos_sent {
                        st.eos_sent = true;
                        drop(st);
                        let _g = self.push_lock.lock().unwrap();
                        self.srcpad.push_event(gst::event::Eos::new());
                    }
                    true
                }
                EventView::FlushStart(_) | EventView::FlushStop(_) => {
                    drop(st);
                    self.srcpad.push_event(event)
                }
                _ => {
                    if event.is_serialized() {
                        true
                    } else {
                        drop(st);
                        gst::Pad::event_default(pad, Some(&*self.obj()), event)
                    }
                }
            }
        }

        fn src_query(&self, pad: &gst::Pad, query: &mut gst::QueryRef) -> bool {
            match query.view_mut() {
                gst::QueryViewMut::Latency(q) => {
                    // Zeitstempel am Ausgang sind die Sendezeit: keine zusätzliche Latenz
                    let pads: Vec<gst::Pad> = self.state.lock().unwrap().streams.iter().map(|s| s.pad.clone()).collect();
                    let mut live = false;
                    let mut min = gst::ClockTime::ZERO;
                    for p in pads {
                        let mut uq = gst::query::Latency::new();
                        if p.peer_query(&mut uq) {
                            let (l, mn, _) = uq.result();
                            live |= l;
                            min = min.max(mn);
                        }
                    }
                    let _ = min;
                    q.set(live, gst::ClockTime::ZERO, gst::ClockTime::NONE);
                    true
                }
                _ => gst::Pad::query_default(pad, Some(&*self.obj()), query),
            }
        }

        fn src_event(&self, pad: &gst::Pad, event: gst::Event) -> bool {
            match event.view() {
                // Encoder-Anforderungen (Schlüsselbild) an alle Eingänge weiterreichen
                gst::EventView::CustomUpstream(_) => {
                    let pads: Vec<gst::Pad> = self.state.lock().unwrap().streams.iter().map(|s| s.pad.clone()).collect();
                    pads.iter().fold(false, |a, p| p.push_event(event.clone()) || a)
                }
                _ => gst::Pad::event_default(pad, Some(&*self.obj()), event),
            }
        }
    }
}

glib::wrapper! {
    pub struct LzTsMux(ObjectSubclass<imp::LzTsMux>) @extends gst::Element, gst::Object;
}

/// Einmal je Prozess registrieren.
pub fn register() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = gst::Element::register(None, "lztsmux", gst::Rank::NONE, LzTsMux::static_type());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_of_known_pat() {
        // PAT von mpegtsmux (Programm 1 → PMT 0x20): CRC 0x2ab104b2? – hier gegen die Rechnung selbst prüfen
        let s = pat_section();
        assert_eq!(crc32(&s), 0, "CRC über Tabelle samt CRC ergibt 0");
        assert_eq!(&s[..8], &[0x00, 0xb0, 0x0d, 0x00, 0x01, 0xc1, 0x00, 0x00]);
        assert_eq!(&s[8..12], &[0x00, 0x01, 0xe0, 0x20]);
    }

    #[test]
    fn pmt_has_streams_and_language() {
        let s = pmt_section(3, 0x41, &[EsInfo { pid: 0x41, kind: Kind::H264, language: None }, EsInfo { pid: 0x42, kind: Kind::Aac, language: Some("deu".into()) }]);
        assert_eq!(crc32(&s), 0);
        assert_eq!(s[5], 0xc1 | (3 << 1));
        assert_eq!(&s[8..10], &[0xe0, 0x41]);
        assert_eq!(&s[12..17], &[0x1b, 0xe0, 0x41, 0xf0, 0x00]);
        assert_eq!(&s[17..22], &[0x0f, 0xe0, 0x42, 0xf0, 0x06]);
        assert_eq!(&s[22..28], &[0x0a, 4, b'd', b'e', b'u', 0]);
    }

    #[test]
    fn pts_and_pcr_encoding() {
        assert_eq!(ts_bytes(2, 0), [0x21, 0, 1, 0, 1]);
        assert_eq!(ts_bytes(2, MASK_33), [0x2f, 0xff, 0xff, 0xff, 0xff]);
        let p = pcr_bytes(27_000_000 * 3600 + 299);
        let base = (p[0] as u64) << 25 | (p[1] as u64) << 17 | (p[2] as u64) << 9 | (p[3] as u64) << 1 | (p[4] as u64) >> 7;
        let ext = ((p[4] as u64 & 1) << 8) | p[5] as u64;
        assert_eq!(base * 300 + ext, 27_000_000 * 3600 + 299);
        // 1 s Laufzeit + 1 h Basis
        assert_eq!(ns_to_90k(1_000_000_000), BASE_90K + 90_000);
        assert_eq!(ns_to_90k(-1_000_000_000), BASE_90K - 90_000);
        assert_eq!(ns_to_27m(1_000_000_000), BASE_90K * 300 + 27_000_000);
        assert_eq!(ns_to_27m(37), BASE_90K * 300 + 0); // < ein Takt
    }

    #[test]
    fn packetize_fills_and_stuffs() {
        let pes: Vec<u8> = (0..500u32).map(|i| i as u8).collect();
        let mut out = Vec::new();
        let mut cc = 15;
        packetize(0x41, &mut cc, &pes, Some(123_456), true, &mut out);
        assert_eq!(out.len(), 3);
        assert_eq!(cc, 2, "CC läuft über 15 → 0");
        // erstes Paket: PUSI, AF mit RAI und PCR
        assert_eq!(out[0][1] & 0x40, 0x40);
        assert_eq!(out[0][3] & 0x30, 0x30);
        assert_eq!(out[0][5], 0x50);
        let payload: Vec<u8> = out
            .iter()
            .flat_map(|p| {
                let start = if p[3] & 0x20 != 0 { 5 + p[4] as usize } else { 4 };
                p[start..].to_vec()
            })
            .collect();
        assert_eq!(payload, pes);
        // genau 183 Byte Rest → AF mit Länge 0 (ein Byte)
        let mut out = Vec::new();
        packetize(0x42, &mut cc, &[7u8; 183], None, false, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0][4], 0);
        assert_eq!(out[0][5], 7);
    }

    #[test]
    fn pes_header_lengths() {
        let h = pes_header(0xc0, 100, 1000, None);
        assert_eq!(h.len(), 14);
        assert_eq!(u16::from_be_bytes([h[4], h[5]]) as usize, 3 + 5 + 100);
        let h = pes_header(0xe0, 200_000, 1000, Some(900));
        assert_eq!(h.len(), 19);
        assert_eq!(&h[4..6], &[0, 0], "Bild über 64 KiB: Länge 0");
        assert_eq!(h[7], 0xc0);
    }

    #[test]
    fn adts_from_codec_data() {
        // AAC-LC, 48 kHz, Stereo
        let h = adts_header(&[0x11, 0x90], 100).unwrap();
        assert_eq!(&h[..2], &[0xff, 0xf1]);
        assert_eq!(h[2] >> 6, 1, "Profil LC");
        assert_eq!((h[2] >> 2) & 0x0f, 3, "48 kHz");
        let len = ((h[3] as usize & 3) << 11) | (h[4] as usize) << 3 | (h[5] as usize) >> 5;
        assert_eq!(len, 107);
        assert!(adts_header(&[0x00], 1).is_none());
    }

    #[test]
    fn aud_detection() {
        assert!(starts_with_aud(Kind::H264, &[0, 0, 0, 1, 0x09, 0xf0]));
        assert!(!starts_with_aud(Kind::H264, &[0, 0, 0, 1, 0x67]));
        assert!(starts_with_aud(Kind::H265, &[0, 0, 1, 0x46, 0x01]));
        assert!(!starts_with_aud(Kind::H265, &[0, 0, 0, 1, 0x40, 0x01]));
    }

    #[test]
    fn pcr_only_packet_keeps_cc() {
        let p = pcr_packet(0x41, 5, 1000, false);
        assert_eq!(p[3], 0x20 | 4, "kein Nutzinhalt: CC bleibt beim letzten Wert");
        assert_eq!(p[4], 183);
        assert_eq!(p[5], 0x10);
    }
}
