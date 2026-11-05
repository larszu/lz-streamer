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
        /// Ausgabeformat, z. B. 3840x2160 (größer als HD: Testbild wird hochskaliert)
        #[arg(long, default_value = "1280x720")]
        format: String,
        #[arg(long, default_value_t = 50)]
        fps: u32,
    },
    /// Spracherkennung an einer Aufnahme messen: Text, Verzug, Wortfehlerrate
    UntertitelMessen {
        #[arg(long)]
        audio: PathBuf,
        /// Referenztext (Datei) für die Wortfehlerrate
        #[arg(long)]
        referenz: Option<PathBuf>,
        /// Pfad zur ggml-Modelldatei
        #[arg(long)]
        modell: PathBuf,
        #[arg(long, default_value = "de")]
        sprache: String,
        #[arg(long, default_value_t = 2000)]
        chunk: u32,
    },
    /// Untertitel live messen: Video in Echtzeit durch Encoder (Spracherkennung) → SRT → Decoder
    UntertitelLiveMessen {
        #[arg(long)]
        video: PathBuf,
        /// Sprachbeginn je Äußerung (s) und Text, TAB-getrennt
        #[arg(long)]
        onsets: PathBuf,
        #[arg(long)]
        referenz: PathBuf,
        #[arg(long)]
        modell: PathBuf,
        #[arg(long, default_value = "de")]
        sprache: String,
        /// ins Englische übersetzen
        #[arg(long)]
        uebersetzen: bool,
        /// pop-on, roll-up2, roll-up3 …
        #[arg(long, default_value = "roll-up2")]
        modus: String,
        #[arg(long, default_value_t = 3000)]
        chunk: u32,
        /// Wort für Wort senden (Roll-up)
        #[arg(long)]
        woerter: bool,
        /// Ausgabe nicht nach Sprechzeit takten
        #[arg(long)]
        ungetaktet: bool,
    },
    /// Sprache/Musik/Pause-Erkennung an einem Korpus prüfen (<ordner>/<klasse>/*.wav)
    KlassenMessen {
        #[arg(long)]
        korpus: PathBuf,
    },
    /// Hilfsprozess für ein VST3-Plugin (wird von `lzplugin` gestartet)
    #[command(hide = true)]
    PluginHost {
        #[arg(long)]
        plugin: String,
        /// nur prüfen (für den Scan): laden, kurz verarbeiten, `ok <latenz>` ausgeben
        #[arg(long)]
        probe: bool,
        #[arg(long, default_value_t = 2)]
        channels: usize,
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
        Cmd::Latenz { encoder, h265, srt_latency, runden, ohne_ton, decoder, demux, reserve, format, fps } => {
            let opts = latency::MeasureOptions {
                codec: if h265 { Codec::H265 } else { Codec::H264 },
                encoder,
                srt_latency_ms: srt_latency,
                rounds: runden.max(1),
                audio: !ohne_ton,
                decoder,
                demux_latency_ms: demux,
                output_buffer_ms: reserve,
                width: format.split('x').next().and_then(|w| w.parse().ok()).unwrap_or(1280),
                height: format.split('x').nth(1).and_then(|h| h.parse().ok()).unwrap_or(720),
                fps,
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
        Cmd::UntertitelMessen { audio, referenz, modell, sprache, chunk } => {
            let name = modell.file_stem().map(|s| s.to_string_lossy().trim_start_matches("ggml-").to_string()).unwrap_or_default();
            let cfg = lz_streamer::asr::AsrConfig { model: name, model_path: modell.to_string_lossy().to_string(), language: sprache, chunk_ms: chunk, ..Default::default() };
            let reference = referenz.map(std::fs::read_to_string).transpose()?;
            let m = lz_streamer::asr::measure_file(&audio, &cfg, reference.as_deref())?;
            println!("{}", serde_json::to_string_pretty(&m)?);
            Ok(())
        }
        Cmd::UntertitelLiveMessen { video, onsets, referenz, modell, sprache, uebersetzen, modus, chunk, woerter, ungetaktet } => {
            let mode: lz_streamer::config::CaptionMode = serde_json::from_value(serde_json::Value::String(modus))?;
            let m = lz_streamer::caption_measure::LiveMeasure {
                video,
                onsets,
                reference: referenz,
                language: sprache,
                model_path: modell.to_string_lossy().to_string(),
                translate: uebersetzen,
                mode,
                chunk_ms: chunk,
                word_mode: woerter,
                paced: !ungetaktet,
                work_dir: std::env::temp_dir(),
            };
            let r = lz_streamer::caption_measure::run(&m)?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            Ok(())
        }
        Cmd::PluginHost { plugin, channels, probe } => if probe { lz_streamer::plugin::probe(&plugin, channels) } else { lz_streamer::plugin::run_host(&plugin, channels) },
        Cmd::KlassenMessen { korpus } => {
            let r = lz_streamer::vad::evaluate_corpus(&korpus)?;
            for (c, ok, n) in &r.per_class {
                println!("{c:?}: {ok}/{n} = {:.1} %", if *n > 0 { *ok as f64 * 100.0 / *n as f64 } else { 0.0 });
            }
            println!("Verwechslungen: {:?}", r.confusion.iter().filter(|(a, b, _)| a != b).collect::<Vec<_>>());
            // Gruppe „Musik“ (Gesang oder Orgel) – das, was A2/K1 brauchen
            let music: usize = r.confusion.iter().filter(|(a, b, _)| a.is_music() && b.is_music()).map(|(_, _, n)| n).sum();
            let music_all: usize = r.confusion.iter().filter(|(a, _, _)| a.is_music()).map(|(_, _, n)| n).sum();
            if music_all > 0 {
                println!("Musik als Musik erkannt: {music}/{music_all} = {:.1} %", music as f64 * 100.0 / music_all as f64);
            }
            println!("Rechenzeit: {:.0} µs je 10 ms Audio", r.us_per_10ms);
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
    tokio::spawn(lz_streamer::renderer::osc_listener("127.0.0.1:4211".into(), sup.clone(), app.renderer.clone()));
    tokio::spawn(lz_streamer::renderer::keep_running(sup.clone(), format!("ws://127.0.0.1:{port}/renderer")));
    axum::serve(listener, server::router(app).into_make_service_with_connect_info::<std::net::SocketAddr>())
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}
