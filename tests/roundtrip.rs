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

/// Startzeiten (ms) aller Zeilen einer SRT-Datei
fn srt_starts(srt: &str) -> Vec<(u64, String)> {
    let mut out = Vec::new();
    let mut lines = srt.lines();
    while let Some(l) = lines.next() {
        if let Some((a, _)) = l.split_once(" --> ") {
            let p: Vec<u64> = a.trim().split([':', ',', '.']).filter_map(|x| x.parse().ok()).collect();
            if p.len() == 4 {
                let text = lines.next().unwrap_or_default().to_string();
                out.push((((p[0] * 60 + p[1]) * 60 + p[2]) * 1000 + p[3], text));
            }
        }
    }
    out
}

/// U5: Neben der Aufnahme entstehen .srt und .vtt; ihre Zeiten stimmen mit den
/// CEA-608-Untertiteln in der Aufnahme überein (Abweichung < 200 ms).
#[test]
fn recording_gets_srt_and_vtt_in_sync() {
    use lz_streamer::config::{CaptionMode, CaptionSource};
    init();
    let port = free_port();
    let dir = std::env::temp_dir().join(format!("lzs-u5-{port}"));
    let _ = std::fs::remove_dir_all(&dir);
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    cfg.captions.source = CaptionSource::Live;
    cfg.captions.mode = CaptionMode::PopOn;
    cfg.record.enabled = true;
    cfg.record.dir = dir.to_string_lossy().to_string();
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    std::thread::sleep(Duration::from_secs(2));
    let texts = ["Erste Zeile", "Zweite Zeile", "Dritte Zeile"];
    for t in texts {
        enc.command(&ChildCmd::Caption { text: t.into() });
        let end = Instant::now() + Duration::from_millis(2500);
        while Instant::now() < end {
            status(&mut enc);
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let rec = status(&mut enc).0.recording.expect("keine Aufnahme");
    enc.stop();
    let rec = std::path::PathBuf::from(rec);
    let srt = std::fs::read_to_string(rec.with_extension("srt")).expect(".srt fehlt");
    let vtt = std::fs::read_to_string(rec.with_extension("vtt")).expect(".vtt fehlt");
    assert!(vtt.starts_with("WEBVTT"), "{vtt}");
    let side = srt_starts(&srt);
    assert_eq!(side.iter().map(|c| c.1.as_str()).collect::<Vec<_>>(), texts, "{srt}");
    assert_eq!(srt_starts(&vtt.replace('.', ",")).len(), 3);
    // Vergleich mit den 608-Untertiteln, die ffmpeg aus der Aufnahme liest
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("movie={}[out0+subcc]", rec.display()))
        .args(["-map", "0:1", "-f", "srt", "-"])
        .output()
        .unwrap();
    let embedded = srt_starts(&String::from_utf8_lossy(&out.stdout));
    // Startzeit des Videos in der Datei (ffmpeg zählt ab dort)
    eprintln!("Datei:\n{srt}eingebettet:\n{}", String::from_utf8_lossy(&out.stdout));
    for (t, (ms, _)) in texts.iter().zip(&side) {
        let (e, _) = embedded.iter().find(|(_, x)| x.contains(t)).unwrap_or_else(|| panic!("{t} nicht eingebettet: {embedded:?}"));
        let diff = *e as i64 - *ms as i64;
        eprintln!("{t}: Datei {ms} ms, eingebettet {e} ms, Abweichung {diff} ms");
        // gemessen 150–212 ms ab der zweiten Zeile (ffmpeg zählt später als cea608tojson,
        // siehe docs/untertitel-messung.md); die Grenze fängt Rückschritte ab
        assert!(diff.abs() < 250, "{t}: {diff} ms");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// U6: zweite Untertitelsprache auf CC3 neben CC1 im selben TS; der Decoder
/// wählt den Kanal, neben der Aufnahme liegen Dateien je Sprache.
#[test]
fn second_caption_language_on_cc3() {
    use lz_streamer::config::{CaptionMode, CaptionSource};
    init();
    let port = free_port();
    let dir = std::env::temp_dir().join(format!("lzs-u6-{port}"));
    let _ = std::fs::remove_dir_all(&dir);
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    cfg.captions.source = CaptionSource::Live;
    cfg.captions.mode = CaptionMode::PopOn;
    cfg.captions.second.enabled = true;
    cfg.record.enabled = true;
    cfg.record.dir = dir.to_string_lossy().to_string();
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let mut dc = decoder_cfg(port, SrtMode::Caller);
    dc.caption_channel = 3;
    let mut dec = Channel::start_decoder(&dc, &ChannelOptions::default()).unwrap();
    let t0 = Instant::now();
    run_until(&mut enc, &mut dec, Duration::from_secs(10), |_, d| d.frames >= 25);
    eprintln!("Anlauf {:?}", t0.elapsed());
    for (de, en) in [("Guten Abend", "Good evening"), ("Willkommen", "Welcome")] {
        enc.command(&ChildCmd::Caption { text: de.into() });
        enc.command(&ChildCmd::Caption2 { text: en.into() });
        let end = Instant::now() + Duration::from_millis(2500);
        while Instant::now() < end {
            status(&mut enc);
            status(&mut dec);
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let (_, d) = run_until(&mut enc, &mut dec, Duration::from_secs(8), |_, d| d.captions.text.as_deref().is_some_and(|t| t.contains("Good evening") || t.contains("Welcome")));
    let shown = d.captions.text.unwrap_or_default();
    assert!(!shown.contains("Abend") && !shown.contains("Willkommen"), "CC3-Decoder zeigt CC1: {shown}");
    let rec = std::path::PathBuf::from(status(&mut enc).0.recording.unwrap());
    let t = Instant::now();
    enc.stop();
    eprintln!("Stopp {:?}", t.elapsed());
    let t = Instant::now();
    drop(dec);
    eprintln!("Decoder weg {:?}", t.elapsed());
    let de = std::fs::read_to_string(rec.with_extension("srt")).unwrap();
    let en = std::fs::read_to_string(rec.with_extension("eng.srt")).expect(".eng.srt fehlt");
    assert!(de.contains("Guten Abend") && !de.contains("Good"), "{de}");
    assert!(en.contains("Good evening") && en.contains("Welcome") && !en.contains("Abend"), "{en}");
    assert!(rec.with_extension("eng.vtt").exists());
    eprintln!("CC1:\n{de}CC3:\n{en}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// I1: Eigenschaften live umstellen, ohne den Kanal neu zu starten.
#[test]
fn live_controls_without_restart() {
    use lz_streamer::audio::AudioTrack;
    use lz_streamer::config::{CaptionSource, Config};
    init();
    // SAFETY: Test setzt die Variable vor jedem Hilfsprozess
    unsafe { std::env::set_var("LZS_PLUGIN_HOST_EXE", env!("CARGO_BIN_EXE_lz-streamer")) };
    let port = free_port();
    let mut cfg = Config::default();
    cfg.encoder = encoder_cfg(port, SrtMode::Listener);
    cfg.encoder.captions.source = CaptionSource::Live;
    let mut t = AudioTrack::default();
    t.clear_speech.enabled = true;
    t.clear_speech.keep_singing = false;
    t.leveler.enabled = true;
    #[cfg(target_os = "macos")]
    t.plugins.push(lz_streamer::plugin::PluginConfig { enabled: true, path: "au:aufx:hpas:appl".into(), bypass: false });
    cfg.encoder.audio.tracks = vec![t];
    let mut enc = Channel::start_encoder(&cfg.encoder, &[], &ChannelOptions::default()).unwrap();
    let mut dec = Channel::start_decoder(&decoder_cfg(port, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    let (e, _) = run_until(&mut enc, &mut dec, Duration::from_secs(10), |e, _| e.audio_tracks[0].clear_speech.is_some_and(|c| c.residual_db < -6.0));
    let frames_before = e.frames;
    let set = |cfg: &mut Config, enc: &mut Channel, path: &str, v: serde_json::Value| {
        lz_streamer::control::apply(cfg, path, &v).unwrap();
        enc.command(&ChildCmd::Control { path: path.into(), cfg: Box::new(cfg.clone()) });
    };
    // Klare Sprache aus: Rest geht auf 0 dB
    set(&mut cfg, &mut enc, "audio.tracks.0.clearSpeech.enabled", serde_json::json!(false));
    run_until(&mut enc, &mut dec, Duration::from_secs(5), |e, _| e.audio_tracks[0].clear_speech.is_some_and(|c| c.residual_db > -0.5));
    // Untertitel stumm: nichts kommt an
    set(&mut cfg, &mut enc, "captions.enabled", serde_json::json!(false));
    enc.command(&ChildCmd::Caption { text: "darf nicht erscheinen".into() });
    let (e, _) = run_until(&mut enc, &mut dec, Duration::from_secs(2), |_, _| true);
    assert_ne!(e.captions.text.as_deref(), Some("darf nicht erscheinen"));
    set(&mut cfg, &mut enc, "captions.enabled", serde_json::json!(true));
    enc.command(&ChildCmd::Caption { text: "wieder da".into() });
    run_until(&mut enc, &mut dec, Duration::from_secs(3), |e, _| e.captions.text.as_deref() == Some("wieder da"));
    // Plugin-Bypass
    #[cfg(target_os = "macos")]
    {
        set(&mut cfg, &mut enc, "audio.tracks.0.plugins.0.bypass", serde_json::json!(true));
        run_until(&mut enc, &mut dec, Duration::from_secs(5), |e, _| e.audio_tracks[0].plugins.first().is_some_and(|p| p.bypass));
    }
    // Lautheitsprofil
    set(&mut cfg, &mut enc, "audio.tracks.0.leveler.preset", serde_json::json!("hard"));
    let (e, _) = run_until(&mut enc, &mut dec, Duration::from_secs(2), |_, _| true);
    assert!(e.frames > frames_before + 25, "Kanal lief nicht durch");
}

/// KB-Encoder-Optionen wirken: B-Frames, Profil und Entropie stehen in der Aufnahme.
#[test]
fn encoder_options_reach_the_stream() {
    init();
    if !lz_streamer::caps::element_available("x264enc") {
        return;
    }
    let port = free_port();
    let dir = std::env::temp_dir().join(format!("lzs-opt-{port}"));
    let _ = std::fs::remove_dir_all(&dir);
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    cfg.video.encoder = "x264enc".into();
    cfg.video.bframes = 2;
    cfg.video.profile = "main".into();
    cfg.video.entropy = lz_streamer::config::Entropy::Cavlc;
    cfg.video.extra = "threads=2".into();
    cfg.record.enabled = true;
    cfg.record.dir = dir.to_string_lossy().to_string();
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let end = Instant::now() + Duration::from_secs(3);
    let mut rec = None;
    while Instant::now() < end {
        rec = rec.or(status(&mut enc).0.recording);
        std::thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(enc.pipeline().by_name("venc").unwrap().property::<u32>("threads"), 2, "Zusatzparameter");
    enc.stop();
    let rec = rec.unwrap();
    let probe = |args: &[&str]| String::from_utf8_lossy(&std::process::Command::new("ffprobe").args(["-v", "error", "-select_streams", "v:0"]).args(args).arg(&rec).output().unwrap().stdout).to_string();
    let info = probe(&["-show_entries", "stream=profile,has_b_frames", "-of", "default=nw=1"]);
    let types = probe(&["-show_entries", "frame=pict_type", "-of", "csv=p=0"]);
    eprintln!("{info}B-Frames: {}", types.lines().filter(|l| l.trim() == "B").count());
    assert!(info.contains("profile=Main"), "{info}");
    assert!(types.lines().any(|l| l.trim() == "B"), "keine B-Frames");
    let _ = std::fs::remove_dir_all(&dir);
}

/// KB-40: HLS mit TS-Segmenten; ffprobe findet über die Master-Playlist Bild und beide Sprachen.
#[test]
fn hls_output_with_two_languages() {
    use lz_streamer::config::CaptionSource;
    init();
    let port = free_port();
    let dir = std::env::temp_dir().join(format!("lzs-hls-{port}"));
    let _ = std::fs::remove_dir_all(&dir);
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    three_languages(&mut cfg);
    cfg.audio.tracks.truncate(2);
    cfg.captions.source = CaptionSource::Live;
    cfg.hls.enabled = true;
    cfg.hls.dir = dir.to_string_lossy().to_string();
    cfg.hls.segment_s = 1;
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let end = Instant::now() + Duration::from_secs(8);
    while Instant::now() < end {
        status(&mut enc);
        std::thread::sleep(Duration::from_millis(200));
    }
    let master = std::fs::read_to_string(dir.join("index.m3u8")).unwrap();
    let v = std::fs::read_to_string(dir.join("v.m3u8")).expect("v.m3u8 fehlt");
    let a1 = std::fs::read_to_string(dir.join("a1.m3u8")).expect("a1.m3u8 fehlt");
    assert!(v.matches(".ts").count() >= 3, "{v}");
    assert!(a1.contains("a1_") && a1.contains(".ts"), "{a1}");
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream=codec_type,codec_name:stream_tags=language", "-of", "csv=p=0"])
        .arg(dir.join("index.m3u8"))
        .output()
        .unwrap();
    let probe = String::from_utf8_lossy(&out.stdout);
    eprintln!("{master}\n{probe}");
    enc.stop();
    assert!(probe.contains("video"), "{probe}");
    assert!(probe.contains("audio,de") && probe.contains("audio,en"), "Sprachen fehlen: {probe}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Pollt beliebig viele Kanäle, bis `done` die Status (in derselben Reihenfolge) akzeptiert.
#[track_caller]
fn run_all(chs: &mut [&mut Channel], timeout: Duration, mut done: impl FnMut(&[ChannelStatus]) -> bool) -> Vec<ChannelStatus> {
    let end = Instant::now() + timeout;
    loop {
        let st: Vec<ChannelStatus> = chs.iter_mut().map(|c| status(c).0).collect();
        if done(&st) {
            return st;
        }
        assert!(Instant::now() < end, "Zeit abgelaufen: {st:#?}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Zweiter Encoder mit Netz-Eingang: Quelle ist ein Testbild-Encoder auf `src_port`.
fn net_encoder_cfg(kind: InputKind, src_port: u16, out_port: u16, mode: lz_streamer::config::NetMode) -> EncoderConfig {
    let mut c = encoder_cfg(out_port, SrtMode::Listener);
    c.input.kind = kind;
    c.input.net.mode = mode;
    c.input.net.srt.mode = SrtMode::Caller;
    c.input.net.srt.host = "127.0.0.1".into();
    c.input.net.srt.port = src_port;
    c.input.net.srt.latency_ms = 80;
    c.input.net.host = "127.0.0.1".into();
    c.input.net.port = src_port;
    c.standby.timeout_ms = 500;
    c
}

fn ffmpeg_available() -> bool {
    std::process::Command::new("ffmpeg").arg("-version").output().is_ok()
}

/// KB-13: SRT rein (H.265) → dekodiert → H.264 neu kodiert in anderer Größe → SRT und RTMP raus.
/// Quelle weg → Standbild, Quelle zurück → Netzquelle verbindet neu, Eingang wieder auf Sendung.
#[test]
fn srt_input_transcoded_to_srt_and_rtmp() {
    use lz_streamer::config::NetMode;
    init();
    let (p_src, p_out) = (free_port(), free_port());
    let mut src_cfg = encoder_cfg(p_src, SrtMode::Listener);
    src_cfg.video.codec = Codec::H265;
    let mut src = Channel::start_encoder(&src_cfg, &[], &ChannelOptions::default()).unwrap();

    let mut cfg = net_encoder_cfg(InputKind::Srt, p_src, p_out, NetMode::Transcode);
    cfg.video.width = 320;
    cfg.video.height = 180;
    let rtmp = ffmpeg_available().then(|| {
        let rport = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let url = format!("rtmp://127.0.0.1:{rport}/live/test");
        let log = std::env::temp_dir().join(format!("lzs-net-rtmp-{rport}.log"));
        let ff = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "info", "-listen", "1", "-i", &url, "-f", "null", "-"])
            .stdin(std::process::Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(800));
        cfg.rtmp.enabled = true;
        cfg.rtmp.url = url;
        (ff, log)
    });
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let mut dec = Channel::start_decoder(&decoder_cfg(p_out, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    let st = run_all(&mut [&mut src, &mut enc, &mut dec], Duration::from_secs(30), |s| !s[1].standby && s[2].frames >= 50);
    assert!(st[1].input.contains("SRT") && st[1].input.contains("neu kodiert"), "{}", st[1].input);
    assert_eq!(st[1].signal, "ok");
    assert!(st[1].encoder.is_some());
    assert_eq!(st[2].codec, Some(Codec::H264), "nicht neu kodiert");
    assert_eq!(st[2].resolution.as_deref(), Some("320×180"));
    if let Some((mut ff, log)) = rtmp {
        // ffmpeg als RTMP-Gegenstelle nimmt den neu kodierten Strom an
        let st = run_all(&mut [&mut enc, &mut dec], Duration::from_secs(15), |s| s[0].rtmp.as_deref() == Some("sendet"));
        let _ = ff.kill();
        let _ = ff.wait();
        let _ = std::fs::remove_file(&log);
        assert_eq!(st[0].rtmp.as_deref(), Some("sendet"));
    }

    // Quelle fällt aus: Standbild, Ausgang läuft weiter
    src.stop();
    drop(src);
    let st = run_all(&mut [&mut enc, &mut dec], Duration::from_secs(10), |s| s[0].standby);
    assert_eq!(st[0].signal, "lost");
    let before = st[1].frames;
    run_all(&mut [&mut enc, &mut dec], Duration::from_secs(10), |s| s[1].frames >= before + 25);

    // Quelle zurück: Netzquelle verbindet neu
    let mut src = Channel::start_encoder(&src_cfg, &[], &ChannelOptions::default()).unwrap();
    let st = run_all(&mut [&mut src, &mut enc, &mut dec], Duration::from_secs(25), |s| !s[1].standby);
    assert_eq!(st[1].signal, "ok");
}

/// KB-13 Gateway: SRT rein → unverändert (H.265, 640×360) auf SRT raus, ohne Encoder.
#[test]
fn srt_input_passthrough_keeps_stream() {
    use lz_streamer::config::NetMode;
    init();
    let (p_src, p_out) = (free_port(), free_port());
    let mut src_cfg = encoder_cfg(p_src, SrtMode::Listener);
    src_cfg.video.codec = Codec::H265;
    let mut src = Channel::start_encoder(&src_cfg, &[], &ChannelOptions::default()).unwrap();
    let mut cfg = net_encoder_cfg(InputKind::Srt, p_src, p_out, NetMode::Passthrough);
    cfg.video.width = 320;
    cfg.video.height = 180;
    let mut gw = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let mut dec = Channel::start_decoder(&decoder_cfg(p_out, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    let st = run_all(&mut [&mut src, &mut gw, &mut dec], Duration::from_secs(20), |s| s[2].frames >= 50);
    assert_eq!(st[1].encoder, None, "Durchreichen darf nicht kodieren");
    assert!(st[1].input.contains("durchgereicht"), "{}", st[1].input);
    assert_eq!(st[1].signal, "ok");
    assert_eq!(st[2].codec, Some(Codec::H265));
    assert_eq!(st[2].resolution.as_deref(), Some("640×360"), "Größe verändert");
    // Quelle weg: Gateway meldet Signalverlust, bleibt aber stehen
    src.stop();
    drop(src);
    let st = run_all(&mut [&mut gw, &mut dec], Duration::from_secs(10), |s| s[0].signal == "lost");
    assert!(!st[0].standby);
    let mut src = Channel::start_encoder(&src_cfg, &[], &ChannelOptions::default()).unwrap();
    let before = st[1].frames;
    run_all(&mut [&mut src, &mut gw, &mut dec], Duration::from_secs(25), |s| s[1].signal == "ok" && s[2].frames >= before + 25);
}

/// UDP-TS rein → neu kodiert; RTP-TS rein → durchgereicht.
#[test]
fn udp_and_rtp_inputs() {
    use lz_streamer::config::{ExtraOutput, NetMode, OutputType};
    init();
    let free_udp = || std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let (p_src, p_udp, p_rtp, p_a, p_b) = (free_port(), free_udp(), free_udp(), free_port(), free_port());
    let mut src_cfg = encoder_cfg(p_src, SrtMode::Listener);
    let out = |kind, port| ExtraOutput { kind, host: "127.0.0.1".into(), port, ..Default::default() };
    src_cfg.outputs = vec![out(OutputType::Udp, p_udp), out(OutputType::Rtp, p_rtp)];
    let mut src = Channel::start_encoder(&src_cfg, &[], &ChannelOptions::default()).unwrap();

    let mut a_cfg = net_encoder_cfg(InputKind::Udp, p_udp, p_a, NetMode::Transcode);
    a_cfg.video.width = 320;
    a_cfg.video.height = 180;
    let mut a = Channel::start_encoder(&a_cfg, &[], &ChannelOptions::default()).unwrap();
    let mut b = Channel::start_encoder(&net_encoder_cfg(InputKind::Rtp, p_rtp, p_b, NetMode::Passthrough), &[], &ChannelOptions::default()).unwrap();
    let mut da = Channel::start_decoder(&decoder_cfg(p_a, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    let mut db = Channel::start_decoder(&decoder_cfg(p_b, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    let st = run_all(&mut [&mut src, &mut a, &mut b, &mut da, &mut db], Duration::from_secs(25), |s| {
        !s[1].standby && s[3].frames >= 25 && s[4].frames >= 25
    });
    assert_eq!(st[3].resolution.as_deref(), Some("320×180"));
    assert_eq!(st[4].resolution.as_deref(), Some("640×360"));
    assert!(st[2].input.starts_with("rtp://127.0.0.1:"), "{}", st[2].input);
}

/// RTMP-Quelle per URL abholen (ffmpeg als RTMP-Server) und neu kodieren.
#[test]
fn rtmp_input_pulled_and_transcoded() {
    use lz_streamer::config::NetMode;
    init();
    if !ffmpeg_available() {
        eprintln!("ffmpeg fehlt – Test übersprungen");
        return;
    }
    let rport = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let url = format!("rtmp://127.0.0.1:{rport}/live/quelle");
    let mut ff = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-re", "-f", "lavfi", "-i", "testsrc2=size=480x270:rate=25", "-f", "lavfi", "-i", "sine=frequency=1000"])
        .args(["-c:v", "libx264", "-tune", "zerolatency", "-g", "25", "-pix_fmt", "yuv420p", "-c:a", "aac", "-f", "flv", "-listen", "1", &url])
        .stdin(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(1000));
    let p_out = free_port();
    let mut cfg = net_encoder_cfg(InputKind::Rtmp, 0, p_out, NetMode::Transcode);
    cfg.input.net.url = url;
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let mut dec = Channel::start_decoder(&decoder_cfg(p_out, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    let st = run_all(&mut [&mut enc, &mut dec], Duration::from_secs(20), |s| !s[0].standby && s[1].frames >= 25);
    let _ = ff.kill();
    let _ = ff.wait();
    assert!(st[0].input.contains("rtmp://127.0.0.1"), "{}", st[0].input);
    assert!(!st[0].input.contains("quelle"), "Stream-Key in der Anzeige: {}", st[0].input);
    assert_eq!(st[1].resolution.as_deref(), Some("640×360"));
}


/// Keyframe-Zeiten (PTS in 90 kHz) aller Segmente einer Medien-Playlist
fn hls_keyframes(dir: &std::path::Path, playlist: &str) -> Vec<i64> {
    let text = std::fs::read_to_string(dir.join(playlist)).unwrap();
    let segs: Vec<String> = text.lines().filter(|l| !l.starts_with('#') && !l.is_empty()).map(|l| dir.join(l).to_string_lossy().to_string()).collect();
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "packet=pts,flags", "-of", "csv=p=0"])
        .arg(format!("concat:{}", segs.join("|")))
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains(",K"))
        .filter_map(|l| l.split(',').next()?.parse().ok())
        .collect()
}

/// KB-30: drei Bildstufen gleichzeitig, Keyframes auf denselben PTS, Master-Playlist mit drei Varianten.
#[test]
fn hls_abr_ladder_aligned_keyframes() {
    init();
    // Hardware zuerst, dann der Software-Rückfall
    for enc in ["auto", "x264enc"] {
        if enc == "auto" || gstreamer::ElementFactory::find(enc).is_some() {
            abr_ladder_with(enc);
        }
    }
}

fn abr_ladder_with(encoder: &str) {
    use lz_streamer::config::CaptionSource;
    let port = free_port();
    let dir = std::env::temp_dir().join(format!("lzs-abr-{port}"));
    let _ = std::fs::remove_dir_all(&dir);
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    cfg.video.width = 1280;
    cfg.video.height = 720;
    cfg.video.bitrate_kbps = 3000;
    cfg.video.encoder = encoder.into();
    cfg.captions.source = CaptionSource::Live;
    cfg.hls.enabled = true;
    cfg.hls.dir = dir.to_string_lossy().to_string();
    cfg.hls.segment_s = 1;
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let end = Instant::now() + Duration::from_secs(8);
    let mut st = None;
    while Instant::now() < end {
        st = Some(status(&mut enc).0);
        std::thread::sleep(Duration::from_millis(200));
    }
    let st = st.unwrap();
    enc.stop();
    eprintln!("Encoder {:?}, Stufen {:?}", st.encoder, st.abr);
    assert_eq!(st.abr.len(), 2, "{:?}", st.abr);
    assert!(st.abr.iter().all(|r| r.fps > 20.0), "{:?}", st.abr);
    let master = std::fs::read_to_string(dir.join("index.m3u8")).unwrap();
    assert_eq!(master.matches("#EXT-X-STREAM-INF").count(), 3, "{master}");
    let k0 = hls_keyframes(&dir, "v.m3u8");
    let k1 = hls_keyframes(&dir, "v1.m3u8");
    let k2 = hls_keyframes(&dir, "v2.m3u8");
    eprintln!("Keyframes v: {k0:?}\nv1: {k1:?}\nv2: {k2:?}");
    assert!(k0.len() >= 4, "{k0:?}");
    // gemeinsamer Zeitraum: dort müssen alle Keyframes übereinstimmen
    let lo = *[k0[0], k1[0], k2[0]].iter().max().unwrap();
    let hi = *[*k0.last().unwrap(), *k1.last().unwrap(), *k2.last().unwrap()].iter().min().unwrap();
    let win = |k: &[i64]| k.iter().copied().filter(|&p| p >= lo && p <= hi).collect::<Vec<_>>();
    assert!(win(&k0).len() >= 3, "zu kurzer gemeinsamer Zeitraum {lo}..{hi}");
    assert_eq!(win(&k0), win(&k1), "Stufe 1 weicht ab");
    assert_eq!(win(&k0), win(&k2), "Stufe 2 weicht ab");
    let v2 = std::fs::read_to_string(dir.join("v2.m3u8")).unwrap();
    let seg = v2.lines().rfind(|l| l.ends_with(".ts")).unwrap();
    let out = std::process::Command::new("ffprobe").args(["-v", "error", "-show_entries", "stream=width,height", "-of", "csv=p=0"]).arg(dir.join(seg)).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("640,360"), "{}", String::from_utf8_lossy(&out.stdout));
    let _ = std::fs::remove_dir_all(&dir);
}

/// KB-26: Halbbild-Ausgabe – 50 Halbbilder/s werden zu 25 Bildern mit oberem Halbbild zuerst,
/// auch wenn „automatisch“ sonst VideoToolbox nähme.
#[test]
fn interlaced_output_tff() {
    interlaced_output(lz_streamer::config::OutputFieldOrder::Tff, "tt");
    interlaced_output(lz_streamer::config::OutputFieldOrder::Bff, "bb");
}

fn interlaced_output(order: lz_streamer::config::OutputFieldOrder, want: &str) {
    init();
    if !lz_streamer::caps::element_available("x264enc") {
        return;
    }
    let port = free_port();
    let dir = std::env::temp_dir().join(format!("lzs-il-{port}"));
    let _ = std::fs::remove_dir_all(&dir);
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    cfg.video.fps = 50;
    cfg.video.interlaced = true;
    cfg.video.output_field_order = order;
    cfg.record.enabled = true;
    cfg.record.dir = dir.to_string_lossy().to_string();
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let end = Instant::now() + Duration::from_secs(4);
    let mut rec = None;
    let mut encoder = None;
    while Instant::now() < end {
        let s = status(&mut enc).0;
        rec = rec.or(s.recording);
        encoder = encoder.or(s.encoder);
        std::thread::sleep(Duration::from_millis(200));
    }
    enc.stop();
    assert_eq!(encoder.as_deref(), Some("x264enc"));
    let rec = rec.unwrap();
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=field_order,r_frame_rate,avg_frame_rate", "-of", "default=nw=1"])
        .arg(&rec)
        .output()
        .unwrap();
    let info = String::from_utf8_lossy(&out.stdout);
    eprintln!("{info}");
    assert!(info.contains(&format!("field_order={want}")), "Reihenfolge {want} erwartet: {info}");
    assert!(info.contains("avg_frame_rate=25/1") || info.contains("r_frame_rate=25/1") || info.contains("r_frame_rate=50/1"), "{info}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Halbbild-Eingang (unteres zuerst, wie DV) wird aufgelöst: progressive Ausgabe mit voller Bildrate.
#[test]
fn interlaced_file_input_is_deinterlaced() {
    use lz_streamer::config::{FieldOrder, InputKind};
    init();
    if !lz_streamer::caps::element_available("x264enc") {
        return;
    }
    let port = free_port();
    let dir = std::env::temp_dir().join(format!("lzs-deint-{port}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("bff.ts");
    let maker = gstreamer::parse::launch(&format!(
        "videotestsrc num-buffers=200 pattern=ball ! video/x-raw,width=640,height=360,framerate=50/1 ! interlace field-pattern=1:1 top-field-first=false \
         ! x264enc ! h264parse ! mpegtsmux ! filesink location={}",
        src.display()
    ))
    .unwrap();
    use gstreamer::prelude::*;
    maker.set_state(gstreamer::State::Playing).unwrap();
    let _ = maker.bus().unwrap().timed_pop_filtered(gstreamer::ClockTime::from_seconds(20), &[gstreamer::MessageType::Eos, gstreamer::MessageType::Error]);
    maker.set_state(gstreamer::State::Null).unwrap();
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    cfg.input.kind = InputKind::File;
    cfg.input.file = src.to_string_lossy().to_string();
    cfg.video.fps = 50;
    cfg.video.input_field_order = FieldOrder::Auto;
    cfg.record.enabled = true;
    cfg.record.dir = dir.join("rec").to_string_lossy().to_string();
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let end = Instant::now() + Duration::from_secs(3);
    let mut rec = None;
    let mut deint_caps = String::new();
    while Instant::now() < end {
        rec = rec.or(status(&mut enc).0.recording);
        // der Deinterlacer muss Halbbilder mit unterem zuerst sehen
        for el in enc.pipeline().iterate_recurse().into_iter().flatten() {
            if el.factory().is_some_and(|f| f.name() == "deinterlace")
                && let Some(c) = el.static_pad("sink").and_then(|p| p.current_caps())
            {
                deint_caps = c.to_string();
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    enc.stop();
    assert!(deint_caps.contains("interlace-mode=(string)interleaved") && deint_caps.contains("field-order=(string)bottom-field-first"), "{deint_caps}");
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=field_order,r_frame_rate", "-of", "default=nw=1"])
        .arg(rec.unwrap())
        .output()
        .unwrap();
    let info = String::from_utf8_lossy(&out.stdout);
    assert!(info.contains("field_order=progressive") && info.contains("r_frame_rate=50/1"), "{info}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Nimmt mit ffmpeg als RTMP-Gegenstelle `secs` Sekunden auf und liefert
/// `codec_type,codec_name,width,height` je Spur.
fn rtmp_capture(secs: u32, start: impl FnOnce(String) -> Vec<Channel>) -> String {
    let rport = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let url = format!("rtmp://127.0.0.1:{rport}/live/test");
    let out = std::env::temp_dir().join(format!("lzs-rtmp-{rport}.flv"));
    let mut ff = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-listen", "1", "-i", &url, "-c", "copy", "-t", &secs.to_string()])
        .arg(&out)
        .stdin(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(800));
    let mut chs = start(url);
    let end = Instant::now() + Duration::from_secs(30);
    while ff.try_wait().unwrap().is_none() {
        for c in chs.iter_mut() {
            let _ = c.poll();
        }
        if Instant::now() > end {
            let _ = ff.kill();
            panic!("ffmpeg endet nicht");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    for c in chs.iter_mut() {
        c.stop();
    }
    let probe = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream=codec_type,codec_name,width,height", "-of", "csv=p=0"])
        .arg(&out)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&out);
    String::from_utf8_lossy(&probe.stdout).into_owned()
}

/// RTMP-Ausgang: ffmpeg muss Bildgröße und Ton sehen, nicht „h264, none, unspecified size“.
#[test]
fn rtmp_output_carries_video_size() {
    init();
    if !ffmpeg_available() {
        eprintln!("ffmpeg fehlt – Test übersprungen");
        return;
    }
    // Hardware-Encoder (falls vorhanden) und x264
    for encoder in ["", "x264enc"] {
        let s = rtmp_capture(3, |url| {
            let mut cfg = encoder_cfg(free_port(), SrtMode::Listener);
            cfg.video.encoder = encoder.into();
            cfg.rtmp.enabled = true;
            cfg.rtmp.url = url;
            vec![Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap()]
        });
        assert!(s.lines().any(|l| l.starts_with("h264,") && l.ends_with(",640,360")), "{encoder}: {s}");
        assert!(s.contains("aac"), "{encoder}: kein Ton: {s}");
    }
}

/// Schneidet den TS eines UDP-Zusatzausgangs `secs` Sekunden lang mit (für ffprobe).
/// `during` läuft bei jedem Status-Abruf mit (etwa für Befehle an den Kanal).
fn capture_udp_ts(cfg: &mut EncoderConfig, secs: u64, tag: &str, mut during: impl FnMut(&mut Channel, Duration)) -> std::path::PathBuf {
    use lz_streamer::config::{ExtraOutput, OutputType};
    let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    udp.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    let port = udp.local_addr().unwrap().port();
    cfg.outputs.push(ExtraOutput { name: "Mitschnitt".into(), kind: OutputType::Udp, host: "127.0.0.1".into(), port, ..Default::default() });
    let mut enc = Channel::start_encoder(cfg, &[], &ChannelOptions::default()).unwrap();
    let path = std::env::temp_dir().join(format!("lzs-{tag}-{port}.ts"));
    let mut data = Vec::new();
    let t0 = Instant::now();
    let mut buf = [0u8; 65536];
    let mut last_poll = Instant::now();
    while t0.elapsed() < Duration::from_secs(secs) {
        if let Ok(n) = udp.recv(&mut buf) {
            data.extend_from_slice(&buf[..n]);
        }
        if last_poll.elapsed() > Duration::from_millis(200) {
            let _ = status(&mut enc);
            during(&mut enc, t0.elapsed());
            last_poll = Instant::now();
        }
    }
    enc.stop();
    std::fs::write(&path, &data).unwrap();
    path
}

fn ffprobe_csv(path: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("ffprobe").args(["-v", "error"]).args(args).args(["-of", "csv=p=0"]).arg(path).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// #16: Untertiteldatei → zusätzlich DVB-Untertitel im TS, mit Sprachkennung.
#[test]
fn dvb_subtitles_in_ts_with_language() {
    use lz_streamer::config::CaptionSource;
    init();
    let mut cfg = encoder_cfg(free_port(), SrtMode::Listener);
    cfg.captions.source = CaptionSource::File;
    cfg.captions.file = srt_file("dvb").to_string_lossy().to_string();
    cfg.captions.dvb.enabled = true;
    cfg.captions.dvb.language = "deu".into();
    let ts = capture_udp_ts(&mut cfg, 10, "dvb", |_, _| {});
    let streams = ffprobe_csv(&ts, &["-show_entries", "stream=codec_name,id:stream_tags=language"]);
    let pk = ffprobe_csv(&ts, &["-select_streams", "s", "-show_entries", "packet=size"]);
    let pcr = ffprobe_csv(&ts, &["-show_entries", "program=pcr_pid"]);
    if std::env::var("LZS_KEEP").is_err() { let _ = std::fs::remove_file(&ts); }
    let sizes: Vec<u32> = pk.lines().filter_map(|l| l.split(',').next()?.trim().parse().ok()).collect();
    assert!(streams.contains("h264") && streams.contains("dvb_subtitle,0x60,deu") && streams.contains("h264,0x41") && streams.contains("aac,0x42"), "{streams}");
    assert!(pcr.starts_with("65"), "PCR nicht auf dem Bild: {pcr}");
    // Seiten mit Bild (groß) und Löschseiten (klein)
    assert!(sizes.iter().filter(|&&s| s > 100).count() >= 2, "{sizes:?}");
    assert!(sizes.iter().any(|&s| s < 100), "keine Löschseite: {sizes:?}");
}

type Posts = std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>;

/// Kleiner HTTP-Ersatz: nimmt POSTs an und legt (Anfragezeile, Körper) ab.
fn http_post_stub() -> (u16, Posts) {
    use std::io::{BufRead, BufReader, Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let got: Posts = Default::default();
    let g = got.clone();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut first = String::new();
            let _ = r.read_line(&mut first);
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; len];
            let _ = r.read_exact(&mut body);
            g.lock().unwrap().push((first.trim().to_string(), String::from_utf8_lossy(&body).into_owned()));
            let mut s = s;
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 24\r\nConnection: close\r\n\r\n2026-11-15T10:00:00.000\n");
        }
    });
    (port, got)
}

/// #17: Live-Untertitel gehen zusätzlich per HTTP POST an die YouTube-Ingest-URL.
#[test]
fn youtube_captions_posted_with_sequence() {
    use lz_streamer::config::CaptionSource;
    init();
    let (hport, got) = http_post_stub();
    let port = free_port();
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    cfg.captions.source = CaptionSource::Live;
    cfg.captions.youtube.enabled = true;
    cfg.captions.youtube.url = format!("http://127.0.0.1:{hport}/closedcaption?cid=test");
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let mut dec = Channel::start_decoder(&decoder_cfg(port, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    run_until(&mut enc, &mut dec, Duration::from_secs(10), |_, d| d.frames >= 25);
    let before = std::time::SystemTime::now();
    enc.command(&ChildCmd::Caption { text: "Erste Zeile".into() });
    enc.command(&ChildCmd::Caption { text: "Zweite Zeile".into() });
    let (e, _) = run_until(&mut enc, &mut dec, Duration::from_secs(10), |e, _| e.captions.youtube.as_ref().is_some_and(|y| y.sent >= 2));
    enc.stop();
    let y = e.captions.youtube.unwrap();
    assert_eq!(y.failed, 0, "{y:?}");
    assert_eq!(y.server_time.as_deref(), Some("2026-11-15T10:00:00.000"));
    let got = got.lock().unwrap().clone();
    assert!(got[0].0.starts_with("POST /closedcaption?cid=test&seq=1 "), "{got:?}");
    assert!(got[1].0.contains("&seq=2 "), "{got:?}");
    let (ts, text) = got[0].1.split_once('\n').unwrap();
    assert_eq!(text, "Erste Zeile\n");
    // Zeitstempel UTC, kurz nach dem Absenden (Einspeisen 300 ms in der Zukunft)
    let at = chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%dT%H:%M:%S%.3f").unwrap().and_utc();
    let lead = at.timestamp_millis() - chrono::DateTime::<chrono::Utc>::from(before).timestamp_millis();
    assert!((0..2000).contains(&lead), "Zeitstempel {ts} weicht {lead} ms ab");
}

/// #28: Timecode (aus der Systemzeit gestempelt) geht als ST 2038 auf PID 0x62
/// durch und erscheint am Decoder; die PCR bleibt auf dem Bild.
#[test]
fn timecode_as_st2038_reaches_decoder() {
    init();
    let port = free_port();
    let mut cfg = encoder_cfg(port, SrtMode::Listener);
    cfg.anc.st2038 = true;
    cfg.anc.stamp_timecode = true;
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    let mut dec = Channel::start_decoder(&decoder_cfg(port, SrtMode::Caller), &ChannelOptions::default()).unwrap();
    let (e, d) = run_until(&mut enc, &mut dec, Duration::from_secs(15), |_, d| d.frames >= 25 && d.anc.as_ref().is_some_and(|a| a.packets >= 25));
    enc.stop();
    dec.stop();
    let (ea, da) = (e.anc.unwrap(), d.anc.unwrap());
    let tc = da.timecode.clone().unwrap();
    // hh:mm:ss:ff der Systemzeit, Encoder und Decoder liegen höchstens wenige Sekunden auseinander
    assert_eq!(tc.len(), 11, "{tc}");
    let secs = |t: &str| t.split(':').take(3).map(|x| x.parse::<i64>().unwrap()).fold(0, |a, x| a * 60 + x);
    assert!((secs(&tc) - secs(ea.timecode.as_deref().unwrap())).abs() <= 3, "{tc} / {:?}", ea.timecode);
    assert!(ea.packets >= da.packets, "{ea:?} {da:?}");

    // im TS: Datenstrom „VANC“ auf 0x62, PCR auf dem Bild
    let mut cfg = encoder_cfg(free_port(), SrtMode::Listener);
    cfg.anc.st2038 = true;
    cfg.anc.stamp_timecode = true;
    let ts = capture_udp_ts(&mut cfg, 4, "anc", |_, _| {});
    let streams = ffprobe_csv(&ts, &["-show_entries", "stream=codec_type,codec_tag_string,id"]);
    let pcr = ffprobe_csv(&ts, &["-show_entries", "program=pcr_pid"]);
    let _ = std::fs::remove_file(&ts);
    assert!(streams.contains("data,VANC,0x62"), "{streams}");
    assert!(pcr.starts_with("65"), "{pcr}");
}

/// PES einer PID: (PTS, Nutzlast)
type PesList = Vec<(Option<u64>, Vec<u8>)>;

/// TS zerlegen: PES je PID (PTS, Nutzlast) und Sections je PID.
struct TsParts {
    pes: std::collections::HashMap<u16, PesList>,
    sections: std::collections::HashMap<u16, Vec<Vec<u8>>>,
    /// PID → stream_type aus der PMT
    types: std::collections::HashMap<u16, u8>,
}

fn parse_ts(data: &[u8]) -> TsParts {
    use std::collections::HashMap;
    let mut cur: HashMap<u16, Vec<u8>> = HashMap::new();
    let mut parts = TsParts { pes: HashMap::new(), sections: HashMap::new(), types: HashMap::new() };
    let mut pmt_pid = None;
    let pes_of = |buf: &[u8]| -> Option<(Option<u64>, Vec<u8>)> {
        if buf.len() < 9 || buf[0..3] != [0, 0, 1] {
            return None;
        }
        let hl = buf[8] as usize;
        let pts = (buf[7] & 0x80 != 0).then(|| {
            let b = &buf[9..14];
            ((b[0] as u64 >> 1) & 7) << 30 | (b[1] as u64) << 22 | (b[2] as u64 >> 1) << 15 | (b[3] as u64) << 7 | b[4] as u64 >> 1
        });
        Some((pts, buf.get(9 + hl..)?.to_vec()))
    };
    for pkt in data.as_chunks::<188>().0.iter().filter(|p| p[0] == 0x47) {
        let pusi = pkt[1] & 0x40 != 0;
        let pid = ((pkt[1] as u16 & 0x1f) << 8) | pkt[2] as u16;
        let afc = (pkt[3] >> 4) & 3;
        if afc & 1 == 0 {
            continue;
        }
        let start = if afc == 3 { 5 + pkt[4] as usize } else { 4 };
        if start >= 188 {
            continue;
        }
        let payload = &pkt[start..];
        let is_psi = pid == 0 || Some(pid) == pmt_pid || pid == 0x1f6;
        if is_psi {
            if !pusi {
                continue;
            }
            let sec = &payload[1 + payload[0] as usize..];
            if sec.len() < 3 || sec[0] == 0xff {
                continue;
            }
            let len = ((sec[1] as usize & 0x0f) << 8 | sec[2] as usize) + 3;
            let sec = &sec[..len.min(sec.len())];
            if pid == 0 && sec.len() >= 16 {
                pmt_pid = Some(((sec[10] as u16 & 0x1f) << 8) | sec[11] as u16);
            } else if Some(pid) == pmt_pid {
                let pil = ((sec[10] as usize & 0x0f) << 8) | sec[11] as usize;
                let mut i = 12 + pil;
                while i + 5 <= sec.len() - 4 {
                    let es = ((sec[i + 1] as u16 & 0x1f) << 8) | sec[i + 2] as u16;
                    parts.types.insert(es, sec[i]);
                    i += 5 + (((sec[i + 3] as usize & 0x0f) << 8) | sec[i + 4] as usize);
                }
            } else {
                parts.sections.entry(pid).or_default().push(sec.to_vec());
            }
            continue;
        }
        if pusi {
            if let Some(p) = cur.remove(&pid).and_then(|buf| pes_of(&buf)) {
                parts.pes.entry(pid).or_default().push(p);
            }
            cur.insert(pid, payload.to_vec());
        } else if let Some(buf) = cur.get_mut(&pid) {
            buf.extend_from_slice(payload);
        }
    }
    parts
}

/// H.264-Zugriffseinheit enthält ein IDR-Bild (NAL-Typ 5)?
fn has_idr(au: &[u8]) -> bool {
    au.windows(4).any(|w| w[0..3] == [0, 0, 1] && w[3] & 0x1f == 5)
}

/// #27: Cue per Befehl → SCTE-35 splice_insert auf PID 0x1F6; an der Splice-PTS liegt ein IDR-Bild.
#[test]
fn scte35_cue_with_idr_at_splice_point() {
    use lz_streamer::scte35::{Cue, CueKind};
    init();
    let mut cfg = encoder_cfg(free_port(), SrtMode::Listener);
    cfg.anc.scte35 = true;
    // langes GOP, damit ein IDR am Splice-Punkt nicht zufällig ist
    cfg.video.gop_ms = 10_000;
    let mut sent = false;
    let ts = capture_udp_ts(&mut cfg, 6, "scte", |enc, t| {
        if !sent && t > Duration::from_millis(1500) {
            enc.command(&ChildCmd::Scte35 { cue: Cue { kind: CueKind::Out, preroll_ms: 1000, duration_s: 30.0, event_id: Some(4711) } });
            sent = true;
        }
    });
    let data = std::fs::read(&ts).unwrap();
    let _ = std::fs::remove_file(&ts);
    let parts = parse_ts(&data);
    assert_eq!(parts.types.get(&0x1f6), Some(&0x86), "keine SCTE-35-PID in der PMT: {:?}", parts.types);
    // splice_info_section mit splice_insert (Befehl 5)
    let sec = parts.sections.get(&0x1f6).and_then(|v| v.iter().find(|s| s[0] == 0xfc && s.get(13) == Some(&5))).expect("kein splice_insert").clone();
    let pts_adj = ((sec[4] as u64 & 1) << 32) | u32::from_be_bytes(sec[5..9].try_into().unwrap()) as u64;
    let cmd = &sec[14..];
    let id = u32::from_be_bytes(cmd[0..4].try_into().unwrap());
    assert_eq!(id, 4711);
    assert_eq!(cmd[4] & 0x80, 0, "abgesagt");
    let flags = cmd[5];
    assert!(flags & 0x80 != 0 && flags & 0x40 != 0, "out_of_network/program_splice fehlt: {flags:#x}");
    let t = &cmd[6..11];
    assert!(t[0] & 0x80 != 0, "keine Splice-Zeit");
    let splice = ((((t[0] as u64 & 1) << 32) | u32::from_be_bytes(t[1..5].try_into().unwrap()) as u64) + pts_adj) & ((1 << 33) - 1);
    let video = parts.pes.get(&0x41).expect("kein Bild auf 0x41");
    // Bild an der Splice-PTS (höchstens ein halbes Bild daneben)
    let at = video.iter().filter_map(|(p, d)| p.map(|p| (p as i64 - splice as i64, d))).min_by_key(|(d, _)| d.abs()).expect("keine Bilder");
    let idr_pts: Vec<i64> = video.iter().filter(|(_, d)| has_idr(d)).filter_map(|(p, _)| p.map(|p| p as i64 - splice as i64)).collect();
    assert!(at.0.abs() <= 1800, "nächstes Bild {} Ticks vom Splice-Punkt", at.0);
    assert!(has_idr(at.1), "am Splice-Punkt kein IDR; IDR liegen bei {idr_pts:?} Ticks");
    let idrs = video.iter().filter(|(_, d)| has_idr(d)).count();
    assert!(idrs <= 3, "zu viele IDR ({idrs}), Nachweis wertlos");
}

/// #34: 10 bit, 4:2:2 und HDR-Kennzeichnung kommen im Strom an (ffprobe).
#[test]
fn ten_bit_422_and_hdr_flags_in_stream() {
    use lz_streamer::config::{Chroma, Transfer};
    init();
    let cases = [
        // Hardware zuerst (VideoToolbox, falls vorhanden), dann Software
        (Codec::H265, "auto", Chroma::C422_10, Transfer::Pq, "yuv422p10le", "smpte2084"),
        (Codec::H265, "auto", Chroma::C420_10, Transfer::Hlg, "yuv420p10le", "arib-std-b67"),
        (Codec::H265, "x265enc", Chroma::C422_10, Transfer::Hlg, "yuv422p10le", "arib-std-b67"),
        // H.264 mit 4:2:2 10 bit: VideoToolbox kann das nicht, gewählt wird x264
        (Codec::H264, "auto", Chroma::C422_10, Transfer::Pq, "yuv422p10le", "smpte2084"),
    ];
    for (codec, encoder, chroma, transfer, pix, trc) in cases {
        if encoder == "x265enc" && !lz_streamer::caps::element_available("x265enc") {
            continue;
        }
        let mut cfg = encoder_cfg(free_port(), SrtMode::Listener);
        cfg.video.codec = codec;
        cfg.video.encoder = encoder.into();
        cfg.video.chroma = chroma;
        cfg.video.transfer = transfer;
        let ts = capture_udp_ts(&mut cfg, 3, "hdr", |_, _| {});
        let info = ffprobe_csv(&ts, &["-select_streams", "v:0", "-show_entries", "stream=pix_fmt,color_transfer,color_primaries"]);
        let _ = std::fs::remove_file(&ts);
        let want = format!("{pix},{trc},bt2020");
        assert!(info.lines().any(|l| l.trim() == want), "{codec:?}/{encoder}/{chroma:?}: {info} statt {want}");
    }
}

/// UDP-Weiterleitung mit Störung (statt netem): `drop_every` verwirft jedes n-te
/// Paket (0 = keins), `cut` verwirft alles – wie ein gezogenes Kabel.
struct UdpProxy {
    port: u16,
    drop_every: std::sync::Arc<std::sync::atomic::AtomicU32>,
    cut: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

fn udp_proxy_on(listen: std::net::UdpSocket, target: u16) -> UdpProxy {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
    let port = listen.local_addr().unwrap().port();
    let up = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    up.connect(("127.0.0.1", target)).unwrap();
    let drop_every: Arc<AtomicU32> = Arc::default();
    let cut: Arc<AtomicBool> = Arc::default();
    let client: Arc<std::sync::Mutex<Option<std::net::SocketAddr>>> = Arc::default();
    let n: Arc<AtomicU64> = Arc::default();
    let pass = {
        let (d, c, n) = (drop_every.clone(), cut.clone(), n.clone());
        move || {
            let i = n.fetch_add(1, Ordering::Relaxed) + 1;
            let e = d.load(Ordering::Relaxed) as u64;
            !c.load(Ordering::Relaxed) && (e == 0 || i % e != 0)
        }
    };
    {
        let (l, u, cl, pass) = (listen.try_clone().unwrap(), up.try_clone().unwrap(), client.clone(), pass.clone());
        std::thread::spawn(move || {
            let mut b = [0u8; 65536];
            while let Ok((len, from)) = l.recv_from(&mut b) {
                *cl.lock().unwrap() = Some(from);
                if pass() {
                    let _ = u.send(&b[..len]);
                }
            }
        });
    }
    std::thread::spawn(move || {
        let mut b = [0u8; 65536];
        while let Ok(len) = up.recv(&mut b) {
            let to = *client.lock().unwrap();
            if let (Some(to), true) = (to, pass()) {
                let _ = listen.send_to(&b[..len], to);
            }
        }
    });
    UdpProxy { port, drop_every, cut }
}

fn udp_proxy(target: u16) -> UdpProxy {
    udp_proxy_on(std::net::UdpSocket::bind("127.0.0.1:0").unwrap(), target)
}

/// Gerader Port p, bei dem p und p + 1 frei sind (RTP/RTCP für RIST).
fn even_udp_pair() -> (std::net::UdpSocket, std::net::UdpSocket) {
    loop {
        let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let p = s.local_addr().unwrap().port();
        if !p.is_multiple_of(2) || p == u16::MAX {
            continue;
        }
        if let Ok(s2) = std::net::UdpSocket::bind(("127.0.0.1", p + 1)) {
            return (s, s2);
        }
    }
}

fn even_free_port() -> u16 {
    let (a, _) = even_udp_pair();
    a.local_addr().unwrap().port()
}

/// #33: RIST über zwei Wege (broadcast). Fällt ein Weg aus, laufen die Bilder ohne Lücke weiter.
#[test]
fn rist_two_paths_survive_cut_of_one() {
    use lz_streamer::config::{ExtraOutput, OutputType};
    use std::sync::atomic::Ordering;
    init();
    if !lz_streamer::caps::element_available("ristsink") {
        eprintln!("ristsink fehlt – Test übersprungen");
        return;
    }
    // Empfänger lauscht auf r1/r2, der Sender schickt über je ein Proxy-Paar (RTP + RTCP)
    let (r1, r2) = (even_free_port(), even_free_port());
    let paths: Vec<(UdpProxy, UdpProxy)> = [r1, r2]
        .into_iter()
        .map(|r| {
            let (a, b) = even_udp_pair();
            (udp_proxy_on(a, r), udp_proxy_on(b, r + 1))
        })
        .collect();
    let mut dc = decoder_cfg(free_port(), SrtMode::Caller);
    dc.source = lz_streamer::rsp::DecoderSource::Rist;
    dc.rist.paths = format!("127.0.0.1:{r1},127.0.0.1:{r2}");
    dc.rist.buffer_ms = 300;
    let mut dec = Channel::start_decoder(&dc, &ChannelOptions::default()).unwrap();
    let mut cfg = encoder_cfg(free_port(), SrtMode::Listener);
    cfg.outputs = vec![ExtraOutput {
        name: "RIST".into(),
        kind: OutputType::Rist,
        host: "127.0.0.1".into(),
        port: paths[0].0.port,
        rist_paths: format!("127.0.0.1:{}", paths[1].0.port),
        rist_buffer_ms: 500,
        ..Default::default()
    }];
    let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
    run_until(&mut enc, &mut dec, Duration::from_secs(15), |_, d| d.frames >= 50);
    // je Weg 3 s getrennt; in jeder Sekunde müssen ≥ 20 Bilder ankommen
    for (i, (rtp, rtcp)) in paths.iter().enumerate() {
        rtp.cut.store(true, Ordering::Relaxed);
        rtcp.cut.store(true, Ordering::Relaxed);
        for _ in 0..3 {
            let before = status(&mut dec).0.frames;
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_secs(1) {
                let _ = status(&mut enc);
                std::thread::sleep(Duration::from_millis(100));
            }
            let got = status(&mut dec).0.frames - before;
            assert!(got >= 20, "Weg {} getrennt: nur {got} Bilder in 1 s", i + 1);
        }
        rtp.cut.store(false, Ordering::Relaxed);
        rtcp.cut.store(false, Ordering::Relaxed);
        std::thread::sleep(Duration::from_secs(1));
    }
    // Gegenprobe: beide Wege getrennt → nach dem Puffer kommt nichts mehr
    for (rtp, rtcp) in &paths {
        rtp.cut.store(true, Ordering::Relaxed);
        rtcp.cut.store(true, Ordering::Relaxed);
    }
    std::thread::sleep(Duration::from_millis(800));
    let before = status(&mut dec).0.frames;
    std::thread::sleep(Duration::from_secs(1));
    let got = status(&mut dec).0.frames - before;
    assert!(got < 5, "beide Wege getrennt, trotzdem {got} Bilder – Proxy wirkt nicht");
    enc.stop();
    dec.stop();
}

/// #33: SRT-FEC (Paketfilter) über eine verlustbehaftete Strecke: deutlich
/// weniger Wiederholungen als ohne FEC, Bilder kommen in beiden Fällen an.
#[test]
fn srt_fec_reduces_retransmissions_under_loss() {
    init();
    let run = |fec: &str| -> (u64, u64) {
        let port = free_port();
        let mut cfg = encoder_cfg(port, SrtMode::Listener);
        cfg.srt.fec = fec.into();
        let proxy = udp_proxy(port);
        proxy.drop_every.store(50, std::sync::atomic::Ordering::Relaxed);
        let mut dc = decoder_cfg(proxy.port, SrtMode::Caller);
        dc.srt.fec = fec.into();
        let mut enc = Channel::start_encoder(&cfg, &[], &ChannelOptions::default()).unwrap();
        let mut dec = Channel::start_decoder(&dc, &ChannelOptions::default()).unwrap();
        let (_, d0) = run_until(&mut enc, &mut dec, Duration::from_secs(15), |_, d| d.frames >= 25);
        let t0 = Instant::now();
        let f0 = d0.frames;
        let mut last = (status(&mut enc).0, d0);
        while t0.elapsed() < Duration::from_secs(6) {
            last = (status(&mut enc).0, status(&mut dec).0);
            std::thread::sleep(Duration::from_millis(200));
        }
        enc.stop();
        dec.stop();
        (last.0.srt.retransmitted, last.1.frames - f0)
    };
    let (plain, frames_plain) = run("");
    let (fec, frames_fec) = run("fec,cols:10,rows:5");
    eprintln!("Wiederholungen ohne FEC {plain}, mit FEC {fec}; Bilder {frames_plain}/{frames_fec}");
    assert!(frames_plain >= 120 && frames_fec >= 120, "{frames_plain}/{frames_fec}");
    assert!(plain >= 20, "Verlust-Proxy wirkt nicht ({plain} Wiederholungen)");
    assert!(fec * 3 < plain, "FEC spart keine Wiederholungen: {fec} gegen {plain}");
}
