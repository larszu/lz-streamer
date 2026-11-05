//! Kanal-Konfiguration. Jedes Feld hat eine brauchbare Vorgabe, damit ein
//! leeres oder unvollständiges JSON trotzdem einen lauffähigen Kanal ergibt.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    #[default]
    Encoder,
    Decoder,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum InputKind {
    /// DeckLink, wenn eines gefunden wird, sonst Testbild.
    #[default]
    Auto,
    Test,
    Decklink,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Connection {
    #[default]
    Auto,
    Sdi,
    Hdmi,
}

impl Connection {
    pub fn gst_nick(self) -> &'static str {
        match self {
            Connection::Auto => "auto",
            Connection::Sdi => "sdi",
            Connection::Hdmi => "hdmi",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct InputConfig {
    pub kind: InputKind,
    pub device: u32,
    pub connection: Connection,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self { kind: InputKind::Auto, device: 0, connection: Connection::Auto }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    #[default]
    H264,
    H265,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ScaleQuality {
    Fast,
    /// Lanczos – für HD→UHD
    #[default]
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct VideoConfig {
    pub codec: Codec,
    /// "auto" oder ein GStreamer-Elementname (z. B. "x264enc").
    pub encoder: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    /// GOP-Länge in Millisekunden (kurz = schneller Einstieg, latenzarm).
    pub gop_ms: u32,
    pub scale_quality: ScaleQuality,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            codec: Codec::H264,
            encoder: "auto".into(),
            width: 1920,
            height: 1080,
            fps: 50,
            bitrate_kbps: 8000,
            gop_ms: 1000,
            scale_quality: ScaleQuality::High,
        }
    }
}

impl VideoConfig {
    pub fn gop_frames(&self) -> u32 {
        ((self.gop_ms as u64 * self.fps.max(1) as u64) / 1000).max(1) as u32
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AudioConfig {
    pub enabled: bool,
    pub bitrate_kbps: u32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self { enabled: true, bitrate_kbps: 192 }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum StandbyPattern {
    #[default]
    Bars,
    Black,
    Image,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct StandbyConfig {
    pub pattern: StandbyPattern,
    pub image_path: String,
    pub text: String,
    /// So lange darf der Eingang schweigen, bevor das Standbild übernimmt.
    pub timeout_ms: u32,
}

impl Default for StandbyConfig {
    fn default() -> Self {
        Self {
            pattern: StandbyPattern::Bars,
            image_path: String::new(),
            text: "Kein Eingangssignal".into(),
            timeout_ms: 500,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SrtMode {
    Caller,
    #[default]
    Listener,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SrtConfig {
    pub mode: SrtMode,
    pub host: String,
    pub port: u16,
    pub latency_ms: u32,
    pub passphrase: String,
    pub streamid: String,
}

impl Default for SrtConfig {
    fn default() -> Self {
        Self {
            mode: SrtMode::Listener,
            host: String::new(),
            port: 9000,
            latency_ms: 120,
            passphrase: String::new(),
            streamid: String::new(),
        }
    }
}

impl SrtConfig {
    /// Host für den Caller-Modus; leer heißt „dieser Rechner“.
    pub fn effective_host(&self) -> &str {
        let h = self.host.trim();
        if h.is_empty() { "127.0.0.1" } else { h }
    }

    pub fn uri(&self) -> String {
        match self.mode {
            SrtMode::Caller => {
                let h = self.effective_host();
                let h = if h.contains(':') && !h.starts_with('[') { format!("[{h}]") } else { h.to_string() };
                format!("srt://{h}:{}?mode=caller", self.port)
            }
            SrtMode::Listener => format!("srt://:{}?mode=listener", self.port),
        }
    }

    /// Ein eingefügtes Ziel wie `srt://10.0.0.5:9000?mode=caller` oder
    /// `10.0.0.5:9000` in Host/Port/Modus zerlegen.
    pub fn normalize(&mut self) {
        let raw = self.host.trim().to_string();
        if raw.is_empty() {
            return;
        }
        if let Some(t) = parse_target(&raw) {
            self.host = t.host;
            if let Some(p) = t.port {
                self.port = p;
            }
            if let Some(m) = t.mode {
                self.mode = m;
            } else if raw.contains("://") || t.port.is_some() {
                self.mode = SrtMode::Caller;
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub host: String,
    pub port: Option<u16>,
    pub mode: Option<SrtMode>,
}

/// Zerlegt `srt://host:port?mode=…`, `host:port`, `[v6]:port` oder `host`.
pub fn parse_target(s: &str) -> Option<Target> {
    let s = s.trim();
    let s = s.strip_prefix("srt://").unwrap_or(s);
    let (hostport, query) = match s.split_once('?') {
        Some((a, b)) => (a, Some(b)),
        None => (s, None),
    };
    let hostport = hostport.trim_end_matches('/');
    let mode = query.and_then(|q| {
        q.split('&').find_map(|kv| match kv.split_once('=') {
            Some(("mode", "caller")) => Some(SrtMode::Caller),
            Some(("mode", "listener")) => Some(SrtMode::Listener),
            _ => None,
        })
    });
    let (host, port) = if let Some(rest) = hostport.strip_prefix('[') {
        let (h, after) = rest.split_once(']')?;
        let port = after.strip_prefix(':').and_then(|p| p.parse().ok());
        (h.to_string(), port)
    } else if hostport.matches(':').count() == 1 {
        let (h, p) = hostport.split_once(':')?;
        (h.to_string(), Some(p.parse().ok()?))
    } else {
        (hostport.to_string(), None)
    };
    Some(Target { host, port, mode })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RtmpConfig {
    pub enabled: bool,
    /// Vollständige Ziel-URL einschließlich Stream-Key. Wird nur lokal gespeichert.
    pub url: String,
}

impl Default for RtmpConfig {
    fn default() -> Self {
        Self { enabled: false, url: String::new() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct EncoderConfig {
    pub input: InputConfig,
    pub video: VideoConfig,
    pub audio: AudioConfig,
    pub standby: StandbyConfig,
    pub srt: SrtConfig,
    pub rtmp: RtmpConfig,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum OutputKind {
    /// Fenster auf diesem Rechner.
    #[default]
    Window,
    Decklink,
    /// Keine Ausgabe – nur empfangen und messen.
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct DecoderConfig {
    pub srt: SrtConfig,
    pub output: OutputKind,
    pub device: u32,
    /// DeckLink-Ausgabemodus, z. B. "1080p50"; "auto" nimmt das Format des Stroms.
    pub deck_mode: String,
    pub audio: bool,
    /// Pufferzeit des TS-Demuxers. Der GStreamer-Standard (700 ms) ist für
    /// Contribution viel zu lang.
    pub demux_latency_ms: u32,
    /// "auto" oder ein GStreamer-Decoder (z. B. "avdec_h264").
    pub decoder: String,
}

impl Default for DecoderConfig {
    fn default() -> Self {
        Self {
            srt: SrtConfig { mode: SrtMode::Caller, ..SrtConfig::default() },
            output: OutputKind::Window,
            device: 0,
            deck_mode: "auto".into(),
            audio: true,
            demux_latency_ms: 40,
            decoder: "auto".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Config {
    pub role: Role,
    pub encoder: EncoderConfig,
    pub decoder: DecoderConfig,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Hint {
    pub level: &'static str,
    pub text: String,
}

impl Config {
    pub fn normalize(&mut self) {
        self.encoder.srt.normalize();
        self.decoder.srt.normalize();
        let v = &mut self.encoder.video;
        v.width = v.width.clamp(160, 7680) & !1;
        v.height = v.height.clamp(90, 4320) & !1;
        v.fps = v.fps.clamp(1, 120);
        v.bitrate_kbps = v.bitrate_kbps.clamp(100, 200_000);
        v.gop_ms = v.gop_ms.clamp(40, 10_000);
        if v.encoder.trim().is_empty() {
            v.encoder = "auto".into();
        }
        self.encoder.srt.latency_ms = self.encoder.srt.latency_ms.clamp(20, 8000);
        self.decoder.srt.latency_ms = self.decoder.srt.latency_ms.clamp(20, 8000);
        self.encoder.standby.timeout_ms = self.encoder.standby.timeout_ms.clamp(100, 10_000);
    }

    /// Hinweise statt Pflichtfelder: nichts davon verhindert den Start.
    pub fn hints(&self) -> Vec<Hint> {
        let mut h = Vec::new();
        let warn = |t: &str| Hint { level: "warn", text: t.to_string() };
        let info = |t: &str| Hint { level: "info", text: t.to_string() };
        match self.role {
            Role::Encoder => {
                let e = &self.encoder;
                if e.srt.mode == SrtMode::Caller && e.srt.host.trim().is_empty() {
                    h.push(warn("Kein Ziel eingetragen – gesendet wird an diesen Rechner (127.0.0.1)."));
                }
                if e.srt.mode == SrtMode::Listener {
                    h.push(info(&format!("Wartet auf Port {} auf eine Gegenstelle (Listener).", e.srt.port)));
                }
                if !e.srt.passphrase.is_empty() && !(10..=79).contains(&e.srt.passphrase.chars().count()) {
                    h.push(warn("SRT-Passwort muss 10 bis 79 Zeichen lang sein, sonst lehnt libsrt ab."));
                }
                if e.rtmp.enabled && e.rtmp.url.trim().is_empty() {
                    h.push(warn("RTMP ist eingeschaltet, aber ohne Ziel-URL – RTMP bleibt aus."));
                }
                if e.rtmp.enabled && e.video.codec == Codec::H265 {
                    h.push(warn("RTMP überträgt hier nur H.264 – bei H.265 bleibt RTMP aus."));
                }
                if e.standby.pattern == StandbyPattern::Image && e.standby.image_path.trim().is_empty() {
                    h.push(info("Kein Standbild gewählt – bei Signalverlust kommen Farbbalken."));
                }
                if e.video.width > 1920 && e.video.codec == Codec::H264 {
                    h.push(info("Für UHD ist H.265 sparsamer: gleiche Qualität bei etwa halber Bitrate."));
                }
                if e.video.width > 1920 && e.video.bitrate_kbps < 12_000 {
                    h.push(info("UHD mit weniger als 12 Mbit/s wird sichtbar weich."));
                }
                if e.srt.latency_ms < 80 {
                    h.push(info("SRT-Latenz unter 80 ms: Verluste lassen sich kaum noch reparieren (Faustregel 3–4 × RTT)."));
                }
            }
            Role::Decoder => {
                let d = &self.decoder;
                if d.srt.mode == SrtMode::Caller && d.srt.host.trim().is_empty() {
                    h.push(warn("Keine Encoder-Adresse eingetragen – verbunden wird mit diesem Rechner (127.0.0.1)."));
                }
            }
        }
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_json_gives_defaults() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(c, Config::default());
        assert_eq!(c.encoder.srt.port, 9000);
        assert_eq!(c.encoder.srt.mode, SrtMode::Listener);
        assert_eq!(c.decoder.srt.mode, SrtMode::Caller);
        assert_eq!(c.encoder.video.encoder, "auto");
    }

    #[test]
    fn partial_json_keeps_other_defaults() {
        let c: Config = serde_json::from_str(r#"{"encoder":{"video":{"bitrate_kbps":4000}}}"#).unwrap();
        assert_eq!(c.encoder.video.bitrate_kbps, 4000);
        assert_eq!(c.encoder.video.fps, 50);
        assert!(c.encoder.audio.enabled);
    }

    #[test]
    fn gop_frames_from_ms() {
        let v = VideoConfig { fps: 50, gop_ms: 1000, ..Default::default() };
        assert_eq!(v.gop_frames(), 50);
        let v = VideoConfig { fps: 25, gop_ms: 40, ..Default::default() };
        assert_eq!(v.gop_frames(), 1);
    }

    #[test]
    fn uri_caller_and_listener() {
        let mut s = SrtConfig { mode: SrtMode::Caller, host: "10.1.2.3".into(), port: 9001, ..Default::default() };
        assert_eq!(s.uri(), "srt://10.1.2.3:9001?mode=caller");
        s.host = String::new();
        assert_eq!(s.uri(), "srt://127.0.0.1:9001?mode=caller");
        s.host = "fe80::1".into();
        assert_eq!(s.uri(), "srt://[fe80::1]:9001?mode=caller");
        s.mode = SrtMode::Listener;
        assert_eq!(s.uri(), "srt://:9001?mode=listener");
    }

    #[test]
    fn parse_targets() {
        let t = parse_target("srt://example.org:7000?mode=listener&latency=200").unwrap();
        assert_eq!(t, Target { host: "example.org".into(), port: Some(7000), mode: Some(SrtMode::Listener) });
        let t = parse_target("10.0.0.5:9000").unwrap();
        assert_eq!(t.port, Some(9000));
        assert_eq!(t.mode, None);
        let t = parse_target("[::1]:5000").unwrap();
        assert_eq!(t.host, "::1");
        assert_eq!(t.port, Some(5000));
        let t = parse_target("encoder.local").unwrap();
        assert_eq!(t.port, None);
        assert!(parse_target("host:notaport").is_none());
    }

    #[test]
    fn normalize_pasted_url_switches_to_caller() {
        let mut s = SrtConfig { host: "srt://192.0.2.10:9500".into(), ..Default::default() };
        s.normalize();
        assert_eq!(s.host, "192.0.2.10");
        assert_eq!(s.port, 9500);
        assert_eq!(s.mode, SrtMode::Caller);
    }

    #[test]
    fn normalize_clamps_values() {
        let mut c = Config::default();
        c.encoder.video.width = 1921;
        c.encoder.video.bitrate_kbps = 1;
        c.encoder.video.encoder = " ".into();
        c.normalize();
        assert_eq!(c.encoder.video.width, 1920);
        assert_eq!(c.encoder.video.bitrate_kbps, 100);
        assert_eq!(c.encoder.video.encoder, "auto");
    }

    #[test]
    fn hints_never_block_but_warn() {
        let mut c = Config::default();
        c.encoder.srt.mode = SrtMode::Caller;
        c.encoder.rtmp.enabled = true;
        c.encoder.srt.passphrase = "kurz".into();
        let h = c.hints();
        assert!(h.iter().any(|x| x.text.contains("127.0.0.1")));
        assert!(h.iter().any(|x| x.text.contains("RTMP")));
        assert!(h.iter().any(|x| x.text.contains("10 bis 79")));
    }
}
