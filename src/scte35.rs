//! SCTE-35-Cues (KB-60): manuell per API ausgelöst, von mpegtsmux auf eigener
//! PID (0x1F6, Kennung „CUEI“) geschrieben.
//!
//! Der Splice-Zeitpunkt ist Laufzeit (`is_running_time`); mpegtsmux rechnet
//! ihn in die PTS des Stroms um. Zum selben Zeitpunkt erzwingt der Encoder ein
//! IDR-Bild, damit nachgelagerte Geräte genau dort schneiden können.
//!
//! Die GStreamer-Bibliothek `gstmpegts` hat keine Rust-Bindung; die fünf
//! benötigten Funktionen werden direkt angesprochen.

use std::ffi::c_void;

use gstreamer as gst;
use gstreamer::glib::translate::ToGlibPtr;
use serde::{Deserialize, Serialize};

pub const PID: u16 = 0x1f6;

#[allow(non_camel_case_types)]
type GstMpegtsSCTESIT = c_void;
#[allow(non_camel_case_types)]
type GstMpegtsSection = c_void;

#[link(name = "gstmpegts-1.0")]
unsafe extern "C" {
    fn gst_mpegts_initialize();
    fn gst_mpegts_scte_splice_out_new(event_id: u32, splice_time: u64, duration: u64) -> *mut GstMpegtsSCTESIT;
    fn gst_mpegts_scte_splice_in_new(event_id: u32, splice_time: u64) -> *mut GstMpegtsSCTESIT;
    fn gst_mpegts_section_from_scte_sit(sit: *mut GstMpegtsSCTESIT, pid: u16) -> *mut GstMpegtsSection;
    fn gst_mpegts_section_send_event(section: *mut GstMpegtsSection, element: *mut gst::ffi::GstElement) -> i32;
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum CueKind {
    /// Werbung beginnt (splice_insert, out_of_network)
    #[default]
    Out,
    /// zurück ins Programm
    In,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Cue {
    pub kind: CueKind,
    /// Vorlauf bis zum Splice-Punkt in ms (Vorgabe 4000: SCTE 67 empfiehlt ≥ 4 s)
    pub preroll_ms: u32,
    /// Länge der Unterbrechung bei `out` in s (0 = offen, Rückkehr per `in`)
    pub duration_s: f64,
    /// eigene Ereignisnummer, sonst fortlaufend
    pub event_id: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CueStatus {
    pub sent: u64,
    pub last_event_id: Option<u32>,
    /// Laufzeit des letzten Splice-Punkts in ms
    pub last_splice_ms: Option<u64>,
    pub last_kind: Option<CueKind>,
}

pub fn effective_preroll(c: &Cue) -> gst::ClockTime {
    gst::ClockTime::from_mseconds(if c.preroll_ms == 0 { 4000 } else { c.preroll_ms.clamp(100, 60_000) } as u64)
}

/// Cue an mpegtsmux geben. `splice_rt` ist die Laufzeit des Splice-Punkts.
pub fn send(mux: &gst::Element, cue: &Cue, event_id: u32, splice_rt: gst::ClockTime) -> bool {
    unsafe {
        gst_mpegts_initialize();
        let sit = match cue.kind {
            CueKind::Out => {
                let dur = if cue.duration_s > 0.0 { (cue.duration_s * 1e9) as u64 } else { 0 };
                gst_mpegts_scte_splice_out_new(event_id, splice_rt.nseconds(), dur)
            }
            CueKind::In => gst_mpegts_scte_splice_in_new(event_id, splice_rt.nseconds()),
        };
        if sit.is_null() {
            return false;
        }
        // übernimmt sit
        let section = gst_mpegts_section_from_scte_sit(sit, PID);
        if section.is_null() {
            return false;
        }
        // send_event leiht sich die Section nur (transfer none)
        let el: *mut gst::ffi::GstElement = mux.to_glib_none().0;
        let ok = gst_mpegts_section_send_event(section, el) != 0;
        gst::ffi::gst_mini_object_unref(section as *mut gst::ffi::GstMiniObject);
        ok
    }
}
