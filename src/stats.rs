//! SRT-Statistik aus der `stats`-Eigenschaft von srtsink/srtsrc.

use gstreamer as gst;
use gst::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SrtSummary {
    pub connected: bool,
    pub peers: u32,
    pub rtt_ms: Option<f64>,
    pub rate_mbps: Option<f64>,
    pub bandwidth_mbps: Option<f64>,
    pub lost: u64,
    pub retransmitted: u64,
    pub dropped: u64,
    pub raw: Value,
}

pub fn value_to_json(v: &glib::SendValue) -> Value {
    use gst::glib;
    if let Ok(x) = v.get::<i64>() {
        return Value::from(x);
    }
    if let Ok(x) = v.get::<u64>() {
        return Value::from(x);
    }
    if let Ok(x) = v.get::<i32>() {
        return Value::from(x);
    }
    if let Ok(x) = v.get::<u32>() {
        return Value::from(x);
    }
    if let Ok(x) = v.get::<f64>() {
        return serde_json::Number::from_f64(x).map(Value::Number).unwrap_or(Value::Null);
    }
    if let Ok(x) = v.get::<bool>() {
        return Value::from(x);
    }
    if let Ok(x) = v.get::<String>() {
        return Value::from(x);
    }
    if let Ok(s) = v.get::<gst::Structure>() {
        return structure_to_json(&s);
    }
    if let Ok(arr) = v.get::<glib::ValueArray>() {
        return Value::Array(
            arr.iter()
                .map(|x| match x.get::<gst::Structure>() {
                    Ok(s) => structure_to_json(&s),
                    Err(_) => Value::String(format!("{x:?}")),
                })
                .collect(),
        );
    }
    if let Ok(arr) = v.get::<gst::Array>() {
        return Value::Array(arr.iter().map(value_to_json).collect());
    }
    if let Ok(arr) = v.get::<gst::List>() {
        return Value::Array(arr.iter().map(value_to_json).collect());
    }
    Value::String(format!("{v:?}"))
}

use gst::glib;

pub fn structure_to_json(s: &gst::StructureRef) -> Value {
    let mut m = Map::new();
    for (k, v) in s.iter() {
        m.insert(k.to_string(), value_to_json(v));
    }
    Value::Object(m)
}

fn num(o: &Map<String, Value>, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|k| o.get(*k).and_then(Value::as_f64))
}

fn add_counts(o: &Map<String, Value>, s: &mut SrtSummary) {
    s.lost += num(o, &["packets-sent-lost", "packets-received-lost"]).unwrap_or(0.0) as u64;
    s.retransmitted += num(o, &["packets-retransmitted"]).unwrap_or(0.0) as u64;
    s.dropped += num(o, &["packets-sent-dropped", "packets-received-dropped"]).unwrap_or(0.0) as u64;
}

/// Zusammenfassung über Caller-Statistik (flach) oder Listener-Statistik
/// (`callers` als Liste je Gegenstelle).
pub fn summarize(raw: Value) -> SrtSummary {
    let mut s = SrtSummary::default();
    if let Value::Object(o) = &raw {
        if let Some(Value::Array(callers)) = o.get("callers") {
            s.peers = callers.len() as u32;
            let mut rtts = Vec::new();
            let mut rate = 0.0;
            let mut bw: Option<f64> = None;
            for c in callers.iter().filter_map(Value::as_object) {
                add_counts(c, &mut s);
                if let Some(r) = num(c, &["rtt-ms"]) {
                    rtts.push(r);
                }
                rate += num(c, &["send-rate-mbps", "receive-rate-mbps"]).unwrap_or(0.0);
                bw = bw.or(num(c, &["bandwidth-mbps"]));
            }
            s.rtt_ms = rtts.into_iter().reduce(f64::max);
            s.rate_mbps = (s.peers > 0).then_some(rate);
            s.bandwidth_mbps = bw;
        } else {
            add_counts(o, &mut s);
            s.rtt_ms = num(o, &["rtt-ms"]);
            s.rate_mbps = num(o, &["send-rate-mbps", "receive-rate-mbps"]);
            s.bandwidth_mbps = num(o, &["bandwidth-mbps"]);
            let traffic = num(o, &["bytes-sent", "bytes-received", "packets-sent", "packets-received"]).unwrap_or(0.0);
            s.peers = u32::from(traffic > 0.0 || s.rtt_ms.is_some_and(|r| r > 0.0));
        }
    }
    s.connected = s.peers > 0;
    s.raw = raw;
    s
}

pub fn read(element: &gst::Element) -> SrtSummary {
    if element.find_property("stats").is_none() {
        return SrtSummary::default();
    }
    let st = element.property::<Option<gst::Structure>>("stats");
    match st {
        Some(st) => summarize(structure_to_json(&st)),
        None => SrtSummary::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn caller_flat_stats() {
        let s = summarize(json!({
            "packets-sent": 1000, "packets-sent-lost": 3, "packets-retransmitted": 5,
            "rtt-ms": 1.5, "send-rate-mbps": 8.2, "bandwidth-mbps": 900.0, "bytes-sent": 1316000
        }));
        assert!(s.connected);
        assert_eq!((s.lost, s.retransmitted), (3, 5));
        assert_eq!(s.rtt_ms, Some(1.5));
        assert_eq!(s.rate_mbps, Some(8.2));
    }

    #[test]
    fn listener_callers_list() {
        let s = summarize(json!({"callers": [
            {"rtt-ms": 2.0, "send-rate-mbps": 4.0, "packets-sent-lost": 1, "packets-retransmitted": 2},
            {"rtt-ms": 9.0, "send-rate-mbps": 4.5, "packets-sent-lost": 0, "packets-retransmitted": 1}
        ]}));
        assert_eq!(s.peers, 2);
        assert_eq!(s.rtt_ms, Some(9.0));
        assert_eq!(s.rate_mbps, Some(8.5));
        assert_eq!(s.retransmitted, 3);
    }

    #[test]
    fn listener_without_callers_is_not_connected() {
        let s = summarize(json!({"callers": []}));
        assert!(!s.connected);
        assert_eq!(s.rate_mbps, None);
    }

    #[test]
    fn structure_conversion() {
        gst::init().unwrap();
        let st = gst::Structure::builder("application/x-srt-statistics")
            .field("rtt-ms", 3.25f64)
            .field("packets-sent", 10i64)
            .field("bytes-sent", 13160u64)
            .build();
        let s = summarize(structure_to_json(&st));
        assert_eq!(s.rtt_ms, Some(3.25));
        assert!(s.connected);
    }
}
