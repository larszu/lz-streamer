//! Ton-Routing: Kanalmatrix vom Eingang (bis 16 eingebettete SDI-Kanäle)
//! auf mehrere Tonspuren je Ausgang, jede mit Sprachkennung, Mono oder
//! Stereo, Pegel und Stummschaltung.
//!
//! Die Matrix je Spur hat eine Zeile je Ausgangskanal und eine Spalte je
//! Eingangskanal. Mehrere Quellkanäle auf einem Ausgangskanal werden
//! gemittelt (Downmix ohne Übersteuerung).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    Mono,
    #[default]
    Stereo,
}

impl Layout {
    pub fn channels(self) -> usize {
        match self {
            Layout::Mono => 1,
            Layout::Stereo => 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AudioTrack {
    pub name: String,
    /// ISO 639-2, z. B. "deu", "eng", "fra" (zweistellige Codes werden umgesetzt)
    pub language: String,
    pub layout: Layout,
    /// Quellkanäle, 1-basiert. Mono: alle werden gemischt. Stereo: Paare L,R,L,R …
    pub channels: Vec<u32>,
    pub gain_db: f64,
    pub mute: bool,
    pub bitrate_kbps: u32,
    /// Lautheitsregelung (lzloudness) für diese Spur
    pub leveler: crate::leveler::LevelerConfig,
}

impl Default for AudioTrack {
    fn default() -> Self {
        Self {
            name: "Ton".into(),
            language: "deu".into(),
            layout: Layout::Stereo,
            channels: vec![1, 2],
            gain_db: 0.0,
            mute: false,
            bitrate_kbps: 192,
            leveler: Default::default(),
        }
    }
}

/// Häufige zweistellige Codes (ISO 639-1) auf ISO 639-2/B, wie es im
/// TS-Sprachdeskriptor üblich ist.
const ISO639: &[(&str, &str)] = &[
    ("de", "ger"), ("en", "eng"), ("fr", "fre"), ("es", "spa"), ("it", "ita"), ("nl", "dut"),
    ("pl", "pol"), ("pt", "por"), ("ru", "rus"), ("tr", "tur"), ("ar", "ara"), ("zh", "chi"),
    ("ja", "jpn"), ("th", "tha"), ("uk", "ukr"), ("cs", "cze"), ("da", "dan"), ("sv", "swe"),
    ("no", "nor"), ("fi", "fin"), ("el", "gre"), ("hu", "hun"), ("ro", "rum"), ("ko", "kor"),
];

/// Vergleichsschlüssel: zweistelliger Code, wenn bekannt („deu“, „ger“, „de“ → „de“).
pub fn language_key(s: &str) -> String {
    let l = s.trim().to_ascii_lowercase();
    const T_CODES: &[(&str, &str)] = &[("deu", "de"), ("fra", "fr"), ("nld", "nl"), ("ces", "cs"), ("zho", "zh"), ("ell", "el"), ("ron", "ro")];
    if let Some((_, two)) = T_CODES.iter().find(|(t, _)| *t == l) {
        return two.to_string();
    }
    if let Some((two, _)) = ISO639.iter().find(|(_, three)| *three == l) {
        return two.to_string();
    }
    l
}

/// Sprachcode bereinigen: Kleinbuchstaben, 2→3 Zeichen, „deu“ und „ger“
/// sind beide gültig. Unbekanntes wird „und“ (unbestimmt).
pub fn normalize_language(s: &str) -> String {
    let l = s.trim().to_ascii_lowercase();
    if l.len() == 2 {
        if let Some((_, three)) = ISO639.iter().find(|(two, _)| *two == l) {
            return three.to_string();
        }
    }
    if l.len() == 3 && l.chars().all(|c| c.is_ascii_lowercase()) {
        return l;
    }
    "und".into()
}

/// Matrix (Ausgangskanäle × Eingangskanäle) für eine Spur.
pub fn matrix(track: &AudioTrack, in_channels: usize) -> Vec<Vec<f32>> {
    let outs = track.layout.channels();
    let mut m = vec![vec![0f32; in_channels]; outs];
    let valid: Vec<usize> =
        track.channels.iter().filter(|&&c| c >= 1 && (c as usize) <= in_channels).map(|&c| c as usize - 1).collect();
    if valid.is_empty() {
        return m; // Stille statt Absturz
    }
    match track.layout {
        Layout::Mono => {
            let w = 1.0 / valid.len() as f32;
            for &c in &valid {
                m[0][c] += w;
            }
        }
        Layout::Stereo => {
            if valid.len() == 1 {
                // ein Mono-Kanal auf beide Seiten
                m[0][valid[0]] = 1.0;
                m[1][valid[0]] = 1.0;
            } else {
                let left: Vec<usize> = valid.iter().step_by(2).copied().collect();
                let right: Vec<usize> = valid.iter().skip(1).step_by(2).copied().collect();
                for (row, list) in [(0, &left), (1, &right)] {
                    let w = 1.0 / list.len().max(1) as f32;
                    for &c in list {
                        m[row][c] += w;
                    }
                }
            }
        }
    }
    m
}

/// Matrix in gst-launch-Schreibweise für `audioconvert mix-matrix=…`.
pub fn matrix_string(m: &[Vec<f32>]) -> String {
    let rows: Vec<String> = m
        .iter()
        .map(|r| format!("<{}>", r.iter().map(|v| format!("(float){v:.6}")).collect::<Vec<_>>().join(",")))
        .collect();
    format!("\"<{}>\"", rows.join(","))
}

pub fn db_to_linear(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Ohne eigene Spurliste: eine Stereospur aus Kanal 1/2.
pub fn effective_tracks(tracks: &[AudioTrack]) -> Vec<AudioTrack> {
    if tracks.is_empty() { vec![AudioTrack::default()] } else { tracks.to_vec() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(layout: Layout, ch: &[u32]) -> AudioTrack {
        AudioTrack { layout, channels: ch.to_vec(), ..Default::default() }
    }

    #[test]
    fn stereo_pair_routes_l_and_r() {
        let m = matrix(&t(Layout::Stereo, &[3, 4]), 16);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0][2], 1.0);
        assert_eq!(m[1][3], 1.0);
        assert_eq!(m[0].iter().sum::<f32>(), 1.0);
    }

    #[test]
    fn mono_downmix_averages() {
        let m = matrix(&t(Layout::Mono, &[1, 2]), 16);
        assert_eq!(m.len(), 1);
        assert_eq!((m[0][0], m[0][1]), (0.5, 0.5));
    }

    #[test]
    fn stereo_downmix_of_two_pairs() {
        let m = matrix(&t(Layout::Stereo, &[1, 2, 5, 6]), 8);
        assert_eq!((m[0][0], m[0][4]), (0.5, 0.5));
        assert_eq!((m[1][1], m[1][5]), (0.5, 0.5));
    }

    #[test]
    fn single_channel_to_stereo_is_dual_mono() {
        let m = matrix(&t(Layout::Stereo, &[7]), 16);
        assert_eq!((m[0][6], m[1][6]), (1.0, 1.0));
    }

    #[test]
    fn out_of_range_channels_give_silence() {
        let m = matrix(&t(Layout::Stereo, &[17, 0]), 16);
        assert!(m.iter().flatten().all(|&v| v == 0.0));
    }

    #[test]
    fn languages() {
        assert_eq!(normalize_language("DE"), "ger");
        assert_eq!(normalize_language("deu"), "deu");
        assert_eq!(normalize_language(" eng "), "eng");
        assert_eq!(normalize_language("Deutsch"), "und");
        assert_eq!(normalize_language(""), "und");
    }

    #[test]
    fn matrix_string_format() {
        let s = matrix_string(&[vec![1.0, 0.0], vec![0.0, 0.5]]);
        assert_eq!(s, "\"<<(float)1.000000,(float)0.000000>,<(float)0.000000,(float)0.500000>>\"");
    }

    #[test]
    fn language_keys_match_across_code_forms() {
        for x in ["de", "deu", "ger", "DE"] {
            assert_eq!(language_key(x), "de");
        }
        assert_eq!(language_key("fra"), language_key("fr"));
        assert_eq!(language_key("eng"), "en");
    }

    #[test]
    fn gain() {
        assert!((db_to_linear(-6.0) - 0.501).abs() < 0.001);
        assert_eq!(db_to_linear(0.0), 1.0);
    }
}
