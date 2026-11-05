//! Was dieser Rechner kann: Encoder, Decoder, Ein- und Ausgänge.
//! Hardware zuerst, Software als Rückfallebene.

use crate::config::Codec;
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct EncoderInfo {
    pub factory: &'static str,
    pub label: &'static str,
    pub hardware: bool,
}

const fn enc(factory: &'static str, label: &'static str, hardware: bool) -> EncoderInfo {
    EncoderInfo { factory, label, hardware }
}

/// Reihenfolge = Vorrang bei „Automatisch“.
pub const H264_ENCODERS: &[EncoderInfo] = &[
    enc("vtenc_h264_hw", "Apple VideoToolbox (Hardware)", true),
    enc("nvh264enc", "NVIDIA NVENC", true),
    enc("vah264enc", "VA-API (Intel/AMD)", true),
    enc("qsvh264enc", "Intel Quick Sync", true),
    enc("vaapih264enc", "VA-API (alt)", true),
    enc("x264enc", "x264 (Software)", false),
];

pub const H265_ENCODERS: &[EncoderInfo] = &[
    enc("vtenc_h265_hw", "Apple VideoToolbox (Hardware)", true),
    enc("nvh265enc", "NVIDIA NVENC", true),
    enc("vah265enc", "VA-API (Intel/AMD)", true),
    enc("qsvh265enc", "Intel Quick Sync", true),
    enc("vaapih265enc", "VA-API (alt)", true),
    enc("x265enc", "x265 (Software)", false),
];

pub fn encoders(codec: Codec) -> &'static [EncoderInfo] {
    match codec {
        Codec::H264 => H264_ENCODERS,
        Codec::H265 => H265_ENCODERS,
    }
}

pub fn encoder_info(factory: &str) -> Option<EncoderInfo> {
    H264_ENCODERS.iter().chain(H265_ENCODERS).find(|e| e.factory == factory).copied()
}

pub fn element_available(factory: &str) -> bool {
    gstreamer::ElementFactory::find(factory).is_some()
}

/// Kandidaten in Versuchsreihenfolge. Ein ausdrücklich gewählter Encoder
/// kommt zuerst, die übrigen bleiben als Rückfallebene dahinter.
pub fn encoder_candidates_with(codec: Codec, choice: &str, available: impl Fn(&str) -> bool) -> Vec<EncoderInfo> {
    let all = encoders(codec);
    let mut out: Vec<EncoderInfo> = Vec::new();
    if choice != "auto" {
        if let Some(e) = all.iter().find(|e| e.factory == choice) {
            if available(e.factory) {
                out.push(*e);
            }
        }
    }
    for e in all {
        if !out.contains(e) && available(e.factory) {
            out.push(*e);
        }
    }
    out
}

pub fn encoder_candidates(codec: Codec, choice: &str) -> Vec<EncoderInfo> {
    encoder_candidates_with(codec, choice, element_available)
}

/// Latenzarme CBR-Voreinstellungen je Encoder: keine B-Frames, kurzes GOP,
/// kein Lookahead. Eigenschaften, die ein Element (in einer anderen
/// GStreamer-Version) nicht kennt, werden beim Setzen übersprungen.
pub fn encoder_props(factory: &str, bitrate_kbps: u32, gop_frames: u32) -> Vec<(&'static str, String)> {
    let b = bitrate_kbps.to_string();
    let g = gop_frames.to_string();
    let p = |k: &'static str, v: &str| (k, v.to_string());
    match factory {
        "vtenc_h264_hw" | "vtenc_h265_hw" | "vtenc_h264" | "vtenc_h265" => vec![
            p("realtime", "true"),
            p("allow-frame-reordering", "false"),
            p("rate-control", "cbr"),
            p("bitrate", &b),
            p("max-keyframe-interval", &g),
        ],
        "nvh264enc" | "nvh265enc" => vec![
            p("preset", "p2"),
            p("tune", "ultra-low-latency"),
            p("rc-mode", "cbr"),
            p("bitrate", &b),
            p("gop-size", &g),
            p("bframes", "0"),
            p("zerolatency", "true"),
            p("rc-lookahead", "0"),
        ],
        "vah264enc" | "vah265enc" => vec![
            p("rate-control", "cbr"),
            p("bitrate", &b),
            p("key-int-max", &g),
            p("b-frames", "0"),
            p("target-usage", "6"),
        ],
        "vaapih264enc" | "vaapih265enc" => vec![
            p("rate-control", "cbr"),
            p("bitrate", &b),
            p("keyframe-period", &g),
            p("max-bframes", "0"),
        ],
        "qsvh264enc" | "qsvh265enc" => vec![
            p("rate-control", "cbr"),
            p("bitrate", &b),
            p("gop-size", &g),
            p("b-frames", "0"),
            p("target-usage", "7"),
        ],
        "x264enc" => vec![
            p("tune", "zerolatency"),
            p("speed-preset", "veryfast"),
            p("bitrate", &b),
            p("key-int-max", &g),
            p("bframes", "0"),
            p("vbv-buf-capacity", "500"),
            p("option-string", "nal-hrd=cbr:force-cfr=1"),
        ],
        "x265enc" => vec![
            p("tune", "zerolatency"),
            p("speed-preset", "veryfast"),
            p("bitrate", &b),
            p("key-int-max", &g),
            p("option-string", "bframes=0"),
        ],
        _ => vec![p("bitrate", &b)],
    }
}

/// Zusätzliche Encoder-Einstellungen aus den Encoder-Optionen (nach `encoder_props` setzen).
pub fn option_props(factory: &str, v: &crate::config::VideoConfig) -> Vec<(&'static str, String)> {
    use crate::config::Entropy;
    let b = v.bframes.to_string();
    let p = |k: &'static str, v: String| (k, v);
    let mut out = Vec::new();
    if v.bframes > 0 {
        match factory {
            "vtenc_h264_hw" | "vtenc_h265_hw" | "vtenc_h264" | "vtenc_h265" => out.push(p("allow-frame-reordering", "true".into())),
            "nvh264enc" | "nvh265enc" => out.push(p("bframes", b.clone())),
            "vah264enc" | "vah265enc" | "qsvh264enc" | "qsvh265enc" => out.push(p("b-frames", b.clone())),
            "vaapih264enc" | "vaapih265enc" => out.push(p("max-bframes", b.clone())),
            "x264enc" => out.push(p("bframes", b.clone())),
            "x265enc" => out.push(p("option-string", format!("bframes={b}"))),
            _ => {}
        }
    }
    if factory == "x264enc" {
        match v.entropy {
            Entropy::Cabac => out.push(p("cabac", "true".into())),
            Entropy::Cavlc => out.push(p("cabac", "false".into())),
            Entropy::Auto => {}
        }
        if v.interlaced {
            out.push(p("interlaced", "true".into()));
        }
        // zerolatency schaltet B-Frames ab; mit B-Frames nur die übrigen Latenz-Einstellungen
        if v.bframes > 0 {
            out.push(p("tune", "fastdecode".into()));
        }
    }
    out
}

/// Freie Zusatzparameter `name=wert name=wert` zerlegen.
pub fn extra_props(s: &str) -> Vec<(String, String)> {
    s.split_whitespace().filter_map(|kv| kv.split_once('=')).filter(|(k, _)| !k.is_empty()).map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// Caps hinter dem Encoder für Profil und Level (leer, wenn nichts gewählt).
pub fn profile_caps(v: &crate::config::VideoConfig) -> String {
    let mut f = Vec::new();
    if !v.profile.is_empty() {
        f.push(format!("profile={}", v.profile));
    }
    if !v.level.is_empty() {
        f.push(format!("level=(string){}", v.level));
    }
    if f.is_empty() {
        return String::new();
    }
    let media = match v.codec {
        Codec::H264 => "video/x-h264",
        Codec::H265 => "video/x-h265",
    };
    format!("{media},{} ! ", f.join(","))
}

pub fn parser_for(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "h264parse",
        Codec::H265 => "h265parse",
    }
}

pub fn decoder_candidates(codec: Codec) -> Vec<&'static str> {
    let list: &[&str] = match codec {
        // vtdec_hw hält bei Strömen ohne VUI-Angabe zur Bildumordnung mehrere
        // Bilder zurück (gemessen ≈ +100 ms gegenüber avdec), daher für H.264
        // zuerst libav im Slice-Threading. H.265 in UHD schafft libav am Mac
        // nicht in Echtzeit (gemessen 8–12 fps bei 2160p25), dort Hardware zuerst.
        Codec::H264 => &["nvh264dec", "vah264dec", "qsvh264dec", "avdec_h264", "vtdec_hw"],
        Codec::H265 => &["vtdec_hw", "nvh265dec", "vah265dec", "qsvh265dec", "avdec_h265"],
    };
    list.iter().copied().filter(|f| element_available(f)).collect()
}

/// AAC-Encoder und sein Bitraten-Wert (alle drei erwarten bit/s).
pub fn aac_encoder() -> Option<&'static str> {
    ["fdkaacenc", "avenc_aac", "faac"].into_iter().find(|f| element_available(f))
}

#[derive(Debug, Clone, Serialize)]
pub struct Capabilities {
    pub platform: &'static str,
    pub h264: Vec<EncoderInfo>,
    pub h265: Vec<EncoderInfo>,
    pub srt: bool,
    pub rtmp: bool,
    pub decklink_plugin: bool,
    pub standby_switch: bool,
    pub aac: Option<&'static str>,
    pub gstreamer: String,
}

impl Capabilities {
    pub fn probe() -> Self {
        Self {
            platform: std::env::consts::OS,
            h264: encoder_candidates(Codec::H264, "auto"),
            h265: encoder_candidates(Codec::H265, "auto"),
            srt: element_available("srtsink") && element_available("srtsrc"),
            rtmp: element_available("rtmp2sink") && element_available("flvmux"),
            decklink_plugin: element_available("decklinkvideosrc"),
            standby_switch: element_available("fallbackswitch"),
            aac: aac_encoder(),
            gstreamer: gstreamer::version_string().to_string(),
        }
    }

    /// Fehlende Pflichtbausteine als lesbare Meldungen.
    pub fn missing(&self) -> Vec<String> {
        let mut m = Vec::new();
        if !self.srt {
            m.push("GStreamer-SRT-Elemente fehlen (srtsink/srtsrc, Paket gst-plugins-bad mit libsrt).".into());
        }
        if !self.standby_switch {
            m.push("fallbackswitch fehlt (gst-plugins-rs). Ohne ihn gibt es kein Standbild bei Signalverlust.".into());
        }
        if self.h264.is_empty() {
            m.push("Kein H.264-Encoder gefunden (z. B. x264enc aus gst-plugins-ugly).".into());
        }
        if self.aac.is_none() {
            m.push("Kein AAC-Encoder gefunden (avenc_aac aus gst-libav) – Ton bleibt aus.".into());
        }
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_prefers_hardware_order() {
        let c = encoder_candidates_with(Codec::H264, "auto", |f| f == "x264enc" || f == "nvh264enc");
        assert_eq!(c.iter().map(|e| e.factory).collect::<Vec<_>>(), vec!["nvh264enc", "x264enc"]);
    }

    #[test]
    fn explicit_choice_first_rest_as_fallback() {
        let c = encoder_candidates_with(Codec::H264, "x264enc", |_| true);
        assert_eq!(c[0].factory, "x264enc");
        assert_eq!(c.len(), H264_ENCODERS.len());
    }

    #[test]
    fn unavailable_choice_falls_back() {
        let c = encoder_candidates_with(Codec::H265, "nvh265enc", |f| f == "x265enc");
        assert_eq!(c.iter().map(|e| e.factory).collect::<Vec<_>>(), vec!["x265enc"]);
    }

    #[test]
    fn every_encoder_gets_low_latency_cbr_props() {
        for e in H264_ENCODERS.iter().chain(H265_ENCODERS) {
            let props = encoder_props(e.factory, 6000, 50);
            assert!(props.iter().any(|(k, v)| k.contains("bitrate") && v == "6000"), "{}", e.factory);
            let no_b = props.iter().any(|(k, v)| {
                (*k == "bframes" || *k == "b-frames" || *k == "max-bframes") && v == "0"
                    || *k == "allow-frame-reordering" && v == "false"
                    || *k == "option-string" && v.contains("bframes=0")
            });
            assert!(no_b, "{} ohne B-Frame-Sperre", e.factory);
        }
    }

    #[test]
    fn x264_is_cbr_zerolatency() {
        let mut v = crate::config::VideoConfig { bframes: 2, entropy: crate::config::Entropy::Cavlc, interlaced: true, profile: "high".into(), level: "4.1".into(), ..Default::default() };
        let o = option_props("x264enc", &v);
        assert!(o.contains(&("bframes", "2".into())) && o.contains(&("cabac", "false".into())) && o.contains(&("interlaced", "true".into())));
        assert_eq!(option_props("vtenc_h264_hw", &v), vec![("allow-frame-reordering", "true".to_string())]);
        assert_eq!(profile_caps(&v), "video/x-h264,profile=high,level=(string)4.1 ! ");
        v.codec = Codec::H265;
        v.level.clear();
        v.profile = "main-10".into();
        assert_eq!(profile_caps(&v), "video/x-h265,profile=main-10 ! ");
        assert!(option_props("x264enc", &crate::config::VideoConfig::default()).is_empty());
        assert_eq!(extra_props(" threads=4  sliced-threads=true kaputt =x"), vec![("threads".into(), "4".into()), ("sliced-threads".into(), "true".into())]);
        let p = encoder_props("x264enc", 8000, 25);
        assert!(p.contains(&("tune", "zerolatency".into())));
        assert!(p.iter().any(|(k, v)| *k == "option-string" && v.contains("nal-hrd=cbr")));
        assert!(p.contains(&("key-int-max", "25".into())));
    }
}
