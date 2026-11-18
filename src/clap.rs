//! CLAP-Plugins (P2) über `clack-host`. Läuft wie VST3 und AU im Hilfsprozess
//! `lz-streamer plugin-host`; dort ist alles einthreadig (Haupt- und Audiothread
//! fallen zusammen, das lässt CLAP zu).
//!
//! Kennung: Pfad zum `.clap`-Bündel, optional mit `#<plugin-id>`, wenn das
//! Bündel mehrere Plugins enthält (sonst das erste), dahinter optional feste
//! Parameterwerte `;<Name>=<Wert>` (Wert im Bereich des Parameters), z. B.
//! `…/Airwindows Consolidated.clap;Output Level=0`.

use std::ffi::CString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clack_extensions::latency::PluginLatency;
use clack_host::prelude::*;

struct Shared;

impl SharedHandler<'_> for Shared {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {}
}

struct Host;

impl HostHandlers for Host {
    type Shared<'a> = Shared;
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();
}

/// Ordner, in denen CLAP-Plugins üblicherweise liegen
pub fn dirs() -> Vec<PathBuf> {
    let mut d: Vec<PathBuf> = Vec::new();
    if let Some(h) = dirs::home_dir() {
        d.push(h.join("Library/Audio/Plug-Ins/CLAP"));
        d.push(h.join(".clap"));
    }
    d.extend(["/Library/Audio/Plug-Ins/CLAP", "/usr/lib/clap", "/usr/local/lib/clap", "C:\\Program Files\\Common Files\\CLAP"].map(PathBuf::from));
    d
}

/// `.clap`-Bündel in den üblichen Ordnern
pub fn bundles() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = dirs()
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|rd| rd.flatten())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "clap"))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// (Bündel, Plugin-Kennung, Parameter)
fn split_id(id: &str) -> (&str, Option<&str>, Vec<(String, f64)>) {
    let mut parts = id.split(';');
    let head = parts.next().unwrap_or_default();
    let params = parts.filter_map(|p| p.split_once('=')).filter_map(|(k, v)| Some((k.trim().to_string(), v.trim().replace(',', ".").parse().ok()?))).collect();
    match head.rsplit_once('#') {
        Some((p, pid)) if p.ends_with(".clap") => (p, Some(pid), params),
        _ => (head, None, params),
    }
}

/// Ist `id` eine CLAP-Kennung?
pub fn is_clap(id: &str) -> bool {
    let (p, _, _) = split_id(id);
    p.ends_with(".clap")
}

fn load_entry(path: &str) -> Result<PluginEntry> {
    if !Path::new(path).exists() {
        bail!("{path} fehlt");
    }
    // SAFETY: Laden einer fremden Bibliothek; läuft nur im Hilfsprozess, ein Absturz trifft nur ihn
    unsafe { PluginEntry::load(path) }.map_err(|e| anyhow!("{path}: {e:?}"))
}

/// Plugins in einem Bündel: (Kennung, Name). Lädt die Bibliothek – nur im Prüflauf oder Hilfsprozess aufrufen.
pub fn plugins_in(path: &str) -> Result<Vec<(String, String)>> {
    let entry = load_entry(path)?;
    let f = entry.get_plugin_factory().ok_or_else(|| anyhow!("keine Plugin-Fabrik"))?;
    Ok(f.plugin_descriptors()
        .filter_map(|d| {
            let id = d.id()?.to_string_lossy().into_owned();
            let name = d.name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| id.clone());
            Some((format!("{path}#{id}"), name))
        })
        .collect())
}

pub struct ClapPlugin {
    // Reihenfolge = Abbau-Reihenfolge: erst Prozessor, dann Instanz, dann Bibliothek
    proc: Option<StartedPluginAudioProcessor<Host>>,
    instance: PluginInstance<Host>,
    _entry: PluginEntry,
    ch: usize,
    inb: Vec<Vec<f32>>,
    outb: Vec<Vec<f32>>,
    in_ports: AudioPorts,
    out_ports: AudioPorts,
    ev_out: EventBuffer,
    pending: EventBuffer,
    pub latency_samples: u32,
}

impl Drop for ClapPlugin {
    fn drop(&mut self) {
        if let Some(p) = self.proc.take() {
            let stopped = p.stop_processing();
            self.instance.deactivate(stopped);
        }
    }
}

impl ClapPlugin {
    pub fn load(id: &str, rate: f64, ch: usize, max_block: usize) -> Result<Self> {
        let (path, pid, settings) = split_id(id);
        let entry = load_entry(path)?;
        let factory = entry.get_plugin_factory().ok_or_else(|| anyhow!("keine Plugin-Fabrik"))?;
        let desc = factory
            .plugin_descriptors()
            .find(|d| pid.is_none_or(|p| d.id().is_some_and(|i| i.to_bytes() == p.as_bytes())))
            .ok_or_else(|| anyhow!("Plugin {} nicht im Bündel", pid.unwrap_or("?")))?;
        let plugin_id: CString = desc.id().ok_or_else(|| anyhow!("Plugin ohne Kennung"))?.to_owned();
        let info = HostInfo::new("lz-streamer", "LZ", "https://localhost", env!("CARGO_PKG_VERSION")).map_err(|e| anyhow!("{e:?}"))?;
        let mut instance = PluginInstance::<Host>::new(|_| Shared, |_| (), &entry, &plugin_id, &info).map_err(|e| anyhow!("Instanz: {e:?}"))?;
        let cfg = PluginAudioConfiguration { sample_rate: rate, min_frames_count: 1, max_frames_count: max_block as u32 };
        let stopped = instance.activate(|_, _| (), cfg).map_err(|e| anyhow!("Aktivieren: {e:?}"))?;
        let latency_samples = instance.plugin_shared_handle().get_extension::<PluginLatency>().map(|l| l.get(&instance.plugin_handle())).unwrap_or(0);
        // feste Parameterwerte als Ereignisse im ersten Block
        let mut pending = EventBuffer::new();
        if !settings.is_empty() {
            let pp = instance.plugin_shared_handle().get_extension::<clack_extensions::params::PluginParams>().ok_or_else(|| anyhow!("Plugin ohne Parameter"))?;
            let h = instance.plugin_handle();
            let mut b = clack_extensions::params::ParamInfoBuffer::new();
            let mut known = Vec::new();
            for i in 0..pp.count(&h) {
                if let Some(info) = pp.get_info(&h, i, &mut b) {
                    known.push((String::from_utf8_lossy(info.name).into_owned(), info.id, info.min_value, info.max_value));
                }
            }
            for (name, v) in &settings {
                let (_, pid, lo, hi) = known.iter().find(|k| &k.0 == name).ok_or_else(|| anyhow!("Parameter „{name}“ unbekannt"))?;
                pending.push(&clack_host::events::event_types::ParamValueEvent::new(0, *pid, Pckn::match_all(), v.clamp(*lo, *hi)));
            }
        }
        let proc = stopped.start_processing().map_err(|e| anyhow!("Start: {e:?}")).context("CLAP")?;
        Ok(Self {
            proc: Some(proc),
            instance,
            _entry: entry,
            ch,
            inb: vec![vec![0.0; max_block]; ch],
            outb: vec![vec![0.0; max_block]; ch],
            in_ports: AudioPorts::with_capacity(ch, 1),
            out_ports: AudioPorts::with_capacity(ch, 1),
            ev_out: EventBuffer::new(),
            pending,
            latency_samples,
        })
    }

    /// Interleaved in place verarbeiten
    pub fn process(&mut self, buf: &mut [f32]) -> Result<()> {
        let ch = self.ch;
        let frames = buf.len() / ch;
        if frames > self.inb[0].len() {
            bail!("Block zu groß");
        }
        for (i, v) in buf.iter().enumerate() {
            self.inb[i % ch][i / ch] = *v;
        }
        let proc = self.proc.as_mut().ok_or_else(|| anyhow!("gestoppt"))?;
        let input = self.in_ports.with_input_buffers([AudioPortBuffer {
            latency: 0,
            channels: AudioPortBufferType::f32_input_only(self.inb.iter_mut().map(|b| InputChannel::variable(&mut b[..frames]))),
        }]);
        let mut output = self.out_ports.with_output_buffers([AudioPortBuffer {
            latency: 0,
            channels: AudioPortBufferType::f32_output_only(self.outb.iter_mut().map(|b| &mut b[..frames])),
        }]);
        let ev_in = InputEvents::from_buffer(&self.pending);
        self.ev_out.clear();
        let mut ev_out = OutputEvents::from_buffer(&mut self.ev_out);
        proc.process(&input, &mut output, &ev_in, &mut ev_out, None, None).map_err(|e| anyhow!("process: {e:?}"))?;
        self.pending.clear();
        for (i, v) in buf.iter_mut().enumerate() {
            *v = self.outb[i % ch][i / ch];
        }
        Ok(())
    }
}
