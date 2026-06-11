//! Stroke ↔ utterance link resolution (spec/CAPTURE.md §9, DECISIONS
//! K9/C5/X2) — pure span math over the capture clock.
//!
//! Resolution runs EXACTLY ONCE per event, at its commit, over
//! already-committed same-session events; the link lives on the
//! LATER-committed event as a backward-pointing `linked_event`. The engine
//! owns candidate bookkeeping and the in-flight suppression gate; this
//! module owns the rule.

use crate::id::{ContentHash, EventId};

/// §9.1 rule 2's fallback gap cap.
pub const FALLBACK_GAP_MS: u64 = 10_000;

/// A committed utterance candidate: VAD span (capture clock) + bound
/// targets (empty = session-level).
#[derive(Debug, Clone)]
pub struct UtteranceSpan {
    pub id: EventId,
    pub start: u64,
    pub end: u64,
    pub targets: Vec<ContentHash>,
}

/// A committed stroke candidate: pen-down .. pen-up (capture clock) on one
/// image.
#[derive(Debug, Clone)]
pub struct StrokeSpan {
    pub id: EventId,
    pub start: u64,
    pub end: u64,
    pub image: ContentHash,
}

fn overlap_ms(a: (u64, u64), b: (u64, u64)) -> u64 {
    let lo = a.0.max(b.0);
    let hi = a.1.min(b.1);
    hi.saturating_sub(lo)
}

fn gap_ms(a: (u64, u64), b: (u64, u64)) -> u64 {
    // One side saturates to zero; touching/overlapping spans gap at 0.
    (b.0.saturating_sub(a.1)).max(a.0.saturating_sub(b.1))
}

/// §9.1 over generic candidates: (1) overlap — greatest overlap duration
/// wins (ties: the later-committed candidate, i.e. greatest id —
/// deterministic); (2) else nearest with span-gap ≤ 10 s among candidates
/// passing the scope gate; (3) else unlinked, permanently.
fn resolve<'c, C>(
    span: (u64, u64),
    candidates: impl Iterator<Item = &'c C>,
    candidate_span: impl Fn(&C) -> (u64, u64),
    candidate_id: impl Fn(&C) -> &EventId,
    scope_gate: impl Fn(&C) -> bool,
) -> Option<EventId>
where
    C: 'c,
{
    let mut best_overlap: Option<(u64, &'c C)> = None;
    let mut best_near: Option<(u64, &'c C)> = None;
    for c in candidates {
        let cs = candidate_span(c);
        let ov = overlap_ms(span, cs);
        if ov > 0 {
            let better = match &best_overlap {
                None => true,
                Some((bo, bc)) => ov > *bo || (ov == *bo && candidate_id(c) > candidate_id(bc)),
            };
            if better {
                best_overlap = Some((ov, c));
            }
            continue;
        }
        // Fallback candidates: same scope context only (§9.1 rule 2).
        if !scope_gate(c) {
            continue;
        }
        let gap = gap_ms(span, cs);
        if gap > FALLBACK_GAP_MS {
            continue;
        }
        let better = match &best_near {
            None => true,
            Some((bg, bc)) => gap < *bg || (gap == *bg && candidate_id(c) > candidate_id(bc)),
        };
        if better {
            best_near = Some((gap, c));
        }
    }
    if let Some((_, c)) = best_overlap {
        return Some(candidate_id(c).clone());
    }
    best_near.map(|(_, c)| candidate_id(c).clone())
}

/// A committing STROKE looks backward at committed utterances. Fallback
/// gate: the utterance's bound target set must contain the stroke's image
/// (session-level utterances never link via fallback — §9.1).
pub fn resolve_stroke_link(
    stroke_span: (u64, u64),
    image: &ContentHash,
    utterances: &[UtteranceSpan],
) -> Option<EventId> {
    resolve(
        stroke_span,
        utterances.iter(),
        |u| (u.start, u.end),
        |u| &u.id,
        |u| u.targets.contains(image),
    )
}

/// A committing voice REMARK looks backward at committed strokes. The same
/// scope-context gate, seen from the utterance's side.
pub fn resolve_utterance_link(
    utterance_span: (u64, u64),
    bound_targets: &[ContentHash],
    strokes: &[StrokeSpan],
) -> Option<EventId> {
    resolve(
        utterance_span,
        strokes.iter(),
        |s| (s.start, s.end),
        |s| &s.id,
        |s| bound_targets.contains(&s.image),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> EventId {
        EventId::from_str_strict(s).unwrap()
    }

    fn h(n: u8) -> ContentHash {
        ContentHash::from_bytes_of(&[n])
    }

    fn utt(ids: &str, start: u64, end: u64, targets: Vec<ContentHash>) -> UtteranceSpan {
        UtteranceSpan {
            id: id(ids),
            start,
            end,
            targets,
        }
    }

    const A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAA";
    const B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAB";

    #[test]
    fn overlap_wins_and_greatest_overlap_breaks_contests() {
        let utts = vec![utt(A, 0, 1_000, vec![h(1)]), utt(B, 900, 5_000, vec![h(1)])];
        // Stroke 800..3000 overlaps A by 200 ms and B by 2100 ms → B.
        assert_eq!(resolve_stroke_link((800, 3_000), &h(1), &utts), Some(id(B)));
    }

    #[test]
    fn overlap_needs_no_scope_agreement() {
        // Session-level utterance (zero targets) CAN link via overlap; the
        // scope gate is rule 2's only (§13.8 d gates the no-overlap case).
        let utts = vec![utt(A, 0, 1_000, vec![])];
        assert_eq!(resolve_stroke_link((500, 700), &h(1), &utts), Some(id(A)));
    }

    #[test]
    fn fallback_links_nearest_within_10_s_in_the_same_scope_context() {
        let utts = vec![utt(A, 0, 1_000, vec![h(1)])];
        // 4 s after the utterance ends, same image → backward link (§13.8 b).
        assert_eq!(
            resolve_stroke_link((5_000, 6_000), &h(1), &utts),
            Some(id(A))
        );
        // 11 s after → unlinked (§13.8 c).
        assert_eq!(resolve_stroke_link((12_000, 13_000), &h(1), &utts), None);
        // Same gap, different image → unlinked (scope gate).
        assert_eq!(resolve_stroke_link((5_000, 6_000), &h(2), &utts), None);
        // Session-level utterances never link via fallback (§13.8 d).
        let session = vec![utt(A, 0, 1_000, vec![])];
        assert_eq!(resolve_stroke_link((5_000, 6_000), &h(1), &session), None);
    }

    #[test]
    fn utterance_side_resolution_mirrors_the_gate() {
        let strokes = vec![StrokeSpan {
            id: id(A),
            start: 0,
            end: 500,
            image: h(1),
        }];
        // Overlap → link regardless of targets.
        assert_eq!(
            resolve_utterance_link((400, 2_000), &[], &strokes),
            Some(id(A))
        );
        // Fallback: utterance bound to the stroke's image → link …
        assert_eq!(
            resolve_utterance_link((3_000, 4_000), &[h(1)], &strokes),
            Some(id(A))
        );
        // … bound elsewhere (or session-level) → no fallback.
        assert_eq!(
            resolve_utterance_link((3_000, 4_000), &[h(2)], &strokes),
            None
        );
        assert_eq!(resolve_utterance_link((3_000, 4_000), &[], &strokes), None);
    }
}
