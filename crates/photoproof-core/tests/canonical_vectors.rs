//! spec/EVENTS.md §4.3 — the literal examples, reproduced byte-exactly,
//! plus §4.1 strictness rejections (part of invariant I5).

use photoproof_core::{
    ContentHash, Event, EventId, Kind, Payload, SessionId, Source, StrokePayload, StrokePoint,
    Tool, UtcMillis, canonical_json, parse_canonical,
};

const IMAGE_A: &str = "b3a91c0d5e7f20146aa8c3d9e1f5b2640c7d8e9f1a2b3c4d5e6f708192a3b4c5";
const IMAGE_B: &str = "4d1f8a2b9c0e3657d8e9f0a1b2c3d4e5f60718293a4b5c6d7e8f9012a3b4c5d6";
const SESSION_1: &str = "01JZ5C3HW0RD8PT2M6QK4V9XEA";
const SESSION_2: &str = "01JZ6A0PR4S6T8V0W2X4Y6Z8AB";

const VECTOR_A: &str = r#"{"id":"01JZ5C4R2GW8Q1T9M3N7P5XKDA","kind":"remark","session_id":"01JZ5C3HW0RD8PT2M6QK4V9XEA","source":"typed","targets":["b3a91c0d5e7f20146aa8c3d9e1f5b2640c7d8e9f1a2b3c4d5e6f708192a3b4c5","4d1f8a2b9c0e3657d8e9f0a1b2c3d4e5f60718293a4b5c6d7e8f9012a3b4c5d6"],"text":"these two are the spine of the quiet sequence","ts":"2026-06-09T14:23:05.123Z","v":1}"#;

const VECTOR_B_REMARK: &str = r#"{"id":"01JZ5C5RTQ8W0X2Y4Z6A8B0C2D","kind":"remark","payload":{"conf_pm":912,"dur_ms":4210},"session_id":"01JZ5C3HW0RD8PT2M6QK4V9XEA","source":"voice","targets":["b3a91c0d5e7f20146aa8c3d9e1f5b2640c7d8e9f1a2b3c4d5e6f708192a3b4c5"],"text":"keep this one it has that muddy light I keep chasing","ts":"2026-06-09T14:24:11.480Z","v":1}"#;

const VECTOR_B_REVISION: &str = r#"{"id":"01JZ5C7T8HV2X4B6D8F0G2J4KC","kind":"revision","session_id":"01JZ5C3HW0RD8PT2M6QK4V9XEA","source":"typed","target_event":"01JZ5C5RTQ8W0X2Y4Z6A8B0C2D","targets":[],"text":"keep this one it has that moody light I keep chasing","ts":"2026-06-09T14:26:02.007Z","v":1}"#;

const VECTOR_C_STROKE: &str = r#"{"id":"01JZ5C5SXK3M7P9Q1R3S5T7V9W","kind":"stroke","linked_event":"01JZ5C5RTQ8W0X2Y4Z6A8B0C2D","payload":{"base_w":40,"orientation":1,"points":[[3326,4593,1000,0],[3654,4337,1000,16],[3983,4591,1000,33],[3328,4601,1000,610]],"tool":"pencil"},"session_id":"01JZ5C3HW0RD8PT2M6QK4V9XEA","source":"pencil","targets":["b3a91c0d5e7f20146aa8c3d9e1f5b2640c7d8e9f1a2b3c4d5e6f708192a3b4c5"],"ts":"2026-06-09T14:24:12.950Z","v":1}"#;

const VECTOR_D_REDACTION: &str = r#"{"id":"01JZ6A0QZ3W5X7Y9Z1B3C5D7E9","kind":"redaction","session_id":"01JZ6A0PR4S6T8V0W2X4Y6Z8AB","source":"system","target_event":"01JZ5C5RTQ8W0X2Y4Z6A8B0C2D","targets":[],"ts":"2026-06-14T09:02:44.310Z","v":1}"#;

const VECTOR_D_SCRUBBED: &str = r#"{"id":"01JZ5C5RTQ8W0X2Y4Z6A8B0C2D","kind":"remark","redacted_by":"01JZ6A0QZ3W5X7Y9Z1B3C5D7E9","session_id":"01JZ5C3HW0RD8PT2M6QK4V9XEA","source":"voice","targets":["b3a91c0d5e7f20146aa8c3d9e1f5b2640c7d8e9f1a2b3c4d5e6f708192a3b4c5"],"ts":"2026-06-09T14:24:11.480Z","v":1}"#;

fn id(s: &str) -> EventId {
    EventId::from_str_strict(s).unwrap()
}

fn h(s: &str) -> ContentHash {
    ContentHash::from_hex(s).unwrap()
}

fn base(eid: &str, session: &str, ts: &str, source: Source, kind: Kind) -> Event {
    Event {
        id: id(eid),
        v: 1,
        session_id: SessionId::from_str_strict(session).unwrap(),
        ts: UtcMillis::parse(ts).unwrap(),
        source,
        kind,
        targets: Vec::new(),
        text: None,
        payload: None,
        target_event: None,
        linked_event: None,
        redacted_by: None,
    }
}

fn vector_a_event() -> Event {
    let mut e = base(
        "01JZ5C4R2GW8Q1T9M3N7P5XKDA",
        SESSION_1,
        "2026-06-09T14:23:05.123Z",
        Source::Typed,
        Kind::Remark,
    );
    e.targets = vec![h(IMAGE_A), h(IMAGE_B)];
    e.text = Some("these two are the spine of the quiet sequence".into());
    e
}

fn vector_b_remark_event() -> Event {
    let mut e = base(
        "01JZ5C5RTQ8W0X2Y4Z6A8B0C2D",
        SESSION_1,
        "2026-06-09T14:24:11.480Z",
        Source::Voice,
        Kind::Remark,
    );
    e.targets = vec![h(IMAGE_A)];
    e.text = Some("keep this one it has that muddy light I keep chasing".into());
    e.payload = Some(Payload::Voice {
        conf_pm: 912,
        dur_ms: 4210,
    });
    e
}

fn vector_b_revision_event() -> Event {
    let mut e = base(
        "01JZ5C7T8HV2X4B6D8F0G2J4KC",
        SESSION_1,
        "2026-06-09T14:26:02.007Z",
        Source::Typed,
        Kind::Revision,
    );
    e.target_event = Some(id("01JZ5C5RTQ8W0X2Y4Z6A8B0C2D"));
    e.text = Some("keep this one it has that moody light I keep chasing".into());
    e
}

fn vector_c_stroke_event() -> Event {
    let mut e = base(
        "01JZ5C5SXK3M7P9Q1R3S5T7V9W",
        SESSION_1,
        "2026-06-09T14:24:12.950Z",
        Source::Pencil,
        Kind::Stroke,
    );
    e.targets = vec![h(IMAGE_A)];
    e.linked_event = Some(id("01JZ5C5RTQ8W0X2Y4Z6A8B0C2D"));
    let pts = [
        (3326, 4593, 1000, 0),
        (3654, 4337, 1000, 16),
        (3983, 4591, 1000, 33),
        (3328, 4601, 1000, 610),
    ];
    e.payload = Some(Payload::Stroke(StrokePayload {
        base_w: 40,
        orientation: 1,
        points: pts
            .iter()
            .map(|&(x, y, p, t)| StrokePoint { x, y, p, t })
            .collect(),
        tool: Tool::Pencil,
    }));
    e
}

fn vector_d_redaction_event() -> Event {
    let mut e = base(
        "01JZ6A0QZ3W5X7Y9Z1B3C5D7E9",
        SESSION_2,
        "2026-06-14T09:02:44.310Z",
        Source::System,
        Kind::Redaction,
    );
    e.target_event = Some(id("01JZ5C5RTQ8W0X2Y4Z6A8B0C2D"));
    e
}

fn vector_d_scrubbed_event() -> Event {
    let mut e = base(
        "01JZ5C5RTQ8W0X2Y4Z6A8B0C2D",
        SESSION_1,
        "2026-06-09T14:24:11.480Z",
        Source::Voice,
        Kind::Remark,
    );
    e.targets = vec![h(IMAGE_A)];
    e.redacted_by = Some(id("01JZ6A0QZ3W5X7Y9Z1B3C5D7E9"));
    e
}

fn assert_vector(event: Event, literal: &str) {
    // Serialize → exact spec bytes.
    assert_eq!(
        String::from_utf8(canonical_json(&event)).unwrap(),
        literal,
        "canonical_json must reproduce the spec literal byte-exactly"
    );
    // Parse → identical event → byte-exact round trip (I5).
    let parsed = parse_canonical(literal.as_bytes()).unwrap();
    assert_eq!(parsed, event);
    assert_eq!(canonical_json(&parsed), literal.as_bytes());
}

#[test]
fn spec_4_3_vector_a_typed_remark_two_targets() {
    assert_vector(vector_a_event(), VECTOR_A);
}

#[test]
fn spec_4_3_vector_b_voice_remark() {
    assert_vector(vector_b_remark_event(), VECTOR_B_REMARK);
}

#[test]
fn spec_4_3_vector_b_revision() {
    assert_vector(vector_b_revision_event(), VECTOR_B_REVISION);
}

#[test]
fn spec_4_3_vector_c_linked_stroke() {
    assert_vector(vector_c_stroke_event(), VECTOR_C_STROKE);
}

#[test]
fn spec_4_3_vector_d_redaction_pair() {
    assert_vector(vector_d_redaction_event(), VECTOR_D_REDACTION);
    assert_vector(vector_d_scrubbed_event(), VECTOR_D_SCRUBBED);
}

// -- §4.1 strictness: rejected inputs ----------------------------------------

#[test]
fn rejects_floats_nulls_unknown_keys_and_non_canonical_bytes() {
    // Floats (rule 5).
    let float_v = VECTOR_A.replace("\"v\":1", "\"v\":1.0");
    assert!(parse_canonical(float_v.as_bytes()).is_err());
    let float_conf = VECTOR_B_REMARK.replace("\"conf_pm\":912", "\"conf_pm\":912.0");
    assert!(parse_canonical(float_conf.as_bytes()).is_err());

    // Nulls never appear (rule 6).
    let null_text = VECTOR_A.replace(
        "\"text\":\"these two are the spine of the quiet sequence\"",
        "\"text\":null",
    );
    assert!(parse_canonical(null_text.as_bytes()).is_err());

    // Unknown top-level / payload keys (rule 8).
    let extra = VECTOR_A.replace("\"v\":1}", "\"v\":1,\"zz\":1}");
    assert!(parse_canonical(extra.as_bytes()).is_err());
    let extra_payload = VECTOR_B_REMARK.replace("\"dur_ms\":4210", "\"dur_ms\":4210,\"zz\":1");
    assert!(parse_canonical(extra_payload.as_bytes()).is_err());

    // Unsorted keys (rule 3).
    let unsorted = VECTOR_A.replace(
        "{\"id\":\"01JZ5C4R2GW8Q1T9M3N7P5XKDA\",\"kind\":\"remark\"",
        "{\"kind\":\"remark\",\"id\":\"01JZ5C4R2GW8Q1T9M3N7P5XKDA\"",
    );
    assert!(parse_canonical(unsorted.as_bytes()).is_err());

    // Whitespace / trailing newline (rules 1–2).
    let spaced = VECTOR_A.replace("\"kind\":", "\"kind\": ");
    assert!(parse_canonical(spaced.as_bytes()).is_err());
    let trailing = format!("{VECTOR_A}\n");
    assert!(parse_canonical(trailing.as_bytes()).is_err());

    // Needless \uXXXX escape of a non-control character (rule 4).
    let escaped = VECTOR_A.replace("\"text\":\"these", "\"text\":\"\\u0074hese");
    assert!(parse_canonical(escaped.as_bytes()).is_err());

    // Uppercase hash (§1.1): rejected, not normalized.
    let upper = VECTOR_A.replace(
        "b3a91c0d5e7f20146aa8c3d9e1f5b2640c7d8e9f1a2b3c4d5e6f708192a3b4c5",
        "B3A91C0D5E7F20146AA8C3D9E1F5B2640C7D8E9F1A2B3C4D5E6F708192A3B4C5",
    );
    assert!(parse_canonical(upper.as_bytes()).is_err());

    // Lowercase ULID (§1.2): rejected.
    let lower_id = VECTOR_A.replace("01JZ5C4R2GW8Q1T9M3N7P5XKDA", "01jz5c4r2gw8q1t9m3n7p5xkda");
    assert!(parse_canonical(lower_id.as_bytes()).is_err());
}

#[test]
fn rejects_duplicate_keys_and_noncanonical_numbers() {
    // Duplicate JSON keys, top-level (rule 3/8: one sorted closed key set).
    let dup_top = VECTOR_A.replace("\"v\":1}", "\"v\":1,\"v\":1}");
    assert!(parse_canonical(dup_top.as_bytes()).is_err());
    // Duplicate payload key.
    let dup_payload = VECTOR_B_REMARK.replace("\"conf_pm\":912", "\"conf_pm\":912,\"conf_pm\":912");
    assert!(parse_canonical(dup_payload.as_bytes()).is_err());

    // Negative zero (rule 5: no -0).
    let neg_zero = VECTOR_B_REMARK.replace("\"conf_pm\":912", "\"conf_pm\":-0");
    assert!(parse_canonical(neg_zero.as_bytes()).is_err());

    // Exponent-form numbers, fractional and integral value (rule 5).
    let exp_frac = VECTOR_B_REMARK.replace("\"dur_ms\":4210", "\"dur_ms\":4.21e3");
    assert!(parse_canonical(exp_frac.as_bytes()).is_err());
    let exp_int = VECTOR_B_REMARK.replace("\"dur_ms\":4210", "\"dur_ms\":421e1");
    assert!(parse_canonical(exp_int.as_bytes()).is_err());

    // Leading zeros (rule 5: base 10, no leading zeros).
    let leading_zero = VECTOR_B_REMARK.replace("\"conf_pm\":912", "\"conf_pm\":0912");
    assert!(parse_canonical(leading_zero.as_bytes()).is_err());

    // Over-width integers: in-range JSON, out of range for the field.
    let wide_conf = VECTOR_B_REMARK.replace("\"conf_pm\":912", "\"conf_pm\":70000");
    assert!(parse_canonical(wide_conf.as_bytes()).is_err());
    // u64::MAX (exceeds i64), and one past u64::MAX (exceeds u64 entirely).
    let u64_max = VECTOR_B_REMARK.replace("\"dur_ms\":4210", "\"dur_ms\":18446744073709551615");
    assert!(parse_canonical(u64_max.as_bytes()).is_err());
    let past_u64 = VECTOR_B_REMARK.replace("\"dur_ms\":4210", "\"dur_ms\":18446744073709551616");
    assert!(parse_canonical(past_u64.as_bytes()).is_err());
}

#[test]
fn rejects_surrogate_escapes_bom_and_leading_whitespace() {
    // Lone surrogate escape: not a Unicode scalar value.
    let lone = VECTOR_A.replace("\"text\":\"these", "\"text\":\"\\ud800these");
    assert!(parse_canonical(lone.as_bytes()).is_err());
    // Well-formed surrogate PAIR escape: decodes fine, but rule 4 demands
    // raw UTF-8 for non-control characters — not canonical.
    let pair = VECTOR_A.replace("\"text\":\"these", "\"text\":\"\\ud83d\\ude00these");
    assert!(parse_canonical(pair.as_bytes()).is_err());

    // UTF-8 BOM (rule 1: no BOM).
    let mut bom = vec![0xEF, 0xBB, 0xBF];
    bom.extend_from_slice(VECTOR_A.as_bytes());
    assert!(parse_canonical(&bom).is_err());

    // Leading whitespace (rule 2: compact, nothing outside the object).
    for ws in [" ", "\n", "\t"] {
        let padded = format!("{ws}{VECTOR_A}");
        assert!(parse_canonical(padded.as_bytes()).is_err());
    }
}

// Property round-trip over arbitrary text (I5): any Rust string in `text`
// serializes to canonical bytes that parse back to the identical event and
// re-serialize byte-exactly.
#[test]
fn property_round_trip_arbitrary_text() {
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};

    let mut rng = StdRng::seed_from_u64(0x5eed);
    let mut tested = 0;
    while tested < 300 {
        let len = rng.gen_range(1..=60);
        let s: String = (0..len)
            .map(|_| {
                loop {
                    let c = match rng.gen_range(0..6u8) {
                        0 => char::from(rng.gen_range(0x00..=0x1Fu8)), // control chars
                        1 => char::from(rng.gen_range(0x20..=0x7Eu8)), // ASCII printable
                        2 => ['"', '\\', '/', '\u{7F}'][rng.gen_range(0..4)],
                        3 => match char::from_u32(rng.gen_range(0xA0..=0xD7FF)) {
                            Some(c) => c, // BMP non-ASCII
                            None => continue,
                        },
                        4 => match char::from_u32(rng.gen_range(0xE000..=0xFFFD)) {
                            Some(c) => c, // BMP above the surrogate gap
                            None => continue,
                        },
                        _ => match char::from_u32(rng.gen_range(0x1_0000..=0x10_FFFF)) {
                            Some(c) => c, // astral planes (incl. emoji)
                            None => continue,
                        },
                    };
                    break c;
                }
            })
            .collect();
        if s.trim().is_empty() {
            continue; // remark text must be non-empty after trim (§3.1)
        }
        let mut e = vector_a_event();
        e.text = Some(s);
        let bytes = canonical_json(&e);
        let parsed = parse_canonical(&bytes)
            .unwrap_or_else(|err| panic!("round-trip parse failed for {e:?}: {err}"));
        assert_eq!(parsed, e);
        assert_eq!(canonical_json(&parsed), bytes, "byte-exact (I5)");
        tested += 1;
    }
}

#[test]
fn control_chars_and_unicode_escape_rules() {
    // Text with newline, tab, quote, backslash, raw non-ASCII, and a control
    // char without a short escape (U+0001) — exercises rule 4 both ways.
    let mut e = vector_a_event();
    e.text = Some("a\nb\tc\"d\\e\u{0001}é — 写真".into());
    let bytes = canonical_json(&e);
    let s = String::from_utf8(bytes.clone()).unwrap();
    assert!(s.contains(r#"a\nb\tc\"d\\e\u0001é — 写真"#));
    let parsed = parse_canonical(&bytes).unwrap();
    assert_eq!(parsed, e);
    assert_eq!(canonical_json(&parsed), bytes);
}
