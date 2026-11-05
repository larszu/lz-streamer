//! lz-streamer – eigener Video-Encoder/Decoder auf Rust + GStreamer.

pub mod audio;
pub mod caps;
pub mod channel;
pub mod config;
pub mod devices;
pub mod latency;
pub mod pipeline;
pub mod server;
pub mod stats;
pub mod supervisor;

/// 4181–4198 belegt die AV-Suite, 4199 lz-nvx-toolbox.
pub const DEFAULT_PORT: u16 = 4200;
