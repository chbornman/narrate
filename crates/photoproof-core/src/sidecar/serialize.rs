//! Canonical sidecar serialization (spec/SIDECARS.md §3.5, S1).
//!
//! UTF-8/LF, 2-space pretty-print, object keys sorted ascending by UTF-8
//! byte order at every nesting level, all-numeric arrays compact with
//! `", "` separators, absent fields omitted, exactly one trailing LF.
//! Bytes are a pure function of the value — no timestamps, no writer
//! identity, no platform variation.

use serde_json::Value;

use crate::id::UtcMillis;

/// Pretty canonical bytes of a JSON value, with the §3.5 trailing LF.
pub fn to_pretty_canonical(v: &Value) -> Vec<u8> {
    let mut out = String::new();
    write_pretty(&mut out, v, 0);
    out.push('\n');
    out.into_bytes()
}

/// Compact canonical bytes (EVENTS §4 normal form): sorted keys, no
/// whitespace, same escaping. Used for dedupe, opaque-blob storage, and to
/// hand pretty-printed event objects to the strict canonical event parser.
pub fn to_compact_canonical(v: &Value) -> Vec<u8> {
    let mut out = String::new();
    write_compact(&mut out, v);
    out.into_bytes()
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn write_pretty(out: &mut String, v: &Value, depth: usize) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => escape_into(out, s),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
            } else if items.iter().all(Value::is_number) {
                // §3.5 rule 3: all-numeric arrays compact on one line.
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    write_pretty(out, item, depth);
                }
                out.push(']');
            } else {
                out.push_str("[\n");
                for (i, item) in items.iter().enumerate() {
                    indent(out, depth + 1);
                    write_pretty(out, item, depth + 1);
                    if i + 1 < items.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                indent(out, depth);
                out.push(']');
            }
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
            } else {
                let keys = sorted_keys(map);
                out.push_str("{\n");
                for (i, k) in keys.iter().enumerate() {
                    indent(out, depth + 1);
                    escape_into(out, k);
                    out.push_str(": ");
                    write_pretty(out, &map[*k], depth + 1);
                    if i + 1 < keys.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                indent(out, depth);
                out.push('}');
            }
        }
    }
}

fn write_compact(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => escape_into(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_compact(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, k) in sorted_keys(map).into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                escape_into(out, k);
                out.push(':');
                write_compact(out, &map[k]);
            }
            out.push('}');
        }
    }
}

/// Keys sorted ascending by UTF-8 byte order (`String` `Ord` is byte-wise).
/// serde_json's default map is already a `BTreeMap`, but sorting explicitly
/// keeps determinism independent of feature flags.
fn sorted_keys(map: &serde_json::Map<String, Value>) -> Vec<&String> {
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort_unstable();
    keys
}

/// §3.5 rule 1 (= EVENTS §4.1 rule 4): escape only `"`, `\`, and control
/// characters U+0000–U+001F (`\b \f \n \r \t` where applicable, else
/// `\u00xx` lowercase hex); everything else, including non-ASCII, raw UTF-8.
pub(crate) fn escape_into(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{00}'..='\u{1f}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            _ => out.push(c),
        }
    }
    out.push('"');
}

/// UTC compact timestamp `YYYYMMDDThhmmssZ` (corrupt-aside suffixes, export
/// directory names).
pub fn compact_timestamp(ts: UtcMillis) -> String {
    // to_rfc3339: "2026-06-09T19:30:00.123Z" -> "20260609T193000Z"
    let s = ts.to_rfc3339();
    let mut out = String::with_capacity(16);
    for c in s[..19].chars() {
        if c != '-' && c != ':' {
            out.push(c);
        }
    }
    out.push('Z');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numeric_arrays_compact_others_multiline() {
        let v = json!({
            "points": [[1, 2, 3, 0], [4, 5, 6, 16]],
            "names": ["a", "b"],
            "empty": [],
            "n": 7
        });
        let s = String::from_utf8(to_pretty_canonical(&v)).unwrap();
        assert_eq!(
            s,
            "{\n  \"empty\": [],\n  \"n\": 7,\n  \"names\": [\n    \"a\",\n    \"b\"\n  ],\n  \"points\": [\n    [1, 2, 3, 0],\n    [4, 5, 6, 16]\n  ]\n}\n"
        );
    }

    #[test]
    fn escaping_matches_events_rules() {
        let v = json!({"t": "a\"b\\c\nd\u{1f}é"});
        let s = String::from_utf8(to_compact_canonical(&v)).unwrap();
        assert_eq!(s, "{\"t\":\"a\\\"b\\\\c\\nd\\u001fé\"}");
    }

    #[test]
    fn compact_ts() {
        let t = UtcMillis::parse("2026-06-09T19:30:00.123Z").unwrap();
        assert_eq!(compact_timestamp(t), "20260609T193000Z");
    }
}
