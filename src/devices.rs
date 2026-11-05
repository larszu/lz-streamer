//! Erkennung angeschlossener DeckLink-/UltraStudio-Geräte.

use gstreamer as gst;
use gst::prelude::*;
use serde::Serialize;

use crate::config::{InputConfig, InputKind};
use crate::pipeline::ResolvedInput;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CaptureDevice {
    pub device: u32,
    pub name: String,
    pub direction: &'static str,
}

/// USB-Videogeräte (UVC) und Kameras: (Nummer, Name). macOS über AVFoundation,
/// Linux über V4L2. Das Auflisten öffnet kein Gerät.
pub fn uvc_devices() -> Vec<(u32, String)> {
    let monitor = gst::DeviceMonitor::new();
    monitor.add_filter(Some("Video/Source"), None);
    if monitor.start().is_err() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for dev in monitor.devices() {
        let Ok(el) = dev.create_element(None) else { continue };
        let factory = el.factory().map(|f| f.name().to_string()).unwrap_or_default();
        let index = match factory.as_str() {
            "avfvideosrc" => el.property::<i32>("device-index").max(0) as u32,
            "v4l2src" => el
                .property::<String>("device")
                .trim_start_matches("/dev/video")
                .parse()
                .unwrap_or(0),
            _ => continue,
        };
        out.push((index, dev.display_name().to_string()));
    }
    monitor.stop();
    out.sort();
    out.dedup();
    out
}

/// Fragt den DeckLink-Geräteanbieter von GStreamer ab. Ohne Treiber oder
/// ohne angestecktes Gerät ist die Liste leer.
pub fn decklink_devices() -> Vec<CaptureDevice> {
    if gst::ElementFactory::find("decklinkvideosrc").is_none() {
        return Vec::new();
    }
    let monitor = gst::DeviceMonitor::new();
    monitor.add_filter(Some("Video/Source"), None);
    monitor.add_filter(Some("Video/Sink"), None);
    if monitor.start().is_err() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for dev in monitor.devices() {
        let Ok(el) = dev.create_element(None) else { continue };
        let factory = el.factory().map(|f| f.name().to_string()).unwrap_or_default();
        let direction = match factory.as_str() {
            "decklinkvideosrc" => "input",
            "decklinkvideosink" => "output",
            _ => continue,
        };
        let device = el.property::<i32>("device-number").max(0) as u32;
        out.push(CaptureDevice { device, name: dev.display_name().to_string(), direction });
    }
    monitor.stop();
    out.sort_by_key(|d| (d.direction, d.device));
    out.dedup();
    out
}

pub fn resolve_input(cfg: &InputConfig, devices: &[CaptureDevice]) -> ResolvedInput {
    let has_input = |n: u32| devices.iter().any(|d| d.direction == "input" && d.device == n);
    match cfg.kind {
        InputKind::Test => ResolvedInput::Test,
        InputKind::File => ResolvedInput::File { path: cfg.file.clone() },
        InputKind::Uvc => ResolvedInput::Uvc { index: cfg.uvc_device, audio: cfg.uvc_audio },
        InputKind::Decklink => ResolvedInput::Decklink { device: cfg.device, connection: cfg.connection.gst_nick() },
        InputKind::Auto => {
            if has_input(cfg.device) {
                ResolvedInput::Decklink { device: cfg.device, connection: cfg.connection.gst_nick() }
            } else if let Some(d) = devices.iter().find(|d| d.direction == "input") {
                ResolvedInput::Decklink { device: d.device, connection: cfg.connection.gst_nick() }
            } else {
                ResolvedInput::Test
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Connection;

    #[test]
    fn auto_without_hardware_is_test() {
        assert_eq!(resolve_input(&InputConfig::default(), &[]), ResolvedInput::Test);
    }

    #[test]
    fn auto_with_hardware_uses_decklink() {
        let devs = vec![CaptureDevice { device: 0, name: "UltraStudio Recorder 3G".into(), direction: "input" }];
        let cfg = InputConfig { connection: Connection::Sdi, ..Default::default() };
        assert_eq!(resolve_input(&cfg, &devs), ResolvedInput::Decklink { device: 0, connection: "sdi" });
    }

    #[test]
    fn explicit_test_wins() {
        let devs = vec![CaptureDevice { device: 0, name: "x".into(), direction: "input" }];
        let cfg = InputConfig { kind: InputKind::Test, ..Default::default() };
        assert_eq!(resolve_input(&cfg, &devs), ResolvedInput::Test);
    }
}
