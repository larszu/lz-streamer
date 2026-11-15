//! Ancillary-Daten (KB-61): Timecode (SMPTE 12M-2 ATC) und AFD (SMPTE 2016)
//! als SMPTE ST 2038 im Transportstrom, und zurück.
//!
//! Der Encoder liest je Bild die GStreamer-Metas (Timecode, AFD) und baut
//! daraus ST-2038-Pakete für eine eigene PID (`meta/x-st-2038`, Kennung
//! „VANC“). Der Decoder zerlegt sie wieder und zeigt Timecode und AFD an.

use serde::{Deserialize, Serialize};

/// Ein ANC-Paket nach SMPTE 291 mit Lage im Bild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AncPacket {
    pub c_not_y: bool,
    pub line: u16,
    pub horizontal_offset: u16,
    pub did: u8,
    pub sdid: u8,
    pub data: Vec<u8>,
}

pub const DID_ATC: (u8, u8) = (0x60, 0x60);
pub const DID_AFD: (u8, u8) = (0x41, 0x05);

/// 8-Bit-Wert → 10-Bit-Wort mit gerader Parität (b8) und b9 = !b8.
fn word(v: u8) -> u16 {
    let p = (v.count_ones() & 1) as u16;
    v as u16 | (p << 8) | ((p ^ 1) << 9)
}

struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    fn put(&mut self, bits: u32, v: u64) {
        self.acc = (self.acc << bits) | (v & ((1 << bits) - 1));
        self.n += bits;
        while self.n >= 8 {
            self.n -= 8;
            self.out.push((self.acc >> self.n) as u8);
        }
    }
    /// bis zur Bytegrenze mit Einsen auffüllen
    fn align_ones(&mut self) {
        if self.n > 0 {
            let pad = 8 - self.n;
            self.put(pad, (1 << pad) - 1);
        }
    }
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl BitReader<'_> {
    fn get(&mut self, bits: usize) -> Option<u32> {
        if self.pos + bits > self.data.len() * 8 {
            return None;
        }
        let mut v = 0u32;
        for _ in 0..bits {
            let b = (self.data[self.pos / 8] >> (7 - self.pos % 8)) & 1;
            v = (v << 1) | b as u32;
            self.pos += 1;
        }
        Some(v)
    }
}

/// Pakete in eine ST-2038-Nutzlast (eine PES je Bild) schreiben.
pub fn st2038_encode(packets: &[AncPacket]) -> Vec<u8> {
    let mut w = BitWriter { out: Vec::new(), acc: 0, n: 0 };
    for p in packets {
        let data = &p.data[..p.data.len().min(255)];
        w.put(6, 0);
        w.put(1, p.c_not_y as u64);
        w.put(11, p.line as u64);
        w.put(12, p.horizontal_offset as u64);
        let words: Vec<u16> = [p.did, p.sdid, data.len() as u8].into_iter().chain(data.iter().copied()).map(word).collect();
        let sum = words.iter().fold(0u16, |s, x| s.wrapping_add(x & 0x1ff)) & 0x1ff;
        for x in &words {
            w.put(10, *x as u64);
        }
        w.put(10, (sum | ((!sum & 0x100) << 1)) as u64);
        w.align_ones();
    }
    w.out
}

/// ST-2038-Nutzlast zerlegen. Pakete mit falscher Prüfsumme werden verworfen.
pub fn st2038_decode(data: &[u8]) -> Vec<AncPacket> {
    let mut r = BitReader { data, pos: 0 };
    let mut out = Vec::new();
    while let Some(zero) = r.get(6) {
        if zero != 0 {
            // Füllbits (0xFF) am Ende
            break;
        }
        let (Some(c), Some(line), Some(off), Some(did), Some(sdid), Some(dc)) = (r.get(1), r.get(11), r.get(12), r.get(10), r.get(10), r.get(10)) else { break };
        let n = (dc & 0xff) as usize;
        let mut udw = Vec::with_capacity(n);
        let mut sum = (did & 0x1ff) + (sdid & 0x1ff) + (dc & 0x1ff);
        for _ in 0..n {
            let Some(x) = r.get(10) else { return out };
            sum += x & 0x1ff;
            udw.push((x & 0xff) as u8);
        }
        let Some(cs) = r.get(10) else { break };
        // auf Byte ausrichten
        r.pos = r.pos.div_ceil(8) * 8;
        if cs & 0x1ff != sum & 0x1ff {
            continue;
        }
        out.push(AncPacket { c_not_y: c == 1, line: line as u16, horizontal_offset: off as u16, did: (did & 0xff) as u8, sdid: (sdid & 0xff) as u8, data: udw });
    }
    out
}

/// Timecode, wie er angezeigt wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Timecode {
    pub hours: u8,
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u8,
    pub drop_frame: bool,
}

impl std::fmt::Display for Timecode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sep = if self.drop_frame { ';' } else { ':' };
        write!(f, "{:02}:{:02}:{:02}{sep}{:02}", self.hours, self.minutes, self.seconds, self.frames)
    }
}

/// ATC nach SMPTE 12M-2: 16 Datenwörter, je Wort 4 Bit des 64-Bit-Timecodes
/// in b4–b7 (Einer/Zehner, Binärgruppen 0). DBB1 (b3 der Wörter 1–8) = 0x01, VITC.
pub fn atc_packet(tc: &Timecode, line: u16) -> AncPacket {
    let bcd = |v: u8| (v % 10, v / 10);
    let (fu, ft) = bcd(tc.frames);
    let (su, st) = bcd(tc.seconds);
    let (mu, mt) = bcd(tc.minutes);
    let (hu, ht) = bcd(tc.hours);
    let nib = [fu, 0, ft & 0x3 | if tc.drop_frame { 0x4 } else { 0 }, 0, su, 0, st & 0x7, 0, mu, 0, mt & 0x7, 0, hu, 0, ht & 0x3, 0];
    let dbb1 = 0x01u8;
    let data = nib.iter().enumerate().map(|(i, n)| (n << 4) | if i < 8 && (dbb1 >> i) & 1 == 1 { 0x08 } else { 0 }).collect();
    AncPacket { c_not_y: false, line, horizontal_offset: 0, did: DID_ATC.0, sdid: DID_ATC.1, data }
}

pub fn parse_atc(p: &AncPacket) -> Option<Timecode> {
    if (p.did, p.sdid) != DID_ATC || p.data.len() < 16 {
        return None;
    }
    let n = |i: usize| p.data[i] >> 4;
    Some(Timecode {
        frames: n(0) + 10 * (n(2) & 0x3),
        drop_frame: n(2) & 0x4 != 0,
        seconds: n(4) + 10 * (n(6) & 0x7),
        minutes: n(8) + 10 * (n(10) & 0x7),
        hours: n(12) + 10 * (n(14) & 0x3),
    })
}

/// AFD nach SMPTE 2016-3: erstes Wort AFD-Code (b3–b6) und Seitenverhältnis (b2), Rest Bar-Daten (leer).
pub fn afd_packet(afd: u8, wide: bool, line: u16) -> AncPacket {
    let mut data = vec![0u8; 8];
    data[0] = ((afd & 0xf) << 3) | if wide { 0x04 } else { 0 };
    AncPacket { c_not_y: false, line, horizontal_offset: 0, did: DID_AFD.0, sdid: DID_AFD.1, data }
}

/// (AFD-Code, 16:9)
pub fn parse_afd(p: &AncPacket) -> Option<(u8, bool)> {
    if (p.did, p.sdid) != DID_AFD || p.data.is_empty() {
        return None;
    }
    Some(((p.data[0] >> 3) & 0xf, p.data[0] & 0x04 != 0))
}

/// Klartext für die Anzeige (SMPTE 2016-1, Tabelle 1, gängige Werte).
pub fn afd_label(code: u8, wide: bool) -> String {
    let what = match code {
        0b0010 => "Box 16:9 (oben)",
        0b0011 => "Box 14:9 (oben)",
        0b0100 => "Box > 16:9 (Mitte)",
        0b1000 => "wie Bild",
        0b1001 => "4:3 (Mitte)",
        0b1010 => "16:9 (Mitte)",
        0b1011 => "14:9 (Mitte)",
        0b1101 => "4:3, geschützt 14:9",
        0b1110 => "16:9, geschützt 14:9",
        0b1111 => "16:9, geschützt 4:3",
        _ => "reserviert",
    };
    format!("AFD {code} ({what}, Bild {})", if wide { "16:9" } else { "4:3" })
}

/// Anzeige im Status von Encoder und Decoder.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AncStatus {
    pub timecode: Option<String>,
    pub afd: Option<String>,
    /// ST-2038-Pakete seit Start (Encoder: gesendet, Decoder: empfangen)
    pub packets: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn st2038_roundtrip_with_timecode_and_afd() {
        let tc = Timecode { hours: 23, minutes: 59, seconds: 58, frames: 24, drop_frame: false };
        let pk = [atc_packet(&tc, 9), afd_packet(0b1010, true, 11)];
        let bytes = st2038_encode(&pk);
        let back = st2038_decode(&bytes);
        assert_eq!(back, pk);
        assert_eq!(parse_atc(&back[0]), Some(tc));
        assert_eq!(parse_atc(&back[0]).unwrap().to_string(), "23:59:58:24");
        assert_eq!(parse_afd(&back[1]), Some((10, true)));
        assert!(afd_label(10, true).contains("16:9 (Mitte)"));
        // kaputte Prüfsumme → verworfen
        let mut bad = st2038_encode(&pk[..1]);
        bad[6] ^= 0x10;
        assert!(st2038_decode(&bad).is_empty());
    }

    #[test]
    fn word_parity() {
        assert_eq!(word(0x00), 0x200);
        assert_eq!(word(0x01), 0x101);
        assert_eq!(word(0x60), 0x260);
    }

    /// Gegenprobe mit dem ST-2038-Demuxer von GStreamer: er findet beide Pakete.
    #[test]
    fn gstreamer_parses_our_st2038() {
        use gstreamer as gst;
        use gst::prelude::*;
        gst::init().unwrap();
        if gst::ElementFactory::find("st2038ancdemux").is_none() {
            return;
        }
        let tc = Timecode { hours: 1, minutes: 2, seconds: 3, frames: 4, drop_frame: false };
        let bytes = st2038_encode(&[atc_packet(&tc, 9), afd_packet(8, true, 11)]);
        let p = gst::parse::launch("appsrc name=s caps=meta/x-st-2038,alignment=frame format=time ! st2038ancdemux name=d").unwrap().downcast::<gst::Pipeline>().unwrap();
        let pads: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
        let pw = pads.clone();
        let pl = p.downgrade();
        p.by_name("d").unwrap().connect_pad_added(move |_, pad| {
            pw.lock().unwrap().push(pad.name().to_string());
            if let Some(p) = pl.upgrade() {
                let s = gst::ElementFactory::make("fakesink").build().unwrap();
                p.add(&s).unwrap();
                s.sync_state_with_parent().unwrap();
                pad.link(&s.static_pad("sink").unwrap()).unwrap();
            }
        });
        p.set_state(gst::State::Playing).unwrap();
        let src = p.by_name("s").unwrap().downcast::<gstreamer_app::AppSrc>().unwrap();
        let mut b = gst::Buffer::from_slice(bytes);
        b.get_mut().unwrap().set_pts(gst::ClockTime::ZERO);
        src.push_buffer(b).unwrap();
        src.end_of_stream().unwrap();
        let _ = p.bus().unwrap().timed_pop_filtered(gst::ClockTime::from_seconds(5), &[gst::MessageType::Eos, gst::MessageType::Error]);
        p.set_state(gst::State::Null).unwrap();
        let pads = pads.lock().unwrap().clone();
        assert!(pads.iter().any(|n| n.starts_with("anc_60_60")) && pads.iter().any(|n| n.starts_with("anc_41_05")), "{pads:?}");
    }
}
