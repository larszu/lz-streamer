use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use gstreamer as gst;

use lz_streamer::caps::Capabilities;
use lz_streamer::channel::{self, ChannelOptions};
use lz_streamer::config::{Codec, Config, OutputKind, Role};
use lz_streamer::server::{self, App};
use lz_streamer::supervisor::Supervisor;
use lz_streamer::{devices, latency};

#[derive(Parser)]
#[command(name = "lz-streamer", version, about = "Video-Encoder/Decoder: Eingang → Encoder → SRT/RTMP")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Weboberfläche und Aufpasser starten (Standard)
    Serve {
        #[arg(long, default_value_t = lz_streamer::DEFAULT_PORT)]
        port: u16,
        /// 0.0.0.0, um die Oberfläche im Netz erreichbar zu machen (ohne Anmeldung!)
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
        /// Eigene Konfigurationsdatei statt der im Benutzerordner
        #[arg(long)]
        config: Option<PathBuf>,
        /// Kanal nach dem Start sofort einschalten
        #[arg(long)]
        autostart: bool,
    },
    /// Einen Kanal direkt ausführen (so startet der Aufpasser seine Kindprozesse)
    Channel {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        preview: Option<PathBuf>,
    },
    /// Latenz Testbild → SRT → Decoder auf diesem Rechner messen
    Latenz {
        #[arg(long, default_value = "auto")]
        encoder: String,
        #[arg(long)]
        h265: bool,
        #[arg(long, default_value_t = 120)]
        srt_latency: u32,
        #[arg(long, default_value_t = 7)]
        runden: usize,
        /// Ohne Tonspur messen
        #[arg(long)]
        ohne_ton: bool,
        #[arg(long, default_value = "auto")]
        decoder: String,
        /// Puffer des TS-Demuxers in ms
        #[arg(long, default_value_t = 40)]
        demux: u32,
        /// Reserve am Decoder-Ausgang in ms
        #[arg(long, default_value_t = 60)]
        reserve: u32,
    },
    /// Gefundene Encoder, Geräte und fehlende Bausteine anzeigen
    Info,
}

fn config_path(custom: Option<PathBuf>) -> PathBuf {
    custom.unwrap_or_else(|| {
        dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("lz-streamer").join("config.json")
    })
}

fn load_config(path: &PathBuf) -> Config {
    let mut c: Config = std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    c.normalize();
    c
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .init();
    gst::init().context("GStreamer lässt sich nicht starten – ist es installiert?")?;
    let cli = Cli::parse();
    match cli.cmd.unwrap_or(Cmd::Serve { port: lz_streamer::DEFAULT_PORT, bind: "127.0.0.1".into(), config: None, autostart: false }) {
        Cmd::Channel { config, preview } => {
            let cfg: Config = serde_json::from_slice(&std::fs::read(&config)?)?;
            let opts = ChannelOptions { preview_path: preview };
            // macOS: Videofenster brauchen die Ereignisschleife im Hauptthread
            let windowed = cfg.role == Role::Decoder && cfg.decoder.output == OutputKind::Window;
            #[cfg(target_os = "macos")]
            let code = if windowed { gst::macos_main(move || channel::run_child(cfg, opts)) } else { channel::run_child(cfg, opts) };
            #[cfg(not(target_os = "macos"))]
            let code = {
                let _ = windowed;
                channel::run_child(cfg, opts)
            };
            std::process::exit(code);
        }
        Cmd::Latenz { encoder, h265, srt_latency, runden, ohne_ton, decoder, demux, reserve } => {
            let opts = latency::MeasureOptions {
                codec: if h265 { Codec::H265 } else { Codec::H264 },
                encoder,
                srt_latency_ms: srt_latency,
                rounds: runden.max(1),
                audio: !ohne_ton,
                decoder,
                demux_latency_ms: demux,
                output_buffer_ms: reserve,
                ..Default::default()
            };
            let r = latency::measure(&opts)?;
            println!(
                "Encoder {} · SRT-Latenz {} ms · Testbild→SRT→Decoder-Ausgang: Median {} ms (min {}, max {}) · Einzelwerte {:?} · gemeldete Pipeline-Latenz Encoder {:?} ms, Decoder {:?} ms",
                r.encoder, r.srt_latency_ms, r.median_ms, r.min_ms, r.max_ms, r.samples_ms, r.encoder_pipeline_ms, r.decoder_pipeline_ms
            );
            let s = &r.stages;
            println!(
                "Aufschlüsselung (Median): Quelle {} ms · Encode {} ms · Parser+Mux {} ms · SRT+Demux+Decode {} ms · Ausgabe {} ms",
                s.source_ms, s.encode_ms, s.mux_ms, s.transport_decode_ms, s.output_ms
            );
            Ok(())
        }
        Cmd::Info => {
            let caps = Capabilities::probe();
            println!("{}", serde_json::to_string_pretty(&caps)?);
            println!("DeckLink-Geräte: {:?}", devices::decklink_devices());
            for m in caps.missing() {
                println!("Fehlt: {m}");
            }
            Ok(())
        }
        Cmd::Serve { port, bind, config, autostart } => serve(port, bind, config_path(config), autostart),
    }
}

#[tokio::main]
async fn serve(port: u16, bind: String, cfg_path: PathBuf, autostart: bool) -> Result<()> {
    let cfg = load_config(&cfg_path);
    let run_dir = std::env::temp_dir().join(format!("lz-streamer-{port}"));
    let data = lz_streamer::admin::DataDir::new(cfg_path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from(".")));
    let sup = Supervisor::new(cfg, cfg_path.clone(), run_dir, data.recordings());
    let ops = lz_streamer::ops::Ops::new(data);
    let caps = Capabilities::probe();
    for m in caps.missing() {
        tracing::warn!("{m}");
    }
    let devices = tokio::task::spawn_blocking(devices::decklink_devices).await.unwrap_or_default();
    if devices.is_empty() {
        sup.log("info", "Keine DeckLink/UltraStudio gefunden – Eingang „Automatisch“ nimmt das Testbild.").await;
    } else {
        for d in &devices {
            sup.log("ok", format!("Gefunden: {} ({}, Nr. {})", d.name, d.direction, d.device)).await;
        }
    }
    let app = App::new(sup.clone(), ops.clone(), caps, devices);
    tokio::spawn(sup.clone().run());
    tokio::spawn(ops.clone().monitor(sup.clone()));
    tokio::spawn(ops.clone().scheduler(sup.clone()));
    if autostart {
        sup.start().await;
    }
    let addr = format!("{bind}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await.with_context(|| format!("Port {addr} belegt?"))?;
    tracing::info!("lz-streamer auf http://{addr} · Konfiguration {}", cfg_path.display());
    let shutdown = {
        let sup = sup.clone();
        async move {
            #[cfg(unix)]
            {
                let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM");
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
            }
            #[cfg(not(unix))]
            let _ = tokio::signal::ctrl_c().await;
            sup.stop().await;
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    };
    axum::serve(listener, server::router(app)).with_graceful_shutdown(shutdown).await?;
    Ok(())
}
