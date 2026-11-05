//! P1: Plugin-Host mit LZ Defeedback. Übersprungen, wenn das Plugin fehlt.

use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

const DEFEEDBACK: &str = "Library/Audio/Plug-Ins/VST3/LZ Defeedback.vst3";

fn plugin_path() -> Option<String> {
    let p = dirs::home_dir()?.join(DEFEEDBACK);
    p.exists().then(|| p.to_string_lossy().into_owned())
}

fn rms(b: &gst::BufferRef) -> f64 {
    let m = b.map_readable().unwrap();
    let v: Vec<f32> = m.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    (v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / v.len() as f64).sqrt()
}

#[test]
fn defeedback_crash_bypass_restart() {
    let Some(path) = plugin_path() else {
        eprintln!("LZ Defeedback nicht installiert – übersprungen");
        return;
    };
    // SAFETY: Test setzt die Variable vor jedem Hilfsprozess
    unsafe { std::env::set_var("LZS_PLUGIN_HOST_EXE", env!("CARGO_BIN_EXE_lz-streamer")) };
    gst::init().unwrap();
    lz_streamer::plugin::register();
    let p = gst::parse::launch(
        "audiotestsrc is-live=true freq=440 volume=0.3 samplesperbuffer=480 \
         ! audio/x-raw,format=F32LE,rate=48000,channels=2,layout=interleaved \
         ! lzplugin name=pl ! appsink name=out sync=false emit-signals=false max-buffers=0",
    )
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    let pl = p.by_name("pl").unwrap().downcast::<lz_streamer::plugin::LzPlugin>().unwrap();
    pl.set_path(&path);
    let sink = p.by_name("out").unwrap().downcast::<gst_app::AppSink>().unwrap();
    p.set_state(gst::State::Playing).unwrap();

    let mut last_end: Option<gst::ClockTime> = None;
    let mut gaps = 0;
    let mut pull = |secs: f64| -> Vec<f64> {
        let until = Instant::now() + Duration::from_secs_f64(secs);
        let mut out = Vec::new();
        while Instant::now() < until {
            let s = sink.try_pull_sample(gst::ClockTime::from_mseconds(500)).expect("Ton reißt ab");
            let b = s.buffer().unwrap();
            if let (Some(e), Some(pts)) = (last_end, b.pts()) {
                if pts.nseconds().abs_diff(e.nseconds()) > 1_000_000 {
                    gaps += 1;
                }
            }
            last_end = b.pts().zip(b.duration()).map(|(p, d)| p + d);
            out.push(rms(b));
        }
        out
    };

    // Laden im Hintergrund: Ton fließt sofort (unbearbeitet), Plugin übernimmt danach
    let t0 = Instant::now();
    while !pl.status().running {
        pull(0.2);
        assert!(t0.elapsed() < Duration::from_secs(15), "Plugin lädt nicht");
    }
    eprintln!("geladen nach {:?}", t0.elapsed());
    let processed = pull(2.0);
    // LZ Defeedback dämpft den stehenden Sinus deutlich
    assert!(processed[processed.len() - 10..].iter().all(|r| *r < 0.1), "{:?}", &processed[processed.len() - 10..]);
    let st = pl.status();
    assert!(st.running && st.processed_blocks > 50, "{st:?}");
    let pid = st.pid.unwrap();

    // Hilfsprozess abschießen: Ton läuft ohne Lücke weiter (Bypass), dann Neustart
    std::process::Command::new("kill").args(["-9", &pid.to_string()]).status().unwrap();
    let after = pull(3.0);
    let st2 = pl.status();
    assert!(st2.bypassed_blocks > 0, "{st2:?}");
    assert!(st2.restarts >= 1 && st2.running && st2.pid != Some(pid), "{st2:?}");
    assert!(st2.processed_blocks > st.processed_blocks, "{st2:?}");
    // im Bypass kommt der unbearbeitete Ton durch (Sinus 0,3 → RMS ≈ 0,21)
    assert!(after.iter().any(|r| *r > 0.15), "kein trockener Ton im Bypass");
    p.set_state(gst::State::Null).unwrap();
    assert_eq!(gaps, 0, "Lücken im Ton");
    eprintln!("vorher {st:?}\nnachher {st2:?}");
}
