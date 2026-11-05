//! Steuerseite des Grafik-Renderers (Protokoll aus lz-vision-mixer):
//! WebSocket `/renderer`, Hello mit Bildformat und Bild-Port, Laden der
//! Untertitel-Grafik und `update {text}` bei jedem neuen Untertitel.
//! Ebene 2 nimmt beliebige OGraf-Grafiken (Bauchbinde, Liedtexte …), gesteuert
//! über `POST /api/graphics` oder OSC `/lzs/gfx/<aktion>` (z. B. aus Companion).
//! Dazu: OSC `/lzvm/caption` bzw. `/lzs/caption` und das Starten des
//! Renderer-Programms als Hilfsprozess.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::channel::ChildCmd;
use crate::config::{BurnTarget, Config};
use crate::supervisor::Supervisor;

/// Ebene der freien Grafiken
pub const GFX_LAYER: u32 = 2;

#[derive(Debug, Clone, Default, Serialize)]
pub struct RendererState {
    pub connected: bool,
    pub library: Vec<String>,
    /// letzte Zustandsmeldung je Ebene (`layer`-Nachricht des Renderers)
    pub layers: std::collections::BTreeMap<u32, Value>,
    /// was auf Ebene 2 geladen ist und ob sie läuft (für Neuverbindungen)
    pub gfx: GfxState,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GfxState {
    pub graphic_id: Option<String>,
    pub data: Value,
    pub playing: bool,
}

pub struct RendererHub {
    pub state: Mutex<RendererState>,
    pub cmds: tokio::sync::broadcast::Sender<Value>,
}

impl Default for RendererHub {
    fn default() -> Self {
        Self { state: Mutex::default(), cmds: tokio::sync::broadcast::channel(64).0 }
    }
}

/// Befehl für die freie Grafik (API und OSC).
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct GfxCommand {
    /// load, show (laden + abspielen), play, next, prev, update, stop, clear, custom
    pub action: String,
    pub graphic_id: Option<String>,
    pub data: Option<Value>,
    /// Kennung einer eigenen Aktion (custom)
    pub id: Option<String>,
    pub payload: Option<Value>,
}

/// Renderer-Nachrichten für einen Befehl.
pub fn gfx_messages(c: &GfxCommand) -> Result<Vec<Value>, String> {
    let l = GFX_LAYER;
    let load = || -> Result<Value, String> {
        let id = c.graphic_id.as_deref().filter(|s| !s.is_empty()).ok_or("graphicId fehlt")?;
        Ok(json!({"type": "load", "layer": l, "graphicId": id, "data": c.data.clone().unwrap_or(json!({}))}))
    };
    Ok(match c.action.as_str() {
        "load" => vec![load()?],
        "show" => vec![load()?, json!({"type": "play", "layer": l, "params": {}})],
        "play" => vec![json!({"type": "play", "layer": l, "params": {}})],
        "next" => vec![json!({"type": "play", "layer": l, "params": {"delta": 1}})],
        "prev" => vec![json!({"type": "play", "layer": l, "params": {"delta": -1}})],
        "update" => vec![json!({"type": "update", "layer": l, "data": c.data.clone().unwrap_or(json!({}))})],
        "stop" => vec![json!({"type": "stop", "layer": l, "params": {}})],
        "clear" => vec![json!({"type": "clear", "layer": l})],
        "custom" => vec![json!({"type": "custom", "layer": l, "id": c.id.as_deref().filter(|s| !s.is_empty()).ok_or("id fehlt")?, "payload": c.payload.clone().unwrap_or(json!({}))})],
        a => return Err(format!("unbekannte Aktion „{a}“")),
    })
}

impl GfxState {
    /// Zustand nach einem Befehl nachführen
    pub fn apply(&mut self, c: &GfxCommand) {
        match c.action.as_str() {
            "load" | "show" => {
                self.graphic_id = c.graphic_id.clone();
                self.data = c.data.clone().unwrap_or(json!({}));
                self.playing = c.action == "show";
            }
            "play" | "next" | "prev" => self.playing = self.graphic_id.is_some(),
            "update" => {
                if let (Some(o), Some(Value::Object(n))) = (self.data.as_object_mut(), &c.data) {
                    o.extend(n.clone());
                }
            }
            "stop" => self.playing = false,
            "clear" => *self = Self::default(),
            _ => {}
        }
    }

    /// Nachrichten, um den Zustand nach einer Neuverbindung wiederherzustellen
    pub fn restore(&self) -> Vec<Value> {
        let Some(id) = &self.graphic_id else { return vec![] };
        let action = if self.playing { "show" } else { "load" };
        gfx_messages(&GfxCommand { action: action.into(), graphic_id: Some(id.clone()), data: Some(self.data.clone()), ..Default::default() }).unwrap_or_default()
    }
}

/// Befehl an den verbundenen Renderer schicken und merken.
pub async fn gfx_command(hub: &RendererHub, c: &GfxCommand) -> Result<(), String> {
    let msgs = gfx_messages(c)?;
    hub.state.lock().await.gfx.apply(c);
    for m in msgs {
        let _ = hub.cmds.send(m);
    }
    Ok(())
}

pub fn hello(cfg: &Config) -> Value {
    let v = &cfg.encoder.video;
    json!({"type": "hello", "format": {"width": v.width, "height": v.height, "fps": v.fps}, "layers": 2, "framePorts": cfg.encoder.graphics.layer_ports()})
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
    let mut cmds = hub.cmds.subscribe();
    let mut cfg = sup.config().await;
    let send = |v: Value| Message::Text(v.to_string().into());
    if tx.send(send(hello(&cfg))).await.is_err() {
        return;
    }
    if cfg.encoder.graphics.burn != BurnTarget::Off {
        for m in load(&cfg) {
            let _ = tx.send(send(m)).await;
        }
        let restore = hub.state.lock().await.gfx.restore();
        for m in restore {
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
                        Some("layer") => {
                            let n = v["layer"].as_u64().unwrap_or(1) as u32;
                            st.layers.insert(n, v);
                        }
                        _ => {}
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
            m = cmds.recv() => {
                if let Ok(m) = m {
                    if tx.send(send(m)).await.is_err() { break; }
                }
            }
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
                        let restore = hub.state.lock().await.gfx.restore();
                        for m in restore { let _ = tx.send(send(m)).await; }
                    }
                }
            }
        }
    }
    {
        // Grafikzustand behalten, damit ein neu verbundener Renderer ihn wieder zeigt
        let mut st = hub.state.lock().await;
        let gfx = std::mem::take(&mut st.gfx);
        *st = RendererState { gfx, ..Default::default() };
    }
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

/// OSC `/lzs/gfx/<aktion>` → Grafikbefehl. Argumente: `load`/`show` s:graphicId
/// [s:JSON-Daten], `update` s:JSON, `custom` s:id [s:JSON]; die übrigen ohne.
pub fn osc_gfx(packet: &rosc::OscPacket) -> Option<GfxCommand> {
    match packet {
        rosc::OscPacket::Message(m) => {
            let action = m.addr.strip_prefix("/lzs/gfx/")?.to_string();
            let strs: Vec<&str> = m.args.iter().filter_map(|a| if let rosc::OscType::String(s) = a { Some(s.as_str()) } else { None }).collect();
            let js = |s: Option<&&str>| s.and_then(|s| serde_json::from_str::<Value>(s).ok());
            let mut c = GfxCommand { action: action.clone(), ..Default::default() };
            match action.as_str() {
                "load" | "show" => {
                    c.graphic_id = strs.first().map(|s| s.to_string());
                    c.data = js(strs.get(1));
                }
                "update" => c.data = js(strs.first()),
                "custom" => {
                    c.id = strs.first().map(|s| s.to_string());
                    c.payload = js(strs.get(1));
                }
                _ => {}
            }
            Some(c)
        }
        rosc::OscPacket::Bundle(b) => b.content.iter().find_map(osc_gfx),
    }
}

/// OSC-Empfang für Untertitel (z. B. aus dem Intercom) und Grafikbefehle.
pub async fn osc_listener(addr: String, sup: Arc<Supervisor>, hub: Arc<RendererHub>) {
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
            } else if let Some(c) = osc_gfx(&p) {
                if let Err(e) = gfx_command(&hub, &c).await {
                    sup.log("warn", format!("OSC-Grafikbefehl: {e}")).await;
                }
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
    fn gfx_commands_map_to_layer_2() {
        let c = |a: &str| GfxCommand { action: a.into(), graphic_id: Some("lz-lower-third".into()), data: Some(json!({"name": "Anna"})), ..Default::default() };
        let m = gfx_messages(&c("show")).unwrap();
        assert_eq!((m[0]["type"].as_str(), m[0]["layer"].as_u64(), m[0]["graphicId"].as_str()), (Some("load"), Some(2), Some("lz-lower-third")));
        assert_eq!(m[1]["type"], "play");
        assert_eq!(gfx_messages(&c("next")).unwrap()[0]["params"]["delta"], 1);
        assert_eq!(gfx_messages(&c("prev")).unwrap()[0]["params"]["delta"], -1);
        assert!(gfx_messages(&GfxCommand { action: "load".into(), ..Default::default() }).is_err());
        assert!(gfx_messages(&c("tanzen")).is_err());

        let mut st = GfxState::default();
        st.apply(&c("show"));
        st.apply(&GfxCommand { action: "update".into(), data: Some(json!({"title": "Technik"})), ..Default::default() });
        let r = st.restore();
        assert_eq!(r.len(), 2, "geladen und laufend");
        assert_eq!(r[0]["data"], json!({"name": "Anna", "title": "Technik"}));
        st.apply(&c("stop"));
        assert_eq!(st.restore().len(), 1, "nur geladen");
        st.apply(&c("clear"));
        assert!(st.restore().is_empty());
    }

    #[test]
    fn osc_gfx_addresses() {
        let m = |addr: &str, args: &[&str]| rosc::OscPacket::Message(rosc::OscMessage { addr: addr.into(), args: args.iter().map(|s| rosc::OscType::String(s.to_string())).collect() });
        let c = osc_gfx(&m("/lzs/gfx/show", &["lz-lower-third", r#"{"name":"Anna"}"#])).unwrap();
        assert_eq!((c.action.as_str(), c.graphic_id.as_deref(), c.data), ("show", Some("lz-lower-third"), Some(json!({"name": "Anna"}))));
        assert_eq!(osc_gfx(&m("/lzs/gfx/next", &[])).unwrap().action, "next");
        assert_eq!(osc_gfx(&m("/lzs/caption", &["x"])), None);
    }

    #[test]
    fn osc_caption_addresses() {
        let m = |addr: &str| rosc::OscPacket::Message(rosc::OscMessage { addr: addr.into(), args: vec![rosc::OscType::String("Guten Abend".into())] });
        assert_eq!(osc_caption(&m("/lzvm/caption")).as_deref(), Some("Guten Abend"));
        assert_eq!(osc_caption(&m("/lzs/caption")).as_deref(), Some("Guten Abend"));
        assert_eq!(osc_caption(&m("/lzvm/cut")), None);
    }
}
