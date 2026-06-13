//! Property tests for sidecar parse / round-trip / portable-subset extraction.
//!
//! Two surfaces under test (spec/SIDECARS.md, library/foreign_sidecar.rs):
//!
//! 1. NATIVE `.photoproof.json` round-trip (`SidecarDoc` -> bytes -> parse):
//!    `write(model) -> read == model` for arbitrary valid sidecar contents, and
//!    the canonical byte form is a fixed point (idempotent re-serialize).
//! 2. NATIVE parser robustness: `parse_sidecar` rejects malformed / truncated /
//!    arbitrary bytes GRACEFULLY (a clean `Result`, never a panic).
//! 3. FOREIGN sidecar portable-subset extraction (`read_foreign_edit_from_str`):
//!    never panics on arbitrary unicode / field combinations.
//!
//! All property cases use a fixed, persistence-free `ProptestConfig` so CI is
//! deterministic. We respect the known-ignored
//! `s02_2_case_only_rename_relinks_sidecar` - none of this touches relink.

use std::collections::BTreeMap;

use photoproof_core::sidecar::{
    ImageSnapshot, ParsedSidecar, SessionEntry, SessionMeta, SidecarDoc, SidecarEntry,
    parse_sidecar,
};
use photoproof_core::{ContentHash, Event, Kind, Minter, SessionId, Source, UtcMillis};
use proptest::prelude::*;
use serde_json::Value;

/// A fixed, valid SessionId ULID for the generated docs (the value is not what
/// we vary; the sidecar CONTENTS are).
fn session_id() -> SessionId {
    SessionId::from_str_strict("01JZ5C4R2GW8Q1T9M3N7P5XKDA").expect("valid ULID")
}

/// Build a minted Remark event carrying arbitrary `text`, mirroring the proven
/// `common::Gen::remark` construction so it canonicalizes to a `Canonical`
/// entry (not an opaque blob) on parse.
fn remark_event(minter: &Minter, text: String, target: ContentHash) -> Event {
    let m = minter.mint();
    Event {
        id: m.id,
        v: 1,
        session_id: session_id(),
        ts: m.ts,
        source: Source::Typed,
        kind: Kind::Remark,
        targets: vec![target],
        text: Some(text),
        payload: None,
        target_event: None,
        linked_event: None,
        redacted_by: None,
    }
}

/// A strategy for VALID remark text on the round-trip path. EVENTS §4 requires
/// a remark to carry non-empty (non-whitespace) text, so a generated remark
/// must too - otherwise the canonical parser correctly preserves it as an
/// opaque blob rather than a `Canonical` event (proven below), which is NOT the
/// round-trip we are asserting here. We still exercise arbitrary unicode and
/// surrounding whitespace; we just guarantee a non-whitespace core character so
/// the event is a valid remark.
fn remark_text() -> impl Strategy<Value = String> {
    // A non-whitespace lead char, then arbitrary unicode, then trailing slack.
    ("[A-Za-z0-9]", "\\PC{0,128}").prop_map(|(lead, rest)| format!("{lead}{rest}"))
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    /// ROUND-TRIP IDENTITY: an arbitrary valid sidecar (image snapshot with a
    /// generated filename/byte_size + a set of remark events with arbitrary
    /// text) serialized to canonical bytes and re-parsed yields an EQUAL
    /// `SidecarDoc`. `write(model) -> read == model`.
    #[test]
    fn native_sidecar_round_trips(
        filename in ".{0,64}",
        byte_size in any::<u64>(),
        hash_seed in any::<u8>(),
        texts in prop::collection::vec(remark_text(), 0..6),
    ) {
        let minter = Minter::new();
        let target = ContentHash::from_bytes_of(&[hash_seed]);
        let snap = ImageSnapshot {
            hash: ContentHash::from_bytes_of(&[hash_seed, 0xAB]),
            filename,
            byte_size,
        };
        let mut doc = SidecarDoc::new(Some(snap));
        for t in texts {
            doc.events.push(SidecarEntry::Canonical(remark_event(&minter, t, target.clone())));
        }

        let bytes = doc.to_bytes();
        let ParsedSidecar::Doc(parsed) = parse_sidecar(&bytes).expect("valid sidecar parses") else {
            return Err(TestCaseError::fail("expected a Doc variant"));
        };

        // The events array is sorted ascending by id on BOTH serialize and
        // parse, so compare against the same-sorted original.
        let mut expected = doc.clone();
        expected.events.sort_by_key(SidecarEntry::sort_key);
        prop_assert_eq!(&*parsed, &expected);
    }

    /// IDEMPOTENCE: the canonical byte form is a fixed point - re-serializing a
    /// parsed doc reproduces the exact same bytes (§3.5 canonical form). This is
    /// the property a rewrite-preserving editor relies on.
    #[test]
    fn native_sidecar_bytes_are_a_fixed_point(
        filename in ".{0,64}",
        byte_size in any::<u64>(),
        hash_seed in any::<u8>(),
        texts in prop::collection::vec(remark_text(), 0..6),
    ) {
        let minter = Minter::new();
        let target = ContentHash::from_bytes_of(&[hash_seed]);
        let snap = ImageSnapshot {
            hash: ContentHash::from_bytes_of(&[hash_seed, 0xCD]),
            filename,
            byte_size,
        };
        let mut doc = SidecarDoc::new(Some(snap));
        for t in texts {
            doc.events.push(SidecarEntry::Canonical(remark_event(&minter, t, target.clone())));
        }
        let bytes = doc.to_bytes();
        let ParsedSidecar::Doc(parsed) = parse_sidecar(&bytes).expect("parses") else {
            return Err(TestCaseError::fail("expected a Doc variant"));
        };
        // bytes -> parse -> bytes is the identity on canonical bytes.
        prop_assert_eq!(parsed.to_bytes(), bytes);
    }

    /// OPAQUE PRESERVATION (§5.2): unknown top-level members and opaque session
    /// entries survive a round-trip verbatim - they are never dropped.
    #[test]
    fn native_sidecar_preserves_unknown_and_opaque(
        unknown_key in "[a-z_]{1,12}",
        unknown_num in any::<i64>(),
    ) {
        // Skip keys the parser models as known top-level members.
        prop_assume!(!matches!(
            unknown_key.as_str(),
            "format" | "schema_version" | "image" | "sessions" | "events"
        ));
        let snap = ImageSnapshot {
            hash: ContentHash::from_bytes_of(&[7]),
            filename: "x.ARW".into(),
            byte_size: 10,
        };
        let mut doc = SidecarDoc::new(Some(snap));
        doc.unknown_top.insert(unknown_key.clone(), Value::Number(unknown_num.into()));
        // An opaque session entry (a non-conforming session value).
        doc.sessions.insert(
            "01JZ5C4R2GW8Q1T9M3N7P5XKDA".to_owned(),
            SessionEntry::Opaque(Value::String("not-a-meta-object".into())),
        );

        let bytes = doc.to_bytes();
        let ParsedSidecar::Doc(parsed) = parse_sidecar(&bytes).expect("parses") else {
            return Err(TestCaseError::fail("expected a Doc variant"));
        };
        prop_assert_eq!(
            parsed.unknown_top.get(&unknown_key),
            Some(&Value::Number(unknown_num.into()))
        );
        prop_assert!(matches!(
            parsed.sessions.get("01JZ5C4R2GW8Q1T9M3N7P5XKDA"),
            Some(SessionEntry::Opaque(_))
        ));
    }

    /// GRACEFUL REJECTION: `parse_sidecar` on ARBITRARY bytes (random unicode,
    /// truncated JSON, garbage) NEVER panics - it returns a clean `Result`.
    #[test]
    fn parse_sidecar_never_panics_on_arbitrary_bytes(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        // The contract is "no panic / no UB", not a particular value.
        let _ = parse_sidecar(&bytes);
    }

    /// GRACEFUL REJECTION on TRUNCATED valid sidecars: take a real sidecar's
    /// bytes and cut them at an arbitrary point. Every prefix parses to a clean
    /// `Result` (almost always an `Err`, never a panic).
    #[test]
    fn truncated_sidecar_is_rejected_gracefully(cut in 0usize..400) {
        let snap = ImageSnapshot {
            hash: ContentHash::from_bytes_of(&[1]),
            filename: "IMG.ARW".into(),
            byte_size: 123,
        };
        let bytes = SidecarDoc::new(Some(snap)).to_bytes();
        let cut = cut.min(bytes.len());
        let _ = parse_sidecar(&bytes[..cut]);
    }

    /// A session-journal doc (`image: null`, §7.2) round-trips too - the
    /// `None` image variant is a valid sidecar shape.
    #[test]
    fn session_journal_doc_round_trips(app_version in ".{0,32}") {
        let mut doc = SidecarDoc::new(None);
        doc.sessions.insert(
            "01JZ5C4R2GW8Q1T9M3N7P5XKDA".to_owned(),
            SessionEntry::Meta(SessionMeta {
                started_at: UtcMillis::from_epoch_ms(1_700_000_000_000),
                app_version,
                unknown: BTreeMap::new(),
            }),
        );
        let bytes = doc.to_bytes();
        let ParsedSidecar::Doc(parsed) = parse_sidecar(&bytes).expect("parses") else {
            return Err(TestCaseError::fail("expected a Doc variant"));
        };
        prop_assert_eq!(&parsed.image, &None);
        prop_assert_eq!(&*parsed, &doc);
    }

    // ---- foreign sidecar portable-subset extraction -----------------------

    /// FOREIGN NEVER-PANIC: `read_foreign_edit_from_str` on arbitrary unicode
    /// NEVER panics (the portable-subset extraction is strictly best-effort).
    #[test]
    fn foreign_extract_never_panics_on_arbitrary_unicode(s in "\\PC{0,256}") {
        let _ = photoproof_core::library::read_foreign_edit_from_str(&s);
    }

    /// FOREIGN NEVER-PANIC on arbitrary FIELD COMBINATIONS: assemble an XMP with
    /// a generated source namespace and arbitrary rating / orientation / crop
    /// values (including out-of-range), and assert no panic. When a crop IS
    /// surfaced, its bounds are the sane normalized rectangle the reader
    /// promises (in unit range, right>left, bottom>top).
    #[test]
    fn foreign_arbitrary_fields_never_panic_and_crop_is_sane(
        is_lr in any::<bool>(),
        rating in -100i64..100,
        orientation in 0u32..100,
        top in -2.0f64..2.0,
        left in -2.0f64..2.0,
        bottom in -2.0f64..2.0,
        right in -2.0f64..2.0,
    ) {
        let ns = if is_lr {
            "xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\""
        } else {
            "xmlns:darktable=\"http://darktable.sf.net/\""
        };
        let xmp = format!(
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\
             <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
             <rdf:Description rdf:about=\"\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" {ns} \
             xmp:Rating=\"{rating}\" tiff:Orientation=\"{orientation}\" \
             crs:HasCrop=\"True\" crs:CropTop=\"{top}\" crs:CropLeft=\"{left}\" \
             crs:CropBottom=\"{bottom}\" crs:CropRight=\"{right}\"/>\
             </rdf:RDF></x:xmpmeta>"
        );
        let edit = photoproof_core::library::read_foreign_edit_from_str(&xmp);
        if let Some(edit) = edit {
            // Rating, when surfaced, is always in the legible band -1..=5.
            if let Some(r) = edit.rating {
                prop_assert!((-1..=5).contains(&r), "rating out of band: {r}");
            }
            // Orientation, when surfaced, is a valid EXIF orientation 1..=8.
            if let Some(o) = edit.orientation {
                prop_assert!((1..=8).contains(&o), "orientation out of band: {o}");
            }
            // A surfaced crop is ALWAYS a sane normalized rectangle.
            if let Some(c) = edit.crop {
                let in_unit = |x: f64| (0.0..=1.0).contains(&x);
                prop_assert!(in_unit(c.top) && in_unit(c.left) && in_unit(c.bottom) && in_unit(c.right));
                prop_assert!(c.right > c.left && c.bottom > c.top);
            }
        }
    }
}
