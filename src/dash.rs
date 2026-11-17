//! DASH-Manifest über die CMAF-Segmente der HLS-Ausgabe. `dashsink` taugt
//! nicht (keine Sprachkennung je AdaptationSet, eigener mp4-Muxer), darum
//! beschreibt `index.mpd` dieselben Dateien, die `hlscmafsink` schreibt:
//! ein AdaptationSet Bild mit allen ABR-Stufen, je Tonspur eines mit `lang`.
//! Live (`type="dynamic"`), SegmentTemplate mit SegmentTimeline; die Zeiten
//! kommen aus den Segmenten selbst (tfdt, Dauer), die Uhrzeit aus
//! `PROGRAM-DATE-TIME`. Eine Period je Kanalstart.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::EncoderConfig;
use crate::hls::SegmentFormat;

/// Ein Segment im Manifest: Nummer im Dateinamen, Beginn und Dauer in der Zeitskala
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seg {
    pub number: u64,
    pub t: u64,
    pub d: u64,
}

/// Stand einer Rendition, wie er ins Manifest geht
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepView {
    /// `v`, `v1` …, `a0` …
    pub name: String,
    pub init: String,
    /// Dateiname vor der Nummer, z. B. `v_18c2f_`
    pub prefix: String,
    pub timescale: u32,
    pub pto: u64,
    pub segs: Vec<Seg>,
}

fn secs(x: f64) -> String {
    format!("PT{}S", (x * 1000.0).round() / 1000.0)
}

fn utc(unix: f64) -> String {
    chrono::DateTime::from_timestamp_millis((unix * 1000.0).round() as i64)
        .map(|d| d.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_default()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn timeline(segs: &[Seg]) -> String {
    let mut s = String::new();
    let mut i = 0;
    while i < segs.len() {
        let (t, d) = (segs[i].t, segs[i].d);
        let mut r = 0;
        while i + r + 1 < segs.len() && segs[i + r + 1].d == d && segs[i + r + 1].t == t + (r as u64 + 1) * d {
            r += 1;
        }
        s += &format!("<S t=\"{t}\" d=\"{d}\"{}/>", if r > 0 { format!(" r=\"{r}\"") } else { String::new() });
        i += r + 1;
    }
    s
}

fn template(r: &RepView) -> String {
    format!(
        "<SegmentTemplate timescale=\"{}\" presentationTimeOffset=\"{}\" startNumber=\"{}\" initialization=\"{}\" media=\"{}$Number%05d$.m4s\"><SegmentTimeline>{}</SegmentTimeline></SegmentTemplate>",
        r.timescale,
        r.pto,
        r.segs.first().map(|s| s.number).unwrap_or(0),
        esc(&r.init),
        esc(&r.prefix),
        timeline(&r.segs)
    )
}

/// Manifest aus dem Stand der Renditionen. `ast`/`now`: Unix-Sekunden.
pub fn render(cfg: &EncoderConfig, period: &str, ast: f64, now: f64, reps: &[RepView]) -> String {
    let h = &cfg.hls;
    let seg = h.segment_s.max(1) as f64;
    let window = h.playlist_length.max(3) as f64 * seg;
    let mut s = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" profiles=\"urn:mpeg:dash:profile:isoff-live:2011\" type=\"dynamic\" \
         availabilityStartTime=\"{}\" publishTime=\"{}\" minimumUpdatePeriod=\"{}\" minBufferTime=\"{}\" timeShiftBufferDepth=\"{}\" suggestedPresentationDelay=\"{}\">\n",
        utc(ast),
        utc(now),
        secs(seg),
        secs(seg * 2.0),
        secs(window),
        secs(seg * 3.0)
    );
    s += &format!("<Period id=\"{}\" start=\"PT0S\">\n", esc(period));
    let rep = |n: &str| reps.iter().find(|r| r.name == n);
    let main = crate::hls::RungOut { index: 0, width: cfg.video.width, height: cfg.video.height, bitrate_kbps: cfg.video.bitrate_kbps };
    let video: Vec<_> = std::iter::once(main).chain(crate::hls::rungs(cfg)).collect();
    s += &format!(
        "<AdaptationSet id=\"0\" contentType=\"video\" mimeType=\"video/mp4\" segmentAlignment=\"true\" startWithSAP=\"1\" maxWidth=\"{}\" maxHeight=\"{}\" frameRate=\"{}\">\n",
        cfg.video.width, cfg.video.height, cfg.video.fps
    );
    for v in &video {
        let name = if v.index == 0 { "v".to_string() } else { format!("v{}", v.index) };
        let Some(r) = rep(&name) else { continue };
        s += &format!(
            "<Representation id=\"{name}\" bandwidth=\"{}\" width=\"{}\" height=\"{}\" codecs=\"{}\">{}</Representation>\n",
            v.bitrate_kbps as u64 * 1000,
            v.width,
            v.height,
            crate::hls::video_codec(cfg, v.height),
            template(r)
        );
    }
    s += "</AdaptationSet>\n";
    if cfg.audio.enabled {
        for (i, t) in crate::audio::effective_tracks(&cfg.audio.tracks).iter().enumerate() {
            let Some(r) = rep(&format!("a{i}")) else { continue };
            s += &format!(
                "<AdaptationSet id=\"{}\" contentType=\"audio\" mimeType=\"audio/mp4\" lang=\"{}\" segmentAlignment=\"true\" startWithSAP=\"1\">\n\
                 <Label>{}</Label><Role schemeIdUri=\"urn:mpeg:dash:role:2011\" value=\"{}\"/>\n\
                 <Representation id=\"a{i}\" bandwidth=\"{}\" codecs=\"mp4a.40.2\" audioSamplingRate=\"48000\">\
                 <AudioChannelConfiguration schemeIdUri=\"urn:mpeg:dash:23003:3:audio_channel_configuration:2011\" value=\"{}\"/>{}</Representation>\n\
                 </AdaptationSet>\n",
                i + 1,
                crate::audio::language_key(&t.language),
                esc(&t.name),
                if i == 0 { "main" } else { "alternate" },
                t.bitrate_kbps as u64 * 1000,
                if t.layout == crate::audio::Layout::Mono { 1 } else { 2 },
                template(r)
            );
        }
    }
    s += "</Period>\n<UTCTiming schemeIdUri=\"urn:mpeg:dash:utc:direct:2014\" value=\"";
    s += &utc(now);
    s += "\"/>\n</MPD>\n";
    s
}

/// Eintrag einer Medien-Playlist: Segment, Init-Segment, Uhrzeit
#[derive(Debug, Clone, PartialEq)]
struct Item {
    uri: String,
    map: Option<String>,
    pdt: Option<f64>,
}

fn parse_items(text: &str) -> Vec<Item> {
    let (mut out, mut map, mut pdt) = (Vec::new(), None, None);
    for l in text.lines().map(str::trim) {
        if let Some(rest) = l.strip_prefix("#EXT-X-MAP:") {
            map = rest.split("URI=\"").nth(1).and_then(|u| u.split('"').next()).map(str::to_string);
        } else if let Some(v) = l.strip_prefix("#EXT-X-PROGRAM-DATE-TIME:") {
            pdt = chrono::DateTime::parse_from_rfc3339(v).ok().map(|d| d.timestamp_millis() as f64 / 1000.0);
        } else if !l.is_empty() && !l.starts_with('#') {
            out.push(Item { uri: l.to_string(), map: map.clone(), pdt: pdt.take() });
        }
    }
    out
}

/// `v_18c2f_00012.m4s` → (`v_18c2f_`, 12)
fn split_number(uri: &str) -> Option<(String, u64)> {
    let stem = uri.strip_suffix(".m4s")?;
    let pos = stem.rfind('_')? + 1;
    Some((stem[..pos].to_string(), stem[pos..].parse().ok()?))
}

#[derive(Default)]
struct Rep {
    name: String,
    timescales: HashMap<String, u32>,
    times: HashMap<String, (u64, u64)>,
    pto: Option<u64>,
}

/// Schreibt `index.mpd` neben die Playlists, sobald sich etwas ändert
pub struct DashWriter {
    dir: PathBuf,
    cfg: EncoderConfig,
    reps: Vec<Rep>,
    ast: Option<f64>,
    last: Vec<RepView>,
}

/// Nur bei CMAF-Segmenten
pub fn active(cfg: &EncoderConfig) -> bool {
    cfg.hls.enabled && cfg.hls.format == SegmentFormat::Cmaf && !cfg.hls.dir.trim().is_empty()
}

impl DashWriter {
    pub fn new(cfg: &EncoderConfig) -> Self {
        let reps = crate::hls::rendition_names(cfg).into_iter().map(|name| Rep { name, ..Default::default() }).collect();
        Self { dir: PathBuf::from(cfg.hls.dir.trim()), cfg: cfg.clone(), reps, ast: None, last: Vec::new() }
    }

    fn playlist(&self, name: &str) -> PathBuf {
        if self.cfg.hls.generation.is_empty() { self.dir.join(format!("{name}.m3u8")) } else { self.dir.join(format!("{name}.raw.m3u8")) }
    }

    /// Stand einer Rendition aus ihrer Playlist und den Segmenten; `None`, solange nichts vorliegt
    fn view(dir: &Path, rep: &mut Rep, text: &str, ast: &mut Option<f64>) -> Option<RepView> {
        let items = parse_items(text);
        // nur Segmente zum jüngsten Init-Segment
        let map = items.last()?.map.clone()?;
        let items: Vec<&Item> = items.iter().filter(|i| i.map.as_ref() == Some(&map)).collect();
        let ts = match rep.timescales.get(&map) {
            Some(t) => *t,
            None => {
                let t = crate::segtime::timescale(&std::fs::read(dir.join(&map)).ok()?)?;
                rep.timescales.insert(map.clone(), t);
                t
            }
        };
        let mut segs = Vec::new();
        let mut prefix = String::new();
        for it in &items {
            let Some((p, number)) = split_number(&it.uri) else { break };
            let (t, d) = match rep.times.get(&it.uri) {
                Some(x) => *x,
                None => {
                    let Some(x) = std::fs::read(dir.join(&it.uri)).ok().and_then(|b| crate::segtime::fragment(&b)) else { break };
                    rep.times.insert(it.uri.clone(), x);
                    x
                }
            };
            // Nummern müssen lückenlos sein (SegmentTemplate)
            if segs.last().is_some_and(|s: &Seg| s.number + 1 != number) || (!prefix.is_empty() && prefix != p) {
                segs.clear();
            }
            prefix = p;
            // Bezug zur Uhr: einmal je Lauf, aus dem ersten Segment mit Uhrzeit
            if let Some(pdt) = it.pdt {
                let a = *ast.get_or_insert(pdt - t as f64 / ts as f64);
                rep.pto.get_or_insert_with(|| (t as f64 - (pdt - a) * ts as f64).round().max(0.0) as u64);
            }
            segs.push(Seg { number, t, d });
        }
        let live: std::collections::HashSet<&String> = items.iter().map(|i| &i.uri).collect();
        rep.times.retain(|k, _| live.contains(k));
        if segs.is_empty() {
            return None;
        }
        Some(RepView { name: rep.name.clone(), init: map, prefix, timescale: ts, pto: rep.pto?, segs })
    }

    /// Einmal je Durchgang des Fortschreibers aufrufen
    pub fn tick(&mut self) {
        let mut views = Vec::new();
        // Hauptstufe zuerst: sie legt den Bezug zur Uhr fest
        for i in 0..self.reps.len() {
            let Ok(text) = std::fs::read_to_string(self.playlist(&self.reps[i].name)) else { return };
            let Some(v) = Self::view(&self.dir, &mut self.reps[i], &text, &mut self.ast) else { return };
            views.push(v);
        }
        if views == self.last {
            return;
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let period = if self.cfg.hls.generation.is_empty() { "p0" } else { self.cfg.hls.generation.as_str() };
        let text = render(&self.cfg, period, self.ast.unwrap_or(now), now, &views);
        let tmp = self.dir.join("index.mpd.tmp");
        if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, self.dir.join("index.mpd")).is_ok() {
            self.last = views;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segtime::tests::{init, segment};

    fn cfg(dir: &str) -> EncoderConfig {
        use crate::audio::AudioTrack;
        let mut c = EncoderConfig::default();
        c.audio.tracks = vec![
            AudioTrack { name: "Deutsch".into(), language: "deu".into(), ..Default::default() },
            AudioTrack { name: "English".into(), language: "eng".into(), ..Default::default() },
        ];
        c.hls.enabled = true;
        c.hls.format = SegmentFormat::Cmaf;
        c.hls.dir = dir.into();
        c.hls.segment_s = 1;
        c.hls.generation = "g1".into();
        c
    }

    #[test]
    fn timeline_compresses_equal_segments() {
        let s = |number, t, d| Seg { number, t, d };
        assert_eq!(timeline(&[s(3, 0, 25), s(4, 25, 25), s(5, 50, 25), s(6, 80, 20)]), "<S t=\"0\" d=\"25\" r=\"2\"/><S t=\"80\" d=\"20\"/>");
        assert_eq!(split_number("v1_g1_00012.m4s"), Some(("v1_g1_".into(), 12)));
        assert_eq!(split_number("v_init_000.mp4"), None);
    }

    #[test]
    fn mpd_from_cmaf_segments() {
        let dir = std::env::temp_dir().join(format!("lzs-dash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let c = cfg(dir.to_str().unwrap());
        let names = crate::hls::rendition_names(&c);
        assert_eq!(names, ["v", "v1", "v2", "a0", "a1"]);
        for n in &names {
            let (ts, dur, per) = if n.starts_with('a') { (48_000, None, 47) } else { (2500, Some(100), 25) };
            std::fs::write(dir.join(format!("{n}_g1_init_000.mp4")), init(ts)).unwrap();
            let mut pl = format!("#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:1\n#EXT-X-MAP:URI=\"{n}_g1_init_000.mp4\"\n");
            for k in 1..4u64 {
                let t = if n.starts_with('a') { 60_000 + (k - 1) * 47 * 1024 } else { 2500 + (k - 1) * 2500 };
                std::fs::write(dir.join(format!("{n}_g1_{k:05}.m4s")), segment(t, per, dur)).unwrap();
                pl += &format!("#EXT-X-PROGRAM-DATE-TIME:2026-11-17T12:00:0{}.000Z\n#EXTINF:1,\n{n}_g1_{k:05}.m4s\n", k);
            }
            std::fs::write(dir.join(format!("{n}.raw.m3u8")), pl).unwrap();
        }
        let mut w = DashWriter::new(&c);
        w.tick();
        let m = std::fs::read_to_string(dir.join("index.mpd")).expect("index.mpd fehlt");
        // Bild 2500 = 1 s nach Beginn → Uhr 12:00:01 − 1 s
        assert!(m.contains("type=\"dynamic\"") && m.contains("availabilityStartTime=\"2026-11-17T12:00:00.000Z\""), "{m}");
        assert!(m.contains("<Period id=\"g1\""), "{m}");
        assert_eq!(m.matches("<AdaptationSet").count(), 3, "{m}");
        assert_eq!(m.matches("contentType=\"video\"").count(), 1);
        assert!(m.contains("<Representation id=\"v\" bandwidth=\"8000000\" width=\"1920\" height=\"1080\" codecs=\"avc3.640028\">"), "{m}");
        assert!(m.contains("<Representation id=\"v2\"") && m.contains("height=\"480\""), "{m}");
        assert!(m.contains("lang=\"de\"") && m.contains("lang=\"en\"") && m.contains("<Label>English</Label>"), "{m}");
        assert!(m.contains("media=\"v1_g1_$Number%05d$.m4s\"") && m.contains("startNumber=\"1\"") && m.contains("initialization=\"v1_g1_init_000.mp4\""), "{m}");
        assert!(m.contains("timescale=\"2500\" presentationTimeOffset=\"0\"") && m.contains("<S t=\"2500\" d=\"2500\" r=\"2\"/>"), "{m}");
        // Ton: Uhr 12:00:01 ↔ t = 60000 → Versatz 60000 − 1 s × 48000
        assert!(m.contains("timescale=\"48000\" presentationTimeOffset=\"12000\"") && m.contains("<S t=\"60000\" d=\"48128\" r=\"2\"/>"), "{m}");
        let first = m.clone();
        w.tick();
        assert_eq!(std::fs::read_to_string(dir.join("index.mpd")).unwrap(), first, "unverändert: nicht neu schreiben");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
