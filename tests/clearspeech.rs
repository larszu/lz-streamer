//! K1: Klare-Sprache-Element in einer echten Pipeline – Latenz, Lippensynchronität, Rechenlast.

use std::time::Instant;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

/// PTS des ersten Samples über `thr` (Klick-Suche)
fn first_click(sink: &gst_app::AppSink, thr: f32, after: gst::ClockTime) -> Option<gst::ClockTime> {
    loop {
        let s = sink.try_pull_sample(gst::ClockTime::from_seconds(5))?;
        let b = s.buffer()?;
        if b.pts()? < after {
            continue;
        }
        let m = b.map_readable().ok()?;
        let ch = 1;
        if let Some(i) = m.chunks_exact(4).position(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]).abs() > thr) {
            return Some(b.pts()? + gst::ClockTime::from_nseconds((i / ch) as u64 * 1_000_000_000 / 48_000));
        }
    }
}

#[test]
fn clear_speech_is_lip_sync_and_reports_30ms() {
    gst::init().unwrap();
    lz_streamer::clearspeech::register();
    // Ein Klick je Sekunde; Rest 0 dB, damit der Klick unverändert durchgeht
    let p = gst::parse::launch(
        "audiotestsrc is-live=true wave=ticks tick-interval=1000000000 volume=0.8 samplesperbuffer=480 \
         ! audio/x-raw,format=F32LE,rate=48000,channels=1,layout=interleaved ! tee name=t \
         t. ! queue ! appsink name=dry sync=false \
         t. ! queue ! audiobuffersplit output-buffer-duration=1/100 ! lzclearspeech name=cs ! appsink name=wet sync=false",
    )
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    let cs = p.by_name("cs").unwrap().downcast::<lz_streamer::clearspeech::LzClearSpeech>().unwrap();
    cs.set_config(lz_streamer::clearspeech::ClearSpeechConfig { enabled: true, residual_db: 0.0, keep_singing: false });
    let dry = p.by_name("dry").unwrap().downcast::<gst_app::AppSink>().unwrap();
    let wet = p.by_name("wet").unwrap().downcast::<gst_app::AppSink>().unwrap();
    p.set_state(gst::State::Playing).unwrap();
    // zweiten Klick nehmen (der erste liegt bei 0 und fällt in die Vorfüllung)
    let after = gst::ClockTime::from_mseconds(500);
    let d = first_click(&dry, 0.3, after).unwrap();
    let w = first_click(&wet, 0.3, after).unwrap();
    let mut q = gst::query::Latency::new();
    assert!(wet.static_pad("sink").unwrap().peer_query(&mut q));
    let (_, min, _) = q.result();
    let mut q0 = gst::query::Latency::new();
    assert!(cs.static_pad("sink").unwrap().peer_query(&mut q0));
    let own = min - q0.result().1;
    p.set_state(gst::State::Null).unwrap();
    let off_ms = (w.nseconds() as f64 - d.nseconds() as f64) / 1e6;
    eprintln!("Klick trocken {d}, klar {w}, Versatz {off_ms:.2} ms, Latenz gesamt {min}, davon Klare Sprache {own}");
    assert!(off_ms.abs() < 1.0, "nicht lippensynchron: {off_ms:.2} ms");
    assert_eq!(own, gst::ClockTime::from_mseconds(30), "Zusatzlatenz");
}

#[test]
fn clear_speech_cpu_load() {
    let mut cs = lz_streamer::clearspeech::ClearSpeech::new(Default::default(), 2).unwrap();
    let secs = 10;
    let mut x: Vec<f32> = (0..48_000 * 2 * secs).map(|i| ((i as f32) * 0.01).sin() * 0.1).collect();
    let t = Instant::now();
    for c in x.chunks_mut(960) {
        cs.process(c);
    }
    let load = t.elapsed().as_secs_f64() / secs as f64;
    eprintln!("Rechenlast Stereo: {:.1} % eines Kerns", load * 100.0);
    assert!(load < 1.0, "nicht echtzeitfähig: {load:.2}");
}
