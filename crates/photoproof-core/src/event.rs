//! The event record: kinds, sources, payloads, and shape validation.
//!
//! Contract: spec/EVENTS.md §2–3.

use std::collections::HashSet;

use thiserror::Error;

use crate::id::{ContentHash, EventId, SessionId, UtcMillis};

/// How the event entered (spec/EVENTS.md §2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    Voice,
    Typed,
    Pencil,
    System,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Voice => "voice",
            Source::Typed => "typed",
            Source::Pencil => "pencil",
            Source::System => "system",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "voice" => Source::Voice,
            "typed" => Source::Typed,
            "pencil" => Source::Pencil,
            "system" => Source::System,
            _ => return None,
        })
    }
}

/// What the event means (spec/EVENTS.md §2.2). Exactly six kinds (§3.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Remark,
    Rating,
    Stroke,
    Revision,
    Retraction,
    Redaction,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Remark => "remark",
            Kind::Rating => "rating",
            Kind::Stroke => "stroke",
            Kind::Revision => "revision",
            Kind::Retraction => "retraction",
            Kind::Redaction => "redaction",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "remark" => Kind::Remark,
            "rating" => Kind::Rating,
            "stroke" => Kind::Stroke,
            "revision" => Kind::Revision,
            "retraction" => Kind::Retraction,
            "redaction" => Kind::Redaction,
            _ => return None,
        })
    }

    /// Meta-events store zero targets and act through `target_event` (§2.3).
    pub fn is_meta(self) -> bool {
        matches!(self, Kind::Revision | Kind::Retraction | Kind::Redaction)
    }

    /// Content events: remark, rating, stroke.
    pub fn is_content(self) -> bool {
        !self.is_meta()
    }
}

/// Stroke tool enum; `"pencil"` is the only v1 value (spec/EVENTS.md §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    Pencil,
}

impl Tool {
    pub fn as_str(self) -> &'static str {
        match self {
            Tool::Pencil => "pencil",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pencil" => Some(Tool::Pencil),
            _ => None,
        }
    }
}

/// One stroke sample: `[x, y, p, t]` (spec/EVENTS.md §3.3, X1).
///
/// `x`/`y`: ten-thousandths of the display-oriented extent, −2500..=12500.
/// `p`: pressure per-mille 0..=1000 (1000 = device reports no pressure).
/// `t`: ms since pen-down; first point 0, non-decreasing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StrokePoint {
    pub x: i32,
    pub y: i32,
    pub p: u16,
    pub t: u32,
}

/// `stroke` payload (spec/EVENTS.md §3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrokePayload {
    /// Stroke base width, ten-thousandths of the display-oriented long edge.
    pub base_w: u32,
    /// EXIF orientation value (1..=8) of the display-oriented frame.
    pub orientation: u8,
    /// ≥ 1 point, ≤ 8192 (capture downsamples), capture order.
    pub points: Vec<StrokePoint>,
    pub tool: Tool,
}

/// Kind-specific payload (spec/EVENTS.md §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    /// Voice remark: per-segment ASR confidence (per-mille) + duration.
    Voice {
        conf_pm: u16,
        dur_ms: u32,
    },
    /// Rating value 0..=5 (`0` = explicit zero, distinct from never-rated).
    Rating {
        value: u8,
    },
    Stroke(StrokePayload),
}

/// One journal event (spec/EVENTS.md §2.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub id: EventId,
    /// Event schema version, currently 1.
    pub v: u32,
    pub session_id: SessionId,
    pub ts: UtcMillis,
    pub source: Source,
    pub kind: Kind,
    /// Ordered (selection order at capture); may be empty.
    pub targets: Vec<ContentHash>,
    pub text: Option<String>,
    pub payload: Option<Payload>,
    pub target_event: Option<EventId>,
    pub linked_event: Option<EventId>,
    /// Present iff content has been scrubbed (spec/EVENTS.md §7).
    pub redacted_by: Option<EventId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid event: {0}")]
pub struct ValidationError(pub String);

fn fail(msg: impl Into<String>) -> Result<(), ValidationError> {
    Err(ValidationError(msg.into()))
}

impl Event {
    pub fn scrubbed(&self) -> bool {
        self.redacted_by.is_some()
    }

    /// The scrubbed form (spec/EVENTS.md §4.3 D, §7): `text`/`payload`
    /// absent, `redacted_by` carrying the marker, all else intact.
    pub fn scrubbed_form(&self, redacted_by: EventId) -> Event {
        let mut e = self.clone();
        e.text = None;
        e.payload = None;
        e.redacted_by = Some(redacted_by);
        e
    }

    /// Full shape validation: the §2.2 matrix, §2.3 target counts, and the
    /// §3 per-kind field rules. `append()` rejects violations; `merge()`
    /// quarantines them.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.v != 1 {
            return fail(format!("unsupported event version {}", self.v));
        }

        // source × kind matrix (§2.2)
        let matrix_ok = match self.kind {
            Kind::Remark => matches!(self.source, Source::Voice | Source::Typed),
            Kind::Rating => self.source == Source::Typed,
            Kind::Stroke => self.source == Source::Pencil,
            Kind::Revision => self.source == Source::Typed,
            Kind::Retraction => matches!(self.source, Source::Voice | Source::System),
            Kind::Redaction => self.source == Source::System,
        };
        if !matrix_ok {
            return fail(format!(
                "source {} invalid for kind {}",
                self.source.as_str(),
                self.kind.as_str()
            ));
        }

        // target_event present iff meta kind (§2.1)
        if self.target_event.is_some() != self.kind.is_meta() {
            return fail("target_event present iff kind is revision/retraction/redaction");
        }

        // target counts (§2.3)
        match self.kind {
            Kind::Remark => {}
            Kind::Rating => {
                if self.targets.is_empty() {
                    return fail("rating requires at least one target");
                }
            }
            Kind::Stroke => {
                if self.targets.len() != 1 {
                    return fail("stroke requires exactly one target");
                }
            }
            Kind::Revision | Kind::Retraction | Kind::Redaction => {
                if !self.targets.is_empty() {
                    return fail("meta-events store zero targets");
                }
            }
        }
        let distinct: HashSet<&ContentHash> = self.targets.iter().collect();
        if distinct.len() != self.targets.len() {
            return fail("duplicate target hashes");
        }

        // linked_event: stroke or voice remark only (§2.1, X2)
        if self.linked_event.is_some()
            && !(self.kind == Kind::Stroke
                || (self.kind == Kind::Remark && self.source == Source::Voice))
        {
            return fail("linked_event only on stroke or voice remark");
        }

        if self.scrubbed() {
            // Scrubbed shape (§7 step 5): only remark/revision/stroke can be
            // redacted; text and payload are absent.
            if !matches!(self.kind, Kind::Remark | Kind::Revision | Kind::Stroke) {
                return fail(format!("kind {} cannot be redacted", self.kind.as_str()));
            }
            if self.text.is_some() || self.payload.is_some() {
                return fail("scrubbed event must carry no text or payload");
            }
            return Ok(());
        }

        // Unscrubbed content rules (§3)
        match self.kind {
            Kind::Remark => {
                match &self.text {
                    Some(t) if !t.trim().is_empty() => {}
                    _ => return fail("remark requires non-empty text"),
                }
                match (self.source, &self.payload) {
                    (Source::Voice, Some(Payload::Voice { conf_pm, .. })) => {
                        if *conf_pm > 1000 {
                            return fail("conf_pm must be 0..=1000");
                        }
                    }
                    (Source::Voice, _) => {
                        return fail("voice remark requires {conf_pm, dur_ms} payload");
                    }
                    (Source::Typed, None) => {}
                    (Source::Typed, Some(_)) => return fail("typed remark carries no payload"),
                    _ => unreachable!("matrix checked above"),
                }
            }
            Kind::Rating => {
                if self.text.is_some() {
                    return fail("rating carries no text");
                }
                match &self.payload {
                    Some(Payload::Rating { value }) => {
                        if *value > 5 {
                            return fail("rating value must be 0..=5");
                        }
                    }
                    _ => return fail("rating requires {value} payload"),
                }
            }
            Kind::Stroke => {
                if self.text.is_some() {
                    return fail("stroke carries no text");
                }
                match &self.payload {
                    Some(Payload::Stroke(p)) => validate_stroke_payload(p)?,
                    _ => return fail("stroke requires stroke payload"),
                }
            }
            Kind::Revision => {
                match &self.text {
                    Some(t) if !t.trim().is_empty() => {}
                    _ => return fail("revision requires non-empty text"),
                }
                if self.payload.is_some() {
                    return fail("revision carries no payload");
                }
            }
            Kind::Retraction | Kind::Redaction => {
                if self.text.is_some() || self.payload.is_some() {
                    return fail("retraction/redaction carry no text or payload");
                }
            }
        }
        Ok(())
    }
}

fn validate_stroke_payload(p: &StrokePayload) -> Result<(), ValidationError> {
    if !(1..=8).contains(&p.orientation) {
        return fail("stroke orientation must be 1..=8");
    }
    if p.points.is_empty() || p.points.len() > 8192 {
        return fail("stroke requires 1..=8192 points");
    }
    let mut prev_t: Option<u32> = None;
    for (i, pt) in p.points.iter().enumerate() {
        if !(-2500..=12500).contains(&pt.x) || !(-2500..=12500).contains(&pt.y) {
            return fail("stroke coordinates must be in -2500..=12500");
        }
        if pt.p > 1000 {
            return fail("stroke pressure must be 0..=1000");
        }
        match prev_t {
            None => {
                if i == 0 && pt.t != 0 {
                    return fail("first stroke point must have t = 0");
                }
            }
            Some(prev) => {
                if pt.t < prev {
                    return fail("stroke point times must be non-decreasing");
                }
            }
        }
        prev_t = Some(pt.t);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(n: u8) -> ContentHash {
        ContentHash::from_bytes_of(&[n])
    }

    fn base_remark() -> Event {
        Event {
            id: EventId::from_str_strict("01JZ5C4R2GW8Q1T9M3N7P5XKDA").unwrap(),
            v: 1,
            session_id: SessionId::from_str_strict("01JZ5C3HW0RD8PT2M6QK4V9XEA").unwrap(),
            ts: UtcMillis::parse("2026-06-09T14:23:05.123Z").unwrap(),
            source: Source::Typed,
            kind: Kind::Remark,
            targets: vec![hash(1)],
            text: Some("note".into()),
            payload: None,
            target_event: None,
            linked_event: None,
            redacted_by: None,
        }
    }

    #[test]
    fn matrix_rejects_invalid_pairs() {
        let mut e = base_remark();
        e.source = Source::Pencil;
        assert!(e.validate().is_err());
        e.source = Source::System;
        assert!(e.validate().is_err());
    }

    #[test]
    fn stroke_payload_rules() {
        let ok = StrokePayload {
            base_w: 40,
            orientation: 1,
            points: vec![StrokePoint {
                x: 0,
                y: 0,
                p: 1000,
                t: 0,
            }],
            tool: Tool::Pencil,
        };
        assert!(validate_stroke_payload(&ok).is_ok());
        let mut bad = ok.clone();
        bad.points[0].t = 5;
        assert!(validate_stroke_payload(&bad).is_err());
        let mut bad = ok.clone();
        bad.points[0].x = 12501;
        assert!(validate_stroke_payload(&bad).is_err());
        let mut bad = ok;
        bad.orientation = 9;
        assert!(validate_stroke_payload(&bad).is_err());
    }

    #[test]
    fn remark_requires_text() {
        let mut e = base_remark();
        e.text = Some("   ".into());
        assert!(e.validate().is_err());
        e.text = None;
        assert!(e.validate().is_err());
    }
}
