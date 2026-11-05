//! Schaltbare Eigenschaften (I1): ein Pfad je Eigenschaft, so benannt, dass er
//! später als NMOS-IS-12-Eigenschaft der Bridge abbildbar ist (Rollenpfad +
//! Eigenschaft). Ändern geht ohne Neustart des Kanals: die Konfiguration wird
//! gespeichert und der laufende Kanal stellt live um.
//!
//! | Pfad | Typ |
//! |---|---|
//! | `captions.enabled` | bool |
//! | `audio.tracks.<i>.clearSpeech.enabled` | bool |
//! | `audio.tracks.<i>.leveler.enabled` | bool |
//! | `audio.tracks.<i>.leveler.preset` | `soft` \| `mid` \| `hard` |
//! | `audio.tracks.<i>.plugins.<j>.bypass` | bool |
//! | `graphics.layer2.visible` | bool (Webserver, nicht Kanal) |

use serde::Serialize;
use serde_json::{Value, json};

use crate::config::Config;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Control {
    pub path: String,
    pub value: Value,
    /// bool, enum
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<&'static str>>,
}

const PRESETS: [&str; 3] = ["soft", "mid", "hard"];

/// Alle Eigenschaften mit ihrem aktuellen Wert aus der Konfiguration
/// (`graphics.layer2.visible` ergänzt der Webserver).
pub fn list(cfg: &Config) -> Vec<Control> {
    let b = |path: String, v: bool| Control { path, value: json!(v), kind: "bool", options: None };
    let e = &cfg.encoder;
    let mut out = vec![b("captions.enabled".into(), !e.captions.muted)];
    for (i, t) in crate::audio::effective_tracks(&e.audio.tracks).iter().enumerate() {
        out.push(b(format!("audio.tracks.{i}.clearSpeech.enabled"), t.clear_speech.enabled));
        out.push(b(format!("audio.tracks.{i}.leveler.enabled"), t.leveler.enabled));
        out.push(Control {
            path: format!("audio.tracks.{i}.leveler.preset"),
            value: serde_json::to_value(t.leveler.preset).unwrap_or_default(),
            kind: "enum",
            options: Some(PRESETS.to_vec()),
        });
        for (j, _) in t.plugins.iter().enumerate() {
            out.push(b(format!("audio.tracks.{i}.plugins.{j}.bypass"), t.plugins[j].bypass));
        }
    }
    out
}

/// Ob sich eine Eigenschaft nur mit Neustart umstellen lässt (Element fehlt in
/// der laufenden Pipeline, z. B. Klare Sprache war beim Start aus).
pub fn needs_restart(before: &Config, path: &str) -> bool {
    let tracks = crate::audio::effective_tracks(&before.encoder.audio.tracks);
    let parts: Vec<&str> = path.split('.').collect();
    match parts.as_slice() {
        ["audio", "tracks", i, "clearSpeech", "enabled"] => i.parse::<usize>().ok().and_then(|i| tracks.get(i)).is_some_and(|t| !t.clear_speech.enabled),
        ["audio", "tracks", i, "leveler", _] => i.parse::<usize>().ok().and_then(|i| tracks.get(i)).is_some_and(|t| !t.leveler.enabled),
        _ => false,
    }
}

/// Wert in die Konfiguration schreiben. Fehler: unbekannter Pfad oder falscher Typ.
pub fn apply(cfg: &mut Config, path: &str, value: &Value) -> Result<(), String> {
    let as_bool = || value.as_bool().or_else(|| value.as_i64().map(|n| n != 0)).or_else(|| value.as_str().map(|s| matches!(s, "1" | "true" | "an" | "on"))).ok_or("bool erwartet");
    let parts: Vec<&str> = path.split('.').collect();
    let e = &mut cfg.encoder;
    // Spuren ohne eigene Liste: die eine Standardspur ausdrücklich anlegen
    if parts.first() == Some(&"audio") && e.audio.tracks.is_empty() {
        e.audio.tracks = crate::audio::effective_tracks(&e.audio.tracks);
    }
    let track = |e: &mut crate::config::EncoderConfig, i: &str| -> Result<usize, String> {
        let i: usize = i.parse().map_err(|_| "Spurnummer erwartet")?;
        if i < e.audio.tracks.len() { Ok(i) } else { Err(format!("keine Spur {i}")) }
    };
    match parts.as_slice() {
        ["captions", "enabled"] => e.captions.muted = !as_bool()?,
        ["audio", "tracks", i, "clearSpeech", "enabled"] => {
            let i = track(e, i)?;
            e.audio.tracks[i].clear_speech.enabled = as_bool()?;
        }
        ["audio", "tracks", i, "leveler", "enabled"] => {
            let i = track(e, i)?;
            e.audio.tracks[i].leveler.enabled = as_bool()?;
        }
        ["audio", "tracks", i, "leveler", "preset"] => {
            let i = track(e, i)?;
            let p = value.as_str().filter(|s| PRESETS.contains(s)).ok_or("soft, mid oder hard erwartet")?;
            e.audio.tracks[i].leveler.preset = serde_json::from_value(json!(p)).map_err(|x| x.to_string())?;
        }
        ["audio", "tracks", i, "plugins", j, "bypass"] => {
            let i = track(e, i)?;
            let j: usize = j.parse().map_err(|_| "Plugin-Nummer erwartet")?;
            let b = as_bool()?;
            e.audio.tracks[i].plugins.get_mut(j).ok_or(format!("kein Plugin {j}"))?.bypass = b;
        }
        _ => return Err(format!("unbekannte Eigenschaft „{path}“")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_and_list() {
        let mut c = Config::default();
        apply(&mut c, "captions.enabled", &json!(false)).unwrap();
        apply(&mut c, "audio.tracks.0.leveler.preset", &json!("hard")).unwrap();
        apply(&mut c, "audio.tracks.0.clearSpeech.enabled", &json!(1)).unwrap();
        assert!(apply(&mut c, "audio.tracks.0.leveler.preset", &json!("laut")).is_err());
        assert!(apply(&mut c, "audio.tracks.5.leveler.enabled", &json!(true)).is_err());
        assert!(apply(&mut c, "audio.tracks.0.plugins.0.bypass", &json!(true)).is_err(), "kein Plugin");
        assert!(apply(&mut c, "video.width", &json!(1)).is_err());
        let l = list(&c);
        let v = |p: &str| l.iter().find(|x| x.path == p).map(|x| x.value.clone());
        assert_eq!(v("captions.enabled"), Some(json!(false)));
        assert_eq!(v("audio.tracks.0.leveler.preset"), Some(json!("hard")));
        assert_eq!(v("audio.tracks.0.clearSpeech.enabled"), Some(json!(true)));
        assert!(needs_restart(&Config::default(), "audio.tracks.0.clearSpeech.enabled"));
        assert!(!needs_restart(&c, "audio.tracks.0.clearSpeech.enabled"));
        assert!(!needs_restart(&c, "captions.enabled"));
    }
}
