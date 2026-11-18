//! TS-Prüfung nach ETSI TR 101 290, Priorität 1 und 2, dazu der Transportpuffer
//! des T-STD (ISO/IEC 13818-1 2.4.2). Zeitbezug ist die PCR; die Ankunftszeit
//! eines Pakets ergibt sich aus seiner Byte-Position bei der aus den PCR
//! geschätzten Rate (gilt streng nur bei konstanter Rate).

use std::collections::HashMap;

use serde::Serialize;

use crate::tsmux::{PACKET, RX_AUDIO, TB_SIZE, crc32};

#[derive(Debug, Clone, Default, Serialize)]
pub struct Tr101290 {
    pub packets: u64,
    pub null_packets: u64,
    /// 1.1/1.2: Sync-Byte fehlt
    pub sync_errors: u64,
    /// 1.3: PAT fehlt > 0,5 s, falsche table_id oder verschlüsselt
    pub pat_errors: u64,
    /// 1.4: CC-Sprung
    pub cc_errors: u64,
    /// 1.5: PMT fehlt > 0,5 s
    pub pmt_errors: u64,
    /// 1.6: in der PMT genannte PID kommt > 5 s nicht vor
    pub pid_errors: u64,
    /// 2.1: Transport-Error-Bit gesetzt
    pub transport_errors: u64,
    /// 2.2: CRC von PAT/PMT falsch
    pub crc_errors: u64,
    /// 2.3a: PCR-Abstand > 40 ms
    pub pcr_repetition_errors: u64,
    /// 2.3b: PCR-Sprung > 100 ms ohne Unstetigkeitskennzeichen
    pub pcr_discontinuity_errors: u64,
    /// 2.4: PCR weicht mehr als ±500 ns von der Rate ab (nur bei konstanter Rate aussagekräftig)
    pub pcr_accuracy_errors: u64,
    /// 2.5: PTS-Abstand > 700 ms
    pub pts_errors: u64,
    /// T-STD: Transportpuffer (512 Byte) eines Elementarstroms läuft über
    pub tstd_tb_overflows: u64,
    pub max_pat_gap_ms: f64,
    pub max_pmt_gap_ms: f64,
    pub max_pcr_gap_ms: f64,
    pub max_pcr_accuracy_ns: f64,
    pub max_pts_gap_ms: f64,
    /// PCR → PTS des Bilds (Mittel), also der Puffer, den der Empfänger halten muss
    pub pcr_to_pts_ms: f64,
    pub rate_kbps: f64,
    pub pcr_pid: Option<u16>,
    pub es_pids: Vec<u16>,
}

impl Tr101290 {
    /// Fehler der Priorität 1
    pub fn prio1(&self) -> u64 {
        self.sync_errors + self.pat_errors + self.cc_errors + self.pmt_errors + self.pid_errors
    }
    /// Fehler der Priorität 2 (PCR-Genauigkeit nur, wenn `cbr`)
    pub fn prio2(&self, cbr: bool) -> u64 {
        self.transport_errors
            + self.crc_errors
            + self.pcr_repetition_errors
            + self.pcr_discontinuity_errors
            + if cbr { self.pcr_accuracy_errors } else { 0 }
            + self.pts_errors
    }
}

fn pcr_of(p: &[u8]) -> Option<(u64, bool)> {
    let afc = (p[3] >> 4) & 3;
    if afc & 2 == 0 || p[4] < 7 || p[5] & 0x10 == 0 {
        return None;
    }
    let b = &p[6..12];
    let base = (b[0] as u64) << 25 | (b[1] as u64) << 17 | (b[2] as u64) << 9 | (b[3] as u64) << 1 | (b[4] as u64) >> 7;
    let ext = ((b[4] as u64 & 1) << 8) | b[5] as u64;
    Some((base * 300 + ext, p[5] & 0x80 != 0))
}

fn payload(p: &[u8]) -> Option<&[u8]> {
    let afc = (p[3] >> 4) & 3;
    if afc & 1 == 0 {
        return None;
    }
    let start = if afc == 3 { 5 + p[4] as usize } else { 4 };
    (start < PACKET).then(|| &p[start..])
}

fn pts_of(pes: &[u8]) -> Option<u64> {
    if pes.len() < 14 || pes[..3] != [0, 0, 1] || pes[7] & 0x80 == 0 {
        return None;
    }
    let b = &pes[9..14];
    Some(((b[0] as u64 >> 1) & 7) << 30 | (b[1] as u64) << 22 | (b[2] as u64 >> 1) << 15 | (b[3] as u64) << 7 | b[4] as u64 >> 1)
}

/// Prüft einen aufgezeichneten TS (Vielfaches von 188 Byte, Anfang beliebig).
pub fn analyse(data: &[u8]) -> Tr101290 {
    let mut c = Tr101290::default();
    let pkts: Vec<&[u8]> = data.as_chunks::<PACKET>().0.iter().map(|c| c.as_slice()).collect();
    // 1. Durchgang: PMT-PID, PCR-PID, Rate aus erster und letzter PCR
    let (mut pmt_pid, mut pcr_pid) = (None, None);
    let mut es_kind: HashMap<u16, u8> = HashMap::new();
    for p in &pkts {
        if p[0] != 0x47 || p[1] & 0x40 == 0 {
            continue;
        }
        let pid = ((p[1] as u16 & 0x1f) << 8) | p[2] as u16;
        let Some(pl) = payload(p) else { continue };
        let Some(sec) = pl.get(1 + pl[0] as usize..) else { continue };
        if pid == 0 && sec.len() > 12 && sec[0] == 0 {
            pmt_pid = Some(((sec[10] as u16 & 0x1f) << 8) | sec[11] as u16);
        } else if Some(pid) == pmt_pid && sec.len() > 12 && sec[0] == 2 && pcr_pid.is_none() {
            pcr_pid = Some(((sec[8] as u16 & 0x1f) << 8) | sec[9] as u16);
            let len = (((sec[1] as usize) & 0x0f) << 8 | sec[2] as usize) + 3;
            let pil = ((sec[10] as usize & 0x0f) << 8) | sec[11] as usize;
            let mut i = 12 + pil;
            while i + 5 <= len.saturating_sub(4).min(sec.len()) {
                let st = sec[i];
                let epid = ((sec[i + 1] as u16 & 0x1f) << 8) | sec[i + 2] as u16;
                let il = ((sec[i + 3] as usize & 0x0f) << 8) | sec[i + 4] as usize;
                es_kind.insert(epid, st);
                i += 5 + il;
            }
        }
    }
    c.pcr_pid = pcr_pid;
    let mut es: Vec<u16> = es_kind.keys().copied().collect();
    es.sort();
    c.es_pids = es;
    let pcrs: Vec<(usize, u64, bool)> = pkts
        .iter()
        .enumerate()
        .filter(|(_, p)| p[0] == 0x47 && Some(((p[1] as u16 & 0x1f) << 8) | p[2] as u16) == pcr_pid)
        .filter_map(|(i, p)| pcr_of(p).map(|(v, d)| (i, v, d)))
        .collect();
    let rate = match (pcrs.first(), pcrs.last()) {
        (Some(a), Some(b)) if b.1 > a.1 && b.0 > a.0 => ((b.0 - a.0) * PACKET * 8) as f64 / ((b.1 - a.1) as f64 / 27e6),
        _ => 0.0,
    };
    c.rate_kbps = rate / 1000.0;
    // Zeit eines Pakets: lineare Fortschreibung von der letzten PCR
    let t_of = |i: usize, last: Option<(usize, u64)>| -> Option<f64> {
        let (pi, pv) = last?;
        (rate > 0.0).then(|| pv as f64 / 27e6 + (i as f64 - pi as f64) * (PACKET * 8) as f64 / rate)
    };

    let mut cc: HashMap<u16, u8> = HashMap::new();
    let mut seen: HashMap<u16, f64> = HashMap::new();
    let mut last_pts: HashMap<u16, f64> = HashMap::new();
    let mut tb: HashMap<u16, (f64, f64)> = HashMap::new();
    let mut last_pcr: Option<(usize, u64)> = None;
    let (mut last_pat, mut last_pmt): (Option<f64>, Option<f64>) = (None, None);
    let mut pcr_pts: Vec<f64> = Vec::new();
    let mut t_first = None;
    for (i, p) in pkts.iter().enumerate() {
        c.packets += 1;
        if p[0] != 0x47 {
            c.sync_errors += 1;
            continue;
        }
        if p[1] & 0x80 != 0 {
            c.transport_errors += 1;
        }
        let pid = ((p[1] as u16 & 0x1f) << 8) | p[2] as u16;
        if pid == 0x1fff {
            c.null_packets += 1;
            continue;
        }
        let afc = (p[3] >> 4) & 3;
        let counter = p[3] & 0x0f;
        let disc = afc & 2 != 0 && p[4] > 0 && p[5] & 0x80 != 0;
        if afc & 1 != 0 {
            if let Some(prev) = cc.get(&pid)
                && !disc
                && counter != (prev + 1) & 0x0f
            {
                c.cc_errors += 1;
            }
            cc.insert(pid, counter);
        } else if let Some(prev) = cc.get(&pid)
            && counter != *prev
            && !disc
        {
            // ohne Nutzinhalt darf der Zähler nicht weiterlaufen
            c.cc_errors += 1;
        }
        if Some(pid) == pcr_pid
            && let Some((v, d)) = pcr_of(p)
        {
            if let Some((li, lv)) = last_pcr {
                let gap = v.wrapping_sub(lv) as f64 / 27e3;
                c.max_pcr_gap_ms = c.max_pcr_gap_ms.max(gap);
                if gap > 40.0 {
                    c.pcr_repetition_errors += 1;
                }
                if !d && !(0.0..=100.0).contains(&gap) {
                    c.pcr_discontinuity_errors += 1;
                }
                if rate > 0.0 {
                    let expect = lv as f64 + (i - li) as f64 * (PACKET * 8) as f64 / rate * 27e6;
                    let ns = (v as f64 - expect) / 27.0 * 1000.0;
                    c.max_pcr_accuracy_ns = c.max_pcr_accuracy_ns.max(ns.abs());
                    if ns.abs() > 500.0 {
                        c.pcr_accuracy_errors += 1;
                    }
                }
            }
            last_pcr = Some((i, v));
        }
        let Some(t) = t_of(i, last_pcr) else { continue };
        t_first.get_or_insert(t);
        seen.insert(pid, t);
        // T-STD: Transportpuffer je Elementarstrom
        if let Some(st) = es_kind.get(&pid) {
            let rx = if *st == 0x0f || *st == 0x11 || *st == 0x03 || *st == 0x04 { RX_AUDIO } else { 1.2 * 62_500_000.0 };
            let e = tb.entry(pid).or_insert((0.0, t));
            e.0 = (e.0 - rx / 8.0 * (t - e.1)).max(0.0) + PACKET as f64;
            e.1 = t;
            if e.0 > TB_SIZE + 0.5 {
                c.tstd_tb_overflows += 1;
                e.0 = TB_SIZE;
            }
        }
        if p[1] & 0x40 == 0 {
            continue;
        }
        let Some(pl) = payload(p) else { continue };
        if pid == 0 || Some(pid) == pmt_pid {
            let Some(sec) = pl.get(1 + pl[0] as usize..) else { continue };
            if sec.len() < 3 {
                continue;
            }
            let len = (((sec[1] as usize) & 0x0f) << 8 | sec[2] as usize) + 3;
            if len <= sec.len() && crc32(&sec[..len]) != 0 {
                c.crc_errors += 1;
            }
            let slot = if pid == 0 { &mut last_pat } else { &mut last_pmt };
            if pid == 0 && sec[0] != 0 || p[3] & 0xc0 != 0 && pid == 0 {
                c.pat_errors += 1;
            }
            if let Some(a) = *slot {
                let g = (t - a) * 1000.0;
                if pid == 0 {
                    c.max_pat_gap_ms = c.max_pat_gap_ms.max(g);
                    if g > 500.0 {
                        c.pat_errors += 1;
                    }
                } else {
                    c.max_pmt_gap_ms = c.max_pmt_gap_ms.max(g);
                    if g > 500.0 {
                        c.pmt_errors += 1;
                    }
                }
            }
            *slot = Some(t);
        } else if let Some(pts) = pts_of(pl) {
            let pts_s = pts as f64 / 90_000.0;
            if let Some(l) = last_pts.get(&pid) {
                let g = (pts_s - l) * 1000.0;
                c.max_pts_gap_ms = c.max_pts_gap_ms.max(g);
                if g > 700.0 {
                    c.pts_errors += 1;
                }
            }
            last_pts.insert(pid, pts_s);
            if Some(pid) == pcr_pid
                && let Some((_, v)) = last_pcr
            {
                // PCR zum Ankunftszeitpunkt dieses Pakets (33-bit-Überlauf beachten)
                let now = (t * 90_000.0) as i64;
                let d = (pts as i64 - now).rem_euclid(1 << 33);
                let _ = v;
                pcr_pts.push(d as f64 / 90.0);
            }
        }
    }
    // PID genannt, aber > 5 s nicht gesehen (nur, wenn die Aufnahme so lang ist)
    if let (Some(t0), Some((li, _))) = (t_first, last_pcr) {
        let t_end = t_of(li, last_pcr).unwrap_or(t0);
        if t_end - t0 > 5.0 {
            for pid in &c.es_pids {
                if seen.get(pid).is_none_or(|t| t_end - t > 5.0) {
                    c.pid_errors += 1;
                }
            }
        }
    }
    if !pcr_pts.is_empty() {
        c.pcr_to_pts_ms = pcr_pts.iter().sum::<f64>() / pcr_pts.len() as f64;
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tsmux::*;

    /// Ein kleiner CBR-Strom von Hand: PAT/PMT, PCR alle 20 ms, ein Bild je 40 ms
    fn synthetic(rate: f64, pcr_jitter_ns: i64, skip_cc_at: Option<usize>) -> Vec<u8> {
        let mut out: Vec<[u8; PACKET]> = Vec::new();
        let (mut pat_cc, mut pmt_cc, mut v_cc, mut sent_cc) = (0u8, 0u8, 0u8, 15u8);
        let streams = [EsInfo { pid: 0x41, kind: Kind::H264, language: None }];
        let slot = PACKET as f64 * 8.0 / rate;
        let n = (2.0 / slot) as usize;
        let mut last_pcr_t = -1.0f64;
        let mut last_si = -1.0f64;
        let mut last_pic = -1.0f64;
        let mut pending: Vec<[u8; PACKET]> = Vec::new();
        let (mut n_pcr, mut n_es) = (0usize, 0usize);
        for k in 0..n {
            let t = k as f64 * slot;
            if t - last_pic >= 0.04 {
                last_pic = t;
                let pts = ((t + 1.08) * 90_000.0) as u64;
                let mut pes = pes_header(0xe0, 3000, pts, Some(pts));
                pes.extend(std::iter::repeat_n(0u8, 3000));
                packetize(0x41, &mut v_cc, &pes, None, false, &mut pending);
                pending.reverse();
            }
            if t - last_pcr_t >= 0.02 {
                last_pcr_t = t;
                n_pcr += 1;
                let pcr = 27_000_000 + (t * 27e6) as i64 + pcr_jitter_ns * 27 / 1000 * if n_pcr % 2 == 0 { 1 } else { -1 };
                out.push(pcr_packet(0x41, (sent_cc + 1) & 0x0f, pcr as u64, false));
            } else if t - last_si >= 0.1 {
                last_si = t;
                out.push(psi_packet(0, &mut pat_cc, &pat_section()));
            } else if t - last_si < slot * 1.5 && k > 0 && out.last().is_some_and(|p| p[2] == 0) {
                out.push(psi_packet(PMT_PID, &mut pmt_cc, &pmt_section(0, 0x41, &streams)));
            } else if let Some(p) = pending.pop() {
                n_es += 1;
                if skip_cc_at == Some(n_es) {
                    out.push(null_packet());
                    continue;
                }
                sent_cc = p[3] & 0x0f;
                out.push(p);
            } else {
                out.push(null_packet());
            }
        }
        out.concat()
    }

    #[test]
    fn clean_cbr_stream_passes() {
        let c = analyse(&synthetic(4_000_000.0, 0, None));
        assert_eq!(c.prio1(), 0, "{c:?}");
        assert_eq!(c.prio2(true), 0, "{c:?}");
        assert_eq!(c.tstd_tb_overflows, 0, "{c:?}");
        assert!((c.rate_kbps - 4000.0).abs() < 10.0, "{c:?}");
        assert!(c.max_pcr_gap_ms <= 20.5 && c.max_pat_gap_ms <= 101.0, "{c:?}");
        assert!((c.pcr_to_pts_ms - 80.0).abs() < 41.0, "{c:?}");
    }

    #[test]
    fn detects_pcr_jitter_and_cc_gap() {
        let c = analyse(&synthetic(4_000_000.0, 2000, None));
        assert!(c.pcr_accuracy_errors > 0 && c.max_pcr_accuracy_ns > 1500.0, "{c:?}");
        let c = analyse(&synthetic(4_000_000.0, 0, Some(300)));
        assert!(c.cc_errors >= 1, "{c:?}");
    }

    #[test]
    fn detects_crc_and_tb_overflow() {
        let mut d = synthetic(4_000_000.0, 0, None);
        // ein Byte der ersten PAT verfälschen
        let i = d.as_chunks::<PACKET>().0.iter().position(|p| p[1] & 0x1f == 0 && p[2] == 0 && p[1] & 0x40 != 0).unwrap();
        d[i * PACKET + 12] ^= 0xff;
        assert!(analyse(&d).crc_errors >= 1);
        // Ton als Block von 10 Paketen hintereinander bei 20 Mbit/s: TB (2 Mbit/s Abfluss) läuft über
        let mut out = Vec::new();
        let (mut pat_cc, mut pmt_cc, mut a_cc) = (0u8, 0u8, 0u8);
        let streams = [EsInfo { pid: 0x42, kind: Kind::Aac, language: None }];
        out.push(psi_packet(0, &mut pat_cc, &pat_section()));
        out.push(psi_packet(PMT_PID, &mut pmt_cc, &pmt_section(0, 0x42, &streams)));
        out.push(pcr_packet(0x42, 0, 0, false));
        let mut es = Vec::new();
        packetize(0x42, &mut a_cc, &vec![0u8; 1800], None, false, &mut es);
        out.extend(es);
        for _ in 0..200 {
            out.push(null_packet());
        }
        out.push(pcr_packet(0x42, a_cc, (out.len() as f64 * 188.0 * 8.0 / 20e6 * 27e6) as u64, false));
        assert!(analyse(&out.concat()).tstd_tb_overflows > 0);
    }
}
