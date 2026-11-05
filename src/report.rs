//! Statistik als gestalteter Bericht: HTML mit Vektor-Graphen (SVG), als PDF
//! über Chrome/Chromium im Headless-Modus gedruckt. Ohne Chrome bleibt der
//! HTML-Bericht (druckfertig, „Als PDF sichern“ im Browser).

use std::path::{Path, PathBuf};

use crate::ops::StatPoint;
use crate::supervisor::Event;

const LOGO: &str = include_str!("../web/brand/lzm_hauptlogo_navy.svg");
const FONT: &[u8] = include_bytes!("../web/fonts/public-sans-latin-wght-normal.woff2");

#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub points: usize,
    pub running_share: f64,
    pub connected_share: f64,
    pub disconnects: usize,
    pub bitrate: (f64, f64, f64),
    pub rtt: Option<(f64, f64, f64)>,
    pub lost_total: u64,
    pub retrans_total: u64,
    pub standby_s: f64,
    pub cpu: (f64, f64, f64),
}

fn min_mean_max(v: &[f64]) -> Option<(f64, f64, f64)> {
    if v.is_empty() {
        return None;
    }
    let min = v.iter().copied().fold(f64::INFINITY, f64::min);
    let max = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Some((min, v.iter().sum::<f64>() / v.len() as f64, max))
}

pub fn summarize(p: &[StatPoint]) -> Summary {
    let run: Vec<&StatPoint> = p.iter().filter(|x| x.running).collect();
    let n = p.len().max(1) as f64;
    let disconnects = p.windows(2).filter(|w| w[0].connected && !w[1].connected && w[1].running).count();
    let bitrate: Vec<f64> = run.iter().map(|x| x.bitrate_kbps).collect();
    let rtt: Vec<f64> = run.iter().filter_map(|x| x.rtt_ms).collect();
    let cpu: Vec<f64> = p.iter().map(|x| x.cpu).collect();
    let span = |f: fn(&StatPoint) -> u64| match (run.first(), run.last()) {
        (Some(a), Some(b)) => f(b).saturating_sub(f(a)),
        _ => 0,
    };
    Summary {
        points: p.len(),
        running_share: run.len() as f64 / n,
        connected_share: if run.is_empty() { 0.0 } else { run.iter().filter(|x| x.connected).count() as f64 / run.len() as f64 },
        disconnects,
        bitrate: min_mean_max(&bitrate).unwrap_or_default(),
        rtt: min_mean_max(&rtt),
        lost_total: span(|x| x.lost),
        retrans_total: span(|x| x.retransmitted),
        standby_s: run.iter().filter(|x| x.standby).count() as f64 * 5.0,
        cpu: min_mean_max(&cpu).unwrap_or_default(),
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn ts(ms: u64, fmt: &str) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64).map(|d| d.with_timezone(&chrono::Local).format(fmt).to_string()).unwrap_or_default()
}

/// Liniendiagramm als SVG (vektoriell im PDF).
pub fn line_chart(title: &str, unit: &str, points: &[StatPoint], series: &[(&str, &str, fn(&StatPoint) -> Option<f64>)]) -> String {
    let (w, h, l, r, t, b) = (720.0, 190.0, 52.0, 12.0, 28.0, 26.0);
    let (t0, t1) = match (points.first(), points.last()) {
        (Some(a), Some(z)) if z.at_ms > a.at_ms => (a.at_ms, z.at_ms),
        _ => return format!("<section class=chart><h3>{}</h3><p class=empty>Keine Werte im Zeitraum.</p></section>", esc(title)),
    };
    let vals: Vec<f64> = points.iter().flat_map(|p| series.iter().filter_map(|s| (s.2)(p))).collect();
    let max = vals.iter().copied().fold(0.0f64, f64::max).max(1e-9);
    let min = vals.iter().copied().fold(f64::INFINITY, f64::min).min(0.0);
    let x = |ms: u64| l + (ms - t0) as f64 / (t1 - t0) as f64 * (w - l - r);
    let y = |v: f64| t + (1.0 - (v - min) / (max - min).max(1e-9)) * (h - t - b);
    let mut svg = format!("<svg viewBox='0 0 {w} {h}' xmlns='http://www.w3.org/2000/svg'>");
    for i in 0..=4 {
        let v = min + (max - min) * i as f64 / 4.0;
        let yy = y(v);
        svg += &format!("<line x1='{l}' x2='{}' y1='{yy:.1}' y2='{yy:.1}' class='grid'/><text x='{}' y='{:.1}' class='ax' text-anchor='end'>{}</text>", w - r, l - 6.0, yy + 3.5, fmt_num(v));
    }
    for i in 0..=4 {
        let ms = t0 + (t1 - t0) * i / 4;
        svg += &format!("<text x='{:.1}' y='{}' class='ax' text-anchor='middle'>{}</text>", x(ms), h - 8.0, ts(ms, "%H:%M:%S"));
    }
    for (name, class, f) in series {
        let mut d = String::new();
        let mut pen = false;
        for p in points {
            match f(p) {
                Some(v) => {
                    d += &format!("{}{:.1},{:.1}", if pen { "L" } else { "M" }, x(p.at_ms), y(v));
                    pen = true;
                }
                None => pen = false,
            }
        }
        svg += &format!("<path d='{d}' class='{class}'><title>{}</title></path>", esc(name));
    }
    svg += "</svg>";
    let legend: String = series.iter().map(|(n, c, _)| format!("<span class='lg {c}'>{}</span>", esc(n))).collect();
    format!("<section class=chart><h3>{} <small>{}</small></h3>{svg}<div class=legend>{legend}</div></section>", esc(title), esc(unit))
}

fn fmt_num(v: f64) -> String {
    if v.abs() >= 100.0 { format!("{v:.0}") } else if v.abs() >= 10.0 { format!("{v:.1}") } else { format!("{v:.2}") }
        .replace('.', ",")
}

pub fn html(points: &[StatPoint], events: &[Event], from_ms: u64, to_ms: u64, host: &str) -> String {
    use base64::Engine;
    let s = summarize(points);
    let font = base64::engine::general_purpose::STANDARD.encode(FONT);
    let tile = |label: &str, value: String| format!("<div class=tile><b>{value}</b><span>{}</span></div>", esc(label));
    let pct = |v: f64| format!("{} %", fmt_num(v * 100.0));
    let mmm = |t: (f64, f64, f64), unit: &str| format!("{} / {} / {} {unit}", fmt_num(t.0), fmt_num(t.1), fmt_num(t.2));
    let tracks = points.iter().map(|p| p.loudness.len()).max().unwrap_or(0);
    let mut charts = vec![
        line_chart("Bitrate", "kbit/s", points, &[("Bitrate", "s1", |p| p.running.then_some(p.bitrate_kbps))]),
        line_chart("Laufzeit (RTT)", "ms", points, &[("RTT", "s2", |p| p.rtt_ms)]),
        line_chart("Verloren / wiederholt (Summe)", "Pakete", points, &[("verloren", "s3", |p| Some(p.lost as f64)), ("wiederholt", "s2", |p| Some(p.retransmitted as f64))]),
    ];
    if tracks > 0 {
        type F = fn(&StatPoint) -> Option<f64>;
        let fs: [F; 4] = [
            |p| p.loudness.first().copied().flatten(),
            |p| p.loudness.get(1).copied().flatten(),
            |p| p.loudness.get(2).copied().flatten(),
            |p| p.loudness.get(3).copied().flatten(),
        ];
        let names = ["Spur 1", "Spur 2", "Spur 3", "Spur 4"];
        let classes = ["s1", "s2", "s3", "s4"];
        let series: Vec<(&str, &str, F)> = (0..tracks.min(4)).map(|i| (names[i], classes[i], fs[i])).collect();
        charts.push(line_chart("Lautheit (Short-Term)", "LUFS", points, &series));
    }
    charts.push(line_chart("CPU", "%", points, &[("CPU", "s4", |p| Some(p.cpu))]));
    let ev: String = events
        .iter()
        .filter(|e| e.at_ms >= from_ms && e.at_ms <= to_ms)
        .map(|e| format!("<tr class='{}'><td>{}</td><td>{}</td></tr>", esc(&e.level), ts(e.at_ms, "%d.%m. %H:%M:%S"), esc(&e.text)))
        .collect();
    format!(
        r#"<!doctype html><html lang=de><head><meta charset=utf-8><title>lz-streamer Bericht</title><style>
@font-face{{font-family:'Public Sans';src:url(data:font/woff2;base64,{font}) format('woff2');font-weight:100 900}}
@page{{size:A4;margin:16mm 14mm}}
*{{box-sizing:border-box}}body{{font:400 10pt/1.45 'Public Sans',system-ui,sans-serif;color:#2B2E35;margin:0;background:#fff}}
header{{display:flex;justify-content:space-between;align-items:flex-end;border-bottom:1px solid #E5E5E2;padding-bottom:8mm;margin-bottom:6mm}}
header .logo svg{{height:11mm;width:55mm;fill:#1D324F;display:block;margin-left:auto}}header .meta{{text-align:right;color:#6A6E77;font-size:9pt}}
h1{{font-size:18pt;margin:0 0 1mm;color:#16181D}}h2{{font-size:12pt;margin:6mm 0 3mm;color:#16181D}}h3{{font-size:10.5pt;margin:0 0 2mm;color:#16181D}}h3 small{{font-weight:400;color:#6A6E77}}
.tiles{{display:grid;grid-template-columns:repeat(4,1fr);gap:3mm}}.tile{{background:#F1F1EE;border-radius:3mm;padding:3mm}}.tile b{{display:block;font-size:12pt;color:#16181D;font-variant-numeric:tabular-nums}}.tile span{{color:#6A6E77;font-size:8.5pt}}
.chart{{break-inside:avoid;margin:5mm 0}}.chart svg{{width:100%;height:auto}}.grid{{stroke:#E5E5E2;stroke-width:.6}}.ax{{font:7.5pt 'Public Sans';fill:#6A6E77}}
.chart path{{fill:none;stroke-width:1.4}}.s1{{stroke:#1D324F;color:#1D324F}}.s2{{stroke:#C8892B;color:#C8892B}}.s3{{stroke:#B04A3F;color:#B04A3F}}.s4{{stroke:#2F7D5C;color:#2F7D5C}}
.legend{{display:flex;gap:5mm;font-size:8.5pt;color:#6A6E77}}.lg::before{{content:'';display:inline-block;width:5mm;height:1.4mm;background:currentColor;margin-right:1.5mm;vertical-align:middle}}
.empty{{color:#6A6E77}}table{{width:100%;border-collapse:collapse;font-size:8.5pt}}td{{padding:1.2mm 2mm;border-bottom:1px solid #EEEEEB;vertical-align:top}}td:first-child{{white-space:nowrap;color:#6A6E77;width:28mm}}
tr.error td:last-child{{color:#B04A3F}}tr.warn td:last-child{{color:#8A5D1D}}tr.ok td:last-child{{color:#2F7D5C}}tr{{break-inside:avoid}}
footer{{margin-top:8mm;color:#6A6E77;font-size:8pt}}.events{{break-before:auto}}
</style></head><body>
<header><div><h1>Betriebsbericht</h1><div>{from} – {to}</div></div><div class=meta><div class=logo>{logo}</div><div>{host} · lz-streamer {ver}</div></div></header>
<h2>Kennzahlen</h2>
<div class=tiles>{t1}{t2}{t3}{t4}{t5}{t6}{t7}{t8}</div>
<h2>Verlauf</h2>
{charts}
<section class=events><h2>Ereignisse und Alarme</h2>{evtable}</section>
<footer>Erstellt {now} · Messpunkte alle 5 s · {n} Messpunkte im Zeitraum</footer>
</body></html>"#,
        logo = LOGO,
        from = ts(from_ms, "%d.%m.%Y %H:%M"),
        to = ts(to_ms, "%d.%m.%Y %H:%M"),
        host = esc(host),
        ver = env!("CARGO_PKG_VERSION"),
        t1 = tile("Sendeanteil", pct(s.running_share)),
        t2 = tile("Verbunden (beim Senden)", pct(s.connected_share)),
        t3 = tile("Verbindungsabbrüche", s.disconnects.to_string()),
        t4 = tile("Standbild (Signalverlust)", format!("{} s", fmt_num(s.standby_s))),
        t5 = tile("Bitrate min / Mittel / max", mmm((s.bitrate.0 / 1000.0, s.bitrate.1 / 1000.0, s.bitrate.2 / 1000.0), "Mbit/s")),
        t6 = tile("RTT min / Mittel / max", s.rtt.map(|r| mmm(r, "ms")).unwrap_or_else(|| "–".into())),
        t7 = tile("Pakete verloren / wiederholt", format!("{} / {}", s.lost_total, s.retrans_total)),
        t8 = tile("CPU min / Mittel / max", mmm(s.cpu, "%")),
        charts = charts.join(""),
        evtable = if ev.is_empty() { "<p class=empty>Keine Ereignisse im Zeitraum.</p>".into() } else { format!("<table>{ev}</table>") },
        now = chrono::Local::now().format("%d.%m.%Y %H:%M"),
        n = s.points,
    )
}

/// Chrome/Chromium für den PDF-Druck suchen.
pub fn find_browser() -> Option<PathBuf> {
    let candidates = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/usr/bin/google-chrome",
    ];
    candidates.iter().map(PathBuf::from).find(|p| p.is_file())
}

/// HTML → PDF mit Headless-Chrome (eigenes Profil, kein Netz nötig).
pub async fn to_pdf(html: &str, work: &Path) -> anyhow::Result<Vec<u8>> {
    let browser = find_browser().ok_or_else(|| anyhow::anyhow!("Kein Chrome/Chromium für den PDF-Druck gefunden"))?;
    let dir = work.join(format!("bericht-{}", crate::admin::new_token()));
    std::fs::create_dir_all(&dir)?;
    let src = dir.join("bericht.html");
    let out = dir.join("bericht.pdf");
    std::fs::write(&src, html)?;
    let status = tokio::process::Command::new(browser)
        .arg("--headless=new")
        .arg("--disable-gpu")
        .arg("--no-pdf-header-footer")
        .arg(format!("--user-data-dir={}", dir.join("profil").display()))
        .arg(format!("--print-to-pdf={}", out.display()))
        .arg(format!("file://{}", src.display()))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await?;
    let pdf = std::fs::read(&out).map_err(|e| anyhow::anyhow!("PDF nicht erzeugt ({status}): {e}"))?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(pdf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pts() -> Vec<StatPoint> {
        (0..120)
            .map(|i| StatPoint {
                at_ms: 1_762_300_000_000 + i * 5000,
                running: i > 5,
                bitrate_kbps: 8000.0 + (i % 7) as f64 * 10.0,
                rtt_ms: Some(1.0 + (i % 3) as f64),
                lost: i / 10,
                retransmitted: i / 5,
                fps: 50.0,
                standby: (40..44).contains(&i),
                connected: !(60..64).contains(&i),
                cpu: 20.0,
                loudness: vec![Some(-23.0), Some(-30.0)],
                true_peak: vec![Some(-3.0)],
            })
            .collect()
    }

    #[test]
    fn summary_counts() {
        let s = summarize(&pts());
        assert_eq!(s.disconnects, 1);
        assert_eq!(s.standby_s, 20.0);
        assert!(s.running_share > 0.9);
        assert_eq!(s.lost_total, 11 - 0);
    }

    #[test]
    fn html_has_sections_and_vector_charts() {
        let e = vec![Event { at_ms: 1_762_300_100_000, level: "warn".into(), text: "Gegenstelle getrennt".into() }];
        let h = html(&pts(), &e, 1_762_300_000_000, 1_762_300_600_000, "studio");
        for needle in ["Betriebsbericht", "Kennzahlen", "Verlauf", "Ereignisse und Alarme", "Lautheit", "<svg", "<path d='M", "Gegenstelle getrennt", "@page"] {
            assert!(h.contains(needle), "fehlt {needle}");
        }
    }
}
