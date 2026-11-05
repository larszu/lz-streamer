//! Durchlauf Testquelle → Encoder → SRT → Decoder im selben Prozess, über
//! echte SRT-Verbindungen auf 127.0.0.1.

use std::time::{Duration, Instant};

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
