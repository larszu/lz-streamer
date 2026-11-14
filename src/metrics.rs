//! Messwerte für Überwachungssysteme: `/metrics` im Prometheus-Textformat
//! und optional Influx-Line-Protocol per HTTP-Push.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::config::{Config, Role};
use crate::ops::{ActiveAlarm, SystemInfo};
use crate::supervisor::Snapshot;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct InfluxSettings {
    /// vollständige Schreib-Adresse, z. B. `http://influx:8086/api/v2/write?org=technik&bucket=streamer`;
    /// leer = aus
    pub url: String,
    /// API-Token (Kopfzeile `Authorization: Token …`); wird über die API nie zurückgegeben
    pub token: String,
    /// Sekunden zwischen zwei Sendungen
    pub interval_s: u32,
}

impl Default for InfluxSettings {
    fn default() -> Self {
        Self { url: String::new(), token: String::new(), interval_s: 10 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct MetricsSettings {
    /// `/metrics` auch ohne Anmeldung abrufbar, wenn Benutzer angelegt sind
    pub public: bool,
    pub influx: InfluxSettings,
}

/// Label einer Messreihe: Name, Wert
pub type Label = (&'static str, String);

#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub name: &'static str,
    pub help: &'static str,
    pub counter: bool,
    pub labels: Vec<Label>,
    pub value: f64,
}

/// Alle Messwerte aus dem aktuellen Zustand.
pub fn collect(snap: &Snapshot, cfg: &Config, sys: &SystemInfo, alarms: &[ActiveAlarm]) -> Vec<Sample> {
    let mut v = Vec::new();
    let mut g = |name, help, labels: Vec<(&'static str, String)>, value: f64| v.push(Sample { name, help, counter: false, labels, value });
    let role = match cfg.role {
        Role::Encoder => "encoder",
        Role::Decoder => "decoder",
    };
    g("lzs_info", "Version und Rolle", vec![("version", env!("CARGO_PKG_VERSION").into()), ("role", role.into())], 1.0);
    g("lzs_running", "Kanal soll laufen (1) oder ist gestoppt (0)", vec![], snap.running as u8 as f64);
    let st = snap.status.as_ref();
    if let Some(s) = st {
        g("lzs_uptime_seconds", "Laufzeit des Kanals seit dem letzten Start", vec![], s.uptime_s);
        if s.signal != "unknown" {
            g("lzs_signal_ok", "Eingangssignal vorhanden (1) oder verloren (0)", vec![], (s.signal == "ok") as u8 as f64);
        }
        g("lzs_standby", "Standbild läuft statt Eingang", vec![], s.standby as u8 as f64);
        g("lzs_bitrate_bits_per_second", "Ausgangsbitrate", vec![], s.bitrate_kbps * 1000.0);
        g("lzs_frames_per_second", "gemessene Bildrate", vec![], s.fps);
        if cfg.role == Role::Encoder {
            g("lzs_frames_per_second_target", "eingestellte Bildrate", vec![], cfg.encoder.video.fps as f64);
        }
        g("lzs_srt_connected", "SRT-Gegenstelle verbunden", vec![], s.srt.connected as u8 as f64);
        g("lzs_srt_peers", "Anzahl SRT-Gegenstellen", vec![], s.srt.peers as f64);
        if let Some(r) = s.srt.rtt_ms {
            g("lzs_srt_rtt_seconds", "SRT-Umlaufzeit (größte aller Gegenstellen)", vec![], r / 1000.0);
        }
        if let Some(b) = s.srt.bandwidth_mbps {
            g("lzs_srt_bandwidth_bits_per_second", "von SRT geschätzte Bandbreite", vec![], b * 1e6);
        }
        for o in &s.outputs {
            let l = || vec![("output", o.name.clone())];
            if let Some(c) = o.connected {
                g("lzs_output_connected", "weiterer Ausgang verbunden", l(), c as u8 as f64);
            }
            if let Some(r) = o.rtt_ms {
                g("lzs_output_rtt_seconds", "Umlaufzeit des weiteren Ausgangs", l(), r / 1000.0);
            }
        }
        for t in &s.audio_tracks {
            let Some(lo) = &t.loudness else { continue };
            let l = || {
                vec![
                    ("track", (t.index + 1).to_string()),
                    ("name", t.name.clone().unwrap_or_else(|| format!("Spur {}", t.index + 1))),
                    ("language", t.language.clone().unwrap_or_default()),
                ]
            };
            let opt = [
                ("lzs_audio_momentary_lufs", "Lautheit Momentary (400 ms)", lo.momentary),
                ("lzs_audio_short_term_lufs", "Lautheit Short-Term (3 s)", lo.short_term),
                ("lzs_audio_integrated_lufs", "Lautheit integriert seit Start", lo.integrated),
                ("lzs_audio_loudness_range_lu", "Lautheitsbereich (LRA)", lo.lra),
                ("lzs_audio_true_peak_dbtp", "True Peak", lo.true_peak),
            ];
            for (name, help, x) in opt {
                if let Some(x) = x {
                    g(name, help, l(), x);
                }
            }
            g("lzs_audio_silence_seconds", "so lange ist die Spur still", l(), lo.silent_s);
        }
    }
    g("lzs_cpu_percent", "CPU-Last des Rechners", vec![], sys.cpu_percent);
    g("lzs_memory_percent", "belegter Arbeitsspeicher", vec![], sys.memory_percent);
    if let Some(x) = sys.gpu_percent {
        g("lzs_gpu_percent", "GPU-Last", vec![], x);
    }
    if sys.disk_total_gb > 0.0 {
        g("lzs_disk_free_bytes", "freier Platz für Aufnahmen", vec![], sys.disk_free_gb * 1e9);
    }
    g("lzs_alarms_active", "Anzahl aktiver Alarme", vec![], alarms.len() as f64);
    for a in alarms {
        g("lzs_alarm", "aktiver Alarm", vec![("id", a.alarm.id.clone()), ("level", a.alarm.level.into())], 1.0);
    }
    let mut c = |name, help, value: f64| v.push(Sample { name, help, counter: true, labels: vec![], value });
    c("lzs_restarts_total", "Neustarts des Kanals seit Programmstart", snap.restarts as f64);
    if let Some(s) = st {
        c("lzs_frames_total", "Bilder seit Kanalstart", s.frames as f64);
        c("lzs_srt_packets_lost_total", "verlorene SRT-Pakete seit Kanalstart", s.srt.lost as f64);
        c("lzs_srt_packets_retransmitted_total", "wiederholte SRT-Pakete seit Kanalstart", s.srt.retransmitted as f64);
        c("lzs_srt_packets_dropped_total", "verworfene SRT-Pakete seit Kanalstart", s.srt.dropped as f64);
        for o in &s.outputs {
            v.push(Sample {
                name: "lzs_output_packets_lost_total",
                help: "verlorene Pakete des weiteren Ausgangs",
                counter: true,
                labels: vec![("output", o.name.clone())],
                value: o.lost as f64,
            });
        }
    }
    v
}

fn num(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 { format!("{}", x as i64) } else { format!("{x}") }
}

/// Prometheus-Textformat 0.0.4
pub fn prometheus(samples: &[Sample]) -> String {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n");
    let mut names: Vec<&str> = Vec::new();
    for s in samples {
        if !names.contains(&s.name) {
            names.push(s.name);
        }
    }
    let mut out = String::new();
    for n in names {
        let group: Vec<&Sample> = samples.iter().filter(|s| s.name == n).collect();
        let first = group[0];
        out.push_str(&format!("# HELP {n} {}\n# TYPE {n} {}\n", first.help, if first.counter { "counter" } else { "gauge" }));
        for s in group {
            out.push_str(n);
            if !s.labels.is_empty() {
                let l: Vec<String> = s.labels.iter().map(|(k, v)| format!("{k}=\"{}\"", esc(v))).collect();
                out.push_str(&format!("{{{}}}", l.join(",")));
            }
            out.push_str(&format!(" {}\n", num(s.value)));
        }
    }
    out
}

/// Influx-Line-Protocol: eine Zeile je Label-Satz, Messung `lz_streamer`, Zeit in ns.
pub fn influx(samples: &[Sample], host: &str, ts_ns: i128) -> String {
    let tag = |s: &str| s.replace('\\', "\\\\").replace(',', "\\,").replace('=', "\\=").replace(' ', "\\ ");
    let mut groups: Vec<(&[Label], Vec<String>)> = Vec::new();
    for s in samples.iter().filter(|s| s.name != "lzs_info") {
        let field = format!("{}={}{}", s.name.trim_start_matches("lzs_"), num(s.value), if s.counter { "i" } else { "" });
        match groups.iter_mut().find(|(l, _)| *l == s.labels.as_slice()) {
            Some((_, f)) => f.push(field),
            None => groups.push((&s.labels, vec![field])),
        }
    }
    let mut out = String::new();
    for (labels, fields) in groups {
        out.push_str("lz_streamer,host=");
        out.push_str(&tag(if host.is_empty() { "unbekannt" } else { host }));
        for (k, v) in labels.iter().filter(|(_, v)| !v.is_empty()) {
            out.push_str(&format!(",{k}={}", tag(v)));
        }
        out.push_str(&format!(" {} {ts_ns}\n", fields.join(",")));
    }
    out
}

// ------------------------------------------------------------ HTTP-Push

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// Schlichter HTTP/1.1-POST (http und https), liefert den Statuscode.
pub async fn post(url: &str, token: &str, body: &str) -> Result<u16> {
    let (tls, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        bail!("Adresse muss mit http:// oder https:// beginnen");
    };
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let path = if path.is_empty() { "/" } else { path };
    let (host, port) = match authority.rsplit_once(':').filter(|(_, p)| p.parse::<u16>().is_ok()) {
        Some((h, p)) => (h.trim_matches(['[', ']']), p.parse().unwrap_or(80)),
        None => (authority.trim_matches(['[', ']']), if tls { 443 } else { 80 }),
    };
    let tcp = tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect((host, port)))
        .await
        .context("Zeitüberschreitung beim Verbinden")??;
    let mut stream: Box<dyn Io> = if tls {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()?
            .with_root_certificates(roots)
            .with_no_client_auth();
        let name = rustls::pki_types::ServerName::try_from(host.to_string())?;
        Box::new(tokio_rustls::TlsConnector::from(Arc::new(cfg)).connect(name, tcp).await?)
    } else {
        Box::new(tcp)
    };
    let mut req = format!(
        "POST {path} HTTP/1.1\r\nHost: {authority}\r\nUser-Agent: lz-streamer\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if !token.is_empty() {
        req.push_str(&format!("Authorization: Token {token}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    let io = async {
        stream.write_all(req.as_bytes()).await?;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        // nur die Statuszeile ist wichtig
        while !buf.windows(2).any(|w| w == b"\r\n") {
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        anyhow::Ok(buf)
    };
    let buf = tokio::time::timeout(Duration::from_secs(10), io).await.context("Zeitüberschreitung")??;
    let line = String::from_utf8_lossy(&buf);
    line.split_whitespace().nth(1).and_then(|c| c.parse().ok()).context("keine HTTP-Antwort")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{ChannelStatus, OutputStatus, TrackInfo};
    use crate::loudness::Loudness;

    fn sample_state() -> (Snapshot, Config, SystemInfo, Vec<ActiveAlarm>) {
        let mut st = ChannelStatus { signal: "lost".into(), standby: true, bitrate_kbps: 8000.0, fps: 50.0, frames: 1234, uptime_s: 12.5, ..Default::default() };
        st.srt.connected = true;
        st.srt.peers = 1;
        st.srt.rtt_ms = Some(2.5);
        st.srt.lost = 7;
        st.srt.retransmitted = 3;
        st.outputs.push(OutputStatus { name: "Sicherung".into(), target: String::new(), connected: Some(true), rtt_ms: None, lost: 1 });
        st.audio_tracks.push(TrackInfo {
            index: 0,
            language: Some("deu".into()),
            name: Some("Deutsch \"Saal\"".into()),
            loudness: Some(Loudness { short_term: Some(-23.1), true_peak: Some(-2.0), silent_s: 0.0, ..Default::default() }),
            ..Default::default()
        });
        let snap = Snapshot { running: true, restarts: 2, status: Some(st), ..Default::default() };
        let sys = SystemInfo { cpu_percent: 12.0, memory_percent: 40.0, disk_total_gb: 100.0, disk_free_gb: 50.0, gpu_percent: Some(5.0), ..Default::default() };
        let alarms = vec![ActiveAlarm { alarm: crate::admin::Alarm { id: "signal".into(), level: "error", text: "x".into() }, since_ms: 0 }];
        (snap, Config::default(), sys, alarms)
    }

    #[test]
    fn prometheus_text() {
        let (snap, cfg, sys, alarms) = sample_state();
        let t = prometheus(&collect(&snap, &cfg, &sys, &alarms));
        for line in [
            "# TYPE lzs_bitrate_bits_per_second gauge",
            "lzs_bitrate_bits_per_second 8000000",
            "lzs_srt_rtt_seconds 0.0025",
            "# TYPE lzs_srt_packets_lost_total counter",
            "lzs_srt_packets_lost_total 7",
            "lzs_srt_packets_retransmitted_total 3",
            "lzs_restarts_total 2",
            "lzs_signal_ok 0",
            "lzs_frames_per_second 50",
            "lzs_frames_per_second_target 50",
            "lzs_cpu_percent 12",
            "lzs_gpu_percent 5",
            "lzs_audio_short_term_lufs{track=\"1\",name=\"Deutsch \\\"Saal\\\"\",language=\"deu\"} -23.1",
            "lzs_alarm{id=\"signal\",level=\"error\"} 1",
            "lzs_output_packets_lost_total{output=\"Sicherung\"} 1",
        ] {
            assert!(t.lines().any(|l| l == line), "fehlt: {line}\n{t}");
        }
        // jede Metrik genau ein HELP/TYPE
        assert_eq!(t.matches("# TYPE lzs_audio_short_term_lufs ").count(), 1);
        // gestoppter Kanal: keine Kanalwerte, aber Systemwerte
        let idle = prometheus(&collect(&Snapshot::default(), &cfg, &sys, &[]));
        assert!(idle.contains("lzs_running 0") && idle.contains("lzs_cpu_percent") && !idle.contains("lzs_bitrate"));
    }

    #[test]
    fn influx_lines() {
        let (snap, cfg, sys, alarms) = sample_state();
        let t = influx(&collect(&snap, &cfg, &sys, &alarms), "regie pc", 1_000);
        let first = t.lines().next().unwrap();
        assert!(first.starts_with("lz_streamer,host=regie\\ pc running=1,"), "{first}");
        assert!(first.contains("bitrate_bits_per_second=8000000,") && first.contains("restarts_total=2i") && first.ends_with(" 1000"), "{first}");
        assert!(t.contains("lz_streamer,host=regie\\ pc,track=1,name=Deutsch\\ \"Saal\",language=deu audio_short_term_lufs=-23.1,"), "{t}");
        assert!(!t.contains("info="));
    }

    #[tokio::test]
    async fn http_push_reaches_server() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let srv = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut c = [0u8; 4096];
            loop {
                let n = s.read(&mut c).await.unwrap();
                buf.extend_from_slice(&c[..n]);
                let t = String::from_utf8_lossy(&buf);
                if n == 0 || t.ends_with(" 1000\n") {
                    break;
                }
            }
            s.write_all(b"HTTP/1.1 204 No Content\r\n\r\n").await.unwrap();
            String::from_utf8(buf).unwrap()
        });
        let code = post(&format!("http://127.0.0.1:{port}/api/v2/write?bucket=b"), "abc", "lz_streamer,host=x running=1 1000\n").await.unwrap();
        assert_eq!(code, 204);
        let req = srv.await.unwrap();
        assert!(req.starts_with("POST /api/v2/write?bucket=b HTTP/1.1\r\n"), "{req}");
        assert!(req.contains("Authorization: Token abc\r\n"));
        assert!(post("ftp://x", "", "").await.is_err());
    }
}
