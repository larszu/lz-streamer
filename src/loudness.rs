//! Lautheit nach ITU-R BS.1770 / EBU R 128 je Tonspur: Momentary (400 ms),
//! Short-Term (3 s), Integrated (gegatet), LRA, True Peak, Sample Peak.
//! Gemessen mit `ebur128level` (gst-plugins-rs, ebur128-Crate) direkt hinter
//! Matrix und Pegel jeder Spur – also das, was gesendet wird.

use gstreamer as gst;
use serde::{Deserialize, Serialize};

/// Unter diesem Momentary-Wert gilt eine Spur als still.
pub const SILENCE_LUFS: f64 = -60.0;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Loudness {
    pub momentary: Option<f64>,
    pub short_term: Option<f64>,
    pub integrated: Option<f64>,
    pub lra: Option<f64>,
    /// höchster True Peak über alle Kanäle, dBTP
    pub true_peak: Option<f64>,
    pub sample_peak: Option<f64>,
    /// so lange ist die Spur schon still (Sekunden)
    pub silent_s: f64,
}

fn finite(v: f64) -> Option<f64> {
    (v.is_finite() && v > -200.0).then_some((v * 100.0).round() / 100.0)
}

fn peak_db(s: &gst::StructureRef, field: &str) -> Option<f64> {
    let arr = s.get::<gst::Array>(field).ok()?;
    let max = arr.iter().filter_map(|v| v.get::<f64>().ok()).fold(0.0f64, f64::max);
    finite(20.0 * max.log10())
}

/// `ebur128-level`-Nachricht lesen.
pub fn from_message(s: &gst::StructureRef) -> Option<Loudness> {
    if s.name() != "ebur128-level" {
        return None;
    }
    let g = |f: &str| s.get::<f64>(f).ok().and_then(finite);
    Some(Loudness {
        momentary: g("momentary-loudness"),
        short_term: g("shortterm-loudness"),
        integrated: g("global-loudness"),
        lra: s.get::<f64>("loudness-range").ok().map(|v| (v * 100.0).round() / 100.0),
        true_peak: peak_db(s, "true-peak"),
        sample_peak: peak_db(s, "sample-peak"),
        silent_s: 0.0,
    })
}

/// Offline-Messung für Prüfsignale: Kanäle als f32-Blöcke, 48 kHz.
/// Liefert die letzte Messung nach dem gesamten Signal.
pub fn measure_interleaved(samples: &[f32], channels: u32, rate: u32) -> anyhow::Result<Loudness> {
    use gst::prelude::*;
    let _ = gst::init();
    let p = gst::parse::launch(&format!(
        "appsrc name=src format=time caps=audio/x-raw,format=F32LE,layout=interleaved,rate={rate},channels={channels}{} \
         ! ebur128level name=lvl interval=100000000 ! fakesink sync=false",
        if channels == 2 { ",channel-mask=(bitmask)0x3" } else { "" }
    ))?
    .downcast::<gst::Pipeline>()
    .map_err(|_| anyhow::anyhow!("keine Pipeline"))?;
    let src = p.by_name("src").unwrap().downcast::<gstreamer_app::AppSrc>().unwrap();
    p.set_state(gst::State::Playing)?;
    let frames_per_buf = 4800usize;
    let mut pts = 0u64;
    for chunk in samples.chunks(frames_per_buf * channels as usize) {
        let bytes: Vec<u8> = chunk.iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut buf = gst::Buffer::from_mut_slice(bytes);
        let n = (chunk.len() / channels as usize) as u64;
        {
            let b = buf.get_mut().unwrap();
            b.set_pts(gst::ClockTime::from_nseconds(pts * 1_000_000_000 / rate as u64));
            b.set_duration(gst::ClockTime::from_nseconds(n * 1_000_000_000 / rate as u64));
        }
        pts += n;
        src.push_buffer(buf)?;
    }
    src.end_of_stream()?;
    let bus = p.bus().unwrap();
    let mut last = None;
    for msg in bus.iter_timed(gst::ClockTime::from_seconds(60)) {
        match msg.view() {
            gst::MessageView::Element(e) => {
                if let Some(l) = e.structure().and_then(from_message) {
                    last = Some(l);
                }
            }
            gst::MessageView::Eos(_) => break,
            gst::MessageView::Error(e) => anyhow::bail!("{}", e.error()),
            _ => {}
        }
    }
    p.set_state(gst::State::Null)?;
    last.ok_or_else(|| anyhow::anyhow!("keine Messung"))
}

/// Sinus-Abschnitte (Dauer s, Pegel dBFS Spitze; None = Stille) als Stereo.
pub fn tone_segments(segs: &[(f64, Option<f64>)], rate: u32, freq: f64) -> Vec<f32> {
    let mut out = Vec::new();
    let mut n = 0u64;
    for &(dur, db) in segs {
        let a = db.map(|d| 10f64.powf(d / 20.0)).unwrap_or(0.0);
        let len = (dur * rate as f64).round() as u64;
        for _ in 0..len {
            let v = (a * (2.0 * std::f64::consts::PI * freq * n as f64 / rate as f64).sin()) as f32;
            out.push(v);
            out.push(v);
            n += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn integrated(segs: &[(f64, Option<f64>)]) -> f64 {
        measure_interleaved(&tone_segments(segs, 48000, 1000.0), 2, 48000).unwrap().integrated.unwrap()
    }

    // EBU Tech 3341, Fälle 1–5 (Integrated, Toleranz ±0,1 LU), wie in lz-scopes
    #[test]
    fn tech3341_case1_minus23() {
        assert!((integrated(&[(20.0, Some(-23.0))]) + 23.0).abs() <= 0.1);
    }

    #[test]
    fn tech3341_case2_minus33() {
        assert!((integrated(&[(20.0, Some(-33.0))]) + 33.0).abs() <= 0.1);
    }

    #[test]
    fn tech3341_case3_gating() {
        let v = integrated(&[(10.0, Some(-36.0)), (60.0, Some(-23.0)), (10.0, Some(-36.0))]);
        assert!((v + 23.0).abs() <= 0.1, "{v}");
    }

    #[test]
    fn tech3341_case4_absolute_gate() {
        let v = integrated(&[(10.0, Some(-72.0)), (10.0, Some(-36.0)), (60.0, Some(-23.0)), (10.0, Some(-36.0)), (10.0, Some(-72.0))]);
        assert!((v + 23.0).abs() <= 0.1, "{v}");
    }

    #[test]
    fn tech3341_case5_relative_gate() {
        let v = integrated(&[(20.0, Some(-26.0)), (20.1, Some(-20.0)), (20.0, Some(-26.0))]);
        assert!((v + 23.0).abs() <= 0.1, "{v}");
    }

    // EBU Tech 3342, LRA ±1 LU
    #[test]
    fn tech3342_lra_cases() {
        for (levels, want) in [(vec![-20.0, -30.0], 10.0), (vec![-20.0, -15.0], 5.0), (vec![-40.0, -20.0], 20.0)] {
            let segs: Vec<_> = levels.iter().map(|&l| (20.0, Some(l))).collect();
            let l = measure_interleaved(&tone_segments(&segs, 48000, 1000.0), 2, 48000).unwrap();
            let lra = l.lra.unwrap();
            assert!((lra - want).abs() <= 1.0, "LRA {lra} statt {want}");
        }
    }

    #[test]
    fn true_peak_of_minus6_sine() {
        let l = measure_interleaved(&tone_segments(&[(3.0, Some(-6.0))], 48000, 1000.0), 2, 48000).unwrap();
        assert!((l.true_peak.unwrap() + 6.0).abs() <= 0.3, "{:?}", l.true_peak);
    }
}
