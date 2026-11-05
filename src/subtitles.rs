//! Untertiteldateien neben der Aufnahme (U5): `.srt` und `.vtt` je
//! Aufnahme-Segment. Die Zeiten kommen aus den Zeitstempeln der Untertitel
//! (Laufzeit der Pipeline) relativ zum Segmentbeginn, nicht aus der Uhrzeit
//! der Abfrage.

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cue {
    /// Laufzeit der Pipeline, ns
    pub start_ns: u64,
    pub end_ns: u64,
    pub text: String,
}

/// Neue Zeile anhängen; die vorige endet spätestens, wo die neue beginnt.
pub fn push(cues: &mut Vec<Cue>, start_ns: u64, dur_ns: u64, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    if let Some(prev) = cues.last_mut() {
        if prev.end_ns > start_ns {
            prev.end_ns = start_ns.max(prev.start_ns + 1);
        }
    }
    cues.push(Cue { start_ns, end_ns: start_ns + dur_ns.max(1), text: text.to_string() });
}

fn ts(ms: u64, sep: char) -> String {
    format!("{:02}:{:02}:{:02}{sep}{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// Zeilen, die in das Segment ab `seg_start_ns` fallen, mit Zeiten relativ dazu.
fn relative(cues: &[Cue], seg_start_ns: u64) -> Vec<(u64, u64, &str)> {
    cues.iter()
        .filter(|c| c.end_ns > seg_start_ns)
        .map(|c| ((c.start_ns.saturating_sub(seg_start_ns)) / 1_000_000, (c.end_ns - seg_start_ns) / 1_000_000, c.text.as_str()))
        .collect()
}

pub fn to_srt(cues: &[Cue], seg_start_ns: u64) -> String {
    relative(cues, seg_start_ns)
        .iter()
        .enumerate()
        .map(|(i, (s, e, t))| format!("{}\n{} --> {}\n{t}\n\n", i + 1, ts(*s, ','), ts(*e, ',')))
        .collect()
}

pub fn to_vtt(cues: &[Cue], seg_start_ns: u64) -> String {
    let body: String = relative(cues, seg_start_ns).iter().map(|(s, e, t)| format!("{} --> {}\n{t}\n\n", ts(*s, '.'), ts(*e, '.'))).collect();
    format!("WEBVTT\n\n{body}")
}

/// Beide Dateien neben die Aufnahme schreiben (ganz neu, damit sie stets vollständig sind).
pub fn write(recording: &Path, cues: &[Cue], seg_start_ns: u64) -> std::io::Result<()> {
    std::fs::write(recording.with_extension("srt"), to_srt(cues, seg_start_ns))?;
    std::fs::write(recording.with_extension("vtt"), to_vtt(cues, seg_start_ns))
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 1_000_000_000;

    #[test]
    fn cues_relative_to_segment_and_clipped() {
        let mut c = Vec::new();
        push(&mut c, 10 * S, 4 * S, "Erste Zeile");
        push(&mut c, 12 * S, 3 * S, "Zweite");
        push(&mut c, 13 * S, 0, "   ");
        assert_eq!(c[0].end_ns, 12 * S, "vorige endet bei der nächsten");
        let srt = to_srt(&c, 9 * S);
        assert_eq!(srt, "1\n00:00:01,000 --> 00:00:03,000\nErste Zeile\n\n2\n00:00:03,000 --> 00:00:06,000\nZweite\n\n");
        let vtt = to_vtt(&c, 11 * S);
        assert!(vtt.starts_with("WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nErste Zeile\n\n"), "{vtt}");
        // nach dem Ende einer Zeile beginnt ein Segment: Zeile fehlt dort
        assert_eq!(to_srt(&c, 13 * S).matches("-->").count(), 1);
    }
}
