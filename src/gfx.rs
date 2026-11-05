//! Grafik einbrennen (OGraf über den Renderer aus lz-vision-mixer).
//!
//! Protokoll wie in lz-vision-mixer `docs/api.md` („Renderer protocol“):
//! Der Renderer verbindet sich per WebSocket mit `/renderer` am Webserver
//! (Steuerung, siehe `server.rs`) und schreibt je Ebene rohe BGRA-Bilder
//! (`Breite × Höhe × 4` Byte, ohne Kopf, nicht vormultipliziert) auf einen
//! TCP-Port. Den Port öffnet der Kanalprozess selbst; das letzte Bild bleibt
//! stehen, ein ganz durchsichtiges Bild löscht die Ebene.
//!
//! Eingeblendet wird mit `overlaycomposition` nur das Rechteck, in dem die
//! Grafik etwas zeichnet. Fällt der Renderer weg, verschwindet die Grafik, das
//! Bild läuft weiter, und Untertiteltext erscheint über `textoverlay`.

use std::io::Read;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gst::prelude::*;
use gstreamer as gst;
use gstreamer_video as gst_video;

/// Kleinstes Rechteck mit Deckkraft > 0 (x, y, w, h) in einem BGRA-Bild.
pub fn alpha_bbox(bgra: &[u8], width: u32, height: u32) -> Option<(u32, u32, u32, u32)> {
    let (w, h) = (width as usize, height as usize);
    if bgra.len() < w * h * 4 {
        return None;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    for y in 0..h {
        let row = &bgra[y * w * 4..(y + 1) * w * 4];
        let mut first = None;
        let mut last = None;
        for x in 0..w {
            if row[x * 4 + 3] != 0 {
                first.get_or_insert(x);
                last = Some(x);
            }
        }
        if let (Some(f), Some(l)) = (first, last) {
            x0 = x0.min(f);
            x1 = x1.max(l);
            y0 = y0.min(y);
            y1 = y1.max(y);
        }
    }
    (x0 != usize::MAX).then(|| (x0 as u32, y0 as u32, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32))
}

/// Ausschnitt eines BGRA-Bildes kopieren.
pub fn crop(bgra: &[u8], width: u32, r: (u32, u32, u32, u32)) -> Vec<u8> {
    let (x, y, w, h) = r;
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for row in y..y + h {
        let start = ((row * width + x) * 4) as usize;
        out.extend_from_slice(&bgra[start..start + (w * 4) as usize]);
    }
    out
}

/// je Ebene das letzte sichtbare Rechteck
#[derive(Default)]
struct Latest {
    layers: Vec<Option<gst_video::VideoOverlayRectangle>>,
}

impl Latest {
    fn composition(&self) -> Option<gst_video::VideoOverlayComposition> {
        let rects: Vec<&gst_video::VideoOverlayRectangle> = self.layers.iter().flatten().collect();
        if rects.is_empty() { None } else { gst_video::VideoOverlayComposition::new(rects).ok() }
    }
}

/// Nimmt Renderer-Bilder auf einem TCP-Port je Ebene entgegen. Ebene 1 trägt
/// die Untertitel, Ebene 2 freie Grafiken (Bauchbinde, Liedtexte …); sie
/// werden übereinander gelegt, Ebene 2 oben.
pub struct FrameReceiver {
    latest: Arc<Mutex<Latest>>,
    connected: Arc<Vec<AtomicBool>>,
    pub ports: Vec<u16>,
}

fn rectangle_from(bgra: &[u8], width: u32, height: u32) -> Option<gst_video::VideoOverlayRectangle> {
    let r = alpha_bbox(bgra, width, height)?;
    let pixels = crop(bgra, width, r);
    let mut buf = gst::Buffer::from_mut_slice(pixels);
    gst_video::VideoMeta::add(buf.get_mut().unwrap(), gst_video::VideoFrameFlags::empty(), gst_video::VideoFormat::Bgra, r.2, r.3).ok()?;
    Some(gst_video::VideoOverlayRectangle::new_raw(&buf, r.0 as i32, r.1 as i32, r.2, r.3, gst_video::VideoOverlayFormatFlags::empty()))
}

#[cfg(test)]
fn composition_from(bgra: &[u8], width: u32, height: u32) -> Option<gst_video::VideoOverlayComposition> {
    let r = rectangle_from(bgra, width, height)?;
    gst_video::VideoOverlayComposition::new([&r]).ok()
}

impl FrameReceiver {
    /// Ports öffnen (0 = frei wählen) und im Hintergrund Bilder lesen. Die
    /// Größe muss dem Ausgabeformat entsprechen (so meldet es der Webserver
    /// dem Renderer).
    pub fn listen(ports: &[u16], width: u32, height: u32) -> std::io::Result<Self> {
        // Ebene 1 (Untertitel) ist Pflicht; ist ein weiterer Port belegt, fehlt nur diese Ebene
        let mut listeners = vec![TcpListener::bind(("127.0.0.1", ports[0]))?];
        listeners.extend(ports[1..].iter().map_while(|p| TcpListener::bind(("127.0.0.1", *p)).ok()));
        let ports = listeners.iter().map(|l| l.local_addr().map(|a| a.port())).collect::<std::io::Result<Vec<_>>>()?;
        let latest = Arc::new(Mutex::new(Latest { layers: vec![None; listeners.len()] }));
        let connected: Arc<Vec<AtomicBool>> = Arc::new(listeners.iter().map(|_| AtomicBool::new(false)).collect());
        for (i, listener) in listeners.into_iter().enumerate() {
            let (l2, c2) = (latest.clone(), connected.clone());
            std::thread::spawn(move || {
                let size = (width * height * 4) as usize;
                for stream in listener.incoming() {
                    let Ok(mut s) = stream else { continue };
                    c2[i].store(true, Ordering::Relaxed);
                    let mut frame = vec![0u8; size];
                    while s.read_exact(&mut frame).is_ok() {
                        let r = rectangle_from(&frame, width, height);
                        l2.lock().unwrap().layers[i] = r;
                    }
                    // Verbindung weg: Grafik entfernen, Bild läuft ohne sie weiter
                    c2[i].store(false, Ordering::Relaxed);
                    l2.lock().unwrap().layers[i] = None;
                }
            });
        }
        Ok(Self { latest, connected, ports })
    }

    /// Port der ersten Ebene (Untertitel)
    pub fn port(&self) -> u16 {
        self.ports[0]
    }

    pub fn connected(&self) -> bool {
        self.connected.iter().any(|c| c.load(Ordering::Relaxed))
    }

    /// sichtbare Grafik auf Ebene `i` (0-basiert)
    pub fn layer_visible(&self, i: usize) -> bool {
        self.latest.lock().unwrap().layers.get(i).is_some_and(|l| l.is_some())
    }

    pub fn has_graphic(&self) -> bool {
        self.latest.lock().unwrap().layers.iter().any(|l| l.is_some())
    }

    /// `overlaycomposition`-Element mit den jeweils letzten Bildern verbinden.
    pub fn attach(&self, el: &gst::Element) {
        let latest = self.latest.clone();
        el.connect("draw", false, move |_| {
            // Signal verlangt immer einen Wert des Typs; ohne Grafik ist er NULL
            let c: Option<gst_video::VideoOverlayComposition> = latest.lock().unwrap().composition();
            Some(c.to_value())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bbox_of_drawn_area() {
        let (w, h) = (8u32, 6u32);
        let mut img = vec![0u8; (w * h * 4) as usize];
        assert_eq!(alpha_bbox(&img, w, h), None, "ganz durchsichtig = keine Grafik");
        for (x, y) in [(2, 3), (5, 4)] {
            img[((y * w + x) * 4 + 3) as usize] = 255;
        }
        assert_eq!(alpha_bbox(&img, w, h), Some((2, 3, 4, 2)));
        let c = crop(&img, w, (2, 3, 4, 2));
        assert_eq!(c.len(), 4 * 2 * 4);
        assert_eq!(c[3], 255);
    }

    #[test]
    fn composition_needs_visible_pixels() {
        let _ = gst::init();
        let (w, h) = (16u32, 9u32);
        let mut img = vec![0u8; (w * h * 4) as usize];
        assert!(composition_from(&img, w, h).is_none());
        img[3] = 200;
        assert!(composition_from(&img, w, h).is_some());
    }
}
