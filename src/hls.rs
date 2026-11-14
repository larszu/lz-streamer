//! HLS-Ausgabe (KB-40): je Rendition (Bild, je Tonspur ein Ton) ein Segmentierer,
//! TS (`hlssink3`, Vorgabe) oder CMAF (`hlscmafsink`), dazu eine eigene
//! Master-Playlist. Chrome spielt die CMAF-Segmente von GStreamer 1.28 nicht
//! („Failed to prepare video sample“), TS dagegen schon – darum TS als Vorgabe. Die Untertitel
//! stecken als CEA-608 im Bild und werden als CLOSED-CAPTIONS (CC1, CC3)
//! angemeldet – hls.js und Safari zeigen sie so ohne eigene WebVTT-Segmente.
//!
//! ABR-Leiter (KB-30): weitere Bildstufen aus demselben Rohbild, je Stufe ein
//! eigener Encoder. Damit die Stufen umschaltbar bleiben, erzwingt
//! [`KeyAlign`] die Keyframes auf demselben Bildraster (PTS-Vielfache der GOP).

use std::sync::atomic::{AtomicU64, Ordering};

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
    /// ABR-Leiter: weitere Bildstufen unterhalb der Hauptauflösung
    pub abr: bool,
    /// Stufen der Leiter; leer = Vorschlag nach Hauptauflösung
    pub ladder: Vec<Rung>,
    /// Ziel für HTTP PUT (z. B. `https://ingest.example/live`); leer = nur lokal
    pub upload_url: String,
    /// zusätzliche Kopfzeile beim Hochladen, z. B. `Authorization: Bearer …`
    pub upload_header: String,
    /// herausgefallene Segmente am Ziel per HTTP DELETE löschen
    pub upload_delete: bool,
    /// Kennung des laufenden Kanalstarts (Segmentnamen, Fortschreiben der Playlists); leer = direkt schreiben
    #[serde(skip)]
    pub generation: String,
}

/// Eine Stufe der ABR-Leiter; die Breite folgt dem Seitenverhältnis der Hauptauflösung
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Rung {
    pub height: u32,
    pub bitrate_kbps: u32,
}

/// Wirksame Bildstufe (ohne die Hauptstufe)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RungOut {
    /// Name der Playlist und Elementnummer: `v1`, `v2` …
    pub index: usize,
    pub width: u32,
    pub height: u32,
    pub bitrate_kbps: u32,
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
        Self {
            enabled: false,
            dir: String::new(),
            segment_s: 2,
            playlist_length: 6,
            format: SegmentFormat::Ts,
            abr: true,
            ladder: Vec::new(),
            upload_url: String::new(),
            upload_header: String::new(),
            upload_delete: false,
            generation: String::new(),
        }
    }
}

/// Bitrate einer kleineren Stufe: mit der Bildfläche hoch 0,75 (übliche Faustregel)
fn scaled_bitrate(main_kbps: u32, main_px: u64, px: u64) -> u32 {
    let r = (px as f64 / main_px.max(1) as f64).powf(0.75);
    (((main_kbps as f64 * r) / 100.0).round() as u32 * 100).max(300)
}

/// Vorschlag: zwei Stufen unter der Hauptauflösung (2160 → 1080/720, 1080 → 720/480 …)
pub fn suggested_ladder(cfg: &EncoderConfig) -> Vec<Rung> {
    let (w, h) = (cfg.video.width as u64, cfg.video.height.max(1) as u64);
    [1080u32, 720, 480, 360]
        .into_iter()
        .filter(|&r| (r as u64) < h)
        .take(2)
        .map(|r| {
            let rw = (w * r as u64) / h;
            Rung { height: r, bitrate_kbps: scaled_bitrate(cfg.video.bitrate_kbps, w * h, rw * r as u64) }
        })
        .collect()
}

/// Wirksame Leiter: nur Stufen unter der Hauptauflösung, absteigend, höchstens vier
pub fn rungs(cfg: &EncoderConfig) -> Vec<RungOut> {
    let h = &cfg.hls;
    if !h.enabled || !h.abr {
        return vec![];
    }
    let mut list = if h.ladder.is_empty() { suggested_ladder(cfg) } else { h.ladder.clone() };
    list.retain(|r| r.height >= 144 && r.height < cfg.video.height);
    list.sort_by_key(|r| std::cmp::Reverse(r.height));
    list.dedup_by_key(|r| r.height);
    let (w, hh) = (cfg.video.width as u64, cfg.video.height.max(1) as u64);
    list.into_iter()
        .take(4)
        .enumerate()
        .map(|(i, r)| {
            let width = (((w * r.height as u64) as f64 / hh as f64 / 2.0).round() as u32 * 2).max(2);
            let height = r.height & !1;
            let bitrate_kbps = if r.bitrate_kbps == 0 {
                scaled_bitrate(cfg.video.bitrate_kbps, w * hh, width as u64 * height as u64)
            } else {
                r.bitrate_kbps
            };
            RungOut { index: i + 1, width, height, bitrate_kbps }
        })
        .collect()
}

/// Gemeinsames Keyframe-Raster aller Bild-Encoder eines Kanals. Jeder Encoder
/// sieht dieselben Bilder mit denselben PTS (aus einem Tee); wer die Grenze
/// eines GOP-Rasters überschreitet, bekommt vor dem Bild einen
/// Force-Key-Unit-Befehl. So liegen die Keyframes aller Stufen auf denselben
/// Bildern, unabhängig von Encoder-eigenen Entscheidungen.
#[derive(Debug)]
pub struct KeyAlign {
    gop: u64,
    fps: u64,
    /// angeforderter Keyframe (Bildnummer), 0 = keiner
    request: AtomicU64,
    /// zuletzt gesehene Bildnummer
    last: AtomicU64,
}

/// Zustand je Encoder-Eingang
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyState {
    grid: Option<u64>,
    request: u64,
}

impl KeyAlign {
    pub fn new(gop_frames: u32, fps: u32) -> Self {
        Self { gop: gop_frames.max(1) as u64, fps: fps.max(1) as u64, request: AtomicU64::new(0), last: AtomicU64::new(0) }
    }

    fn frame(&self, pts_ns: u64) -> u64 {
        ((pts_ns as u128 * self.fps as u128 + 500_000_000) / 1_000_000_000) as u64
    }

    /// Soll das Bild mit diesem PTS ein Keyframe werden?
    pub fn should_force(&self, st: &mut KeyState, pts_ns: u64) -> bool {
        let f = self.frame(pts_ns);
        self.last.fetch_max(f, Ordering::Relaxed);
        let grid = f / self.gop;
        let mut key = st.grid != Some(grid);
        st.grid = Some(grid);
        let req = self.request.load(Ordering::Relaxed);
        if req != 0 && f >= req && st.request < req {
            st.request = req;
            key = true;
        }
        key
    }

    /// Außerplanmäßigen Keyframe für alle Stufen anfordern (zwei Bilder Vorlauf)
    pub fn request_now(&self) {
        let at = self.last.load(Ordering::Relaxed) + 2;
        self.request.fetch_max(at, Ordering::Relaxed);
    }
}

/// RFC-6381-Codec-Kennung, aus Codec und gewähltem Profil geschätzt
pub fn codec_string(cfg: &EncoderConfig) -> String {
    codec_string_for(cfg, cfg.video.height)
}

fn codec_string_for(cfg: &EncoderConfig, height: u32) -> String {
    let cmaf = cfg.hls.format == SegmentFormat::Cmaf;
    let v = match cfg.video.codec {
        Codec::H264 => {
            let p = match cfg.video.profile.as_str() {
                "baseline" => "42E01F",
                "main" => "4D401F",
                _ if height > 1080 => "640033",
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
    let audio_kbps = if audio { tracks.iter().map(|t| t.bitrate_kbps as u64).max().unwrap_or(0) } else { 0 };
    let main = RungOut { index: 0, width: cfg.video.width, height: cfg.video.height, bitrate_kbps: cfg.video.bitrate_kbps };
    for r in std::iter::once(main).chain(rungs(cfg)) {
        let uri = if r.index == 0 { "v.m3u8".to_string() } else { format!("v{}.m3u8", r.index) };
        s += &format!(
            "#EXT-X-STREAM-INF:BANDWIDTH={},CODECS=\"{}\",RESOLUTION={}x{},FRAME-RATE={:.3}{}{}\n{uri}\n",
            (r.bitrate_kbps as u64 + audio_kbps) * 1100,
            codec_string_for(cfg, r.height),
            r.width,
            r.height,
            cfg.video.fps as f64,
            if audio { ",AUDIO=\"aud\"" } else { "" },
            if cc { ",CLOSED-CAPTIONS=\"cc\"" } else { ",CLOSED-CAPTIONS=NONE" }
        );
    }
    s
}

/// Basisnamen aller Renditionen: `v`, `v1` …, `a0` …
pub fn rendition_names(cfg: &EncoderConfig) -> Vec<String> {
    let mut n = vec!["v".to_string()];
    n.extend(rungs(cfg).iter().map(|r| format!("v{}", r.index)));
    if cfg.audio.enabled {
        n.extend((0..crate::audio::effective_tracks(&cfg.audio.tracks).len()).map(|i| format!("a{i}")));
    }
    n
}

/// Ordner vorbereiten und Master-Playlist schreiben. Liegt ein frischer Stand
/// des vorigen Laufs vor (jünger als die doppelte Playlist-Dauer plus 30 s),
/// bleiben dessen Playlists und Segmente stehen, damit `hlsplaylist` sie
/// fortschreibt (Rückgabe `true`); sonst wird geleert.
pub fn prepare_dir(cfg: &EncoderConfig) -> std::io::Result<bool> {
    let dir = std::path::Path::new(&cfg.hls.dir);
    std::fs::create_dir_all(dir)?;
    let window = (cfg.hls.playlist_length.max(3) * cfg.hls.segment_s.max(1)) as u64 * 2 + 30;
    let fresh = std::fs::metadata(dir.join("v.m3u8"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs() < window);
    let resume = !cfg.hls.generation.is_empty() && fresh;
    let mut keep = std::collections::HashSet::new();
    if resume {
        for n in rendition_names(cfg) {
            if let Ok(t) = std::fs::read_to_string(dir.join(format!("{n}.m3u8"))) {
                keep.extend(crate::hlsupload::referenced(&t));
                keep.insert(format!("{n}.m3u8"));
            }
        }
    }
    for e in std::fs::read_dir(dir)?.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        let media = p.extension().is_some_and(|x| x == "m4s" || x == "m3u8" || x == "mp4" || x == "ts" || x == "tmp");
        if media && !keep.contains(&name) {
            let _ = std::fs::remove_file(p);
        }
    }
    std::fs::write(dir.join("index.m3u8"), master_playlist(cfg))?;
    Ok(resume)
}

fn active(cfg: &EncoderConfig) -> bool {
    cfg.hls.enabled && !cfg.hls.dir.trim().is_empty()
}

/// Segmentierer für eine Rendition
fn sink(cfg: &EncoderConfig, name: &str, base: &str) -> String {
    let h = &cfg.hls;
    let dir = h.dir.trim_end_matches('/');
    let (len, seg) = (h.playlist_length.max(3), h.segment_s.max(1));
    // Mit Kennung: Rohplaylist für den Fortschreiber, Segmentnamen je Lauf eindeutig.
    // Kein ENDLIST beim Anhalten: der Kanal ist live und kann weiterlaufen.
    let (list, stem) = if h.generation.is_empty() {
        (format!("{dir}/{base}.m3u8"), format!("{dir}/{base}"))
    } else {
        (format!("{dir}/{base}.raw.m3u8"), format!("{dir}/{base}_{}", h.generation))
    };
    let common = format!(
        "playlist-location={} target-duration={seg} playlist-length={len} max-files={} async-handling=true enable-program-date-time=true enable-endlist=false",
        crate::pipeline::quote(&list),
        len + 4
    );
    match h.format {
        SegmentFormat::Ts => format!("hlssink3 name={name} location={} {common}", crate::pipeline::quote(&format!("{stem}_%05d.ts"))),
        SegmentFormat::Cmaf => format!(
            "hlscmafsink name={name} location={} init-location={} {common}",
            crate::pipeline::quote(&format!("{stem}_%05d.m4s")),
            crate::pipeline::quote(&format!("{stem}_init_%03d.mp4")),
        ),
    }
}

/// Parser und Caps vor dem Segmentierer
fn video_caps(cfg: &EncoderConfig) -> &'static str {
    match (cfg.video.codec, cfg.hls.format) {
        (Codec::H264, SegmentFormat::Ts) => "h264parse ! video/x-h264,stream-format=byte-stream,alignment=au",
        (Codec::H265, SegmentFormat::Ts) => "h265parse ! video/x-h265,stream-format=byte-stream,alignment=au",
        (Codec::H264, SegmentFormat::Cmaf) => "h264parse ! video/x-h264,stream-format=avc3,alignment=au",
        (Codec::H265, SegmentFormat::Cmaf) => "h265parse ! video/x-h265,stream-format=hev1,alignment=au",
    }
}

const QUEUE: &str = "queue max-size-buffers=0 max-size-bytes=0 max-size-time=3000000000";

/// Pipeline-Zweige: Bild aus dem Tee `ve`, Ton aus `ae<i>`
pub fn branches(cfg: &EncoderConfig) -> Vec<String> {
    if !active(cfg) {
        return vec![];
    }
    let mut out = vec![format!("ve. ! {QUEUE} ! {} ! {}", video_caps(cfg), sink(cfg, "hlsv", "v"))];
    if cfg.audio.enabled {
        for (i, _) in crate::audio::effective_tracks(&cfg.audio.tracks).iter().enumerate() {
            out.push(format!("ae{i}. ! {QUEUE} ! {}", sink(cfg, &format!("hlsa{i}"), &format!("a{i}"))));
        }
    }
    out
}

/// Zweige der weiteren Bildstufen: aus dem Roh-Tee `src` skalieren und mit
/// eigenem Encoder `venc_r<i>` codieren. `tail(i)` folgt dem Parser (z. B. der
/// 608-Einfüger), damit jede Stufe die Untertitel trägt.
pub fn rung_branches(cfg: &EncoderConfig, src: &str, encoder: &str, tail: &dyn Fn(usize) -> String) -> Vec<String> {
    if !active(cfg) {
        return vec![];
    }
    let parser = crate::caps::parser_for(cfg.video.codec);
    let pcaps = crate::caps::profile_caps(&cfg.video);
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 4);
    rungs(cfg)
        .into_iter()
        .map(|r| {
            let i = r.index;
            format!(
                "{src}. ! queue name=vrq{i} max-size-buffers=3 max-size-bytes=0 max-size-time=0 \
                 ! videoscale n-threads={n} method=bilinear ! video/x-raw,width={},height={},pixel-aspect-ratio=1/1 \
                 ! {encoder} name=venc_r{i} ! {pcaps}{parser} config-interval=-1{} ! {QUEUE} ! {} ! {}",
                r.width,
                r.height,
                tail(i),
                video_caps(cfg),
                sink(cfg, &format!("hlsv{i}"), &format!("v{i}"))
            )
        })
        .collect()
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

    #[test]
    fn ladder_follows_main_resolution() {
        let mut c = EncoderConfig::default();
        c.hls.enabled = true;
        c.hls.dir = "/tmp/h".into();
        c.video.bitrate_kbps = 8000;
        let r = rungs(&c);
        assert_eq!(r.iter().map(|r| (r.index, r.width, r.height)).collect::<Vec<_>>(), vec![(1, 1280, 720), (2, 854, 480)]);
        assert!(r[0].bitrate_kbps > r[1].bitrate_kbps && r[0].bitrate_kbps < 8000, "{r:?}");
        c.video.width = 3840;
        c.video.height = 2160;
        assert_eq!(rungs(&c).iter().map(|r| r.height).collect::<Vec<_>>(), vec![1080, 720]);
        // eigene Stufen: zu große fallen weg, Reihenfolge absteigend, ohne Bitrate geschätzt
        c.hls.ladder = vec![Rung { height: 360, bitrate_kbps: 800 }, Rung { height: 2160, bitrate_kbps: 1 }, Rung { height: 1440, bitrate_kbps: 0 }];
        let r = rungs(&c);
        assert_eq!(r.iter().map(|r| (r.width, r.height, r.index)).collect::<Vec<_>>(), vec![(2560, 1440, 1), (640, 360, 2)]);
        assert!(r[0].bitrate_kbps > 300 && r[1].bitrate_kbps == 800);
        let m = master_playlist(&c);
        assert_eq!(m.matches("#EXT-X-STREAM-INF").count(), 3, "{m}");
        assert!(m.contains("RESOLUTION=2560x1440") && m.contains("\nv1.m3u8\n") && m.contains("\nv2.m3u8\n"), "{m}");
        let b = rung_branches(&c, "vt", "vtenc_h264_hw", &|i| format!(" ! x name=ccins_r{i}"));
        assert_eq!(b.len(), 2);
        assert!(b[1].starts_with("vt. ! queue name=vrq2") && b[1].contains("width=640,height=360") && b[1].contains("vtenc_h264_hw name=venc_r2"));
        assert!(b[1].contains("ccins_r2") && b[1].contains("hlssink3 name=hlsv2") && b[1].contains("\"/tmp/h/v2.m3u8\""), "{}", b[1]);
        c.hls.abr = false;
        assert!(rungs(&c).is_empty() && master_playlist(&c).matches("STREAM-INF").count() == 1);
    }

    #[test]
    fn keyframes_on_common_grid() {
        // 25 fps, GOP 25: Keyframes bei Bild 0, 25, 50 – für jeden Encoder gleich
        let a = KeyAlign::new(25, 25);
        let ns = |f: u64| f * 40_000_000 + 3; // kleine PTS-Unschärfe
        let keys = |a: &KeyAlign, st: &mut KeyState, from: u64, to: u64| (from..to).filter(|&f| a.should_force(st, ns(f))).collect::<Vec<_>>();
        let (mut s1, mut s2) = (KeyState::default(), KeyState::default());
        assert_eq!(keys(&a, &mut s1, 10, 60), vec![10, 25, 50]);
        assert_eq!(keys(&a, &mut s2, 10, 60), vec![10, 25, 50]);
        // außerplanmäßig: zwei Bilder nach dem zuletzt gesehenen, für beide gleich
        a.request_now();
        assert_eq!(keys(&a, &mut s1, 60, 80), vec![61, 75]);
        assert_eq!(keys(&a, &mut s2, 60, 80), vec![61, 75]);
        // fehlendes Bild an der Rastergrenze: das nächste wird Keyframe
        let mut s3 = KeyState::default();
        let f: Vec<u64> = [98u64, 99, 101, 102].into_iter().filter(|&f| a.should_force(&mut s3, ns(f))).collect();
        assert_eq!(f, vec![98, 101]);
    }
}
