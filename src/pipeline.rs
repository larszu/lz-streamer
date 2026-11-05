//! Pipeline-Beschreibungen (gst-launch-Syntax). Reine Funktionen, damit sie
//! ohne laufendes GStreamer prüfbar sind.
//!
//! Encoder:
//! ```text
//! Eingang ─► valve(sigv) ─► fallbackswitch(vsw) ─► NV12 ─► tee ─► Encoder ─► Parser ─► tee ─► mpegtsmux ─► srtsink
//! Standby ───────────────►        ▲ sink_1                 └► Vorschau (JPEG)        └► RTMP-Abgriff
//! ```

use crate::config::{Codec, EncoderConfig, OutputKind, OutputType, RecordContainer, ScaleQuality, StandbyPattern};

/// Eingang nach Auflösung von „Automatisch“.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedInput {
    Test,
    Decklink { device: u32, connection: &'static str },
}

impl ResolvedInput {
    pub fn label(&self) -> String {
        match self {
            ResolvedInput::Test => "Testbild (keine Capture-Hardware)".into(),
            ResolvedInput::Decklink { device, connection } => {
                format!("DeckLink/UltraStudio Nr. {device} ({})", connection.to_uppercase())
            }
        }
    }
}

/// Wert für gst-launch in Anführungszeichen setzen.
pub fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn raw_caps(cfg: &EncoderConfig) -> String {
    let v = &cfg.video;
    format!(
        "video/x-raw,format=NV12,width={},height={},framerate={}/1,pixel-aspect-ratio=1/1,interlace-mode=progressive",
        v.width, v.height, v.fps
    )
}


fn threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8)
}

/// Skalierer für Hoch-/Herunterskalieren (z. B. HD-Eingang auf UHD-Ausgang).
/// Mehrere Threads sind Pflicht: einthreadig schafft videoscale HD→UHD am
/// M-Mac nur ≈ 25 fps, mit 8 Threads 50 fps auch mit Lanczos.
pub fn scaler(cfg: &EncoderConfig) -> String {
    // Lanczos mit Hüllkurve 3 statt 2: gemessen +0,3 dB PSNR-Y und +0,8 VMAF-NEG
    // bei +1,8 ms je UHD-Bild (docs/skalierung.md)
    let method = match cfg.video.scale_quality {
        ScaleQuality::Fast => "method=bilinear",
        ScaleQuality::High => "method=lanczos envelope=3",
    };
    let n = threads();
    format!("videoconvert n-threads={n} ! videoscale n-threads={n} {method} add-borders=true")
}

/// Das Testbild entsteht wie eine HD-Kamera höchstens in 1920×1080 und wird
/// für größere Ausgänge hochskaliert – damit läuft derselbe Weg wie beim
/// HD-Eingang auf einen UHD-Stream.
pub fn test_source_size(cfg: &EncoderConfig) -> (u32, u32) {
    let v = &cfg.video;
    if v.width > 1920 || v.height > 1080 { (1920, 1080) } else { (v.width, v.height) }
}

/// `videocrop` für den Beschnitt, leer ohne Beschnitt.
pub fn crop(cfg: &EncoderConfig) -> String {
    let [t, b, l, r] = cfg.input.crop;
    if t + b + l + r == 0 { String::new() } else { format!(" ! videocrop top={t} bottom={b} left={l} right={r}") }
}

pub fn primary_video(cfg: &EncoderConfig, input: &ResolvedInput) -> String {
    let caps = raw_caps(cfg);
    let crop = crop(cfg);
    match input {
        ResolvedInput::Test => {
            let (w, h) = test_source_size(cfg);
            let src_caps = format!("video/x-raw,format=NV12,width={w},height={h},framerate={}/1,pixel-aspect-ratio=1/1", cfg.video.fps);
            let scale = if (w, h) == (cfg.video.width, cfg.video.height) && crop.is_empty() {
                String::new()
            } else {
                format!("{crop} ! {}", scaler(cfg))
            };
            format!(
                "videotestsrc name=testsrc is-live=true pattern=smpte ! {src_caps} \
                 ! timeoverlay halignment=right valignment=bottom font-desc={}{scale} ! {caps}",
                quote("Sans 28")
            )
        }
        ResolvedInput::Decklink { device, connection } => format!(
            "decklinkvideosrc name=capsrc device-number={device} connection={connection} mode=auto \
             drop-no-signal-frames=true ! deinterlace{crop} ! {} \
             ! videorate max-duplication-time=100000000 ! {caps}",
            scaler(cfg)
        ),
    }
}

pub fn standby_video(cfg: &EncoderConfig, image_ok: bool) -> String {
    let caps = raw_caps(cfg);
    let s = &cfg.standby;
    let text = if s.text.trim().is_empty() {
        String::new()
    } else {
        format!(
            " ! textoverlay text={} valignment=center halignment=center font-desc={} shaded-background=true",
            quote(&s.text),
            quote("Sans Bold 40")
        )
    };
    match (s.pattern, image_ok) {
        (StandbyPattern::Image, true) => format!(
            "filesrc location={} ! decodebin ! videoconvert ! videoscale add-borders=true \
             ! video/x-raw,width={},height={},pixel-aspect-ratio=1/1 ! imagefreeze is-live=true \
             ! video/x-raw,framerate={}/1 ! videoconvert ! {caps}",
            quote(&s.image_path),
            cfg.video.width,
            cfg.video.height,
            cfg.video.fps
        ),
        (StandbyPattern::Black, _) => format!("videotestsrc is-live=true pattern=black ! {caps}{text}"),
        _ => format!("videotestsrc is-live=true pattern=smpte ! {caps}{text}"),
    }
}

/// Roh-Ton am Umschalter: N Kanäle ohne Positionen (wie eingebettetes SDI-Audio).
pub fn audio_caps(n: u32) -> String {
    format!("audio/x-raw,format=F32LE,layout=interleaved,rate=48000,channels={n},channel-mask=(bitmask)0x0")
}

pub fn primary_audio(input: &ResolvedInput, n: u32) -> String {
    let caps = audio_caps(n);
    match input {
        // 1 kHz bei -20 dBFS, der übliche Messton, auf allen Kanälen
        ResolvedInput::Test => format!(
            "audiotestsrc name=testasrc is-live=true wave=sine freq=1000 volume=0.1 \
             ! audio/x-raw,format=F32LE,layout=interleaved,rate=48000,channels=1 \
             ! audioconvert mix-matrix={} ! {caps}",
            crate::audio::matrix_string(&vec![vec![1.0f32]; n as usize])
        ),
        ResolvedInput::Decklink { device, .. } => format!(
            "decklinkaudiosrc name=capasrc device-number={device} channels={n} ! audioconvert ! audioresample ! {caps}"
        ),
    }
}

pub struct EncoderParts<'a> {
    pub input: &'a ResolvedInput,
    pub encoder: &'a str,
    pub aac: Option<&'a str>,
    pub image_ok: bool,
    pub rtmp: bool,
    /// Aufnahme-Dateimuster (splitmuxsink-`location`), wenn aufgenommen wird
    pub record: Option<&'a str>,
}

pub fn rtmp_active(cfg: &EncoderConfig) -> bool {
    cfg.rtmp.enabled && !cfg.rtmp.url.trim().is_empty() && cfg.video.codec == Codec::H264
}

/// Komplette Encoder-Pipeline. Encoder-, SRT- und Pad-Eigenschaften werden
/// danach programmatisch gesetzt (siehe `channel.rs`), weil nicht jede
/// GStreamer-Version jede Eigenschaft kennt.
pub fn encoder_description(cfg: &EncoderConfig, parts: &EncoderParts) -> String {
    let timeout_ns = cfg.standby.timeout_ms as u64 * 1_000_000;
    let parser = crate::caps::parser_for(cfg.video.codec);
    let mut d = Vec::new();

    d.push(format!(
        "fallbackswitch name=vsw timeout={timeout_ns} immediate-fallback=true ! videoconvert ! {} ! tee name=vt",
        raw_caps(cfg)
    ));
    d.push(format!(
        "vt. ! queue name=vencq max-size-buffers=3 max-size-bytes=0 max-size-time=0 ! {} name=venc \
         ! {parser} config-interval=-1 ! tee name=ve",
        parts.encoder
    ));
    d.push("ve. ! queue max-size-buffers=0 max-size-bytes=0 max-size-time=2000000000 ! mux.".into());
    if parts.rtmp {
        d.push(
            "ve. ! queue leaky=downstream max-size-buffers=120 max-size-bytes=0 max-size-time=0 \
             ! appsink name=rtmp_v sync=false async=false emit-signals=false max-buffers=120 drop=true"
                .into(),
        );
    }
    d.push(
        "vt. ! queue leaky=downstream max-size-buffers=1 max-size-bytes=0 max-size-time=0 \
         ! videorate drop-only=true max-rate=2 ! videoscale ! videoconvert ! video/x-raw,width=480,pixel-aspect-ratio=1/1 \
         ! jpegenc quality=70 ! appsink name=preview sync=false async=false max-buffers=1 drop=true"
            .into(),
    );
    d.push(format!("{} ! valve name=sigv drop=false ! vsw.sink_0", primary_video(cfg, parts.input)));
    d.push(format!("{} ! vsw.sink_1", standby_video(cfg, parts.image_ok)));

    let tracks = crate::audio::effective_tracks(&cfg.audio.tracks);
    if let (true, Some(aac)) = (cfg.audio.enabled, parts.aac) {
        let n = cfg.audio.input_channels;
        d.push(format!("fallbackswitch name=asw timeout={timeout_ns} immediate-fallback=true ! tee name=ain allow-not-linked=true"));
        for (i, t) in tracks.iter().enumerate() {
            let k = t.layout.channels();
            let out_caps = if k == 2 { "audio/x-raw,channels=2,channel-mask=(bitmask)0x3" } else { "audio/x-raw,channels=1" };
            let mix = crate::audio::matrix_string(&crate::audio::matrix(t, n as usize));
            d.push(format!(
                "ain. ! queue max-size-buffers=0 max-size-bytes=0 max-size-time=1000000000 \
                 ! audioconvert name=amx{i} mix-matrix={mix} ! {out_caps} \
                 ! volume name=avol{i} volume={:.4} mute={} ! audioconvert ! {aac} name=aenc{i} bitrate={} \
                 ! aacparse ! taginject name=atag{i} ! tee name=ae{i}",
                crate::audio::db_to_linear(t.gain_db),
                t.mute,
                t.bitrate_kbps.clamp(32, 512) * 1000
            ));
            d.push(format!("ae{i}. ! queue max-size-buffers=0 max-size-bytes=0 max-size-time=2000000000 ! mux."));
            if parts.rtmp && i == 0 {
                d.push(
                    "ae0. ! queue leaky=downstream max-size-buffers=200 max-size-bytes=0 max-size-time=0 \
                     ! appsink name=rtmp_a sync=false async=false max-buffers=200 drop=true"
                        .into(),
                );
            }
            if parts.record.is_some() {
                d.push(format!("ae{i}. ! queue max-size-buffers=0 max-size-bytes=0 max-size-time=2000000000 ! rec.audio_{i}"));
            }
        }
        d.push(format!("{} ! valve name=siga drop=false ! asw.sink_0", primary_audio(parts.input, n)));
        d.push(format!("audiotestsrc is-live=true wave=silence ! {} ! asw.sink_1", audio_caps(n)));
    }

    if let Some(location) = parts.record {
        // eigener Parser: MP4 braucht avc-Format, der TS-Weg liefert byte-stream
        d.push(format!("ve. ! queue max-size-buffers=0 max-size-bytes=0 max-size-time=2000000000 ! {parser} ! rec.video"));
        let muxer = match cfg.record.container {
            RecordContainer::Ts => "mpegtsmux",
            RecordContainer::Mp4 => "mp4mux",
        };
        let seg = cfg.record.segment_minutes as u64 * 60_000_000_000;
        d.push(format!(
            "splitmuxsink name=rec location={} muxer-factory={muxer} max-size-time={seg} async-handling=true",
            quote(location)
        ));
    }

    d.push(format!(
        "mpegtsmux name=mux alignment=7 pcr-interval=3600 ! tee name=tsout allow-not-linked=true \
         tsout. ! queue max-size-buffers=0 max-size-bytes=0 max-size-time=1000000000 ! srtsink name=srtout uri={} latency={} \
         wait-for-connection=false async=false sync=false",
        quote(&cfg.srt.uri()),
        cfg.srt.latency_ms
    ));
    for (j, o) in cfg.outputs.iter().enumerate().filter(|(_, o)| o.enabled) {
        let q = "tsout. ! queue leaky=downstream max-size-buffers=0 max-size-bytes=0 max-size-time=1000000000";
        d.push(match o.kind {
            OutputType::Srt => format!(
                "{q} ! srtsink name=out{j} uri={} latency={} wait-for-connection=false async=false sync=false",
                quote(&o.srt.uri()),
                o.srt.latency_ms
            ),
            OutputType::Udp => format!("{q} ! udpsink name=out{j} host={} port={} ttl-mc={} sync=false async=false", quote(&o.host), o.port, o.ttl),
            OutputType::Rtp => format!(
                "{q} ! rtpmp2tpay ! udpsink name=out{j} host={} port={} ttl-mc={} sync=false async=false",
                quote(&o.host),
                o.port,
                o.ttl
            ),
        });
    }
    d.join("  ")
}

/// Teil-Pipeline für RTMP: gespeist aus den Abgriffen der Haupt-Pipeline,
/// damit ein RTMP-Abbruch den SRT-Ausgang nicht mitreißt.
pub fn rtmp_description(url: &str, audio: bool) -> String {
    let mut d = vec![
        "appsrc name=v is-live=true format=time do-timestamp=false ! h264parse ! video/x-h264,stream-format=avc \
         ! flvmux name=fm streamable=true latency=100000000 ! rtmp2sink name=rtmpout async=false sync=false location="
            .to_string()
            + &quote(url),
    ];
    if audio {
        d.push("appsrc name=a is-live=true format=time do-timestamp=false ! aacparse ! fm.".into());
    }
    d.join("  ")
}

/// Zweig hinter dem Video-Pad des TS-Demuxers.
///
/// `ts_offset_ns` zieht den bereits in SRT abgelaufenen Puffer vom
/// Ausgabezeitpunkt ab: srtsrc meldet die SRT-Latenz als Pipeline-Latenz,
/// obwohl die Pakete erst nach Ablauf dieses Puffers ankommen. Ohne Ausgleich
/// käme sie am Ausgang ein zweites Mal dazu (gemessen: +120 ms bei 120 ms SRT).
pub fn decoder_video_branch(
    codec: Codec,
    decoder: &str,
    output: OutputKind,
    device: u32,
    deck_mode: &str,
    ts_offset_ns: i64,
) -> String {
    let parser = crate::caps::parser_for(codec);
    let sink = match output {
        OutputKind::Window => format!("autovideosink name=vsink sync=true async=false ts-offset={ts_offset_ns}"),
        OutputKind::Decklink => format!(
            "videoconvert ! decklinkvideosink name=vsink device-number={device} mode={} sync=true async=false ts-offset={ts_offset_ns}",
            if deck_mode.trim().is_empty() { "auto" } else { deck_mode.trim() }
        ),
        OutputKind::None => {
            format!("videoscale ! videoconvert ! video/x-raw,format=GRAY8,width=64,height=36 ! fakesink name=vsink sync=true async=false ts-offset={ts_offset_ns}")
        }
    };
    // libav dekodiert sonst im Frame-Threading und hält dafür mehrere Bilder zurück
    let dec_props = if decoder.starts_with("avdec_") { " thread-type=slice" } else { "" };
    format!(
        "queue name=vq max-size-buffers=0 max-size-bytes=0 max-size-time=1000000000 ! {parser} ! {decoder} name=vdec{dec_props} \
         ! videoconvert name=vconv ! tee name=dt  \
         dt. ! queue max-size-buffers=2 max-size-bytes=0 max-size-time=0 ! {sink}  \
         dt. ! queue leaky=downstream max-size-buffers=1 max-size-bytes=0 max-size-time=0 \
         ! videorate drop-only=true max-rate=2 ! videoscale ! videoconvert ! video/x-raw,width=480,pixel-aspect-ratio=1/1 \
         ! jpegenc quality=70 ! appsink name=preview sync=false async=false max-buffers=1 drop=true"
    )
}

pub fn decoder_audio_branch(output: OutputKind, audio: bool, device: u32, ts_offset_ns: i64) -> String {
    let sink = match (audio, output) {
        (false, _) | (_, OutputKind::None) => "fakesink sync=true async=false".to_string(),
        (true, OutputKind::Window) => format!("autoaudiosink sync=true async=false ts-offset={ts_offset_ns}"),
        (true, OutputKind::Decklink) => format!("decklinkaudiosink device-number={device} sync=true async=false ts-offset={ts_offset_ns}"),
    };
    format!(
        "queue max-size-buffers=0 max-size-bytes=0 max-size-time=1000000000 ! aacparse ! avdec_aac \
         ! audioconvert ! audioresample ! {sink}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;

    fn parts<'a>(input: &'a ResolvedInput) -> EncoderParts<'a> {
        EncoderParts { input, encoder: "x264enc", aac: Some("avenc_aac"), image_ok: false, rtmp: false, record: None }
    }

    #[test]
    fn test_input_pipeline_has_all_stages() {
        let cfg = EncoderConfig::default();
        let d = encoder_description(&cfg, &parts(&ResolvedInput::Test));
        for needle in [
            "videotestsrc name=testsrc",
            "valve name=sigv",
            "fallbackswitch name=vsw",
            "vsw.sink_1",
            "x264enc name=venc",
            "h264parse config-interval=-1",
            "mpegtsmux name=mux alignment=7",
            "srtsink name=srtout uri=\"srt://:9000?mode=listener\" latency=120",
            "fallbackswitch name=asw",
            "avenc_aac name=aenc0 bitrate=192000",
            "appsink name=preview",
            "framerate=50/1",
            "width=1920,height=1080",
        ] {
            assert!(d.contains(needle), "fehlt: {needle}\n{d}");
        }
        assert!(!d.contains("decklink"));
        assert!(!d.contains("rtmp_v"));
    }

    #[test]
    fn decklink_input_and_timeout() {
        let mut cfg = EncoderConfig::default();
        cfg.standby.timeout_ms = 300;
        let input = ResolvedInput::Decklink { device: 1, connection: "sdi" };
        let d = encoder_description(&cfg, &parts(&input));
        assert!(d.contains("decklinkvideosrc name=capsrc device-number=1 connection=sdi mode=auto drop-no-signal-frames=true"));
        assert!(d.contains("decklinkaudiosrc name=capasrc device-number=1"));
        assert!(d.contains("timeout=300000000"));
        assert!(d.contains("max-duplication-time"));
    }

    #[test]
    fn uhd_output_upscales_hd_test_source() {
        let mut cfg = EncoderConfig::default();
        cfg.video.width = 3840;
        cfg.video.height = 2160;
        assert_eq!(test_source_size(&cfg), (1920, 1080));
        let d = primary_video(&cfg, &ResolvedInput::Test);
        assert!(d.contains("width=1920,height=1080"));
        assert!(d.contains("method=lanczos envelope=3"));
        assert!(d.contains("width=3840,height=2160"));
        cfg.video.scale_quality = ScaleQuality::Fast;
        assert!(primary_video(&cfg, &ResolvedInput::Test).contains("method=bilinear"));
        // gleiche Größe: kein Skalierer im Testbildweg
        let cfg = EncoderConfig::default();
        assert!(!primary_video(&cfg, &ResolvedInput::Test).contains("videoscale"));
    }

    #[test]
    fn print_default_description() {
        let cfg = EncoderConfig::default();
        let d = encoder_description(&cfg, &parts(&ResolvedInput::Test));
        if std::env::var("LZS_PRINT").is_ok() {
            println!("{d}");
        }
        crate::latency::ensure_init();
        gstreamer::parse::launch(&d).map(|_| ()).unwrap_or_else(|e| panic!("{e}\n{d}"));
    }

    #[test]
    fn crop_only_when_set() {
        let mut cfg = EncoderConfig::default();
        assert_eq!(crop(&cfg), "");
        cfg.input.crop = [0, 0, 240, 240];
        assert_eq!(crop(&cfg), " ! videocrop top=0 bottom=0 left=240 right=240");
        let d = primary_video(&cfg, &ResolvedInput::Test);
        assert!(d.contains("videocrop") && d.contains("videoscale"));
    }

    #[test]
    fn audio_off_removes_audio_branch() {
        let mut cfg = EncoderConfig::default();
        cfg.audio.enabled = false;
        let d = encoder_description(&cfg, &parts(&ResolvedInput::Test));
        assert!(!d.contains("asw"));
        assert!(!d.contains("aenc"));
    }

    #[test]
    fn h265_uses_h265parse() {
        let mut cfg = EncoderConfig::default();
        cfg.video.codec = Codec::H265;
        let input = ResolvedInput::Test;
        let p = EncoderParts { encoder: "x265enc", ..parts(&input) };
        let d = encoder_description(&cfg, &p);
        assert!(d.contains("x265enc name=venc ! h265parse"));
    }

    #[test]
    fn rtmp_taps_only_when_active() {
        let mut cfg = EncoderConfig::default();
        cfg.rtmp.enabled = true;
        assert!(!rtmp_active(&cfg), "ohne URL kein RTMP");
        cfg.rtmp.url = "rtmp://example.invalid/live/key".into();
        assert!(rtmp_active(&cfg));
        let input = ResolvedInput::Test;
        let p = EncoderParts { rtmp: true, ..parts(&input) };
        let d = encoder_description(&cfg, &p);
        assert!(d.contains("appsink name=rtmp_v") && d.contains("appsink name=rtmp_a"));
        cfg.video.codec = Codec::H265;
        assert!(!rtmp_active(&cfg));
        let r = rtmp_description("rtmp://example.invalid/live/key", true);
        assert!(r.contains("flvmux name=fm") && r.contains("appsrc name=a"));
    }

    #[test]
    fn standby_variants_and_quoting() {
        let mut cfg = EncoderConfig::default();
        cfg.standby.text = "Bitte \"warten\"".into();
        assert!(standby_video(&cfg, false).contains("text=\"Bitte \\\"warten\\\"\""));
        cfg.standby.pattern = StandbyPattern::Black;
        assert!(standby_video(&cfg, false).contains("pattern=black"));
        cfg.standby.pattern = StandbyPattern::Image;
        cfg.standby.image_path = "/tmp/standbild.png".into();
        assert!(standby_video(&cfg, true).contains("imagefreeze is-live=true"));
        // fehlende Datei -> Balken statt Absturz
        assert!(standby_video(&cfg, false).contains("pattern=smpte"));
    }

    #[test]
    fn decoder_branches() {
        let v = decoder_video_branch(Codec::H264, "vtdec_hw", OutputKind::Decklink, 0, "1080p50", -120_000_000);
        assert!(v.contains("ts-offset=-120000000"));
        assert!(v.contains("h264parse ! vtdec_hw name=vdec"));
        assert!(v.contains("decklinkvideosink name=vsink device-number=0 mode=1080p50"));
        let v = decoder_video_branch(Codec::H265, "avdec_h265", OutputKind::None, 0, "", 0);
        assert!(v.contains("thread-type=slice"));
        assert!(v.contains("GRAY8") && v.contains("h265parse"));
        assert!(decoder_audio_branch(OutputKind::Window, false, 0, 0).contains("fakesink"));
        assert!(decoder_audio_branch(OutputKind::Window, true, 0, 0).contains("autoaudiosink"));
    }
}
