//! lz-streamer – eigener Video-Encoder/Decoder auf Rust + GStreamer.

pub mod admin;
pub mod asr;
#[cfg(target_os = "macos")]
pub mod au;
pub mod audio;
pub mod caps;
pub mod caption_measure;
pub mod channel;
pub mod clearspeech;
pub mod config;
pub mod control;
pub mod devices;
pub mod gfx;
pub mod hls;
pub mod latency;
pub mod leveler;
pub mod loudness;
pub mod lzloudness;
pub mod mail;
pub mod metrics;
pub mod ops;
pub mod plugin;
pub mod pipeline;
pub mod renderer;
pub mod report;
pub mod server;
pub mod stats;
pub mod subtitles;
pub mod supervisor;
pub mod tls;
pub mod vad;

/// 4181–4198 belegt die AV-Suite, 4199 lz-nvx-toolbox.
pub const DEFAULT_PORT: u16 = 4200;
