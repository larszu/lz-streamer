//! WebVTT-Untertitel als HLS-Rendition (TYPE=SUBTITLES), je Sprache eine.
//! Die Texte sind dieselben Zeilen, die `cea608tojson` aus dem gesendeten
//! 608-Strom liest (wie für `.srt`/`.vtt` neben der Aufnahme), mit Laufzeit.
//!
//! Die Segmente folgen den Bildsegmenten: zu jedem fertigen Bildsegment ein
//! `.vtt` mit gleicher Dauer und Uhrzeit, sobald das nächste Bildsegment
//! vorliegt (bis dahin kann sich das Ende einer Zeile noch ändern). Eine Zeile
//! steht in dem Segment, in dem sie beginnt. Die Zeiten in den Dateien sind
//! Laufzeit; `X-TIMESTAMP-MAP` bildet Laufzeit 0 auf die PTS der Segmente ab
//! (`mpegtsmux` beginnt bei 3600 s, CMAF bei 0).

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;

use crate::config::EncoderConfig;
use crate::hls::SegmentFormat;
use crate::subtitles::{Cue, CueStore};

/// PTS (90 kHz) zur Laufzeit 0
pub fn pts_offset(format: SegmentFormat) -> u64 {
    match format {
        SegmentFormat::Ts => 3600 * 90_000,
        SegmentFormat::Cmaf => 0,
    }
}

fn ts(ns: u64) -> String {
    let ms = ns / 1_000_000;
    format!("{:02}:{:02}:{:02}.{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// Inhalt eines Segments: alle Zeilen, die in `[start, end)` beginnen
pub fn segment(cues: &[Cue], start_ns: u64, end_ns: u64, offset: u64) -> String {
    let mut s = format!("WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:{offset},LOCAL:00:00:00.000\n\n");
    for c in cues.iter().filter(|c| c.start_ns >= start_ns && c.start_ns < end_ns) {
        s += &format!("{} --> {}\n{}\n\n", ts(c.start_ns), ts(c.end_ns.max(c.start_ns + 1_000_000)), c.text.replace("-->", "->"));
    }
    s
}

/// Eintrag der Bild-Playlist
#[derive(Debug, Clone)]
struct Item {
    uri: String,
    tags: Vec<String>,
}

fn parse_items(text: &str) -> Vec<Item> {
    let (mut out, mut tags) = (Vec::new(), Vec::new());
    for l in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if l.starts_with("#EXTINF") || l.starts_with("#EXT-X-PROGRAM-DATE-TIME") {
            tags.push(l.to_string());
        } else if !l.starts_with('#') {
            out.push(Item { uri: l.to_string(), tags: std::mem::take(&mut tags) });
        }
    }
    out
}

/// Eine Sprache: Zeilenquelle und eigene Playlist
pub struct Language {
    /// Basisname der Rendition: `s0`, `s1`
    pub name: String,
    pub cues: CueStore,
}

struct Track {
    lang: Language,
    /// eigene Kopie der Zeilen, nach Beginn (die Quelle wird anderswo gekürzt)
    seen: BTreeMap<u64, Cue>,
    entries: VecDeque<(Vec<String>, String)>,
    seq: u64,
}

pub struct VttWriter {
    dir: PathBuf,
    video: String,
    generation: String,
    format: SegmentFormat,
    target: u32,
    keep: usize,
    tracks: Vec<Track>,
    /// Laufzeit des Beginns je Bildsegment
    starts: std::collections::HashMap<String, u64>,
    done: std::collections::HashSet<String>,
}

/// Basisnamen der Untertitel-Renditionen: `s0`, bei zweiter Sprache `s1`
pub fn names(cfg: &EncoderConfig) -> Vec<String> {
    if !active(cfg) {
        return vec![];
    }
    let mut n = vec!["s0".to_string()];
    if cfg.captions.second_active() {
        n.push("s1".into());
    }
    n
}

/// Nur mit HLS und einer Untertitelquelle, deren Text der Encoder selbst erzeugt
pub fn active(cfg: &EncoderConfig) -> bool {
    cfg.hls.enabled && !cfg.hls.dir.trim().is_empty() && crate::pipeline::caption_branch(cfg).is_some()
}

impl VttWriter {
    pub fn new(cfg: &EncoderConfig, langs: Vec<Language>) -> Self {
        let raw = |n: &str| if cfg.hls.generation.is_empty() { format!("{n}.m3u8") } else { format!("{n}.raw.m3u8") };
        Self {
            dir: PathBuf::from(cfg.hls.dir.trim()),
            video: raw("v"),
            generation: cfg.hls.generation.clone(),
            format: cfg.hls.format,
            target: cfg.hls.segment_s.max(1),
            keep: cfg.hls.playlist_length.max(3) as usize,
            tracks: langs.into_iter().map(|lang| Track { lang, seen: BTreeMap::new(), entries: VecDeque::new(), seq: 0 }).collect(),
            starts: Default::default(),
            done: Default::default(),
        }
    }

    /// Laufzeit des ersten Bilds eines Bildsegments
    fn start_of(&mut self, uri: &str) -> Option<u64> {
        if let Some(s) = self.starts.get(uri) {
            return Some(*s);
        }
        let bytes = std::fs::read(self.dir.join(uri)).ok()?;
        let ns = match self.format {
            SegmentFormat::Ts => {
                let pts = crate::segtime::ts_first_pts(&bytes)?;
                let rel = pts.wrapping_sub(pts_offset(self.format)) & ((1u64 << 33) - 1);
                rel * 100_000 / 9
            }
            SegmentFormat::Cmaf => {
                // Zeitskala aus dem Init-Segment der Playlist
                let text = std::fs::read_to_string(self.dir.join(&self.video)).ok()?;
                let init = text.lines().rev().find_map(|l| l.strip_prefix("#EXT-X-MAP:URI=\""))?.trim_end_matches('"').to_string();
                let scale = crate::segtime::timescale(&std::fs::read(self.dir.join(init)).ok()?)? as u64;
                let (t, _) = crate::segtime::fragment(&bytes)?;
                (t as u128 * 1_000_000_000 / scale.max(1) as u128) as u64
            }
        };
        self.starts.insert(uri.to_string(), ns);
        Some(ns)
    }

    fn file_name(&self, track: &str, seq: u64) -> String {
        if self.generation.is_empty() { format!("{track}_{seq:05}.vtt") } else { format!("{track}_{}_{seq:05}.vtt", self.generation) }
    }

    fn write_playlist(&self, t: &Track) {
        let mut s = format!(
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{}\n#EXT-X-MEDIA-SEQUENCE:{}\n",
            self.target,
            t.seq - t.entries.len() as u64
        );
        for (tags, uri) in &t.entries {
            for l in tags {
                s += l;
                s.push('\n');
            }
            s += uri;
            s.push('\n');
        }
        let name = if self.generation.is_empty() { format!("{}.m3u8", t.lang.name) } else { format!("{}.raw.m3u8", t.lang.name) };
        let tmp = self.dir.join(format!("{name}.tmp"));
        if std::fs::write(&tmp, s).is_ok() {
            let _ = std::fs::rename(&tmp, self.dir.join(name));
        }
    }

    /// Einmal je Durchgang des Fortschreibers aufrufen
    pub fn tick(&mut self) {
        for t in self.tracks.iter_mut() {
            for c in t.lang.cues.lock().unwrap().iter() {
                t.seen.insert(c.start_ns, c.clone());
            }
        }
        let Ok(text) = std::fs::read_to_string(self.dir.join(&self.video)) else { return };
        let items = parse_items(&text);
        let mut changed = false;
        for w in items.windows(2) {
            if self.done.contains(&w[0].uri) {
                continue;
            }
            let (Some(start), Some(end)) = (self.start_of(&w[0].uri), self.start_of(&w[1].uri)) else { break };
            let offset = pts_offset(self.format);
            for i in 0..self.tracks.len() {
                let name = self.file_name(&self.tracks[i].lang.name, self.tracks[i].seq);
                let cues: Vec<Cue> = self.tracks[i].seen.values().cloned().collect();
                if std::fs::write(self.dir.join(&name), segment(&cues, start, end, offset)).is_err() {
                    continue;
                }
                let keep = self.keep;
                let t = &mut self.tracks[i];
                t.seq += 1;
                t.entries.push_back((w[0].tags.clone(), name));
                while t.entries.len() > keep {
                    t.entries.pop_front();
                }
                // ältere Segmente löschen (wie die Segmentierer: einige mehr als die Playlist)
                if t.seq > keep as u64 + 4 {
                    let old = t.seq - keep as u64 - 5;
                    let _ = std::fs::remove_file(self.dir.join(if self.generation.is_empty() {
                        format!("{}_{old:05}.vtt", t.lang.name)
                    } else {
                        format!("{}_{}_{old:05}.vtt", t.lang.name, self.generation)
                    }));
                }
                // Zeilen, die vor diesem Segment begannen, braucht keins mehr
                t.seen.retain(|s, _| *s >= start);
            }
            self.done.insert(w[0].uri.clone());
            changed = true;
        }
        let live: std::collections::HashSet<&String> = items.iter().map(|i| &i.uri).collect();
        self.done.retain(|u| live.contains(u));
        self.starts.retain(|u, _| live.contains(u));
        if changed {
            for t in &self.tracks {
                self.write_playlist(t);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 1_000_000_000;

    fn ts_segment(pts: u64) -> Vec<u8> {
        let mut v = vec![0x47, 0x41, 0x00, 0x10, 0, 0, 1, 0xE0, 0, 0, 0x80, 0x80, 5];
        v.extend_from_slice(&[
            0x21 | ((pts >> 29) & 0x0e) as u8,
            (pts >> 22) as u8,
            0x01 | ((pts >> 14) & 0xfe) as u8,
            (pts >> 7) as u8,
            0x01 | ((pts << 1) & 0xfe) as u8,
        ]);
        v.resize(188, 0xff);
        v
    }

    #[test]
    fn segment_maps_running_time_to_pts() {
        let cues = vec![
            Cue { start_ns: 3 * S, end_ns: 5 * S, text: "Erste".into() },
            Cue { start_ns: 5 * S, end_ns: 9 * S, text: "Zweite".into() },
        ];
        let v = segment(&cues, 4 * S, 6 * S, pts_offset(SegmentFormat::Ts));
        assert_eq!(v, "WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:324000000,LOCAL:00:00:00.000\n\n00:00:05.000 --> 00:00:09.000\nZweite\n\n");
    }

    #[test]
    fn follows_video_segments() {
        let dir = std::env::temp_dir().join(format!("lzs-vtt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = EncoderConfig::default();
        cfg.hls.enabled = true;
        cfg.hls.dir = dir.to_string_lossy().to_string();
        cfg.hls.generation = "g".into();
        cfg.hls.playlist_length = 3;
        let cues: CueStore = Default::default();
        let mut w = VttWriter::new(&cfg, vec![Language { name: "s0".into(), cues: cues.clone() }]);
        let mut pl = String::from("#EXTM3U\n#EXT-X-TARGETDURATION:2\n");
        let mut add = |k: u64| {
            std::fs::write(dir.join(format!("v_g_{k:05}.ts")), ts_segment(324_000_000 + k * 2 * 90_000)).unwrap();
            pl += &format!("#EXT-X-PROGRAM-DATE-TIME:2026-11-17T12:00:{:02}.000Z\n#EXTINF:2,\nv_g_{k:05}.ts\n", k * 2);
            std::fs::write(dir.join("v.raw.m3u8"), &pl).unwrap();
        };
        add(0);
        cues.lock().unwrap().push(Cue { start_ns: S, end_ns: 3 * S, text: "Hallo".into() });
        w.tick();
        assert!(!dir.join("s0.raw.m3u8").exists(), "erstes Segment wartet auf das nächste");
        add(1);
        // Quelle wird gekürzt (Aufnahme-Segment), die Kopie bleibt
        cues.lock().unwrap().clear();
        cues.lock().unwrap().push(Cue { start_ns: 2 * S + S / 2, end_ns: 4 * S, text: "Welt".into() });
        w.tick();
        let p = std::fs::read_to_string(dir.join("s0.raw.m3u8")).unwrap();
        assert!(p.contains("#EXT-X-PROGRAM-DATE-TIME:2026-11-17T12:00:00.000Z\n#EXTINF:2,\ns0_g_00000.vtt\n"), "{p}");
        let v0 = std::fs::read_to_string(dir.join("s0_g_00000.vtt")).unwrap();
        assert!(v0.contains("00:00:01.000 --> 00:00:03.000\nHallo") && !v0.contains("Welt"), "{v0}");
        for k in 2..9 {
            add(k);
            w.tick();
        }
        let v1 = std::fs::read_to_string(dir.join("s0_g_00001.vtt")).unwrap();
        assert!(v1.contains("00:00:02.500 --> 00:00:04.000\nWelt") && !v1.contains("Hallo"), "{v1}");
        let p = std::fs::read_to_string(dir.join("s0.raw.m3u8")).unwrap();
        assert!(p.contains("#EXT-X-MEDIA-SEQUENCE:5\n") && p.matches(".vtt").count() == 3, "{p}");
        assert!(!dir.join("s0_g_00000.vtt").exists() && dir.join("s0_g_00007.vtt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
