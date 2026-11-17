//! Zeitangaben aus fertigen Segmenten: Zeitskala des Init-Segments (mdhd)
//! und Decodierzeit samt Dauer eines CMAF-Segments (moof/traf: tfdt, tfhd, trun).

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

/// Boxen einer Ebene: (Typ, Inhalt ohne Kopf)
fn boxes(b: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut o = 0usize;
    while o + 8 <= b.len() {
        let size = u32_at(b, o).unwrap_or(0) as u64;
        let kind: [u8; 4] = b[o + 4..o + 8].try_into().unwrap();
        let (head, size) = match size {
            1 => (16usize, u64_at(b, o + 8).unwrap_or(0)),
            0 => (8, (b.len() - o) as u64),
            s => (8, s),
        };
        let end = o.saturating_add(size as usize).min(b.len());
        if size < head as u64 || o + head > end {
            break;
        }
        out.push((kind, &b[o + head..end]));
        o = end;
    }
    out
}

fn child<'a>(b: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(b).into_iter().find(|(k, _)| k == kind).map(|(_, p)| p)
}

/// Zeitskala der ersten Spur eines Init-Segments
pub fn timescale(init: &[u8]) -> Option<u32> {
    let trak = child(child(init, b"moov")?, b"trak")?;
    let mdhd = child(child(trak, b"mdia")?, b"mdhd")?;
    match mdhd.first()? {
        1 => u32_at(mdhd, 20),
        _ => u32_at(mdhd, 12),
    }
}

/// Decodierzeit des ersten Bilds und Gesamtdauer eines Segments (in der Zeitskala)
pub fn fragment(seg: &[u8]) -> Option<(u64, u64)> {
    let (mut first, mut dur) = (None, 0u64);
    for (_, moof) in boxes(seg).into_iter().filter(|(k, _)| k == b"moof") {
        let traf = child(moof, b"traf")?;
        let tfhd = child(traf, b"tfhd")?;
        let fl = u32_at(tfhd, 0)? & 0xff_ffff;
        let mut o = 8; // Version/Flags, Spurnummer
        if fl & 0x01 != 0 {
            o += 8;
        }
        if fl & 0x02 != 0 {
            o += 4;
        }
        let default_dur = if fl & 0x08 != 0 { u32_at(tfhd, o) } else { None };
        if let Some(tfdt) = child(traf, b"tfdt") {
            let t = if tfdt.first() == Some(&1) { u64_at(tfdt, 4)? } else { u32_at(tfdt, 4)? as u64 };
            first.get_or_insert(t);
        }
        for (_, trun) in boxes(traf).into_iter().filter(|(k, _)| k == b"trun") {
            let fl = u32_at(trun, 0)? & 0xff_ffff;
            let n = u32_at(trun, 4)? as u64;
            if fl & 0x100 == 0 {
                dur += n * default_dur? as u64;
                continue;
            }
            let mut o = 8 + if fl & 0x01 != 0 { 4 } else { 0 } + if fl & 0x04 != 0 { 4 } else { 0 };
            let per = [0x100, 0x200, 0x400, 0x800].iter().filter(|&&f| fl & f != 0).count() * 4;
            for _ in 0..n {
                dur += u32_at(trun, o)? as u64;
                o += per;
            }
        }
    }
    Some((first?, dur))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn mk(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    /// Segment wie von cmafmux: Vorgabedauer im tfhd oder Dauer je Bild im trun
    pub fn segment(tfdt: u64, samples: u32, default_dur: Option<u32>) -> Vec<u8> {
        let mut tfhd = vec![0, 0x02, 0, 0x28 * default_dur.is_some() as u8];
        tfhd.extend_from_slice(&1u32.to_be_bytes());
        if let Some(d) = default_dur {
            tfhd.extend_from_slice(&d.to_be_bytes());
            tfhd.extend_from_slice(&0u32.to_be_bytes());
        }
        let mut tfdt_b = vec![1, 0, 0, 0];
        tfdt_b.extend_from_slice(&tfdt.to_be_bytes());
        let fl: u32 = if default_dur.is_some() { 0x000a05 } else { 0x000301 };
        let mut trun = fl.to_be_bytes().to_vec();
        trun.extend_from_slice(&samples.to_be_bytes());
        trun.extend_from_slice(&0u32.to_be_bytes()); // data offset
        if default_dur.is_some() {
            trun.extend_from_slice(&0u32.to_be_bytes()); // first sample flags
        }
        for _ in 0..samples {
            if default_dur.is_none() {
                trun.extend_from_slice(&1024u32.to_be_bytes());
            }
            trun.extend_from_slice(&100u32.to_be_bytes()); // Größe
            if default_dur.is_some() {
                trun.extend_from_slice(&0u32.to_be_bytes()); // Zeitversatz
            }
        }
        let traf = [mk(b"tfhd", &tfhd), mk(b"tfdt", &tfdt_b), mk(b"trun", &trun)].concat();
        let moof = [mk(b"mfhd", &[0; 8]), mk(b"traf", &traf)].concat();
        [mk(b"styp", b"cmfs"), mk(b"moof", &moof), mk(b"mdat", &[0; 16])].concat()
    }

    pub fn init(timescale: u32) -> Vec<u8> {
        let mut mdhd = vec![0u8; 12];
        mdhd.extend_from_slice(&timescale.to_be_bytes());
        mdhd.extend_from_slice(&[0; 8]);
        let trak = mk(b"trak", &mk(b"mdia", &mk(b"mdhd", &mdhd)));
        [mk(b"ftyp", b"iso6"), mk(b"moov", &[mk(b"mvhd", &[0; 100]), trak].concat())].concat()
    }

    #[test]
    fn reads_timescale_and_fragment_times() {
        assert_eq!(timescale(&init(2500)), Some(2500));
        // Video: 13 Bilder à 100 (wie hlscmafsink bei 25 fps, Zeitskala 2500)
        assert_eq!(fragment(&segment(2585, 13, Some(100))), Some((2585, 1300)));
        // Ton: Dauer je Paket
        assert_eq!(fragment(&segment(48_000, 47, None)), Some((48_000, 47 * 1024)));
        assert_eq!(fragment(b"kaputt"), None);
    }
}
