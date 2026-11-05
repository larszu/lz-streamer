//! `lzloudness`: die Lautheitsregelung aus `leveler.rs` als GStreamer-Element
//! (F32, 48 kHz, 1–2 Kanäle, in place). Meldet die Vorausschau als Latenz.

use std::sync::{LazyLock, Mutex, Once};

use gstreamer as gst;
use gstreamer::glib;
use gstreamer::prelude::*;
use gstreamer::subclass::prelude::*;
use gstreamer_base as gst_base;
use gstreamer_base::subclass::prelude::*;

use crate::leveler::{Leveler, LevelerConfig, RATE};

mod imp {
    use super::*;

    pub struct LzLoudness {
        pub cfg: Mutex<LevelerConfig>,
        pub lev: Mutex<Option<Leveler>>,
    }

    impl Default for LzLoudness {
        fn default() -> Self {
            Self { cfg: Mutex::new(LevelerConfig { enabled: true, ..Default::default() }), lev: Mutex::new(None) }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LzLoudness {
        const NAME: &'static str = "LzStreamerLoudness";
        type Type = super::LzLoudness;
        type ParentType = gst_base::BaseTransform;
    }

    impl ObjectImpl for LzLoudness {}
    impl GstObjectImpl for LzLoudness {}

    impl ElementImpl for LzLoudness {
        fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
            static META: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
                gst::subclass::ElementMetadata::new(
                    "LZ loudness leveler",
                    "Filter/Effect/Audio",
                    "Slow AGC to a loudness target, freeze in pauses, transient control and true-peak limiter (1.5 ms lookahead)",
                    "Lars Zumpe",
                )
            });
            Some(&*META)
        }

        fn pad_templates() -> &'static [gst::PadTemplate] {
            static T: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
                let caps = gst::Caps::builder("audio/x-raw")
                    .field("format", "F32LE")
                    .field("rate", RATE as i32)
                    .field("channels", gst::IntRange::new(1, 2))
                    .field("layout", "interleaved")
                    .build();
                vec![
                    gst::PadTemplate::new("src", gst::PadDirection::Src, gst::PadPresence::Always, &caps).unwrap(),
                    gst::PadTemplate::new("sink", gst::PadDirection::Sink, gst::PadPresence::Always, &caps).unwrap(),
                ]
            });
            T.as_ref()
        }
    }

    impl BaseTransformImpl for LzLoudness {
        const MODE: gst_base::subclass::BaseTransformMode = gst_base::subclass::BaseTransformMode::AlwaysInPlace;
        const PASSTHROUGH_ON_SAME_CAPS: bool = false;
        const TRANSFORM_IP_ON_PASSTHROUGH: bool = false;

        fn set_caps(&self, incaps: &gst::Caps, _outcaps: &gst::Caps) -> Result<(), gst::LoggableError> {
            let ch = incaps.structure(0).and_then(|s| s.get::<i32>("channels").ok()).unwrap_or(2).max(1) as usize;
            *self.lev.lock().unwrap() = Some(Leveler::new(self.cfg.lock().unwrap().clone(), ch));
            Ok(())
        }

        fn query(&self, direction: gst::PadDirection, query: &mut gst::QueryRef) -> bool {
            if let gst::QueryViewMut::Latency(q) = query.view_mut() {
                let ok = BaseTransformImplExt::parent_query(self, direction, q.query_mut());
                if ok {
                    let (live, min, max) = q.result();
                    let la = gst::ClockTime::from_nseconds((self.lookahead_s() * 1e9) as u64);
                    q.set(live, min + la, max.map(|m| m + la));
                }
                return ok;
            }
            BaseTransformImplExt::parent_query(self, direction, query)
        }

        fn transform_ip(&self, buf: &mut gst::BufferRef) -> Result<gst::FlowSuccess, gst::FlowError> {
            let mut map = buf.map_writable().map_err(|_| gst::FlowError::Error)?;
            // SAFETY: f32 hat keine ungültigen Bitmuster; align_to_mut gibt nur den passend ausgerichteten Teil
            let (head, samples, tail) = unsafe { map.as_mut_slice().align_to_mut::<f32>() };
            if !head.is_empty() || !tail.is_empty() {
                return Err(gst::FlowError::Error);
            }
            if let Some(l) = self.lev.lock().unwrap().as_mut() {
                l.process(samples, false);
            }
            Ok(gst::FlowSuccess::Ok)
        }
    }

    impl LzLoudness {
        pub fn lookahead_s(&self) -> f64 {
            self.lev.lock().unwrap().as_ref().map(|l| l.latency_samples() as f64 / RATE).unwrap_or(0.0015)
        }
    }
}

glib::wrapper! {
    pub struct LzLoudness(ObjectSubclass<imp::LzLoudness>) @extends gst_base::BaseTransform, gst::Element, gst::Object;
}

impl LzLoudness {
    pub fn set_config(&self, c: LevelerConfig) {
        *self.imp().cfg.lock().unwrap() = c.clone();
        if let Some(l) = self.imp().lev.lock().unwrap().as_mut() {
            l.cfg = c;
        }
    }
    /// aktuelle Regelung: (AGC dB, schnelle Absenkung dB)
    pub fn gain(&self) -> Option<(f64, f64)> {
        self.imp().lev.lock().unwrap().as_ref().map(|l| (l.agc_db, l.fast_db))
    }
}

/// Einmal je Prozess registrieren.
pub fn register() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = gst::Element::register(None, "lzloudness", gst::Rank::NONE, LzLoudness::static_type());
    });
}
