//! The §10.2 union-merge (C2 family), as a PURE function over two
//! documents. This is the ONLY implementation of the rules: file import
//! loads the database into a document, merges here, and writes the merged
//! result back — so the SQL layer never re-states a rule it could get
//! wrong.
//!
//! Rules (spec/RETRIEVAL.md §10.2):
//! - collections merge by id;
//! - `notes` set-union by note id (append-only, conflict-free);
//! - `members` union by `(collection_id, image_hash, added_ts)`; for a
//!   matching key a non-null `removed_ts` beats null (a recorded removal
//!   never un-happens by merging a stale copy), two non-null resolve to
//!   the earlier;
//! - metadata (name/description/status) last-writer-wins by `updated_ts`
//!   — the one sanctioned LWW exception, confined to display fields.

use std::collections::BTreeMap;

use super::doc::{CollectionEntry, CollectionsDoc, MemberEntry, NoteEntry};

/// Merge two documents. Commutative and idempotent: ties are broken by
/// deterministic byte order, never by argument position, so every replica
/// converges to identical bytes regardless of merge order.
pub fn merge_docs(a: &CollectionsDoc, b: &CollectionsDoc) -> CollectionsDoc {
    let mut by_id: BTreeMap<String, CollectionEntry> = BTreeMap::new();
    for c in a.collections.iter().chain(b.collections.iter()) {
        match by_id.get_mut(&c.id) {
            None => {
                by_id.insert(c.id.clone(), c.clone());
            }
            Some(existing) => merge_into(existing, c),
        }
    }
    let mut doc = CollectionsDoc {
        collections: by_id.into_values().collect(),
    };
    doc.normalize();
    doc
}

fn merge_into(ours: &mut CollectionEntry, theirs: &CollectionEntry) {
    // Metadata LWW by updated_ts (canonical fixed-width timestamps, so
    // string order is time order). On an exact updated_ts tie the
    // byte-wise lesser (name, description, status) tuple wins — arbitrary
    // but commutative, mirroring the rebuild path's "byte-wise lesser
    // canonical wins" tie rule (SIDECARS §10.2).
    let ours_key = (
        &ours.updated_ts,
        &ours.name,
        &ours.description,
        ours.status.as_str(),
    );
    let theirs_key = (
        &theirs.updated_ts,
        &theirs.name,
        &theirs.description,
        theirs.status.as_str(),
    );
    let theirs_wins = theirs.updated_ts > ours.updated_ts
        || (theirs.updated_ts == ours.updated_ts && theirs_key < ours_key);
    if theirs_wins {
        ours.name = theirs.name.clone();
        ours.description = theirs.description.clone();
        ours.status = theirs.status;
        ours.updated_ts = theirs.updated_ts.clone();
    }
    // created_ts should agree for one id; the earlier wins deterministically
    // if two writers ever disagreed.
    if theirs.created_ts < ours.created_ts {
        ours.created_ts = theirs.created_ts.clone();
    }

    // Notes: set-union by id. Same id with different content is producer
    // corruption; the (ts, text) lesser tuple wins so both sides converge.
    let mut notes: BTreeMap<&str, &NoteEntry> = BTreeMap::new();
    for n in ours.notes.iter().chain(theirs.notes.iter()) {
        notes
            .entry(n.id.as_str())
            .and_modify(|kept| {
                if (&n.ts, &n.text) < (&kept.ts, &kept.text) {
                    *kept = n;
                }
            })
            .or_insert(n);
    }
    let merged_notes: Vec<NoteEntry> = notes.into_values().cloned().collect();

    // Members: union by (image_hash, added_ts); non-null removed_ts beats
    // null, two non-null resolve to the earlier.
    let mut members: BTreeMap<(&str, &str), Option<&str>> = BTreeMap::new();
    for m in ours.members.iter().chain(theirs.members.iter()) {
        members
            .entry((m.image_hash.as_str(), m.added_ts.as_str()))
            .and_modify(|kept| {
                *kept = match (*kept, m.removed_ts.as_deref()) {
                    (None, r) => r,
                    (k, None) => k,
                    (Some(k), Some(r)) => Some(k.min(r)),
                };
            })
            .or_insert(m.removed_ts.as_deref());
    }
    let merged_members: Vec<MemberEntry> = members
        .into_iter()
        .map(|((hash, added), removed)| MemberEntry {
            image_hash: hash.to_owned(),
            added_ts: added.to_owned(),
            removed_ts: removed.map(str::to_owned),
        })
        .collect();

    ours.notes = merged_notes;
    ours.members = merged_members;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collections::doc::CollectionStatus;

    const C1: &str = "01HV00000000000000000000A1";
    const C2: &str = "01HV00000000000000000000B2";
    const N1: &str = "01HV0000000000000000000N01";
    const N2: &str = "01HV0000000000000000000N02";
    const T1: &str = "2026-01-01T10:00:00.000Z";
    const T2: &str = "2026-01-02T10:00:00.000Z";
    const T3: &str = "2026-01-03T10:00:00.000Z";

    fn hash(n: u8) -> String {
        format!("{:02x}", n).repeat(32)
    }

    fn coll(id: &str, name: &str, updated: &str) -> CollectionEntry {
        CollectionEntry {
            id: id.into(),
            name: name.into(),
            description: String::new(),
            status: CollectionStatus::Active,
            created_ts: T1.into(),
            updated_ts: updated.into(),
            notes: Vec::new(),
            members: Vec::new(),
        }
    }

    fn doc(collections: Vec<CollectionEntry>) -> CollectionsDoc {
        let mut d = CollectionsDoc { collections };
        d.normalize();
        d
    }

    fn note(id: &str, ts: &str, text: &str) -> NoteEntry {
        NoteEntry {
            id: id.into(),
            ts: ts.into(),
            text: text.into(),
        }
    }

    fn member(h: &str, added: &str, removed: Option<&str>) -> MemberEntry {
        MemberEntry {
            image_hash: h.into(),
            added_ts: added.into(),
            removed_ts: removed.map(str::to_owned),
        }
    }

    /// One table over the §10.2 member-interval resolution rules.
    #[test]
    fn member_removed_ts_resolution_table() {
        // (ours, theirs, expected) for a matching (image_hash, added_ts).
        let cases: &[(Option<&str>, Option<&str>, Option<&str>)] = &[
            // Both open: stays open.
            (None, None, None),
            // A recorded removal never un-happens by merging a stale copy.
            (Some(T2), None, Some(T2)),
            (None, Some(T2), Some(T2)),
            // Two different removals: the earlier wins.
            (Some(T2), Some(T3), Some(T2)),
            (Some(T3), Some(T2), Some(T2)),
            // Identical removals: unchanged.
            (Some(T2), Some(T2), Some(T2)),
        ];
        for (ours_r, theirs_r, expect) in cases {
            let h = hash(0xaa);
            let mut a = coll(C1, "x", T1);
            a.members = vec![member(&h, T1, *ours_r)];
            let mut b = coll(C1, "x", T1);
            b.members = vec![member(&h, T1, *theirs_r)];
            let merged = merge_docs(&doc(vec![a]), &doc(vec![b]));
            assert_eq!(
                merged.collections[0].members,
                vec![member(&h, T1, *expect)],
                "ours={ours_r:?} theirs={theirs_r:?}"
            );
        }
    }

    /// One side's (name, updated_ts) in the LWW table.
    type Meta<'a> = (&'a str, &'a str);

    /// Metadata rules: LWW by updated_ts; deterministic tie-break.
    #[test]
    fn metadata_lww_table() {
        // (ours: name/updated, theirs: name/updated, expected name).
        let cases: &[(Meta, Meta, &str)] = &[
            // Later updated_ts wins regardless of argument order.
            (("old", T1), ("new", T2), "new"),
            (("new", T2), ("old", T1), "new"),
            // Exact tie: byte-wise lesser tuple wins (commutative).
            (("beta", T1), ("alpha", T1), "alpha"),
            (("alpha", T1), ("beta", T1), "alpha"),
        ];
        for ((an, at), (bn, bt), expect) in cases {
            let merged = merge_docs(&doc(vec![coll(C1, an, at)]), &doc(vec![coll(C1, bn, bt)]));
            assert_eq!(
                merged.collections[0].name, *expect,
                "{an}@{at} vs {bn}@{bt}"
            );
            assert_eq!(
                merged.collections[0].updated_ts,
                *at.max(bt),
                "winning updated_ts"
            );
        }
    }

    /// Notes are set-union by id; disjoint collections pass through whole.
    #[test]
    fn notes_union_and_disjoint_collections() {
        let mut a1 = coll(C1, "a", T1);
        a1.notes = vec![note(N1, T1, "first")];
        let mut b1 = coll(C1, "a", T1);
        b1.notes = vec![note(N1, T1, "first"), note(N2, T2, "second")];
        let b2 = coll(C2, "b", T1);

        let merged = merge_docs(&doc(vec![a1]), &doc(vec![b1, b2]));
        assert_eq!(merged.collections.len(), 2);
        assert_eq!(
            merged.collections[0].notes,
            vec![note(N1, T1, "first"), note(N2, T2, "second")]
        );
        assert_eq!(merged.collections[1].id, C2);
    }

    /// Distinct added_ts are distinct interval rows, never collapsed:
    /// full membership history survives the merge (M4 temporal questions).
    #[test]
    fn member_history_intervals_are_preserved() {
        let h = hash(0xbb);
        let mut a = coll(C1, "x", T1);
        a.members = vec![member(&h, T1, Some(T2))]; // first interval, closed
        let mut b = coll(C1, "x", T1);
        b.members = vec![member(&h, T1, Some(T2)), member(&h, T3, None)]; // re-added
        let merged = merge_docs(&doc(vec![a]), &doc(vec![b]));
        assert_eq!(
            merged.collections[0].members,
            vec![member(&h, T1, Some(T2)), member(&h, T3, None)]
        );
    }

    /// Merge laws the C2 family promises: idempotent, commutative, and
    /// byte-deterministic output.
    #[test]
    fn merge_is_idempotent_and_commutative() {
        let h = hash(0xcc);
        let mut a1 = coll(C1, "alpha", T2);
        a1.notes = vec![note(N1, T1, "n")];
        a1.members = vec![member(&h, T1, None)];
        let mut b1 = coll(C1, "beta", T1);
        b1.members = vec![member(&h, T1, Some(T3)), member(&h, T3, None)];
        let a = doc(vec![a1]);
        let b = doc(vec![b1, coll(C2, "other", T1)]);

        let ab = merge_docs(&a, &b);
        let ba = merge_docs(&b, &a);
        assert_eq!(ab, ba, "commutative");
        assert_eq!(merge_docs(&ab, &ab), ab, "idempotent");
        assert_eq!(merge_docs(&ab, &b), ab, "absorbs inputs");
        assert_eq!(ab.to_bytes(), ba.to_bytes(), "byte-deterministic");
    }
}
