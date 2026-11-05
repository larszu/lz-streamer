//! Durchlauf Testquelle → Encoder → SRT → Decoder im selben Prozess, über
//! echte SRT-Verbindungen auf 127.0.0.1.

use std::time::{Duration, Instant};

use gstreamer::prelude::*;

use lz_streamer::channel::{Channel, ChannelOptions, ChannelStatus, ChildCmd, ChildMsg};
use lz_streamer::config::{Codec, DecoderConfig, EncoderConfig, InputKind, OutputKind, SrtMode};
use lz_streamer::latency::{self, free_port};

fn init() {
    let _ = gstreamer::init();
}

fn encoder_cfg(port: u16, mode: SrtMode) -> EncoderConfig {
    let mut c = EncoderConfig::default();
    c.input.kind = InputKind::Test;
    c.video.width = 640;
    c.video.height = 360;
    c.video.fps = 25;
    c.video.bitrate_kbps = 1500;
    c.srt.mode = mode;
    c.srt.host = "127.0.0.1".into();
    c.srt.port = port;
    c.srt.latency_ms = 80;
    c
}

fn decoder_cfg(port: u16, mode: SrtMode) -> DecoderConfig {
    let mut d = DecoderConfig::default();
    d.srt.mode = mode;
    d.srt.host = "127.0.0.1".into();
    d.srt.port = port;
    d.srt.latency_ms = 80;
    d.output = OutputKind::None;
    d
}

fn status(ch: &mut Channel) -> (ChannelStatus, Vec<String>) {
    let mut st = None;
    let mut ev = Vec::new();
    for m in ch.poll() {
        match m {
            ChildMsg::Status(s) => st = Some(s),
            ChildMsg::Event { text, .. } => ev.push(text),
            ChildMsg::Fatal { text } => panic!("Kanal gescheitert: {text}"),
            ChildMsg::Caption { .. } => {}
        }
    }
    (st.unwrap(), ev)
}

/// Pollt beide Kanäle, bis `done` den Decoder-Status akzeptiert.
fn run_until(enc: &mut Channel, dec: &mut Channel, timeout: Duration, mut done: impl FnMut(&ChannelStatus, &ChannelStatus) -> bool) -> (ChannelStatus, ChannelStatus) {
    let end = Instant::now() + timeout;
    loop {
        let (e, _) = status(enc);
        let (d, _) = status(dec);
        if done(&e, &d) {
            return (e, d);
        }
        assert!(Instant::now() < end, "Zeit abgelaufen. Encoder: {e:?}\nDecoder: {d:?}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn listener_encoder_to_caller_decoder_delivers_frames() {
    init();
    let port = free_port();
    let mut enc = Channel::start_encoder(&encoder_cfg(port, SrtMode::Listener), &[], &ChannelOptions::default()).unwrap();
    let mut dec = Channel::start_decoder(&decoder_cfg(port, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    let (e, d) = run_until(&mut enc, &mut dec, Duration::from_secs(15), |_, d| d.frames >= 50);
    assert_eq!(d.resolution.as_deref(), Some("640×360"));
    assert_eq!(d.codec, Some(Codec::H264));
    assert!(e.encoder.is_some());
    // Statistik des Encoders kennt die Gegenstelle
    let (e, _) = run_until(&mut enc, &mut dec, Duration::from_secs(5), |e, _| e.srt.connected);
    assert!(e.bitrate_kbps > 100.0, "Bitrate {:?}", e.bitrate_kbps);
    let _ = d;
}

#[test]
fn caller_encoder_to_listener_decoder_delivers_frames() {
    init();
    let port = free_port();
    let mut dec = Channel::start_decoder(&decoder_cfg(port, SrtMode::Listener), &ChannelOptions::default()).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let mut enc = Channel::start_encoder(&encoder_cfg(port, SrtMode::Caller), &[], &ChannelOptions::default()).unwrap();
    let (_, d) = run_until(&mut enc, &mut dec, Duration::from_secs(15), |_, d| d.frames >= 50);
    assert!(d.srt.connected || d.bytes > 0);
}

#[test]
fn h265_roundtrip() {
    init();
    let port = free_port();
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    cfg.video.codec = Codec::H265;
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let mut dec = Channel::start_decoder(&decoder_cfg(port, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    let (_, d) = run_until(&mut enc, &mut dec, Duration::from_secs(15), |_, d| d.frames >= 25);
    assert_eq!(d.codec, Some(Codec::H265));
}

#[test]
fn software_x264_roundtrip() {
    init();
    let port = free_port();
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    cfg.video.encoder = "x264enc".into();
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let mut dec = Channel::start_decoder(&decoder_cfg(port, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    let (e, _) = run_until(&mut enc, &mut dec, Duration::from_secs(15), |_, d| d.frames >= 25);
    assert_eq!(e.encoder.as_deref(), Some("x264enc"));
    assert!(!e.encoder_hw);
}

#[test]
fn signal_loss_switches_to_standby_without_dropping_output() {
    init();
    let port = free_port();
    let mut enc = Channel::start_encoder(&encoder_cfg(port, SrtMode::Listener), &[], &ChannelOptions::default()).unwrap();
    let mut dec = Channel::start_decoder(&decoder_cfg(port, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    run_until(&mut enc, &mut dec, Duration::from_secs(15), |_, d| d.frames >= 25);

    enc.command(&ChildCmd::Signal { lost: true });
    let (e, d) = run_until(&mut enc, &mut dec, Duration::from_secs(5), |e, _| e.standby);
    assert_eq!(e.signal, "lost");
    let frames_at_loss = d.frames;
    // Ausgang läuft mit dem Standbild weiter
    // (die Umschaltlücke entspricht standby.timeout_ms, danach läuft das Standbild voll)
    run_until(&mut enc, &mut dec, Duration::from_secs(8), |_, d| d.frames >= frames_at_loss + 25);

    enc.command(&ChildCmd::Signal { lost: false });
    let (e, _) = run_until(&mut enc, &mut dec, Duration::from_secs(5), |e, _| !e.standby);
    assert_eq!(e.signal, "ok");
}

#[test]
fn latency_is_measurable_and_bounded() {
    init();
    let opts = latency::MeasureOptions { rounds: 3, ..Default::default() };
    let r = latency::measure(&opts).unwrap();
    println!("Latenz: {r:?}");
    assert_eq!(r.samples_ms.len(), 3);
    assert!(r.median_ms > 0.0 && r.median_ms < 1000.0, "Median {} ms", r.median_ms);
}

/// RTMP-Abbruch darf SRT nicht mitreißen. Braucht ffmpeg als RTMP-Gegenstelle.
#[test]
fn rtmp_failure_does_not_stop_srt() {
    init();
    if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("ffmpeg fehlt – Test übersprungen");
        return;
    }
    let rport = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let url = format!("rtmp://127.0.0.1:{rport}/live/test");
    let mut ff = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-listen", "1", "-i", &url, "-f", "null", "-"])
        .stdin(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(800));

    let port = free_port();
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    cfg.rtmp.enabled = true;
    cfg.rtmp.url = url;
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let mut dec = Channel::start_decoder(&decoder_cfg(port, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    let (e, _) = run_until(&mut enc, &mut dec, Duration::from_secs(15), |e, d| {
        d.frames >= 25 && e.rtmp.as_deref() == Some("sendet")
    });
    assert_eq!(e.rtmp.as_deref(), Some("sendet"));

    let _ = ff.kill();
    let _ = ff.wait();
    let (e, d) = run_until(&mut enc, &mut dec, Duration::from_secs(15), |e, _| {
        e.rtmp.as_deref().is_some_and(|s| s.starts_with("Fehler"))
    });
    let before = d.frames;
    let (_, d) = run_until(&mut enc, &mut dec, Duration::from_secs(5), |_, d| d.frames >= before + 25);
    assert!(d.frames > before, "SRT stand nach RTMP-Abbruch: {e:?}");
}

/// Kindprozess-Protokoll: Status als JSON-Zeilen, Ende bei geschlossenem stdin.
#[test]
fn child_process_reports_status_and_exits_with_parent() {
    use std::io::{BufRead, BufReader};
    let port = free_port();
    let mut cfg = lz_streamer::config::Config::default();
    cfg.encoder = encoder_cfg(port, SrtMode::Listener);
    let dir = std::env::temp_dir().join(format!("lzs-test-{port}"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("c.json");
    std::fs::write(&path, serde_json::to_vec(&cfg).unwrap()).unwrap();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_lz-streamer"))
        .args(["channel", "--config"])
        .arg(&path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let out = BufReader::new(child.stdout.take().unwrap());
    let mut frames = 0;
    for line in out.lines().take(120) {
        if let Ok(ChildMsg::Status(s)) = serde_json::from_str::<ChildMsg>(&line.unwrap()) {
            frames = s.frames;
            if frames > 10 {
                break;
            }
        }
    }
    assert!(frames > 10, "Kindprozess meldet keine Bilder ({frames})");
    drop(child.stdin.take()); // Elternteil „stirbt“
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(code) = child.try_wait().unwrap() {
            assert_eq!(code.code(), Some(0));
            break;
        }
        assert!(Instant::now() < end, "Kindprozess läuft ohne Elternteil weiter");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// HD-Testbild wird auf UHD hochskaliert und als H.265 übertragen.
#[test]
fn hd_upscaled_to_uhd_h265_roundtrip() {
    init();
    let port = free_port();
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    cfg.video.codec = Codec::H265;
    cfg.video.width = 3840;
    cfg.video.height = 2160;
    cfg.video.fps = 25;
    cfg.video.bitrate_kbps = 15000;
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let mut dec = Channel::start_decoder(&decoder_cfg(port, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    let (e, d) = run_until(&mut enc, &mut dec, Duration::from_secs(20), |_, d| d.frames >= 25);
    assert_eq!(d.resolution.as_deref(), Some("3840×2160"));
    assert_eq!(e.input_format.as_deref(), Some("1920×1080p25 (Testbild)"));
}

fn three_languages(cfg: &mut EncoderConfig) {
    use lz_streamer::audio::{AudioTrack, Layout};
    cfg.audio.tracks = vec![
        AudioTrack { name: "Deutsch".into(), language: "deu".into(), layout: Layout::Stereo, channels: vec![1, 2], ..Default::default() },
        AudioTrack { name: "English".into(), language: "eng".into(), layout: Layout::Stereo, channels: vec![3, 4], ..Default::default() },
        AudioTrack { name: "Français".into(), language: "fra".into(), layout: Layout::Mono, channels: vec![5], gain_db: -6.0, ..Default::default() },
    ];
}

/// Mehrere Tonspuren mit Sprachkennung über SRT; der Decoder sieht alle drei.
#[test]
fn multilingual_audio_tracks_arrive_with_language_codes() {
    init();
    let port = free_port();
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    three_languages(&mut cfg);
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let mut dec = Channel::start_decoder(&decoder_cfg(port, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    let (_, d) = run_until(&mut enc, &mut dec, Duration::from_secs(15), |_, d| {
        d.frames >= 25 && d.audio_tracks.len() == 3 && d.audio_tracks.iter().all(|t| t.language.is_some())
    });
    // GStreamer meldet die PMT-Sprachen als ISO 639-1 („de“), verglichen wird über den Schlüssel
    let langs: Vec<String> = d.audio_tracks.iter().map(|t| lz_streamer::audio::language_key(t.language.as_deref().unwrap())).collect();
    assert_eq!(langs, vec!["de", "en", "fr"], "Reihenfolge/Sprachen");
    assert_eq!(d.audio_tracks.iter().filter(|t| t.selected).count(), 1);
    // ohne Wunsch: erste Spur mit Sprachkennung
    assert!(d.audio_tracks.iter().find(|t| t.selected).unwrap().language.as_deref().is_some_and(|l| l.starts_with("de")));

    // Sprachwahl: Englisch bevorzugt
    let mut dc = decoder_cfg(port, SrtMode::Caller);
    dc.audio_language = "eng".into();
    drop(dec);
    let mut dec = Channel::start_decoder(&dc, &ChannelOptions::default()).unwrap();
    let (_, d) = run_until(&mut enc, &mut dec, Duration::from_secs(15), |_, d| {
        d.audio_tracks.iter().any(|t| t.selected && t.language.as_deref() == Some("en"))
    });
    assert_eq!(d.audio_tracks.iter().filter(|t| t.selected).count(), 1);
}

fn ffprobe_streams(path: &std::path::Path) -> Vec<(String, u32, String)> {
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream=codec_type,channels:stream_tags=language", "-of", "json"])
        .arg(path)
        .output()
        .expect("ffprobe");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    v["streams"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["codec_type"].as_str().unwrap_or("").to_string(),
                s["channels"].as_u64().unwrap_or(0) as u32,
                s["tags"]["language"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

fn record_roundtrip(container: lz_streamer::config::RecordContainer, ext: &str) {
    init();
    if std::process::Command::new("ffprobe").arg("-version").output().is_err() {
        eprintln!("ffprobe fehlt – Test übersprungen");
        return;
    }
    let port = free_port();
    let dir = std::env::temp_dir().join(format!("lzs-rec-{port}"));
    let _ = std::fs::remove_dir_all(&dir);
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    three_languages(&mut cfg);
    cfg.record.enabled = true;
    cfg.record.dir = dir.to_string_lossy().to_string();
    cfg.record.container = container;
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let end = Instant::now() + Duration::from_secs(3);
    let mut rec = None;
    while Instant::now() < end {
        let (s, _) = status(&mut enc);
        rec = rec.or(s.recording);
        std::thread::sleep(Duration::from_millis(200));
    }
    enc.stop();
    let file = std::path::PathBuf::from(rec.expect("keine Aufnahme gemeldet"));
    assert!(file.to_string_lossy().ends_with(ext));
    let streams = ffprobe_streams(&file);
    assert!(streams.iter().any(|s| s.0 == "video"), "{streams:?}");
    let audio: Vec<_> = streams.iter().filter(|s| s.0 == "audio").collect();
    assert_eq!(audio.len(), 3, "{streams:?}");
    assert_eq!(audio.iter().map(|a| a.1).collect::<Vec<_>>(), vec![2, 2, 1], "{streams:?}");
    let langs: Vec<&str> = audio.iter().map(|a| a.2.as_str()).collect();
    assert_eq!(langs, vec!["deu", "eng", "fra"], "{streams:?}");
}

#[test]
fn recording_ts_in_parallel_keeps_all_tracks() {
    record_roundtrip(lz_streamer::config::RecordContainer::Ts, ".ts");
}

#[test]
fn recording_mp4_in_parallel_keeps_all_tracks() {
    record_roundtrip(lz_streamer::config::RecordContainer::Mp4, ".mp4");
}

/// Zusätzliche Ausgänge: zweiter SRT-Listener und UDP liefern denselben Strom.
#[test]
fn extra_outputs_srt_and_udp() {
    use lz_streamer::config::{ExtraOutput, OutputType};
    init();
    let (port, port2) = (free_port(), free_port());
    let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    udp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let uport = udp.local_addr().unwrap().port();
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    let mut second = ExtraOutput { name: "Backup".into(), ..Default::default() };
    second.srt.mode = SrtMode::Listener;
    second.srt.port = port2;
    let third = ExtraOutput { name: "Lokal UDP".into(), kind: OutputType::Udp, host: "127.0.0.1".into(), port: uport, ..Default::default() };
    cfg.outputs = vec![second, third];
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let mut buf = [0u8; 2048];
    let n = udp.recv(&mut buf).expect("kein UDP-Paket");
    assert!(n >= 188 && buf[0] == 0x47, "kein TS über UDP ({n} Byte)");
    let mut dec = Channel::start_decoder(&decoder_cfg(port2, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    let (e, _) = run_until(&mut enc, &mut dec, Duration::from_secs(15), |e, d| {
        d.frames >= 25 && e.outputs.first().is_some_and(|o| o.connected == Some(true))
    });
    assert_eq!(e.outputs.len(), 2);
    assert_eq!(e.outputs[1].connected, None);
}

/// Testton −20 dBFS je Kanal, Spur mit −3 dB → −23 LUFS nach R 128,
/// gemessen am Encoder und nach AAC/SRT am Decoder.
#[test]
fn loudness_per_track_minus23() {
    use lz_streamer::audio::AudioTrack;
    init();
    let port = free_port();
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    cfg.audio.tracks = vec![
        AudioTrack { name: "Programm".into(), channels: vec![1, 2], gain_db: -3.0, ..Default::default() },
        AudioTrack { name: "Stumm".into(), channels: vec![3, 4], mute: true, ..Default::default() },
    ];
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let mut dec = Channel::start_decoder(&decoder_cfg(port, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    let (e, d) = run_until(&mut enc, &mut dec, Duration::from_secs(20), |e, d| {
        e.audio_tracks.first().and_then(|t| t.loudness.as_ref()).is_some_and(|l| l.short_term.is_some() && e.uptime_s >= 4.0)
            && d.audio_tracks.iter().any(|t| t.selected && t.loudness.as_ref().is_some_and(|l| l.short_term.is_some()))
    });
    let l = e.audio_tracks[0].loudness.clone().unwrap();
    eprintln!("Encoder: {l:?}");
    assert!((l.integrated.unwrap() + 23.0).abs() <= 0.1, "I = {:?}", l.integrated);
    assert!((l.momentary.unwrap() + 23.0).abs() <= 0.2, "M = {:?}", l.momentary);
    assert!((l.true_peak.unwrap() + 23.0).abs() <= 0.5, "TP = {:?}", l.true_peak);
    let muted = e.audio_tracks[1].loudness.clone().unwrap();
    assert!(muted.momentary.is_none() && muted.silent_s > 0.0, "{muted:?}");
    let dl = d.audio_tracks.iter().find(|t| t.selected).unwrap().loudness.clone().unwrap();
    eprintln!("Decoder: {dl:?}");
    assert!((dl.momentary.unwrap() + 23.0).abs() <= 0.5, "nach AAC: M = {:?}", dl.momentary);
}

fn srt_file(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("lzs-cc-{tag}-{}.srt", std::process::id()));
    std::fs::write(&p, "1\n00:00:03,000 --> 00:00:05,000\nHallo Welt\n\n2\n00:00:06,000 --> 00:00:08,000\nZweite Zeile Test\n\n").unwrap();
    p
}

/// Untertiteldatei → CEA-608 in der H.264-SEI → SRT → Decoder liest den Text.
#[test]
fn captions_from_file_arrive_as_cea608() {
    use lz_streamer::config::CaptionSource;
    init();
    let port = free_port();
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    cfg.captions.source = CaptionSource::File;
    cfg.captions.file = srt_file("file").to_string_lossy().to_string();
    let mut dec = Channel::start_decoder(&decoder_cfg(port, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let mut seen = Vec::new();
    let (e, _) = run_until(&mut enc, &mut dec, Duration::from_secs(20), |_, d| {
        if let Some(t) = &d.captions.text {
            if !seen.contains(t) {
                seen.push(t.clone());
            }
        }
        seen.iter().any(|t| t.contains("Zweite Zeile Test"))
    });
    assert!(seen.iter().any(|t| t.contains("Hallo Welt")), "{seen:?}");
    assert!(e.captions.frames > 0, "Encoder sah keine Untertitel-Meta");
}

/// Live-Text per Befehl (wie POST /api/caption) kommt beim Decoder an,
/// und die Aufnahme enthält die Untertitel (ffmpeg liest sie als 608).
#[test]
fn live_captions_and_recording_contain_cea608() {
    use lz_streamer::config::{CaptionMode, CaptionSource};
    init();
    let port = free_port();
    let dir = std::env::temp_dir().join(format!("lzs-ccrec-{port}"));
    let _ = std::fs::remove_dir_all(&dir);
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    cfg.captions.source = CaptionSource::Live;
    cfg.captions.mode = CaptionMode::PopOn;
    cfg.record.enabled = true;
    cfg.record.dir = dir.to_string_lossy().to_string();
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let mut dec = Channel::start_decoder(&decoder_cfg(port, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    run_until(&mut enc, &mut dec, Duration::from_secs(10), |_, d| d.frames >= 25);
    // cea608tott gibt eine Pop-on-Zeile aus, sobald die nächste kommt
    enc.command(&ChildCmd::Caption { text: "Live Untertitel eins".into() });
    std::thread::sleep(Duration::from_millis(2500));
    enc.command(&ChildCmd::Caption { text: "Live Untertitel zwei".into() });
    let (_, d) = run_until(&mut enc, &mut dec, Duration::from_secs(10), |_, d| {
        d.captions.text.as_deref().is_some_and(|t| t.contains("Untertitel eins"))
    });
    assert!(d.captions.frames > 0);
    std::thread::sleep(Duration::from_secs(1));
    let rec = status(&mut enc).0.recording.unwrap();
    enc.stop();
    if std::process::Command::new("ffmpeg").arg("-version").output().is_ok() {
        let out = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-f", "lavfi", "-i"])
            .arg(format!("movie={}[out0+subcc]", rec))
            .args(["-map", "0:1", "-f", "srt", "-"])
            .output()
            .unwrap();
        let srt = String::from_utf8_lossy(&out.stdout);
        assert!(srt.contains("Untertitel eins"), "ffmpeg fand keine 608-Untertitel: {srt}");
    }
}

/// Spracherkennung ohne Modell: Kanal läuft, Status erklärt, was fehlt.
#[test]
fn speech_captions_without_model_keep_channel_running() {
    use lz_streamer::config::CaptionSource;
    init();
    let port = free_port();
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    cfg.captions.source = CaptionSource::Speech;
    cfg.captions.asr.model_path = "/nicht/vorhanden/ggml-small.bin".into();
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let mut dec = Channel::start_decoder(&decoder_cfg(port, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    let (e, d) = run_until(&mut enc, &mut dec, Duration::from_secs(10), |e, d| {
        d.frames >= 50 && e.captions.asr.as_ref().is_some_and(|a| a.state.contains("Modell"))
    });
    assert!(e.captions.asr.unwrap().state.contains("fehlt"));
    assert!(d.fps > 10.0);
}

/// Mit Modell (LZS_WHISPER_MODEL=…/ggml-small.bin): deutsche Testaufnahme
/// (macOS-Sprachausgabe) → WER und Textverzug; Absturz der Erkennung lässt
/// den Kanal weiterlaufen.
#[test]
fn speech_captions_with_whisper_model() {
    use lz_streamer::config::CaptionSource;
    init();
    let Ok(model) = std::env::var("LZS_WHISPER_MODEL") else {
        eprintln!("LZS_WHISPER_MODEL nicht gesetzt – Test übersprungen");
        return;
    };
    let reference = "Guten Abend und herzlich willkommen zum Gottesdienst. Heute hören wir die Predigt über den barmherzigen Samariter. \
                     Bitte schalten Sie Ihre Mobiltelefone aus. Wir beginnen mit dem ersten Lied.";
    let wav = std::env::temp_dir().join("lzs-asr-probe.wav");
    if !wav.is_file() {
        let ok = std::process::Command::new("say").args(["-v", "Anna", "--file-format=WAVE", "--data-format=LEI16@16000", "-o"]).arg(&wav).arg(reference).status().map(|s| s.success()).unwrap_or(false);
        if !ok {
            eprintln!("keine Sprachausgabe – Test übersprungen");
            return;
        }
    }
    let name = std::path::Path::new(&model).file_stem().unwrap().to_string_lossy().trim_start_matches("ggml-").to_string();
    let acfg = lz_streamer::asr::AsrConfig { model: name, model_path: model.clone(), ..Default::default() };
    let m = lz_streamer::asr::measure_file(&wav, &acfg, Some(reference)).unwrap();
    eprintln!("ASR-Messung: WER {:?}, Verzug Median {:?} ms, Einzelwerte {:?}\nText: {}", m.wer, m.median_delay_ms, m.delays_ms, m.text);
    assert!(m.wer.unwrap() < 0.35, "WER {:?}", m.wer);
    assert!(m.median_delay_ms.unwrap() <= 5000.0, "Verzug {:?}", m.median_delay_ms);

    // Kanal mit Erkennung, dann Absturz der Erkennung
    let port = free_port();
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    cfg.captions.source = CaptionSource::Speech;
    cfg.captions.asr.model_path = model;
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let mut dec = Channel::start_decoder(&decoder_cfg(port, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    run_until(&mut enc, &mut dec, Duration::from_secs(15), |e, _| e.captions.asr.as_ref().is_some_and(|a| a.state == "läuft"));
    enc.crash_asr();
    let (_, d) = run_until(&mut enc, &mut dec, Duration::from_secs(10), |e, _| e.captions.asr.as_ref().is_some_and(|a| a.state.starts_with("Fehler")));
    let before = d.frames;
    let (e, d) = run_until(&mut enc, &mut dec, Duration::from_secs(20), |e, d| {
        d.frames > before + 50 && e.captions.asr.as_ref().is_some_and(|a| a.state == "läuft")
    });
    assert!(d.frames > before && e.captions.asr.unwrap().state == "läuft");
}

/// Grafik einbrennen: ein „Renderer“ schickt ein BGRA-Bild mit weißem Balken
/// auf den Bild-Port, der Decoder sieht das Bild heller. Fällt der Renderer
/// weg, läuft das Bild ohne Grafik weiter.
#[test]
fn graphics_burn_in_and_renderer_loss() {
    use lz_streamer::config::BurnTarget;
    use std::io::Write;
    init();
    let port = free_port();
    let gport = free_gfx_ports();
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    cfg.graphics.burn = BurnTarget::All;
    cfg.graphics.frame_port = gport;
    let preview = std::env::temp_dir().join(format!("lzs-gfx-preview-{port}.jpg"));
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions { preview_path: Some(preview.clone()) }).unwrap();
    if let Some(src) = enc.pipeline().by_name("testsrc") {
        src.set_property_from_str("pattern", "black");
    }
    let mut dec = Channel::start_decoder(&decoder_cfg(port, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    let (_, d) = run_until(&mut enc, &mut dec, Duration::from_secs(10), |_, d| d.frames >= 25 && d.luma.is_some_and(|l| l < 40.0));
    let dark = d.luma.unwrap();

    // Renderer: untere Hälfte weiß, deckend
    let (w, h) = (cfg.video.width as usize, cfg.video.height as usize);
    let mut frame = vec![0u8; w * h * 4];
    for y in h / 2..h {
        for x in 0..w {
            frame[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
    }
    let mut sock = std::net::TcpStream::connect(("127.0.0.1", gport)).unwrap();
    sock.write_all(&frame).unwrap();
    let (e, d) = run_until(&mut enc, &mut dec, Duration::from_secs(10), |e, d| {
        e.graphics.as_ref().is_some_and(|g| g.graphic_visible) && d.luma.is_some_and(|l| l > dark + 40.0)
    });
    assert!(e.graphics.unwrap().renderer_connected);
    let bright = d.luma.unwrap();
    // Vorschaubild (Schnappschuss): obere Hälfte dunkel, untere hell
    let end = Instant::now() + Duration::from_secs(5);
    while std::fs::metadata(&preview).map(|m| m.len()).unwrap_or(0) == 0 && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_millis(800));
    eprintln!("Vorschau {} Byte", std::fs::metadata(&preview).map(|m| m.len()).unwrap_or(0));
    let (top, bottom) = jpeg_halves(&preview);
    assert!(bottom > top + 100.0, "Vorschau zeigt die Grafik nicht: oben {top:.0}, unten {bottom:.0}");
    assert!(e.fps > 20.0, "Bildrate mit Grafik: {}", e.fps);

    // Renderer stürzt ab: Verbindung weg → Grafik weg, Bild läuft weiter
    drop(sock);
    let (e, d) = run_until(&mut enc, &mut dec, Duration::from_secs(10), |e, d| {
        e.graphics.as_ref().is_some_and(|g| !g.renderer_connected && !g.graphic_visible) && d.luma.is_some_and(|l| l < dark + 10.0)
    });
    let before = d.frames;
    let (_, d) = run_until(&mut enc, &mut dec, Duration::from_secs(5), |_, d| d.frames > before + 25);
    eprintln!("Luma dunkel {dark:.0}, mit Grafik {bright:.0}, nach Absturz {:.0}; Encoder {:.1} fps", d.luma.unwrap(), e.fps);
}

/// Ohne Renderer: Untertiteltext erscheint als Rückfall-Einblendung im Bild.
#[test]
fn caption_text_fallback_burned_without_renderer() {
    use lz_streamer::config::{BurnTarget, CaptionSource};
    init();
    let port = free_port();
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    cfg.graphics.burn = BurnTarget::All;
    cfg.graphics.frame_port = 0;
    cfg.captions.source = CaptionSource::Live;
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    if let Some(src) = enc.pipeline().by_name("testsrc") {
        src.set_property_from_str("pattern", "black");
    }
    let mut dec = Channel::start_decoder(&decoder_cfg(port, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    let (_, d) = run_until(&mut enc, &mut dec, Duration::from_secs(10), |_, d| d.frames >= 25 && d.luma.is_some_and(|l| l < 40.0));
    let dark = d.luma.unwrap();
    enc.command(&ChildCmd::Caption { text: "WWWWWWWWWWWWWWWWWWWWWWWW".into() });
    let (e, d) = run_until(&mut enc, &mut dec, Duration::from_secs(8), |e, d| {
        eprintln!("gfx {:?} luma {:?}", e.graphics, d.luma);
        e.graphics.as_ref().is_some_and(|g| g.text_fallback) && d.luma.is_some_and(|l| l > dark + 1.5)
    });
    eprintln!("Luma ohne Text {dark:.1}, mit Text {:.1}", d.luma.unwrap());
    assert!(e.graphics.unwrap().text_fallback);
}

/// 1080p50 mit eingebrannter Grafik: keine Bildverluste (Encoder hält 50 fps).
#[test]
fn graphics_burn_in_1080p50_keeps_frame_rate() {
    use lz_streamer::config::BurnTarget;
    use std::io::Write;
    init();
    let port = free_port();
    let gport = free_gfx_ports();
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    cfg.video.width = 1920;
    cfg.video.height = 1080;
    cfg.video.fps = 50;
    cfg.video.bitrate_kbps = 8000;
    cfg.graphics.burn = BurnTarget::All;
    cfg.graphics.frame_port = gport;
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let mut dec = Channel::start_decoder(&decoder_cfg(port, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    // Untertitelbalken unten (wie lz-captions): 1920×200 halbtransparent
    let (w, h) = (1920usize, 1080usize);
    let mut frame = vec![0u8; w * h * 4];
    for y in 860..1060 {
        for x in 160..1760 {
            frame[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[40, 40, 40, 200]);
        }
    }
    let mut sock = std::net::TcpStream::connect(("127.0.0.1", gport)).unwrap();
    sock.write_all(&frame).unwrap();
    run_until(&mut enc, &mut dec, Duration::from_secs(10), |e, d| e.graphics.as_ref().is_some_and(|g| g.graphic_visible) && d.frames > 50);
    let f0 = status(&mut enc).0.frames;
    let t0 = Instant::now();
    std::thread::sleep(Duration::from_secs(4));
    let (e, _) = status(&mut enc);
    let fps = (e.frames - f0) as f64 / t0.elapsed().as_secs_f64();
    eprintln!("1080p50 mit Grafik: {fps:.1} fps");
    assert!(fps >= 48.5, "Bildverlust: {fps:.1} fps");
    drop(sock);
}

/// Mittlere Helligkeit der oberen und unteren Bildhälfte eines JPEGs.
fn jpeg_halves(path: &std::path::Path) -> (f64, f64) {
    use gstreamer as gst;
    let p = gst::parse::launch(&format!(
        "filesrc location=\"{}\" ! jpegparse ! jpegdec ! videoconvert ! videoscale ! video/x-raw,format=GRAY8,width=64,height=36 ! appsink name=s",
        path.display()
    ))
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    let sink = p.by_name("s").unwrap().downcast::<gstreamer_app::AppSink>().unwrap();
    p.set_state(gst::State::Playing).unwrap();
    let sample = sink.pull_sample().unwrap();
    let map = sample.buffer().unwrap().map_readable().unwrap();
    let d = map.as_slice();
    let half = d.len() / 2;
    let mean = |x: &[u8]| x.iter().map(|&v| v as f64).sum::<f64>() / x.len() as f64;
    let r = (mean(&d[..half]), mean(&d[half..]));
    drop(map);
    p.set_state(gst::State::Null).unwrap();
    r
}

/// lzloudness in der Spur: Testton −20 LUFS wird auf das Ziel −16 geregelt,
/// True Peak bleibt unter der Grenze, die Vorausschau steht in der Latenz.
#[test]
fn leveler_in_track_reaches_target() {
    use lz_streamer::audio::AudioTrack;
    use lz_streamer::leveler::{LevelerConfig, Preset};
    init();
    let port = free_port();
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    cfg.audio.tracks = vec![AudioTrack {
        channels: vec![1, 2],
        leveler: LevelerConfig { enabled: true, target: -16.0, preset: Preset::Hard, ceiling: -1.0, ..Default::default() },
        ..Default::default()
    }];
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let mut dec = Channel::start_decoder(&decoder_cfg(port, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    let (e, _) = run_until(&mut enc, &mut dec, Duration::from_secs(20), |e, _| {
        e.audio_tracks[0].loudness.as_ref().and_then(|l| l.short_term).is_some_and(|s| (s + 16.0).abs() < 1.0) && e.uptime_s > 5.0
    });
    let l = e.audio_tracks[0].loudness.clone().unwrap();
    eprintln!("geregelt: {l:?}, Regelung {:?}", e.audio_tracks[0].leveler_gain);
    assert!(l.true_peak.unwrap() <= -0.9, "TP {:?}", l.true_peak);
    assert!(e.audio_tracks[0].leveler_gain.unwrap().0 > 3.0);
    // Latenz: Vorausschau 1,5 ms wird gemeldet
    let lev = enc.pipeline().by_name("lev0").unwrap();
    let mut q = gstreamer::query::Latency::new();
    assert!(lev.static_pad("src").unwrap().query(&mut q), "Latenzabfrage");
    let (_, min, _) = q.result();
    eprintln!("Latenz bis lev0: {min}");
    assert!(min >= gstreamer::ClockTime::from_useconds(1500));
}

/// K1: Hauptspur plus Klare-Sprache-Spur kommen beide an; die Klare-Sprache-Spur
/// meldet ihre Absenkung, der Kanal läuft in Echtzeit.
#[test]
fn clear_speech_track_arrives_next_to_main_track() {
    use lz_streamer::audio::AudioTrack;
    init();
    let port = free_port();
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    let mut cs = AudioTrack { name: "Klare Sprache".into(), ..Default::default() };
    cs.clear_speech.enabled = true;
    cs.clear_speech.keep_singing = false;
    cs.leveler.enabled = true;
    cfg.audio.tracks = vec![AudioTrack { name: "Programm".into(), ..Default::default() }, cs];
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let mut dec = Channel::start_decoder(&decoder_cfg(port, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    let (e, d) = run_until(&mut enc, &mut dec, Duration::from_secs(15), |e, d| {
        d.frames >= 50 && d.audio_tracks.len() == 2 && e.audio_tracks.get(1).and_then(|t| t.clear_speech).is_some_and(|c| c.residual_db < -6.0)
    });
    let st = e.audio_tracks[1].clear_speech.unwrap();
    assert!(e.audio_tracks[0].clear_speech.is_none());
    assert!(e.fps > cfg.video.fps as f64 * 0.9, "{:.1} fps", e.fps);
    eprintln!("Klare Sprache: {st:?}, Encoder {:.1} fps, Decoder {} Bilder", e.fps, d.frames);
}

/// U4: Ebene 2 (freie Grafik) liegt zusätzlich zu Ebene 1 im Bild und
/// verschwindet allein, wenn ihre Verbindung endet.
#[test]
fn second_graphics_layer_is_composited() {
    use lz_streamer::config::BurnTarget;
    use std::io::Write;
    init();
    let port = free_port();
    // zwei aufeinanderfolgende freie Ports
    let gport = free_gfx_ports();
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    cfg.graphics.burn = BurnTarget::All;
    cfg.graphics.frame_port = gport;
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    if let Some(src) = enc.pipeline().by_name("testsrc") {
        src.set_property_from_str("pattern", "black");
    }
    let mut dec = Channel::start_decoder(&decoder_cfg(port, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    let (_, d) = run_until(&mut enc, &mut dec, Duration::from_secs(10), |_, d| d.frames >= 25 && d.luma.is_some_and(|l| l < 40.0));
    let dark = d.luma.unwrap();
    let (w, h) = (cfg.video.width as usize, cfg.video.height as usize);
    let half = |top: bool| {
        let mut f = vec![0u8; w * h * 4];
        let rows = if top { 0..h / 4 } else { h * 3 / 4..h };
        for y in rows {
            for x in 0..w {
                f[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        f
    };
    let mut l1 = std::net::TcpStream::connect(("127.0.0.1", gport)).unwrap();
    l1.write_all(&half(false)).unwrap();
    let (_, d) = run_until(&mut enc, &mut dec, Duration::from_secs(10), |e, d| {
        e.graphics.as_ref().is_some_and(|g| g.graphic_visible && !g.layer2_visible) && d.luma.is_some_and(|l| l > dark + 20.0)
    });
    let one = d.luma.unwrap();
    let mut l2 = std::net::TcpStream::connect(("127.0.0.1", gport + 1)).unwrap();
    l2.write_all(&half(true)).unwrap();
    let (e, d) = run_until(&mut enc, &mut dec, Duration::from_secs(10), |e, d| {
        e.graphics.as_ref().is_some_and(|g| g.layer2_visible) && d.luma.is_some_and(|l| l > one + 20.0)
    });
    let both = d.luma.unwrap();
    assert_eq!(e.graphics.as_ref().unwrap().frame_ports, vec![gport, gport + 1]);
    drop(l2);
    let (_, d) = run_until(&mut enc, &mut dec, Duration::from_secs(10), |e, d| {
        e.graphics.as_ref().is_some_and(|g| g.graphic_visible && !g.layer2_visible) && d.luma.is_some_and(|l| l < both - 20.0)
    });
    eprintln!("Luma dunkel {dark:.0}, Ebene 1 {one:.0}, beide {both:.0}, Ebene 2 weg {:.0}", d.luma.unwrap());
    drop(l1);
}

/// zwei aufeinanderfolgende freie Ports (Grafik-Ebene 1 und 2); pro Testlauf
/// verschieden, damit parallel laufende Tests sich nicht in die Quere kommen
fn free_gfx_ports() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static NEXT: AtomicU16 = AtomicU16::new(0);
    let base = 20000 + (std::process::id() % 2000) as u16 * 10;
    loop {
        let p = base + NEXT.fetch_add(2, Ordering::Relaxed);
        if std::net::TcpListener::bind(("127.0.0.1", p)).is_ok() && std::net::TcpListener::bind(("127.0.0.1", p + 1)).is_ok() {
            return p;
        }
    }
}
