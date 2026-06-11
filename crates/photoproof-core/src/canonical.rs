//! Canonical JSON serialization — **the** serialization sidecars embed.
//!
//! Contract: spec/EVENTS.md §4 (E1): UTF-8, compact, keys sorted ascending
//! byte order, integers only, no nulls, no `\uXXXX` escapes beyond control
//! characters, closed field set per `v`. Round-trip is byte-exact (I5).

use serde_json::Value;
use thiserror::Error;

use crate::event::{
    Event, Kind, Payload, Source, StrokePayload, StrokePoint, Tool, ValidationError,
};
use crate::id::{ContentHash, EventId, SessionId, UtcMillis};

#[derive(Debug, Error)]
pub enum CanonicalParseError {
    #[error("invalid JSON: {0}")]
    Json(String),
    #[error("canonical event must be a JSON object")]
    NotObject,
    #[error("unknown key for v:1: {0}")]
    UnknownKey(String),
    #[error("missing field: {0}")]
    Missing(&'static str),
    #[error("invalid field {0}: {1}")]
    Invalid(&'static str, String),
    #[error(transparent)]
    Validation(#[from] ValidationError),
    #[error("bytes are not in canonical form")]
    NotCanonical,
}

type PResult<T> = Result<T, CanonicalParseError>;

// ---------------------------------------------------------------------------
// Serialization
// ---------------------------------------------------------------------------

/// Canonical bytes of one event (spec/EVENTS.md §4.2 field order).
pub fn canonical_json(e: &Event) -> Vec<u8> {
    let mut out = Vec::with_capacity(256);
    out.extend_from_slice(b"{\"id\":");
    write_string(&mut out, e.id.as_str());
    out.extend_from_slice(b",\"kind\":");
    write_string(&mut out, e.kind.as_str());
    if let Some(l) = &e.linked_event {
        out.extend_from_slice(b",\"linked_event\":");
        write_string(&mut out, l.as_str());
    }
    if let Some(p) = &e.payload {
        out.extend_from_slice(b",\"payload\":");
        write_payload(&mut out, p);
    }
    if let Some(r) = &e.redacted_by {
        out.extend_from_slice(b",\"redacted_by\":");
        write_string(&mut out, r.as_str());
    }
    out.extend_from_slice(b",\"session_id\":");
    write_string(&mut out, e.session_id.as_str());
    out.extend_from_slice(b",\"source\":");
    write_string(&mut out, e.source.as_str());
    if let Some(t) = &e.target_event {
        out.extend_from_slice(b",\"target_event\":");
        write_string(&mut out, t.as_str());
    }
    out.extend_from_slice(b",\"targets\":[");
    for (i, h) in e.targets.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        write_string(&mut out, h.as_str());
    }
    out.push(b']');
    if let Some(t) = &e.text {
        out.extend_from_slice(b",\"text\":");
        write_string(&mut out, t);
    }
    out.extend_from_slice(b",\"ts\":");
    write_string(&mut out, &e.ts.to_rfc3339());
    out.extend_from_slice(b",\"v\":");
    out.extend_from_slice(e.v.to_string().as_bytes());
    out.push(b'}');
    out
}

/// Canonical JSON of a payload object alone (also the `payload` column form).
pub(crate) fn payload_canonical_json(p: &Payload) -> String {
    let mut out = Vec::new();
    write_payload(&mut out, p);
    String::from_utf8(out).expect("payload JSON is UTF-8")
}

fn write_payload(out: &mut Vec<u8>, p: &Payload) {
    match p {
        Payload::Voice { conf_pm, dur_ms } => {
            // conf_pm is optional (CAPTURE §6.5): omitted entirely when the
            // model exposes no token log-probs (§4.1 rule 6 — never null).
            out.push(b'{');
            if let Some(conf_pm) = conf_pm {
                out.extend_from_slice(b"\"conf_pm\":");
                out.extend_from_slice(conf_pm.to_string().as_bytes());
                out.push(b',');
            }
            out.extend_from_slice(b"\"dur_ms\":");
            out.extend_from_slice(dur_ms.to_string().as_bytes());
            out.push(b'}');
        }
        Payload::Rating { value } => {
            out.extend_from_slice(b"{\"value\":");
            out.extend_from_slice(value.to_string().as_bytes());
            out.push(b'}');
        }
        Payload::Stroke(s) => {
            out.extend_from_slice(b"{\"base_w\":");
            out.extend_from_slice(s.base_w.to_string().as_bytes());
            out.extend_from_slice(b",\"orientation\":");
            out.extend_from_slice(s.orientation.to_string().as_bytes());
            out.extend_from_slice(b",\"points\":[");
            for (i, pt) in s.points.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                out.push(b'[');
                out.extend_from_slice(pt.x.to_string().as_bytes());
                out.push(b',');
                out.extend_from_slice(pt.y.to_string().as_bytes());
                out.push(b',');
                out.extend_from_slice(pt.p.to_string().as_bytes());
                out.push(b',');
                out.extend_from_slice(pt.t.to_string().as_bytes());
                out.push(b']');
            }
            out.extend_from_slice(b"],\"tool\":");
            write_string(out, s.tool.as_str());
            out.push(b'}');
        }
    }
}

/// §4.1 rule 4: escape only `"`, `\`, and U+0000–U+001F (`\b \f \n \r \t`
/// where applicable, else `\u00xx` lowercase hex); everything else,
/// including non-ASCII, raw UTF-8.
fn write_string(out: &mut Vec<u8>, s: &str) {
    out.push(b'"');
    for &b in s.as_bytes() {
        match b {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x0c => out.extend_from_slice(b"\\f"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0x00..=0x1f => {
                out.extend_from_slice(format!("\\u{:04x}", b).as_bytes());
            }
            _ => out.push(b),
        }
    }
    out.push(b'"');
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

const TOP_LEVEL_KEYS: [&str; 12] = [
    "id",
    "kind",
    "linked_event",
    "payload",
    "redacted_by",
    "session_id",
    "source",
    "target_event",
    "targets",
    "text",
    "ts",
    "v",
];

/// Parse canonical bytes. Strict per §4.1: unknown keys, floats, nulls, or
/// any non-canonical formatting are rejected (the parsed event must
/// re-serialize to exactly the input bytes — I5).
pub fn parse_canonical(bytes: &[u8]) -> PResult<Event> {
    // Cheap §4.1 rule 1–2 pre-check: canonical bytes are exactly one compact
    // JSON object — first byte `{`, last byte `}`. Rejects a UTF-8 BOM,
    // leading whitespace, and trailing newlines before any JSON parsing.
    // The byte-identical re-serialization below remains the backstop.
    if bytes.first() != Some(&b'{') || bytes.last() != Some(&b'}') {
        return Err(CanonicalParseError::NotCanonical);
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|e| CanonicalParseError::Json(e.to_string()))?;
    let obj = value.as_object().ok_or(CanonicalParseError::NotObject)?;
    for k in obj.keys() {
        if !TOP_LEVEL_KEYS.contains(&k.as_str()) {
            return Err(CanonicalParseError::UnknownKey(k.clone()));
        }
    }

    let id = event_id_field(obj, "id")?.ok_or(CanonicalParseError::Missing("id"))?;
    let kind_s = str_field(obj, "kind")?.ok_or(CanonicalParseError::Missing("kind"))?;
    let kind = Kind::parse(kind_s)
        .ok_or_else(|| CanonicalParseError::Invalid("kind", kind_s.to_owned()))?;
    let source_s = str_field(obj, "source")?.ok_or(CanonicalParseError::Missing("source"))?;
    let source = Source::parse(source_s)
        .ok_or_else(|| CanonicalParseError::Invalid("source", source_s.to_owned()))?;
    let session_s =
        str_field(obj, "session_id")?.ok_or(CanonicalParseError::Missing("session_id"))?;
    let session_id = SessionId::from_str_strict(session_s)
        .map_err(|e| CanonicalParseError::Invalid("session_id", e.to_string()))?;
    let ts_s = str_field(obj, "ts")?.ok_or(CanonicalParseError::Missing("ts"))?;
    let ts =
        UtcMillis::parse(ts_s).map_err(|e| CanonicalParseError::Invalid("ts", e.to_string()))?;
    let v_raw = obj.get("v").ok_or(CanonicalParseError::Missing("v"))?;
    let v = int_of(v_raw, "v")?;
    let v = u32::try_from(v).map_err(|_| CanonicalParseError::Invalid("v", v.to_string()))?;

    let targets_v = obj
        .get("targets")
        .ok_or(CanonicalParseError::Missing("targets"))?;
    let targets_arr = targets_v
        .as_array()
        .ok_or_else(|| CanonicalParseError::Invalid("targets", "must be an array".into()))?;
    let mut targets = Vec::with_capacity(targets_arr.len());
    for t in targets_arr {
        let s = t
            .as_str()
            .ok_or_else(|| CanonicalParseError::Invalid("targets", "must be strings".into()))?;
        targets.push(
            ContentHash::from_hex(s)
                .map_err(|e| CanonicalParseError::Invalid("targets", e.to_string()))?,
        );
    }

    let text = str_field(obj, "text")?.map(str::to_owned);
    let target_event = event_id_field(obj, "target_event")?;
    let linked_event = event_id_field(obj, "linked_event")?;
    let redacted_by = event_id_field(obj, "redacted_by")?;

    let payload = match obj.get("payload") {
        None => None,
        Some(p) => Some(parse_payload_value(kind, source, p)?),
    };

    let event = Event {
        id,
        v,
        session_id,
        ts,
        source,
        kind,
        targets,
        text,
        payload,
        target_event,
        linked_event,
        redacted_by,
    };
    event.validate()?;

    // Final guarantee: byte-identical re-serialization (sorted keys, compact
    // form, no duplicate keys, no escapes beyond §4.1 rule 4, ...).
    if canonical_json(&event) != bytes {
        return Err(CanonicalParseError::NotCanonical);
    }
    Ok(event)
}

/// Parse the `payload` column / member for a given kind+source; closed key
/// set per §4.1 rule 8.
pub(crate) fn parse_payload_value(kind: Kind, source: Source, v: &Value) -> PResult<Payload> {
    let obj = v
        .as_object()
        .ok_or_else(|| CanonicalParseError::Invalid("payload", "must be an object".into()))?;
    match (kind, source) {
        (Kind::Remark, Source::Voice) => {
            expect_keys(obj, &["conf_pm", "dur_ms"])?;
            // conf_pm optional (CAPTURE §6.5); dur_ms required.
            let conf_pm = match obj.get("conf_pm") {
                None => None,
                Some(v) => {
                    let n = int_of(v, "conf_pm")?;
                    Some(
                        u16::try_from(n)
                            .map_err(|_| CanonicalParseError::Invalid("conf_pm", n.to_string()))?,
                    )
                }
            };
            let dur_ms = req_int(obj, "dur_ms")?;
            Ok(Payload::Voice {
                conf_pm,
                dur_ms: u32::try_from(dur_ms)
                    .map_err(|_| CanonicalParseError::Invalid("dur_ms", dur_ms.to_string()))?,
            })
        }
        (Kind::Rating, _) => {
            expect_keys(obj, &["value"])?;
            let value = req_int(obj, "value")?;
            Ok(Payload::Rating {
                value: u8::try_from(value)
                    .map_err(|_| CanonicalParseError::Invalid("value", value.to_string()))?,
            })
        }
        (Kind::Stroke, _) => {
            expect_keys(obj, &["base_w", "orientation", "points", "tool"])?;
            let base_w = req_int(obj, "base_w")?;
            let orientation = req_int(obj, "orientation")?;
            let tool_s = obj
                .get("tool")
                .and_then(Value::as_str)
                .ok_or_else(|| CanonicalParseError::Invalid("tool", "must be a string".into()))?;
            let tool = Tool::parse(tool_s)
                .ok_or_else(|| CanonicalParseError::Invalid("tool", tool_s.to_owned()))?;
            let points_v = obj
                .get("points")
                .and_then(Value::as_array)
                .ok_or_else(|| CanonicalParseError::Invalid("points", "must be an array".into()))?;
            let mut points = Vec::with_capacity(points_v.len());
            for pt in points_v {
                let tuple = pt.as_array().filter(|a| a.len() == 4).ok_or_else(|| {
                    CanonicalParseError::Invalid("points", "each point is [x,y,p,t]".into())
                })?;
                let x = int_of(&tuple[0], "points.x")?;
                let y = int_of(&tuple[1], "points.y")?;
                let p = int_of(&tuple[2], "points.p")?;
                let t = int_of(&tuple[3], "points.t")?;
                points.push(StrokePoint {
                    x: i32::try_from(x)
                        .map_err(|_| CanonicalParseError::Invalid("points.x", x.to_string()))?,
                    y: i32::try_from(y)
                        .map_err(|_| CanonicalParseError::Invalid("points.y", y.to_string()))?,
                    p: u16::try_from(p)
                        .map_err(|_| CanonicalParseError::Invalid("points.p", p.to_string()))?,
                    t: u32::try_from(t)
                        .map_err(|_| CanonicalParseError::Invalid("points.t", t.to_string()))?,
                });
            }
            Ok(Payload::Stroke(StrokePayload {
                base_w: u32::try_from(base_w)
                    .map_err(|_| CanonicalParseError::Invalid("base_w", base_w.to_string()))?,
                orientation: u8::try_from(orientation).map_err(|_| {
                    CanonicalParseError::Invalid("orientation", orientation.to_string())
                })?,
                points,
                tool,
            }))
        }
        _ => Err(CanonicalParseError::Invalid(
            "payload",
            format!("not allowed for {} {}", source.as_str(), kind.as_str()),
        )),
    }
}

/// Parse the `payload` column text from the database.
pub(crate) fn parse_payload_json(kind: Kind, source: Source, json: &str) -> PResult<Payload> {
    let v: Value =
        serde_json::from_str(json).map_err(|e| CanonicalParseError::Json(e.to_string()))?;
    parse_payload_value(kind, source, &v)
}

fn expect_keys(obj: &serde_json::Map<String, Value>, allowed: &[&str]) -> PResult<()> {
    for k in obj.keys() {
        if !allowed.contains(&k.as_str()) {
            return Err(CanonicalParseError::UnknownKey(format!("payload.{k}")));
        }
    }
    Ok(())
}

/// §4.1 rule 5: numbers are integers only — base 10, no `-0`, no
/// exponent/fraction. Floats are rejected outright.
fn int_of(v: &Value, field: &'static str) -> PResult<i64> {
    match v {
        Value::Number(n) => n
            .as_i64()
            .filter(|_| !n.is_f64())
            .ok_or_else(|| CanonicalParseError::Invalid(field, format!("not an integer: {n}"))),
        other => Err(CanonicalParseError::Invalid(
            field,
            format!("expected integer, got {other}"),
        )),
    }
}

fn req_int(obj: &serde_json::Map<String, Value>, key: &'static str) -> PResult<i64> {
    int_of(obj.get(key).ok_or(CanonicalParseError::Missing(key))?, key)
}

fn str_field<'a>(
    obj: &'a serde_json::Map<String, Value>,
    key: &'static str,
) -> PResult<Option<&'a str>> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(other) => Err(CanonicalParseError::Invalid(
            key,
            format!("expected string, got {other}"),
        )),
    }
}

fn event_id_field(
    obj: &serde_json::Map<String, Value>,
    key: &'static str,
) -> PResult<Option<EventId>> {
    match str_field(obj, key)? {
        None => Ok(None),
        Some(s) => {
            Ok(Some(EventId::from_str_strict(s).map_err(|e| {
                CanonicalParseError::Invalid(key, e.to_string())
            })?))
        }
    }
}
