//! Fortlaufende Medien-Playlists über Neustarts hinweg. Die Segmentierer
//! beginnen nach jedem Kanalstart wieder bei Sequenz 0 – ein laufender Player
//! bleibt daran hängen. Deshalb schreiben sie in `X.raw.m3u8`, und dieser
//! Fortschreiber veröffentlicht `X.m3u8`: Segmente des vorigen Laufs bleiben
//! stehen, die Sequenznummern zählen weiter und vor dem ersten neuen Segment
//! steht `#EXT-X-DISCONTINUITY`. Innerhalb eines Laufs ist keine Marke nötig:
//! das Standbild bei Signalverlust hat Format und Zeitachse des Eingangs.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
struct Entry {
    /// Zeilen vor der URI (EXTINF, PROGRAM-DATE-TIME …)
    tags: Vec<String>,
    uri: String,
    seq: u64,
    disc: bool,
    /// EXT-X-MAP (CMAF), falls vorhanden
    map: Option<String>,
}

#[derive(Debug, Default)]
pub struct Playlist {
    entries: VecDeque<Entry>,
    next_seq: u64,
    disc_seq: u64,
    /// das nächste neue Segment beginnt nach einem Neustart
    pending_disc: bool,
    /// gesehene URIs des laufenden Laufs
    seen: std::collections::HashSet<String>,
    version: u32,
    target: u32,
    /// herausgefallene Segmente früherer Läufe (zum Löschen)
    pub dropped: Vec<String>,
}

fn parse(text: &str) -> (Vec<Entry>, u64, u64, u32, u32) {
    let (mut out, mut tags, mut map, mut disc) = (Vec::new(), Vec::new(), None, false);
    let (mut seq, mut dseq, mut version, mut target) = (0u64, 0u64, 3u32, 1u32);
    for l in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(v) = l.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            seq = v.parse().unwrap_or(0);
        } else if let Some(v) = l.strip_prefix("#EXT-X-DISCONTINUITY-SEQUENCE:") {
            dseq = v.parse().unwrap_or(0);
        } else if let Some(v) = l.strip_prefix("#EXT-X-VERSION:") {
            version = v.parse().unwrap_or(3);
        } else if let Some(v) = l.strip_prefix("#EXT-X-TARGETDURATION:") {
            target = v.parse().unwrap_or(1);
        } else if l == "#EXT-X-DISCONTINUITY" {
            disc = true;
        } else if l.starts_with("#EXT-X-MAP:") {
            map = Some(l.to_string());
        } else if l.starts_with("#EXTINF") || l.starts_with("#EXT-X-PROGRAM-DATE-TIME") || l.starts_with("#EXT-X-BYTERANGE") {
            tags.push(l.to_string());
        } else if !l.starts_with('#') {
            out.push(Entry { tags: std::mem::take(&mut tags), uri: l.to_string(), seq: 0, disc, map: map.clone() });
            disc = false;
        }
    }
    for (i, e) in out.iter_mut().enumerate() {
        e.seq = seq + i as u64;
    }
    (out, seq, dseq, version, target)
}

impl Playlist {
    /// Mit der veröffentlichten Playlist des vorigen Laufs beginnen (leer = neu)
    pub fn resume(previous: &str) -> Self {
        let (entries, _, disc_seq, version, target) = parse(previous);
        let next_seq = entries.last().map(|e| e.seq + 1).unwrap_or(0);
        Self { pending_disc: !entries.is_empty(), entries: entries.into(), next_seq, disc_seq, version, target, ..Default::default() }
    }

    /// Neuen Stand des Segmentierers übernehmen; `keep` = Segmente in der Playlist
    pub fn update(&mut self, raw: &str, keep: usize) {
        let (list, _, _, version, target) = parse(raw);
        self.version = self.version.max(version);
        self.target = self.target.max(target);
        for mut e in list {
            if !self.seen.insert(e.uri.clone()) {
                continue;
            }
            e.seq = self.next_seq;
            self.next_seq += 1;
            e.disc = std::mem::take(&mut self.pending_disc);
            self.entries.push_back(e);
        }
        while self.entries.len() > keep.max(1) {
            let e = self.entries.pop_front().unwrap();
            if e.disc {
                self.disc_seq += 1;
            }
            if !self.seen.contains(&e.uri) {
                self.dropped.push(e.uri);
            }
        }
    }

    pub fn has_segments(&self) -> bool {
        !self.entries.is_empty()
    }

    pub fn render(&self) -> String {
        let mut s = format!(
            "#EXTM3U\n#EXT-X-VERSION:{}\n#EXT-X-TARGETDURATION:{}\n#EXT-X-MEDIA-SEQUENCE:{}\n",
            self.version,
            self.target,
            self.entries.front().map(|e| e.seq).unwrap_or(self.next_seq)
        );
        if self.disc_seq > 0 {
            s += &format!("#EXT-X-DISCONTINUITY-SEQUENCE:{}\n", self.disc_seq);
        }
        let mut map: Option<&String> = None;
        for e in &self.entries {
            if e.disc {
                s += "#EXT-X-DISCONTINUITY\n";
            }
            if let Some(m) = e.map.as_ref().filter(|m| Some(*m) != map || e.disc) {
                s += m;
                s.push('\n');
            }
            map = e.map.as_ref();
            for t in &e.tags {
                s += t;
                s.push('\n');
            }
            s += &e.uri;
            s.push('\n');
        }
        s
    }
}

/// Alle Medien-Playlists eines Ordners fortschreiben, bis `stop` gesetzt ist
pub struct Publisher {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// Rohname → veröffentlichter Name: `v1.raw.m3u8` → `v1.m3u8`
pub fn published_name(raw: &str) -> Option<String> {
    raw.strip_suffix(".raw.m3u8").map(|b| format!("{b}.m3u8"))
}

fn write_atomic(path: &Path, text: &str) {
    let tmp = path.with_extension("m3u8.tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

impl Publisher {
    /// `names`: Basisnamen der Renditionen (`v`, `v1`, `a0` …)
    pub fn start(dir: PathBuf, names: Vec<String>, keep: usize, resume: bool) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let mut lists: Vec<(String, Playlist)> = names
            .into_iter()
            .map(|n| {
                let prev = if resume { std::fs::read_to_string(dir.join(format!("{n}.m3u8"))).unwrap_or_default() } else { String::new() };
                (n, Playlist::resume(&prev))
            })
            .collect();
        let handle = std::thread::Builder::new()
            .name("hls-playlist".into())
            .spawn(move || {
                let mut last: Vec<String> = vec![String::new(); lists.len()];
                let mut grace: VecDeque<(u32, String)> = VecDeque::new();
                while !s2.load(Ordering::Relaxed) {
                    for (i, (n, p)) in lists.iter_mut().enumerate() {
                        let Ok(raw) = std::fs::read_to_string(dir.join(format!("{n}.raw.m3u8"))) else { continue };
                        if raw == last[i] {
                            continue;
                        }
                        p.update(&raw, keep);
                        if p.has_segments() {
                            write_atomic(&dir.join(format!("{n}.m3u8")), &p.render());
                        }
                        last[i] = raw;
                        grace.extend(p.dropped.drain(..).map(|u| (0, u)));
                    }
                    // Segmente früherer Läufe erst eine Weile nach dem Herausfallen löschen
                    for g in grace.iter_mut() {
                        g.0 += 1;
                    }
                    while grace.front().is_some_and(|g| g.0 > 200) {
                        let (_, u) = grace.pop_front().unwrap();
                        if !u.contains('/') && !u.contains("..") {
                            let _ = std::fs::remove_file(dir.join(u));
                        }
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            })
            .ok();
        Self { stop, handle }
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Publisher {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(seq: u64, uris: &[&str]) -> String {
        let mut s = format!("#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:{seq}\n");
        for u in uris {
            s += &format!("#EXT-X-PROGRAM-DATE-TIME:2026-01-01T00:00:00Z\n#EXTINF:2,\n{u}\n");
        }
        s
    }

    #[test]
    fn continues_sequence_with_discontinuity_after_restart() {
        // erster Lauf
        let mut p = Playlist::resume("");
        p.update(&raw(0, &["v_a_00000.ts", "v_a_00001.ts"]), 3);
        p.update(&raw(1, &["v_a_00001.ts", "v_a_00002.ts", "v_a_00003.ts"]), 3);
        let first = p.render();
        assert!(first.contains("#EXT-X-MEDIA-SEQUENCE:1\n") && !first.contains("DISCONTINUITY"), "{first}");
        // Neustart: Segmentierer beginnt wieder bei 0
        let mut q = Playlist::resume(&first);
        q.update(&raw(0, &["v_b_00000.ts"]), 3);
        let s = q.render();
        assert!(s.contains("#EXT-X-MEDIA-SEQUENCE:2\n"), "{s}");
        assert!(s.contains("v_a_00003.ts\n#EXT-X-DISCONTINUITY\n#EXT-X-PROGRAM-DATE-TIME"), "{s}");
        assert!(q.dropped == vec!["v_a_00001.ts".to_string()]);
        // alte Segmente fallen heraus: Diskontinuitäts-Zähler steigt, sobald die Marke vorn wegfällt
        q.update(&raw(0, &["v_b_00000.ts", "v_b_00001.ts", "v_b_00002.ts"]), 3);
        let s = q.render();
        assert!(s.contains("#EXT-X-MEDIA-SEQUENCE:4\n") && s.contains("#EXT-X-DISCONTINUITY\n") && !s.contains("DISCONTINUITY-SEQUENCE"), "{s}");
        q.update(&raw(1, &["v_b_00001.ts", "v_b_00002.ts", "v_b_00003.ts"]), 3);
        let s = q.render();
        assert!(s.contains("#EXT-X-MEDIA-SEQUENCE:5\n#EXT-X-DISCONTINUITY-SEQUENCE:1\n") && !s.contains("#EXT-X-DISCONTINUITY\n"), "{s}");
        // Segmente des laufenden Laufs löscht der Segmentierer selbst
        assert!(q.dropped.iter().all(|u| u.starts_with("v_a_")), "{:?}", q.dropped);
    }

    #[test]
    fn cmaf_map_restated_after_discontinuity() {
        let mut p = Playlist::resume("");
        p.update("#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"v_a_init_000.mp4\"\n#EXTINF:2,\nv_a_00000.m4s\n", 4);
        let mut q = Playlist::resume(&p.render());
        q.update("#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"v_b_init_000.mp4\"\n#EXTINF:2,\nv_b_00000.m4s\n", 4);
        let s = q.render();
        assert!(s.contains("#EXT-X-VERSION:6"), "{s}");
        assert!(s.contains("#EXT-X-MAP:URI=\"v_a_init_000.mp4\"\n#EXTINF:2,\nv_a_00000.m4s\n#EXT-X-DISCONTINUITY\n#EXT-X-MAP:URI=\"v_b_init_000.mp4\"\n#EXTINF:2,\nv_b_00000.m4s"), "{s}");
        assert_eq!(published_name("v1.raw.m3u8").as_deref(), Some("v1.m3u8"));
        assert_eq!(published_name("v1.m3u8"), None);
    }
}
