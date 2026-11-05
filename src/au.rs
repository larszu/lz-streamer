//! Audio Units (AUv2/AUv3 über AudioComponent) am Mac, über AudioToolbox per
//! FFI. Läuft im Hilfsprozess `lz-streamer plugin-host`, wie VST3.
//! Kennung eines AU: `au:<typ>:<subtyp>:<hersteller>` (je vier Zeichen), z. B.
//! `au:aufx:dely:appl` für Apples AUDelay.

#![allow(non_snake_case, non_upper_case_globals)]

use std::ffi::c_void;
use std::ptr;

use anyhow::{Context, Result, bail};

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct AudioComponentDescription {
    componentType: u32,
    componentSubType: u32,
    componentManufacturer: u32,
    componentFlags: u32,
    componentFlagsMask: u32,
}

#[repr(C)]
struct AudioStreamBasicDescription {
    mSampleRate: f64,
    mFormatID: u32,
    mFormatFlags: u32,
    mBytesPerPacket: u32,
    mFramesPerPacket: u32,
    mBytesPerFrame: u32,
    mChannelsPerFrame: u32,
    mBitsPerChannel: u32,
    mReserved: u32,
}

#[repr(C)]
#[derive(Default)]
struct SmpteTime {
    mSubframes: i16,
    mSubframeDivisor: i16,
    mCounter: u32,
    mType: u32,
    mFlags: u32,
    mHours: i16,
    mMinutes: i16,
    mSeconds: i16,
    mFrames: i16,
}

#[repr(C)]
#[derive(Default)]
struct AudioTimeStamp {
    mSampleTime: f64,
    mHostTime: u64,
    mRateScalar: f64,
    mWordClockTime: u64,
    mSMPTETime: SmpteTime,
    mFlags: u32,
    mReserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioBuffer {
    mNumberChannels: u32,
    mDataByteSize: u32,
    mData: *mut c_void,
}

/// AudioBufferList mit Platz für zwei Kanäle (nicht verschachtelt)
#[repr(C)]
struct AudioBufferList2 {
    mNumberBuffers: u32,
    mBuffers: [AudioBuffer; 2],
}

type RenderProc = extern "C" fn(*mut c_void, *mut u32, *const AudioTimeStamp, u32, u32, *mut AudioBufferList2) -> i32;

#[repr(C)]
struct AURenderCallbackStruct {
    inputProc: RenderProc,
    inputProcRefCon: *mut c_void,
}

type AudioComponent = *mut c_void;
type AudioUnit = *mut c_void;
type CFStringRef = *const c_void;

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    fn AudioComponentFindNext(c: AudioComponent, desc: *const AudioComponentDescription) -> AudioComponent;
    fn AudioComponentCopyName(c: AudioComponent, name: *mut CFStringRef) -> i32;
    fn AudioComponentGetDescription(c: AudioComponent, desc: *mut AudioComponentDescription) -> i32;
    fn AudioComponentInstanceNew(c: AudioComponent, out: *mut AudioUnit) -> i32;
    fn AudioComponentInstanceDispose(u: AudioUnit) -> i32;
    fn AudioUnitSetProperty(u: AudioUnit, id: u32, scope: u32, elem: u32, data: *const c_void, size: u32) -> i32;
    fn AudioUnitGetProperty(u: AudioUnit, id: u32, scope: u32, elem: u32, data: *mut c_void, size: *mut u32) -> i32;
    fn AudioUnitInitialize(u: AudioUnit) -> i32;
    fn AudioUnitUninitialize(u: AudioUnit) -> i32;
    fn AudioUnitRender(u: AudioUnit, flags: *mut u32, ts: *const AudioTimeStamp, bus: u32, frames: u32, data: *mut AudioBufferList2) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringGetCString(s: CFStringRef, buf: *mut u8, size: isize, encoding: u32) -> bool;
    fn CFRelease(p: *const c_void);
}

const kAudioUnitProperty_StreamFormat: u32 = 8;
const kAudioUnitProperty_Latency: u32 = 12;
const kAudioUnitProperty_MaximumFramesPerSlice: u32 = 14;
const kAudioUnitProperty_SetRenderCallback: u32 = 23;
const kScopeGlobal: u32 = 0;
const kScopeInput: u32 = 1;
const kScopeOutput: u32 = 2;

fn fourcc(s: &str) -> Option<u32> {
    let b = s.as_bytes();
    (b.len() == 4).then(|| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn fourcc_str(v: u32) -> String {
    String::from_utf8_lossy(&v.to_be_bytes()).into_owned()
}

fn parse_id(id: &str) -> Option<AudioComponentDescription> {
    let mut p = id.strip_prefix("au:")?.split(':');
    Some(AudioComponentDescription {
        componentType: fourcc(p.next()?)?,
        componentSubType: fourcc(p.next()?)?,
        componentManufacturer: fourcc(p.next()?)?,
        ..Default::default()
    })
}

/// Alle Effekt-AUs (Typ `aufx`) mit Kennung und Namen.
pub fn list_effects() -> Vec<(String, String)> {
    let want = AudioComponentDescription { componentType: fourcc("aufx").unwrap(), ..Default::default() };
    let mut out = Vec::new();
    let mut c: AudioComponent = ptr::null_mut();
    loop {
        // SAFETY: AudioComponent-API, Beschreibung lebt über den Aufruf
        c = unsafe { AudioComponentFindNext(c, &want) };
        if c.is_null() {
            break;
        }
        let mut d = AudioComponentDescription::default();
        let mut name: CFStringRef = ptr::null();
        // SAFETY: gültige Komponente; Name wird nach dem Kopieren freigegeben
        let label = unsafe {
            AudioComponentGetDescription(c, &mut d);
            if AudioComponentCopyName(c, &mut name) == 0 && !name.is_null() {
                let mut buf = [0u8; 256];
                let ok = CFStringGetCString(name, buf.as_mut_ptr(), buf.len() as isize, 0x0800_0100);
                CFRelease(name);
                ok.then(|| String::from_utf8_lossy(&buf[..buf.iter().position(|b| *b == 0).unwrap_or(0)]).into_owned())
            } else {
                None
            }
        };
        let id = format!("au:{}:{}:{}", fourcc_str(d.componentType), fourcc_str(d.componentSubType), fourcc_str(d.componentManufacturer));
        out.push((id.clone(), label.unwrap_or(id)));
    }
    out
}

/// Eingang für den Render-Rückruf: aktueller Block, je Kanal getrennt
struct Input {
    planes: Vec<Vec<f32>>,
}

extern "C" fn render_input(refcon: *mut c_void, _flags: *mut u32, _ts: *const AudioTimeStamp, _bus: u32, frames: u32, data: *mut AudioBufferList2) -> i32 {
    // SAFETY: refcon zeigt auf das `Input` der laufenden `AuPlugin`-Instanz, data auf die Liste des AU
    unsafe {
        let input = &*(refcon as *const Input);
        let list = &mut *data;
        for b in 0..(list.mNumberBuffers as usize).min(2) {
            let buf = &mut list.mBuffers[b];
            let src = input.planes.get(b).or(input.planes.first());
            let n = (frames as usize).min(buf.mDataByteSize as usize / 4);
            if buf.mData.is_null() {
                continue;
            }
            let dst = std::slice::from_raw_parts_mut(buf.mData as *mut f32, n);
            match src {
                Some(s) => dst.copy_from_slice(&s[..n.min(s.len())]),
                None => dst.fill(0.0),
            }
        }
    }
    0
}

pub struct AuPlugin {
    unit: AudioUnit,
    channels: usize,
    input: Box<Input>,
    out: Vec<Vec<f32>>,
    sample_time: f64,
    pub latency_samples: u32,
}

// Die Instanz lebt nur im Hilfsprozess-Thread
unsafe impl Send for AuPlugin {}

fn check(status: i32, what: &str) -> Result<()> {
    if status != 0 {
        bail!("{what}: OSStatus {status}");
    }
    Ok(())
}

impl AuPlugin {
    pub fn load(id: &str, rate: f64, channels: usize, max_frames: usize) -> Result<Self> {
        let desc = parse_id(id).context("AU-Kennung au:typ:subtyp:hersteller erwartet")?;
        // SAFETY: Aufrufe der AudioUnit-API in der dokumentierten Reihenfolge; Zeiger leben über den Aufruf
        unsafe {
            let c = AudioComponentFindNext(ptr::null_mut(), &desc);
            if c.is_null() {
                bail!("AU {id} nicht gefunden");
            }
            let mut unit: AudioUnit = ptr::null_mut();
            check(AudioComponentInstanceNew(c, &mut unit), "Instanz")?;
            let fmt = AudioStreamBasicDescription {
                mSampleRate: rate,
                mFormatID: fourcc("lpcm").unwrap(),
                // Float | Packed | NonInterleaved
                mFormatFlags: 0x1 | 0x8 | 0x20,
                mBytesPerPacket: 4,
                mFramesPerPacket: 1,
                mBytesPerFrame: 4,
                mChannelsPerFrame: channels as u32,
                mBitsPerChannel: 32,
                mReserved: 0,
            };
            let sz = std::mem::size_of::<AudioStreamBasicDescription>() as u32;
            for scope in [kScopeInput, kScopeOutput] {
                check(AudioUnitSetProperty(unit, kAudioUnitProperty_StreamFormat, scope, 0, &fmt as *const _ as *const c_void, sz), "Format")?;
            }
            let mf = max_frames as u32;
            check(AudioUnitSetProperty(unit, kAudioUnitProperty_MaximumFramesPerSlice, kScopeGlobal, 0, &mf as *const _ as *const c_void, 4), "Blockgröße")?;
            let input = Box::new(Input { planes: vec![vec![0.0; max_frames]; channels] });
            let cb = AURenderCallbackStruct { inputProc: render_input, inputProcRefCon: &*input as *const Input as *mut c_void };
            check(
                AudioUnitSetProperty(unit, kAudioUnitProperty_SetRenderCallback, kScopeInput, 0, &cb as *const _ as *const c_void, std::mem::size_of::<AURenderCallbackStruct>() as u32),
                "Eingang",
            )?;
            check(AudioUnitInitialize(unit), "Initialisieren")?;
            let mut lat = 0f64;
            let mut lsz = 8u32;
            let latency_samples = if AudioUnitGetProperty(unit, kAudioUnitProperty_Latency, kScopeGlobal, 0, &mut lat as *mut _ as *mut c_void, &mut lsz) == 0 {
                (lat * rate).round().max(0.0) as u32
            } else {
                0
            };
            Ok(Self { unit, channels, input, out: vec![vec![0.0; max_frames]; channels], sample_time: 0.0, latency_samples })
        }
    }

    /// Einen Block verarbeiten (interleaved rein und raus).
    pub fn process(&mut self, interleaved: &mut [f32]) -> Result<()> {
        let ch = self.channels;
        let frames = interleaved.len() / ch;
        for c in 0..ch {
            let p = &mut self.input.planes[c];
            if p.len() < frames {
                p.resize(frames, 0.0);
            }
            for t in 0..frames {
                p[t] = interleaved[t * ch + c];
            }
        }
        for o in &mut self.out {
            if o.len() < frames {
                o.resize(frames, 0.0);
            }
        }
        let mk = |o: &mut Vec<f32>| AudioBuffer { mNumberChannels: 1, mDataByteSize: (frames * 4) as u32, mData: o.as_mut_ptr() as *mut c_void };
        let b0 = mk(&mut self.out[0]);
        let b1 = if ch > 1 { mk(&mut self.out[1]) } else { b0 };
        let mut list = AudioBufferList2 { mNumberBuffers: ch as u32, mBuffers: [b0, b1] };
        let ts = AudioTimeStamp { mSampleTime: self.sample_time, mFlags: 1, ..Default::default() };
        let mut flags = 0u32;
        // SAFETY: Puffer reichen für `frames`, Liste und Zeitstempel leben über den Aufruf
        check(unsafe { AudioUnitRender(self.unit, &mut flags, &ts, 0, frames as u32, &mut list) }, "Render")?;
        self.sample_time += frames as f64;
        for c in 0..ch {
            for t in 0..frames {
                interleaved[t * ch + c] = self.out[c][t];
            }
        }
        Ok(())
    }
}

impl Drop for AuPlugin {
    fn drop(&mut self) {
        // SAFETY: Instanz gehört uns
        unsafe {
            AudioUnitUninitialize(self.unit);
            AudioComponentInstanceDispose(self.unit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apple_delay_is_listed_and_processes() {
        let list = list_effects();
        assert!(list.iter().any(|(id, _)| id == "au:aufx:dely:appl"), "{list:?}");
        let mut p = AuPlugin::load("au:aufx:dely:appl", 48_000.0, 2, 4096).unwrap();
        // Impuls rein: AUDelay (Voreinstellung 1 s, 50 % nass) gibt ihn trocken und verzögert aus
        let mut seen_dry = false;
        for block in 0..10 {
            let mut buf = vec![0.0f32; 480 * 2];
            if block == 0 {
                buf[0] = 1.0;
                buf[1] = 1.0;
            }
            p.process(&mut buf).unwrap();
            if block == 0 {
                seen_dry = buf[0].abs() > 0.1;
            }
        }
        assert!(seen_dry, "kein Ausgang");
        assert_eq!(fourcc_str(fourcc("aufx").unwrap()), "aufx");
        assert!(parse_id("au:aufx:dely").is_none());
    }
}
