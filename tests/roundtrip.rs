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
