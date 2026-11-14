//! HLS-Ausgabe (KB-40): je Rendition (Bild, je Tonspur ein Ton) ein Segmentierer,
//! TS (`hlssink3`, Vorgabe) oder CMAF (`hlscmafsink`), dazu eine eigene
//! Master-Playlist. Chrome spielt die CMAF-Segmente von GStreamer 1.28 nicht
//! („Failed to prepare video sample“), TS dagegen schon – darum TS als Vorgabe. Die Untertitel
//! stecken als CEA-608 im Bild und werden als CLOSED-CAPTIONS (CC1, CC3)
//! angemeldet – hls.js und Safari zeigen sie so ohne eigene WebVTT-Segmente.

use serde::{Deserialize, Serialize};

use crate::config::{Codec, EncoderConfig};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct HlsConfig {
    pub enabled: bool,
    /// Zielordner; leer = `hls` im Datenverzeichnis (der Webserver liefert ihn unter /hls/ aus)
    pub dir: String,
    /// Segmentdauer in Sekunden (ein Vielfaches der GOP)
    pub segment_s: u32,
    /// Segmente in der Playlist
    pub playlist_length: u32,
    pub format: SegmentFormat,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SegmentFormat {
    /// MPEG-TS: spielt in Chrome, Safari und hls.js
    #[default]
    Ts,
    /// fragmentiertes MP4 (CMAF): für Player, die fMP4 verlangen
    Cmaf,
}

impl Default for HlsConfig {
    fn default() -> Self {
        Self { enabled: false, dir: String::new(), segment_s: 2, playlist_length: 6, format: SegmentFormat::Ts }
    }
}

/// RFC-6381-Codec-Kennung, aus Codec und gewähltem Profil geschätzt
pub fn codec_string(cfg: &EncoderConfig) -> String {
    let cmaf = cfg.hls.format == SegmentFormat::Cmaf;
    let v = match cfg.video.codec {
        Codec::H264 => {
            let p = match cfg.video.profile.as_str() {
                "baseline" => "42E01F",
                "main" => "4D401F",
                _ if cfg.video.height > 1080 => "640033",
                _ => "640028",
            };
            format!("{}.{p}", if cmaf { "avc3" } else { "avc1" })
        }
        Codec::H265 => {
            let p = if cfg.video.profile == "main-10" { "2.4.L153.B0" } else { "1.6.L153.B0" };
            format!("{}.{p}", if cmaf { "hev1" } else { "hvc1" })
        }
    };
    if cfg.audio.enabled { format!("{v},mp4a.40.2") } else { v }
}

/// Zweistelliger Sprachcode für die Playlist (BCP 47)
fn lang2(code: &str) -> String {
    crate::audio::language_key(code)
}

/// Master-Playlist: Bild mit allen Tonspuren als AUDIO-Gruppe und den 608-Untertiteln
pub fn master_playlist(cfg: &EncoderConfig) -> String {
    let tracks = crate::audio::effective_tracks(&cfg.audio.tracks);
    let mut s = String::from("#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-INDEPENDENT-SEGMENTS\n");
    let audio = cfg.audio.enabled && !tracks.is_empty();
    if audio {
        for (i, t) in tracks.iter().enumerate() {
            s += &format!(
                "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",LANGUAGE=\"{}\",NAME=\"{}\",DEFAULT={},AUTOSELECT=YES,CHANNELS=\"{}\",URI=\"a{i}.m3u8\"\n",
                lang2(&t.language),
                t.name.replace('"', "'"),
                if i == 0 { "YES" } else { "NO" },
                if t.layout == crate::audio::Layout::Mono { 1 } else { 2 }
            );
        }
    }
    let cc = cfg.captions.source != crate::config::CaptionSource::Off;
    if cc {
        let main_lang = tracks.first().map(|t| lang2(&t.language)).unwrap_or_else(|| "de".into());
        s += &format!("#EXT-X-MEDIA:TYPE=CLOSED-CAPTIONS,GROUP-ID=\"cc\",LANGUAGE=\"{main_lang}\",NAME=\"Untertitel\",DEFAULT=YES,AUTOSELECT=YES,INSTREAM-ID=\"CC1\"\n");
        if cfg.captions.second_active() {
            s += &format!(
                "#EXT-X-MEDIA:TYPE=CLOSED-CAPTIONS,GROUP-ID=\"cc\",LANGUAGE=\"{}\",NAME=\"Untertitel 2\",DEFAULT=NO,AUTOSELECT=YES,INSTREAM-ID=\"CC3\"\n",
                lang2(&cfg.captions.second.language)
            );
        }
    }
    let bw = (cfg.video.bitrate_kbps as u64 + if audio { tracks.iter().map(|t| t.bitrate_kbps as u64).sum::<u64>() } else { 0 }) * 1100;
    s += &format!(
        "#EXT-X-STREAM-INF:BANDWIDTH={bw},CODECS=\"{}\",RESOLUTION={}x{},FRAME-RATE={:.3}{}{}\nv.m3u8\n",
        codec_string(cfg),
        cfg.video.width,
        cfg.video.height,
        cfg.video.fps as f64,
        if audio { ",AUDIO=\"aud\"" } else { "" },
        if cc { ",CLOSED-CAPTIONS=\"cc\"" } else { ",CLOSED-CAPTIONS=NONE" }
    );
    s
}

/// Ordner leeren (alte Segmente) und Master-Playlist schreiben
pub fn prepare_dir(cfg: &EncoderConfig) -> std::io::Result<()> {
    let dir = std::path::Path::new(&cfg.hls.dir);
    std::fs::create_dir_all(dir)?;
    for e in std::fs::read_dir(dir)?.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "m4s" || x == "m3u8" || x == "mp4" || x == "ts") {
            let _ = std::fs::remove_file(p);
        }
    }
    std::fs::write(dir.join("index.m3u8"), master_playlist(cfg))
}

/// Pipeline-Zweige: Bild aus dem Tee `ve`, Ton aus `ae<i>`
pub fn branches(cfg: &EncoderConfig) -> Vec<String> {
    let h = &cfg.hls;
    if !h.enabled || h.dir.trim().is_empty() {
        return vec![];
    }
    let dir = h.dir.trim_end_matches('/');
    let (len, seg) = (h.playlist_length.max(3), h.segment_s.max(1));
    let common = |base: &str| {
        format!(
            "playlist-location={} target-duration={seg} playlist-length={len} max-files={} async-handling=true enable-program-date-time=true",
            crate::pipeline::quote(&format!("{dir}/{base}.m3u8")),
            len + 4
        )
    };
    let sink = |name: &str, base: &str| match h.format {
        SegmentFormat::Ts => format!("hlssink3 name={name} location={} {}", crate::pipeline::quote(&format!("{dir}/{base}_%05d.ts")), common(base)),
        SegmentFormat::Cmaf => format!(
            "hlscmafsink name={name} location={} init-location={} {}",
            crate::pipeline::quote(&format!("{dir}/{base}_%05d.m4s")),
            crate::pipeline::quote(&format!("{dir}/{base}_init_%03d.mp4")),
            common(base)
        ),
    };
    let q = "queue max-size-buffers=0 max-size-bytes=0 max-size-time=3000000000";
    let vcaps = match (cfg.video.codec, h.format) {
        (Codec::H264, SegmentFormat::Ts) => "h264parse ! video/x-h264,stream-format=byte-stream,alignment=au",
        (Codec::H265, SegmentFormat::Ts) => "h265parse ! video/x-h265,stream-format=byte-stream,alignment=au",
        (Codec::H264, SegmentFormat::Cmaf) => "h264parse ! video/x-h264,stream-format=avc3,alignment=au",
        (Codec::H265, SegmentFormat::Cmaf) => "h265parse ! video/x-h265,stream-format=hev1,alignment=au",
    };
    let mut out = vec![format!("ve. ! {q} ! {vcaps} ! {}", sink("hlsv", "v"))];
    if cfg.audio.enabled {
        for (i, _) in crate::audio::effective_tracks(&cfg.audio.tracks).iter().enumerate() {
            out.push(format!("ae{i}. ! {q} ! {}", sink(&format!("hlsa{i}"), &format!("a{i}"))));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_lists_audio_and_captions() {
        use crate::audio::AudioTrack;
        let mut c = EncoderConfig::default();
        c.audio.tracks = vec![
            AudioTrack { name: "Deutsch".into(), language: "deu".into(), ..Default::default() },
            AudioTrack { name: "English".into(), language: "eng".into(), ..Default::default() },
        ];
        c.captions.source = crate::config::CaptionSource::Live;
        c.captions.second.enabled = true;
        let m = master_playlist(&c);
        assert!(m.contains("TYPE=AUDIO,GROUP-ID=\"aud\",LANGUAGE=\"de\",NAME=\"Deutsch\",DEFAULT=YES"), "{m}");
        assert!(m.contains("LANGUAGE=\"en\",NAME=\"English\",DEFAULT=NO") && m.contains("URI=\"a1.m3u8\""), "{m}");
        assert!(m.contains("INSTREAM-ID=\"CC1\"") && m.contains("INSTREAM-ID=\"CC3\""), "{m}");
        assert!(m.contains("CODECS=\"avc1.640028,mp4a.40.2\",RESOLUTION=1920x1080") && m.contains("AUDIO=\"aud\",CLOSED-CAPTIONS=\"cc\"\nv.m3u8"), "{m}");
        c.hls.enabled = true;
        c.hls.dir = "/tmp/h".into();
        let b = branches(&c);
        assert_eq!(b.len(), 3);
        assert!(b[0].starts_with("ve. ! ") && b[0].contains("stream-format=byte-stream") && b[0].contains("hlssink3 name=hlsv"));
        assert!(b[2].starts_with("ae1. ! ") && b[2].contains("\"/tmp/h/a1.m3u8\""));
        c.hls.format = SegmentFormat::Cmaf;
        let b = branches(&c);
        assert!(b[0].contains("stream-format=avc3") && b[0].contains("hlscmafsink name=hlsv") && b[0].contains("_init_%03d.mp4"));
        assert!(master_playlist(&c).contains("CODECS=\"avc3.640028"));
        c.hls.dir.clear();
        assert!(branches(&c).is_empty());
    }
}
