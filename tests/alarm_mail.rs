//! Alarme per Mail und `/metrics`: lokaler Mini-SMTP-Server, Passwort nie in der API.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use base64::Engine;
use http_body_util::BodyExt;
use lz_streamer::admin::{self, DataDir};
use lz_streamer::caps::Capabilities;
use lz_streamer::channel::ChannelStatus;
use lz_streamer::config::Config;
use lz_streamer::ops::{AlarmTracker, Ops, SystemInfo};
use lz_streamer::server::{App, router};
use lz_streamer::supervisor::{Snapshot, Supervisor};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tower::ServiceExt;

/// Eine empfangene Mail: AUTH-Zeile (falls gesendet), Empfänger, Inhalt.
#[derive(Debug, Default)]
struct Received {
    auth: Option<String>,
    rcpt: Vec<String>,
    data: String,
}

/// Beantwortet den SMTP-Dialog zeilenweise (ohne TLS) und meldet jede Mail.
async fn mini_smtp() -> (u16, mpsc::UnboundedReceiver<Received>) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let Ok((s, _)) = l.accept().await else { return };
            let tx = tx.clone();
            tokio::spawn(async move {
                let (r, mut w) = s.into_split();
                let mut lines = BufReader::new(r).lines();
                w.write_all(b"220 mini ESMTP\r\n").await.unwrap();
                let mut m = Received::default();
                while let Ok(Some(line)) = lines.next_line().await {
                    let up = line.to_ascii_uppercase();
                    let reply: &[u8] = if up.starts_with("EHLO") {
                        b"250-mini\r\n250 AUTH PLAIN LOGIN\r\n"
                    } else if up.starts_with("AUTH") {
                        m.auth = Some(line.clone());
                        b"235 2.7.0 ok\r\n"
                    } else if up.starts_with("MAIL FROM") {
                        b"250 ok\r\n"
                    } else if up.starts_with("RCPT TO") {
                        m.rcpt.push(line.clone());
                        b"250 ok\r\n"
                    } else if up == "DATA" {
                        w.write_all(b"354 weiter\r\n").await.unwrap();
                        while let Ok(Some(d)) = lines.next_line().await {
                            if d == "." {
                                break;
                            }
                            m.data.push_str(&d);
                            m.data.push('\n');
                        }
                        let _ = tx.send(std::mem::take(&mut m));
                        b"250 queued\r\n"
                    } else if up == "QUIT" {
                        let _ = w.write_all(b"221 bye\r\n").await;
                        return;
                    } else {
                        b"250 ok\r\n"
                    };
                    w.write_all(reply).await.unwrap();
                }
            });
        }
    });
    (port, rx)
}

fn mail_settings(port: u16) -> lz_streamer::mail::MailSettings {
    serde_json::from_value(json!({
        "enabled": true, "host": "127.0.0.1", "port": port, "security": "none",
        "user": "technik", "password": "smtp-geheim-42", "from": "streamer@example.org", "to": "regie@example.org"
    }))
    .unwrap()
}

async fn next_mail(rx: &mut mpsc::UnboundedReceiver<Received>) -> Received {
    tokio::time::timeout(Duration::from_secs(10), rx.recv()).await.expect("keine Mail angekommen").unwrap()
}

fn setup(tag: &str) -> (std::sync::Arc<Supervisor>, std::sync::Arc<Ops>, std::path::PathBuf) {
    let _ = gstreamer::init();
    let root = std::env::temp_dir().join(format!("lzs-mail-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let data = DataDir::new(root.clone());
    let sup = Supervisor::new(Config::default(), root.join("config.json"), root.join("run"), data.recordings());
    (sup, Ops::new(data), root)
}

/// Signalverlust (Standbild läuft) über mehrere Prüfdurchläufe: genau eine Alarm-Mail, danach Entwarnung.
#[tokio::test]
async fn signal_loss_sends_mail() {
    let (port, mut rx) = mini_smtp().await;
    let (sup, ops, _) = setup("signal");
    let mut s = ops.settings.lock().await.clone();
    s.mail = mail_settings(port);
    ops.save_settings(s).await.unwrap();

    let cfg = Config::default();
    let sys = SystemInfo { disk_total_gb: 100.0, disk_free_gb: 50.0, ..Default::default() };
    let snap = |standby: bool| Snapshot {
        running: true,
        status: Some(ChannelStatus { signal: if standby { "lost" } else { "ok" }.into(), standby, uptime_s: 3.0, ..Default::default() }),
        ..Default::default()
    };
    let mut tracker = AlarmTracker::default();
    let mut now = 1_000_000;
    for standby in [false, true, true, true] {
        now += 5000;
        let (input, _) = tracker.step(&snap(standby), &cfg, &sys, now);
        let fresh = admin::evaluate_alarms(&ops.settings.lock().await.alarms, &input);
        ops.apply_alarms(&sup, &fresh, now).await;
    }
    let m = next_mail(&mut rx).await;
    assert!(m.data.contains("Kein Eingangssignal"), "{}", m.data);
    assert!(m.data.contains("Subject: [lz-streamer "), "{}", m.data);
    assert!(m.rcpt[0].contains("regie@example.org"));
    let auth = m.auth.expect("keine Anmeldung am Mailserver");
    let plain = auth.split_whitespace().nth(2).unwrap();
    let dec = base64::engine::general_purpose::STANDARD.decode(plain).unwrap();
    assert_eq!(dec, b"\0technik\0smtp-geheim-42");
    // dauerhafter Alarm: keine zweite Mail
    assert!(tokio::time::timeout(Duration::from_millis(300), rx.recv()).await.is_err(), "Alarm doppelt gemeldet");
    // Signal zurück: Entwarnung
    now += 5000;
    let (input, _) = tracker.step(&snap(false), &cfg, &sys, now);
    let fresh = admin::evaluate_alarms(&ops.settings.lock().await.alarms, &input);
    ops.apply_alarms(&sup, &fresh, now).await;
    let m = next_mail(&mut rx).await;
    assert!(m.data.contains("Erledigt: Kein Eingangssignal"), "{}", m.data);
}

fn app(tag: &str) -> (axum::Router, std::path::PathBuf) {
    let (sup, ops, root) = setup(tag);
    (router(App::new(sup, ops, Capabilities::probe(), vec![])), root)
}

async fn call(r: &axum::Router, method: &str, uri: &str, body: Option<Value>, hdr: Option<(header::HeaderName, &str)>) -> (StatusCode, String, Option<String>) {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some((k, v)) = hdr {
        b = b.header(k, v);
    }
    let req = match body {
        Some(v) => b.header(header::CONTENT_TYPE, "application/json").body(Body::from(v.to_string())).unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let resp = r.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let cookie = resp.headers().get(header::SET_COOKIE).map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned(), cookie)
}

/// Passwort und Token kommen über die API nie zurück; leeres Feld behält den Wert; Testmail-Knopf.
#[tokio::test]
async fn mail_password_stays_local_and_test_mail() {
    let (port, mut rx) = mini_smtp().await;
    let (r, root) = app("api");
    let mut s = json!({"mail": serde_json::to_value(mail_settings(port)).unwrap(), "metrics": {"influx": {"url": "", "token": "influx-token-99"}}});
    assert_eq!(call(&r, "PUT", "/api/settings", Some(s.clone()), None).await.0, StatusCode::OK);
    let (_, back, _) = call(&r, "GET", "/api/settings", None, None).await;
    assert!(!back.contains("smtp-geheim-42") && !back.contains("influx-token-99"), "{back}");
    let v: Value = serde_json::from_str(&back).unwrap();
    assert_eq!(v["mail"]["password"], "");
    assert_eq!(v["mail"]["password_set"], true);
    assert_eq!(v["metrics"]["influx"]["token_set"], true);
    // Oberfläche schickt zurück, was sie bekam (leere Felder): Werte bleiben
    s = v.clone();
    s["mail"]["to"] = json!("andere@example.org");
    assert_eq!(call(&r, "PUT", "/api/settings", Some(s), None).await.0, StatusCode::OK);
    let file = std::fs::read_to_string(root.join("verwaltung.json")).unwrap();
    assert!(file.contains("smtp-geheim-42") && file.contains("influx-token-99") && file.contains("andere@example.org"));
    for uri in ["/api/config", "/api/config/export", "/api/diagnose.json", "/api/state"] {
        let (_, body, _) = call(&r, "GET", uri, None, None).await;
        assert!(!body.contains("smtp-geheim-42") && !body.contains("influx-token-99"), "Geheimnis in {uri}");
    }
    let (st, body, _) = call(&r, "POST", "/api/mail/test", None, None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let m = next_mail(&mut rx).await;
    assert!(m.data.contains("Testmail") && m.rcpt[0].contains("andere@example.org"), "{m:?}");
    // ohne Server: Hinweis statt Absturz
    let mut off = v;
    off["mail"]["host"] = json!("");
    call(&r, "PUT", "/api/settings", Some(off), None).await;
    let (st, body, _) = call(&r, "POST", "/api/mail/test", None, None).await;
    assert_eq!(st, StatusCode::BAD_GATEWAY);
    assert!(body.contains("Kein Mailserver"), "{body}");
}

/// `/metrics`: offen ohne Benutzer, sonst Sitzung oder HTTP-Basic; per Option freigebbar.
#[tokio::test]
async fn metrics_endpoint_and_auth() {
    let (r, _) = app("metrics");
    let (st, body, _) = call(&r, "GET", "/metrics", None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(body.contains("# TYPE lzs_running gauge") && body.contains("lzs_restarts_total 0") && body.contains("lzs_cpu_percent"), "{body}");
    let (st, _, _) = call(&r, "POST", "/api/users", Some(json!({"name":"prom","password":"messwerte-1","role":"observer"})), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(call(&r, "GET", "/metrics", None, None).await.0, StatusCode::UNAUTHORIZED);
    let basic = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("prom:messwerte-1"));
    assert_eq!(call(&r, "GET", "/metrics", None, Some((header::AUTHORIZATION, &basic))).await.0, StatusCode::OK);
    let wrong = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("prom:falsch"));
    assert_eq!(call(&r, "GET", "/metrics", None, Some((header::AUTHORIZATION, &wrong))).await.0, StatusCode::UNAUTHORIZED);
    let (_, _, cookie) = call(&r, "POST", "/api/login", Some(json!({"name":"prom","password":"messwerte-1"})), None).await;
    let cookie = cookie.unwrap();
    assert_eq!(call(&r, "GET", "/metrics", None, Some((header::COOKIE, &cookie))).await.0, StatusCode::OK);
    let (st, _, _) = call(&r, "PUT", "/api/settings", Some(json!({"metrics": {"public": true}})), Some((header::COOKIE, &cookie))).await;
    assert_eq!(st, StatusCode::OK, "erster Benutzer ist Administrator");
    assert_eq!(call(&r, "GET", "/metrics", None, None).await.0, StatusCode::OK, "freigegeben");
}
