//! HTTP-API: Anmeldung/Rollen, Profile, Standbilder, Zeitplan/Alarme,
//! Aufnahmen, Bericht. Ohne laufenden Kanal (der Aufpasser startet hier nicht).

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use lz_streamer::admin::DataDir;
use lz_streamer::caps::Capabilities;
use lz_streamer::config::Config;
use lz_streamer::ops::Ops;
use lz_streamer::server::{App, router};
use lz_streamer::supervisor::Supervisor;
use serde_json::{Value, json};
use tower::ServiceExt;

fn app(tag: &str) -> (axum::Router, std::path::PathBuf) {
    let _ = gstreamer::init();
    let root = std::env::temp_dir().join(format!("lzs-api-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let data = DataDir::new(root.clone());
    let sup = Supervisor::new(Config::default(), root.join("config.json"), root.join("run"), data.recordings());
    let ops = Ops::new(data);
    (router(App::new(sup, ops, Capabilities::probe(), vec![])), root)
}

async fn call(r: &axum::Router, method: &str, uri: &str, body: Option<Value>, cookie: Option<&str>) -> (StatusCode, Value, Option<String>) {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    let req = match body {
        Some(v) => b.header(header::CONTENT_TYPE, "application/json").body(Body::from(v.to_string())).unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let resp = r.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let set_cookie = resp.headers().get(header::SET_COOKIE).map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null), set_cookie)
}

#[tokio::test]
async fn login_and_roles() {
    let (r, _) = app("auth");
    // offen, solange es keine Benutzer gibt
    assert_eq!(call(&r, "GET", "/api/state", None, None).await.0, StatusCode::OK);
    let (s, _, _) = call(&r, "POST", "/api/users", Some(json!({"name":"lars","password":"kurz","role":"admin"})), None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "zu kurzes Passwort");
    let (s, _, _) = call(&r, "POST", "/api/users", Some(json!({"name":"lars","password":"sicher-genug","role":"observer"})), None).await;
    assert_eq!(s, StatusCode::OK);
    // jetzt geschlossen
    assert_eq!(call(&r, "GET", "/api/state", None, None).await.0, StatusCode::UNAUTHORIZED);
    let (s, _, _) = call(&r, "POST", "/api/login", Some(json!({"name":"lars","password":"falsch"})), None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, v, cookie) = call(&r, "POST", "/api/login", Some(json!({"name":"lars","password":"sicher-genug"})), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["role"], "admin", "erster Benutzer wird immer Administrator");
    let admin = cookie.unwrap();
    assert_eq!(call(&r, "GET", "/api/state", None, Some(&admin)).await.0, StatusCode::OK);
    // Bediener anlegen: darf starten, aber keine Einstellungen ändern
    let (s, _, _) = call(&r, "POST", "/api/users", Some(json!({"name":"technik","password":"bediener-123","role":"operator"})), Some(&admin)).await;
    assert_eq!(s, StatusCode::OK);
    let (_, _, op) = call(&r, "POST", "/api/login", Some(json!({"name":"technik","password":"bediener-123"})), None).await;
    let op = op.unwrap();
    assert_eq!(call(&r, "POST", "/api/stop", None, Some(&op)).await.0, StatusCode::OK);
    assert_eq!(call(&r, "PUT", "/api/config", Some(json!({})), Some(&op)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&r, "POST", "/api/users", Some(json!({"name":"x","password":"12345678","role":"admin"})), Some(&op)).await.0, StatusCode::FORBIDDEN);
    // letzter Admin lässt sich nicht löschen
    assert_eq!(call(&r, "DELETE", "/api/users/lars", None, Some(&admin)).await.0, StatusCode::BAD_REQUEST);
    // abmelden
    call(&r, "POST", "/api/logout", None, Some(&op)).await;
    assert_eq!(call(&r, "GET", "/api/state", None, Some(&op)).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn presets_save_and_load() {
    let (r, _) = app("presets");
    let (_, list, _) = call(&r, "GET", "/api/presets", None, None).await;
    assert!(list.as_array().unwrap().iter().any(|p| p["builtin"] == true));
    let (s, _, _) = call(&r, "POST", "/api/presets/H.265%202160p50%20UHD%20(HD%20wird%20hochskaliert)/load", None, None).await;
    assert_eq!(s, StatusCode::OK);
    let (_, cfg, _) = call(&r, "GET", "/api/config", None, None).await;
    assert_eq!(cfg["encoder"]["video"]["width"], 3840);
    assert_eq!(cfg["encoder"]["video"]["codec"], "h265");
    assert_eq!(call(&r, "POST", "/api/presets", Some(json!({"name":"Gottesdienst"})), None).await.0, StatusCode::OK);
    let (_, list, _) = call(&r, "GET", "/api/presets", None, None).await;
    assert!(list.as_array().unwrap().iter().any(|p| p["name"] == "Gottesdienst"));
    assert_eq!(call(&r, "POST", "/api/presets", Some(json!({"name":"../boese"})), None).await.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn standby_images() {
    let (r, _) = app("standby");
    let png = [0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0];
    let req = Request::builder().method("POST").uri("/api/standby?name=Pause.png").body(Body::from(png.to_vec())).unwrap();
    assert_eq!(r.clone().oneshot(req).await.unwrap().status(), StatusCode::OK);
    let req = Request::builder().method("POST").uri("/api/standby?name=x.png").body(Body::from("kein Bild")).unwrap();
    assert_eq!(r.clone().oneshot(req).await.unwrap().status(), StatusCode::BAD_REQUEST);
    let (_, list, _) = call(&r, "GET", "/api/standby", None, None).await;
    assert_eq!(list[0]["name"], "Pause.png");
    assert_eq!(call(&r, "POST", "/api/standby/Pause.png/use", None, None).await.0, StatusCode::OK);
    let (_, cfg, _) = call(&r, "GET", "/api/config", None, None).await;
    assert_eq!(cfg["encoder"]["standby"]["pattern"], "image");
    assert!(cfg["encoder"]["standby"]["image_path"].as_str().unwrap().ends_with("Pause.png"));
    assert_eq!(call(&r, "DELETE", "/api/standby/Pause.png", None, None).await.0, StatusCode::OK);
}

#[tokio::test]
async fn schedule_alarms_report_recordings() {
    let (r, root) = app("settings");
    let s = json!({"schedule":[{"name":"Abendsendung","days":[1,2,3,4,5,6,7],"start":"20:00","stop":"21:30"}],"alarms":{"no_peer_s":10}});
    assert_eq!(call(&r, "PUT", "/api/settings", Some(s), None).await.0, StatusCode::OK);
    let (_, back, _) = call(&r, "GET", "/api/settings", None, None).await;
    assert_eq!(back["schedule"][0]["name"], "Abendsendung");
    assert_eq!(back["alarms"]["no_peer_s"], 10);
    assert_eq!(back["alarms"]["cpu_percent"], 90, "Vorgaben für fehlende Felder");

    let resp = r.clone().oneshot(Request::get("/api/report.csv").body(Body::empty()).unwrap()).await.unwrap();
    assert!(resp.headers()[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/csv"));

    std::fs::write(root.join("aufnahmen").join("lz-test_000.ts"), [0x47u8; 188]).unwrap();
    let (_, list, _) = call(&r, "GET", "/api/recordings", None, None).await;
    assert_eq!(list[0]["name"], "lz-test_000.ts");
    assert_eq!(list[0]["bytes"], 188);
    let resp = r.clone().oneshot(Request::get("/api/recordings/lz-test_000.ts").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.into_body().collect().await.unwrap().to_bytes().len(), 188);
    // Untertiteldateien gehören zur Aufnahme: gelistet, ladbar, mitgelöscht
    std::fs::write(root.join("aufnahmen").join("lz-test_000.vtt"), "WEBVTT\n\n").unwrap();
    std::fs::write(root.join("aufnahmen").join("lz-test_000.srt"), "1\n").unwrap();
    let (_, list, _) = call(&r, "GET", "/api/recordings", None, None).await;
    assert_eq!(list.as_array().unwrap().len(), 1, "Untertitel nicht als eigene Aufnahme");
    assert_eq!(list[0]["subtitles"], json!(["lz-test_000.srt", "lz-test_000.vtt"]));
    let resp = r.clone().oneshot(Request::get("/api/recordings/lz-test_000.vtt").body(Body::empty()).unwrap()).await.unwrap();
    assert!(resp.headers()[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/vtt"));
    assert_eq!(call(&r, "DELETE", "/api/recordings/lz-test_000.ts", None, None).await.0, StatusCode::OK);
    assert!(!root.join("aufnahmen").join("lz-test_000.vtt").exists(), "Untertitel blieben liegen");
    assert_eq!(call(&r, "GET", "/api/recordings/..%2Fconfig.json", None, None).await.0, StatusCode::BAD_REQUEST);
}

/// Renderer-Protokoll: Hello mit Format und Bild-Port, Laden der
/// Untertitel-Grafik bei aktivem Einbrennen, `update {text}` je Untertitel.
#[tokio::test]
async fn renderer_websocket_protocol() {
    use futures_util::{SinkExt, StreamExt};
    let _ = gstreamer::init();
    let root = std::env::temp_dir().join(format!("lzs-api-ws-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let data = DataDir::new(root.clone());
    let mut cfg = Config::default();
    cfg.encoder.graphics.burn = lz_streamer::config::BurnTarget::All;
    cfg.encoder.graphics.frame_port = 4777;
    let sup = Supervisor::new(cfg, root.join("config.json"), root.join("run"), data.recordings());
    let app = App::new(sup.clone(), Ops::new(data), Capabilities::probe(), vec![]);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router(app).into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap();
    });
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/renderer")).await.unwrap();
    let mut next = async || -> Value {
        loop {
            if let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(t))) = ws.next().await {
                return serde_json::from_str(&t).unwrap();
            }
        }
    };
    let hello = next().await;
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["framePorts"][0], 4777);
    assert_eq!(hello["format"]["width"], 1920);
    let load = next().await;
    assert_eq!((load["type"].as_str(), load["graphicId"].as_str()), (Some("load"), Some("lz-captions")));
    assert_eq!(next().await["type"], "play");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    sup.captions.send("Guten Abend".into()).unwrap();
    let up = next().await;
    assert_eq!(up["type"], "update");
    assert_eq!(up["data"]["text"], "Guten Abend");
    drop(next);
    ws.send(tokio_tungstenite::tungstenite::Message::Text(r#"{"type":"library","graphics":[{"id":"lz-captions"}]}"#.into())).await.unwrap();
}

/// PDF-Bericht: entsteht mit erwarteten Abschnitten (braucht Chrome; sonst HTML).
#[tokio::test]
async fn report_pdf_has_sections() {
    let (r, _) = app("pdf");
    let resp = r.clone().oneshot(Request::get("/api/report.pdf").body(Body::empty()).unwrap()).await.unwrap();
    let ct = resp.headers()[header::CONTENT_TYPE].to_str().unwrap().to_string();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    if lz_streamer::report::find_browser().is_none() {
        assert!(ct.starts_with("text/html"));
        return;
    }
    assert_eq!(ct, "application/pdf");
    assert!(bytes.starts_with(b"%PDF"), "kein PDF");
    let f = std::env::temp_dir().join(format!("lzs-bericht-{}.pdf", std::process::id()));
    std::fs::write(&f, &bytes).unwrap();
    if let Ok(o) = std::process::Command::new("pdftotext").arg(&f).arg("-").output() {
        let t = String::from_utf8_lossy(&o.stdout);
        for s in ["Betriebsbericht", "Kennzahlen", "Verlauf", "Ereignisse und Alarme"] {
            assert!(t.contains(s), "Abschnitt {s} fehlt im PDF");
        }
    }
}

/// U4: freie Grafik auf Ebene 2 über `POST /api/graphics`; ein neu verbundener
/// Renderer bekommt die laufende Grafik wieder.
#[tokio::test]
async fn graphics_layer2_commands_and_restore() {
    use futures_util::StreamExt;
    let _ = gstreamer::init();
    let root = std::env::temp_dir().join(format!("lzs-api-gfx-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let data = DataDir::new(root.clone());
    let mut cfg = Config::default();
    cfg.encoder.graphics.burn = lz_streamer::config::BurnTarget::All;
    cfg.encoder.graphics.frame_port = 4777;
    let sup = Supervisor::new(cfg, root.join("config.json"), root.join("run"), data.recordings());
    let app = App::new(sup.clone(), Ops::new(data), Capabilities::probe(), vec![]);
    let r = router(app.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let r2 = r.clone();
    tokio::spawn(async move {
        axum::serve(listener, r2.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap();
    });
    let connect = || async { tokio_tungstenite::connect_async(format!("ws://{addr}/renderer")).await.unwrap().0 };
    async fn next(ws: &mut (impl StreamExt<Item = Result<tokio_tungstenite::tungstenite::Message, tokio_tungstenite::tungstenite::Error>> + Unpin)) -> Value {
        loop {
            if let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(t))) = ws.next().await {
                return serde_json::from_str(&t).unwrap();
            }
        }
    }
    let mut ws = connect().await;
    let hello = next(&mut ws).await;
    assert_eq!(hello["layers"], 2);
    assert_eq!(hello["framePorts"], json!([4777, 4778]));
    assert_eq!(next(&mut ws).await["graphicId"], "lz-captions");
    assert_eq!(next(&mut ws).await["type"], "play");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let show = json!({"action": "show", "graphicId": "lz-lower-third", "data": {"name": "Anna Beispiel", "title": "Technik"}});
    let (st, _, _) = call(&r, "POST", "/api/graphics", Some(show), None).await;
    assert_eq!(st, StatusCode::OK);
    let l = next(&mut ws).await;
    assert_eq!((l["type"].as_str(), l["layer"].as_u64(), l["graphicId"].as_str()), (Some("load"), Some(2), Some("lz-lower-third")));
    assert_eq!(l["data"]["name"], "Anna Beispiel");
    assert_eq!(next(&mut ws).await["type"], "play");
    call(&r, "POST", "/api/graphics", Some(json!({"action": "next"})), None).await;
    assert_eq!(next(&mut ws).await["params"]["delta"], 1);
    call(&r, "POST", "/api/graphics", Some(json!({"action": "update", "data": {"title": "Ton"}})), None).await;
    assert_eq!(next(&mut ws).await["data"]["title"], "Ton");
    assert_eq!(call(&r, "POST", "/api/graphics", Some(json!({"action": "tanzen"})), None).await.0, StatusCode::BAD_REQUEST);

    // Renderer neu verbunden: Untertitel plus laufende Bauchbinde mit aktuellem Text
    drop(ws);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let mut ws = connect().await;
    let msgs: Vec<Value> = { let mut v = vec![]; for _ in 0..5 { v.push(next(&mut ws).await); } v };
    let l2: Vec<&Value> = msgs.iter().filter(|m| m["layer"] == 2).collect();
    assert_eq!(l2.len(), 2, "{msgs:?}");
    assert_eq!(l2[0]["data"], json!({"name": "Anna Beispiel", "title": "Ton"}));
    assert_eq!(l2[1]["type"], "play");
    let (_, state, _) = call(&r, "GET", "/api/graphics", None, None).await;
    assert_eq!(state["gfx"]["graphic_id"], "lz-lower-third");
    assert_eq!(state["gfx"]["playing"], true);
}

/// I1: `/api/control` prüft Pfad und Wert, speichert ohne Neustart, `/api/state` zeigt die Werte.
#[tokio::test]
async fn control_endpoint() {
    let (r, _) = app("control");
    let (st, body, _) = call(&r, "POST", "/api/control", Some(json!({"path": "audio.tracks.0.leveler.preset", "value": "soft"})), None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let (st, _, _) = call(&r, "POST", "/api/control", Some(json!({"path": "audio.tracks.0.leveler.preset", "value": "laut"})), None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(call(&r, "POST", "/api/control", Some(json!({"path": "video.width", "value": 1})), None).await.0, StatusCode::BAD_REQUEST);
    call(&r, "POST", "/api/control", Some(json!({"path": "captions.enabled", "value": false})), None).await;
    let (_, state, _) = call(&r, "GET", "/api/state", None, None).await;
    let c = state["controls"].as_array().unwrap();
    let v = |p: &str| c.iter().find(|x| x["path"] == p).map(|x| x["value"].clone());
    assert_eq!(v("captions.enabled"), Some(json!(false)));
    assert_eq!(v("audio.tracks.0.leveler.preset"), Some(json!("soft")));
    assert_eq!(v("graphics.layer2.visible"), Some(json!(false)));
    assert_eq!(state["config"]["encoder"]["captions"]["muted"], true);
    // ohne laufenden Kanal kein Neustart ausgelöst
    assert!(!state["runtime"]["events"].as_array().unwrap().iter().any(|e| e["text"].as_str().unwrap_or("").contains("startet neu")));
}

/// KB-Überwachung: Diagnose-Paket schwärzt Geheimnisse, Export enthält sie (für ein zweites Gerät).
#[tokio::test]
async fn diagnose_and_export() {
    let (r, _) = app("diag");
    let mut cfg = Config::default();
    cfg.encoder.srt.passphrase = "geheim-123456".into();
    let (st, _, _) = call(&r, "PUT", "/api/config", Some(serde_json::to_value(&cfg).unwrap()), None).await;
    assert_eq!(st, StatusCode::OK);
    let get = |uri: &'static str| {
        let r = r.clone();
        async move {
            let resp = r.oneshot(Request::get(uri).body(Body::empty()).unwrap()).await.unwrap();
            let cd = resp.headers()[header::CONTENT_DISPOSITION].to_str().unwrap().to_string();
            (cd, String::from_utf8(resp.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap())
        }
    };
    let (cd, diag) = get("/api/diagnose.json").await;
    assert!(cd.contains("attachment") && cd.contains("diagnose"), "{cd}");
    assert!(!diag.contains("geheim-123456"), "Passwort im Diagnose-Paket");
    let v: Value = serde_json::from_str(&diag).unwrap();
    for k in ["version", "gstreamer", "system", "konfiguration", "laufzeit", "statistik_letzte_stunde"] {
        assert!(v.get(k).is_some(), "{k} fehlt");
    }
    let (_, exp) = get("/api/config/export").await;
    let back: Config = serde_json::from_str(&exp).unwrap();
    assert_eq!(back.encoder.srt.passphrase, "geheim-123456");
}
