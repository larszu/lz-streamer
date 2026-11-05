//! Steuerseite des Grafik-Renderers (Protokoll aus lz-vision-mixer):
//! WebSocket `/renderer`, Hello mit Bildformat und Bild-Port, Laden der
//! Untertitel-Grafik und `update {text}` bei jedem neuen Untertitel.
//! Dazu: OSC `/lzvm/caption` bzw. `/lzs/caption` und das Starten des
//! Renderer-Programms als Hilfsprozess.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::channel::ChildCmd;
use crate::config::{BurnTarget, Config};
use crate::supervisor::Supervisor;

#[derive(Debug, Clone, Default, Serialize)]
pub struct RendererState {
    pub connected: bool,
    pub library: Vec<String>,
    pub layer: Value,
}

#[derive(Default)]
pub struct RendererHub {
    pub state: Mutex<RendererState>,
}

pub fn hello(cfg: &Config) -> Value {
    let v = &cfg.encoder.video;
    json!({"type": "hello", "format": {"width": v.width, "height": v.height, "fps": v.fps}, "layers": 1, "framePorts": [cfg.encoder.graphics.frame_port]})
}

fn load(cfg: &Config) -> [Value; 2] {
    [
        json!({"type": "load", "layer": 1, "graphicId": cfg.encoder.graphics.graphic, "data": {"text": "", "autoHideS": 6}}),
        json!({"type": "play", "layer": 1, "params": {}}),
    ]
}

pub fn update(text: &str) -> Value {
    json!({"type": "update", "layer": 1, "data": {"text": text}})
}

fn key(cfg: &Config) -> (u32, u32, u32, u16, String, BurnTarget) {
    let v = &cfg.encoder.video;
    let g = &cfg.encoder.graphics;
    (v.width, v.height, v.fps, g.frame_port, g.graphic.clone(), g.burn)
}

/// Eine Renderer-Verbindung bedienen.
pub async fn serve(socket: WebSocket, sup: Arc<Supervisor>, hub: Arc<RendererHub>) {
    let (mut tx, mut rx) = socket.split();
    let mut captions = sup.captions.subscribe();
    let mut cfg = sup.config().await;
    let send = |v: Value| Message::Text(v.to_string().into());
    if tx.send(send(hello(&cfg))).await.is_err() {
        return;
    }
    if cfg.encoder.graphics.burn != BurnTarget::Off {
        for m in load(&cfg) {
            let _ = tx.send(send(m)).await;
        }
    }
    hub.state.lock().await.connected = true;
    sup.log("ok", "Grafik-Renderer verbunden").await;
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            m = rx.next() => match m {
                Some(Ok(Message::Text(t))) => {
                    let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                    let mut st = hub.state.lock().await;
                    match v["type"].as_str() {
                        Some("library") => {
                            st.library = v["graphics"].as_array().map(|a| a.iter().filter_map(|g| g["id"].as_str().map(String::from)).collect()).unwrap_or_default();
                        }
                        Some("layer") => st.layer = v,
                        _ => {}
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
            c = captions.recv() => {
                if let Ok(text) = c {
                    if tx.send(send(update(&text))).await.is_err() { break; }
                }
            }
            _ = tick.tick() => {
                // Format oder Grafik geändert: neu begrüßen, Grafik neu laden
                let now = sup.config().await;
                if key(&now) != key(&cfg) {
                    cfg = now;
                    if tx.send(send(hello(&cfg))).await.is_err() { break; }
                    if cfg.encoder.graphics.burn != BurnTarget::Off {
                        for m in load(&cfg) { let _ = tx.send(send(m)).await; }
                    }
                }
            }
        }
    }
    *hub.state.lock().await = RendererState::default();
    sup.log("warn", "Grafik-Renderer getrennt – Bild läuft ohne Grafik weiter").await;
}

/// Text aus einer OSC-Nachricht `/lzvm/caption s:text` oder `/lzs/caption s:text`.
pub fn osc_caption(packet: &rosc::OscPacket) -> Option<String> {
    match packet {
        rosc::OscPacket::Message(m) if m.addr == "/lzvm/caption" || m.addr == "/lzs/caption" => m.args.iter().find_map(|a| match a {
            rosc::OscType::String(s) => Some(s.clone()),
            _ => None,
        }),
        rosc::OscPacket::Bundle(b) => b.content.iter().find_map(osc_caption),
        _ => None,
    }
}

/// OSC-Empfang für Untertitel (z. B. aus dem Intercom).
pub async fn osc_listener(addr: String, sup: Arc<Supervisor>) {
    let Ok(sock) = tokio::net::UdpSocket::bind(&addr).await else {
        sup.log("warn", format!("OSC-Port {addr} belegt – keine Untertitel per OSC")).await;
        return;
    };
    let mut buf = vec![0u8; 65536];
    loop {
        let Ok((n, _)) = sock.recv_from(&mut buf).await else { continue };
        if let Ok((_, p)) = rosc::decoder::decode_udp(&buf[..n]) {
            if let Some(text) = osc_caption(&p) {
                sup.command(&ChildCmd::Caption { text }).await;
            }
        }
    }
}

/// Startet das Renderer-Programm, solange Einbrennen eingeschaltet und ein
/// Programm eingetragen ist; startet es nach einem Absturz neu.
pub async fn keep_running(sup: Arc<Supervisor>, engine_url: String) {
    loop {
        let cfg = sup.config().await;
        let g = &cfg.encoder.graphics;
        if g.burn == BurnTarget::Off || g.renderer.trim().is_empty() {
            tokio::time::sleep(Duration::from_secs(3)).await;
            continue;
        }
        let mut exe = std::path::PathBuf::from(g.renderer.trim());
        if exe.extension().is_some_and(|e| e == "app") {
            let name = exe.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            exe = exe.join("Contents/MacOS").join(name);
        }
        let mut cmd = tokio::process::Command::new(&exe);
        cmd.arg("--engine").arg(&engine_url).arg("--hidden").kill_on_drop(true);
        if !g.graphics_dir.trim().is_empty() {
            cmd.arg("--graphics").arg(g.graphics_dir.trim());
        }
        match cmd.spawn() {
            Ok(mut child) => {
                sup.log("info", format!("Grafik-Renderer gestartet ({})", exe.display())).await;
                let _ = child.wait().await;
                sup.log("warn", "Grafik-Renderer beendet – Neustart in 5 s").await;
            }
            Err(e) => sup.log("error", format!("Grafik-Renderer startet nicht: {e}")).await,
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_and_update_messages() {
        let mut c = Config::default();
        c.encoder.video.width = 1920;
        c.encoder.graphics.frame_port = 4301;
        let h = hello(&c);
        assert_eq!(h["format"]["width"], 1920);
        assert_eq!(h["framePorts"][0], 4301);
        assert_eq!(update("Hallo")["data"]["text"], "Hallo");
    }

    #[test]
    fn osc_caption_addresses() {
        let m = |addr: &str| rosc::OscPacket::Message(rosc::OscMessage { addr: addr.into(), args: vec![rosc::OscType::String("Guten Abend".into())] });
        assert_eq!(osc_caption(&m("/lzvm/caption")).as_deref(), Some("Guten Abend"));
        assert_eq!(osc_caption(&m("/lzs/caption")).as_deref(), Some("Guten Abend"));
        assert_eq!(osc_caption(&m("/lzvm/cut")), None);
    }
}
