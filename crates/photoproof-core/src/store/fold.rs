//! Fold rules, precisely — in-memory over a fetched event set.
//!
//! Contract: spec/EVENTS.md §6. The store fetches a batched closure
//! (§10.1); everything here runs in application memory over that set.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::event::{Event, Kind, Payload, Source, StrokePayload};
use crate::id::{ContentHash, EventId, UtcMillis};

/// One folded journal entry (§6.2). Revisions/retractions/redactions are
/// never standalone entries; they act on their targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalEntry {
    Remark {
        id: EventId,
        ts: UtcMillis,
        source: Source,
        targets: Vec<ContentHash>,
        /// Effective text: latest live revision in the chain, else the
        /// remark's own text (§6.1).
        text: String,
        corrected: bool,
    },
    Rating {
        id: EventId,
        ts: UtcMillis,
        targets: Vec<ContentHash>,
        value: u8,
    },
    Stroke {
        id: EventId,
        ts: UtcMillis,
        target: ContentHash,
        payload: StrokePayload,
        /// Resolved in both directions: the stroke's own backward link, or a
        /// later event in the set linking back to this stroke (§3.3, X2).
        linked_event: Option<EventId>,
    },
    /// Scrubbed events fold to an inert stub: id, ts, kind, source; no
    /// content (§6.2, Q2).
    RedactedStub {
        id: EventId,
        ts: UtcMillis,
        kind: Kind,
        source: Source,
    },
}

/// A fetched event set keyed (and therefore ordered) by event id.
/// Log order is ULID order (§1.3); `BTreeMap` iteration is the journal.
pub(crate) struct FoldSet {
    pub events: BTreeMap<EventId, Event>,
}

impl FoldSet {
    pub fn new(events: BTreeMap<EventId, Event>) -> Self {
        Self { events }
    }

    pub fn get(&self, id: &EventId) -> Option<&Event> {
        self.events.get(id)
    }

    /// `retracted(id)` := ∃ retraction r: r.target_event == id (§6.1).
    /// Retractions are never themselves retracted or scrubbed (§3.5–3.6).
    pub fn retracted_ids(&self) -> HashSet<EventId> {
        self.events
            .values()
            .filter(|e| e.kind == Kind::Retraction)
            .filter_map(|e| e.target_event.clone())
            .collect()
    }

    /// `root(e)`: follow `target_event` hops until a non-revision event.
    /// Cycles and dangling hops yield `None` — the chain is inert (I10).
    pub fn root_id(&self, e: &Event) -> Option<EventId> {
        let mut seen: HashSet<&EventId> = HashSet::new();
        let mut cur = e;
        while cur.kind == Kind::Revision {
            if !seen.insert(&cur.id) {
                return None; // cycle: corrupt data, chain inert
            }
            let target = cur.target_event.as_ref()?;
            cur = self.get(target)?; // dangling: inert until merge heals it
        }
        Some(cur.id.clone())
    }

    /// root id → revision ids resolving to it, ascending id.
    pub fn chains(&self) -> HashMap<EventId, Vec<EventId>> {
        let mut out: HashMap<EventId, Vec<EventId>> = HashMap::new();
        for e in self.events.values() {
            if e.kind != Kind::Revision {
                continue;
            }
            if let Some(root) = self.root_id(e)
                && root != e.id
            {
                out.entry(root).or_default().push(e.id.clone());
            }
        }
        out
    }

    /// Effective text of a live root R (§6.1): text of the greatest-id live
    /// (non-retracted, non-scrubbed) revision in R's chain, else R's own
    /// text. Returns `(text, corrected)`.
    pub fn effective_text(
        &self,
        root: &Event,
        chains: &HashMap<EventId, Vec<EventId>>,
        retracted: &HashSet<EventId>,
    ) -> (Option<String>, bool) {
        if let Some(revs) = chains.get(&root.id) {
            for rid in revs.iter().rev() {
                let Some(rev) = self.get(rid) else { continue };
                if retracted.contains(rid) || rev.scrubbed() {
                    continue;
                }
                if let Some(t) = &rev.text {
                    return (Some(t.clone()), true);
                }
            }
        }
        (root.text.clone(), false)
    }

    /// `effective_targets(e)` (§2.3): content events answer their own
    /// targets; meta-events resolve through `target_event`; dangling ⇒ [].
    pub fn effective_targets(&self, e: &Event) -> Vec<ContentHash> {
        let mut seen: HashSet<&EventId> = HashSet::new();
        let mut cur = e;
        while cur.kind.is_meta() {
            if !seen.insert(&cur.id) {
                return Vec::new();
            }
            let Some(target) = cur.target_event.as_ref() else {
                return Vec::new();
            };
            match self.get(target) {
                Some(t) => cur = t,
                None => return Vec::new(),
            }
        }
        cur.targets.clone()
    }

    /// Backward links indexed forward: linked target id → linking event id
    /// ("linked" is symmetric even though storage is one-directional, §3.3).
    pub fn reverse_links(&self) -> HashMap<EventId, EventId> {
        let mut out = HashMap::new();
        for e in self.events.values() {
            if let Some(l) = &e.linked_event {
                out.entry(l.clone()).or_insert_with(|| e.id.clone());
            }
        }
        out
    }
}

/// Fold the set into journal entries (§6.2), ascending id, for the content
/// events admitted by `filter`.
pub(crate) fn fold_entries(
    set: &FoldSet,
    mut filter: impl FnMut(&Event) -> bool,
) -> Vec<JournalEntry> {
    let retracted = set.retracted_ids();
    let chains = set.chains();
    let rev_links = set.reverse_links();
    let mut out = Vec::new();
    for e in set.events.values() {
        if !e.kind.is_content() || !filter(e) {
            continue;
        }
        if retracted.contains(&e.id) {
            continue; // folds out, fully (I12)
        }
        match e.kind {
            Kind::Remark => {
                if e.scrubbed() {
                    out.push(JournalEntry::RedactedStub {
                        id: e.id.clone(),
                        ts: e.ts,
                        kind: e.kind,
                        source: e.source,
                    });
                } else {
                    let (text, corrected) = set.effective_text(e, &chains, &retracted);
                    let Some(text) = text else { continue };
                    out.push(JournalEntry::Remark {
                        id: e.id.clone(),
                        ts: e.ts,
                        source: e.source,
                        targets: e.targets.clone(),
                        text,
                        corrected,
                    });
                }
            }
            Kind::Rating => {
                let Some(Payload::Rating { value }) = &e.payload else {
                    continue;
                };
                out.push(JournalEntry::Rating {
                    id: e.id.clone(),
                    ts: e.ts,
                    targets: e.targets.clone(),
                    value: *value,
                });
            }
            Kind::Stroke => {
                if e.scrubbed() {
                    out.push(JournalEntry::RedactedStub {
                        id: e.id.clone(),
                        ts: e.ts,
                        kind: e.kind,
                        source: e.source,
                    });
                } else {
                    let Some(Payload::Stroke(p)) = &e.payload else {
                        continue;
                    };
                    let Some(target) = e.targets.first() else {
                        continue;
                    };
                    out.push(JournalEntry::Stroke {
                        id: e.id.clone(),
                        ts: e.ts,
                        target: target.clone(),
                        payload: p.clone(),
                        linked_event: e
                            .linked_event
                            .clone()
                            .or_else(|| rev_links.get(&e.id).cloned()),
                    });
                }
            }
            _ => unreachable!("is_content() checked"),
        }
    }
    out
}
