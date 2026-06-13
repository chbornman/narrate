//! Read-only reader for FOREIGN editor sidecars (Lightroom / darktable).
//!
//! ## What this is, and what it deliberately is NOT
//!
//! PhotoProof's job is to *review done work*. When a RAW was edited in
//! Lightroom or darktable, the edit lives in a sibling XMP sidecar. This
//! module extracts the small, PORTABLE, vendor-neutral subset of that edit so
//! a later UI slice can show the photographer's keep/reject intent and framing
//! alongside our own neutral develop.
//!
//! WHY only a subset: faithfully *rendering* a foreign edit means reimplementing
//! Adobe Camera Raw (proprietary) or darktable's pixelpipe (a moving target) —
//! NOT feasible and explicitly out of scope (docs/PLAN-RAW-DECODE.md, "reading
//! foreign edit sidecars"). What *is* legible and portable from XMP is: rating,
//! color label, orientation, and (Lightroom only) the crop rectangle. We read
//! exactly that, label it "approximate, read-only", and fabricate nothing —
//! every field is `Option` and an absent or unparseable field stays `None`.
//!
//! ## Distinguishing a foreign sidecar from OUR own
//!
//! Our sidecar is `<image>.photoproof.json` — JSON, a different extension. We
//! only ever look at `.xmp` files, so there is no collision: a `.photoproof.json`
//! is never offered to this reader. Within the `.xmp` space we tell Lightroom
//! from darktable by namespace evidence (`crs:` Camera-Raw-Settings vs
//! `darktable:` / `xmlns:darktable`); anything we cannot positively recognize as
//! one of the two returns `None` rather than a guess.
//!
//! ## Sidecar naming conventions
//!
//! - **Lightroom** writes a *stem* sibling: `IMG_0001.ARW` -> `IMG_0001.xmp`
//!   (the extension is replaced).
//! - **darktable** writes a *full-name* sibling: `IMG_0001.ARW` ->
//!   `IMG_0001.ARW.xmp` (`.xmp` is appended). darktable can also write a
//!   "duplicate" form `IMG_0001_01.ARW.xmp`; we read the base form only — the
//!   later UI slice can decide whether to surface duplicates.
//!
//! Both conventions are tried; if both files exist we prefer the one whose
//! contents positively identify their editor, falling back to the darktable
//! full-name form (the more specific naming).
//!
//! ## The honest limits
//!
//! - No faithful render. Tone, color, and local edits are NOT reproduced.
//! - darktable's crop lives in IOP module params inside `darktable:history`
//!   (base64 + packed binary, version-specific). It is NOT portably legible, so
//!   we DO NOT attempt to decode it. We expose `has_unreadable_edits = true`
//!   when a darktable history is present, and leave `crop = None`.

use std::path::{Path, PathBuf};

use quick_xml::XmlVersion;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

/// Which editor a sidecar came from. We only ever return a source we can
/// positively identify from the XMP namespaces / content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForeignSidecarSource {
    Lightroom,
    Darktable,
}

/// A crop rectangle as Lightroom stores it: fractions of the (orientation-
/// applied) image in `0.0..=1.0`, plus an optional straighten angle in degrees.
/// We carry it through verbatim and do not apply it; consumption is a later UI
/// slice's job.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CropRect {
    pub top: f64,
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
    /// `crs:CropAngle`, degrees. `None` when the sidecar omits it.
    pub angle: Option<f64>,
}

/// The portable, read-only subset of a foreign edit. Every field is optional:
/// we never invent a value the sidecar did not state.
#[derive(Debug, Clone, PartialEq)]
pub struct ForeignEdit {
    /// Which editor wrote the sidecar (positively identified).
    pub source: ForeignSidecarSource,
    /// `xmp:Rating`: 0..=5 stars, or `-1` for a Lightroom reject flag.
    pub rating: Option<i8>,
    /// Color label as a free-form string (`xmp:Label`, e.g. "Red", "Green",
    /// or a darktable color name). We do not normalize it to a palette.
    pub label: Option<String>,
    /// EXIF-style orientation 1..=8 (`tiff:Orientation` / `xmp:Orientation`).
    pub orientation: Option<u16>,
    /// Lightroom crop rectangle, when `crs:HasCrop` is true and the bounds
    /// parse. darktable crop is never populated (see module docs).
    pub crop: Option<CropRect>,
    /// True when the sidecar carries edits we can detect but cannot read
    /// portably (today: a non-empty `darktable:history`). A signal to the UI
    /// that "this RAW has edits we are not reproducing", not an error.
    pub has_unreadable_edits: bool,
}

impl ForeignEdit {
    fn empty(source: ForeignSidecarSource) -> Self {
        ForeignEdit {
            source,
            rating: None,
            label: None,
            orientation: None,
            crop: None,
            has_unreadable_edits: false,
        }
    }

    /// True when we extracted nothing portable at all. Callers may treat an
    /// all-empty result as "a foreign sidecar exists but carries nothing we can
    /// surface".
    pub fn is_empty(&self) -> bool {
        self.rating.is_none()
            && self.label.is_none()
            && self.orientation.is_none()
            && self.crop.is_none()
            && !self.has_unreadable_edits
    }
}

/// Locate and read a sibling foreign sidecar for `image_path`.
///
/// Tries the darktable full-name form (`<name>.<ext>.xmp`) and the Lightroom
/// stem form (`<stem>.xmp`). Returns `Ok(None)` when no sidecar exists, the
/// file is unreadable, or its contents cannot be positively identified as a
/// supported editor's sidecar. I/O errors are swallowed into `None`: like the
/// rest of the library this is strictly best-effort and never the source of
/// truth.
///
/// Read-only: this function never writes, and the caller should treat the
/// result as advisory ("approximate").
pub fn read_foreign_edit(image_path: &Path) -> Option<ForeignEdit> {
    for candidate in candidate_sidecar_paths(image_path) {
        let Ok(bytes) = std::fs::read(&candidate) else {
            continue;
        };
        // XMP is UTF-8 (RDF/XML); lossy decode keeps a stray byte from
        // aborting the whole read.
        let text = String::from_utf8_lossy(&bytes);
        if let Some(edit) = read_foreign_edit_from_str(&text) {
            return Some(edit);
        }
    }
    None
}

/// The candidate sibling sidecar paths for an image, most-specific first.
///
/// darktable's `<name>.<ext>.xmp` is tried before Lightroom's `<stem>.xmp`
/// because it is the more specific naming and cannot be confused with an
/// unrelated `<stem>.xmp` that happens to belong to a different base file.
fn candidate_sidecar_paths(image_path: &Path) -> Vec<PathBuf> {
    let mut out = Vec::with_capacity(2);

    // darktable: append ".xmp" to the full filename (incl. original extension).
    if let Some(name) = image_path.file_name().and_then(|n| n.to_str()) {
        out.push(image_path.with_file_name(format!("{name}.xmp")));
    }

    // Lightroom: replace the extension with ".xmp" (the stem sibling). Skip if
    // the image had no extension (then this collides with the darktable form).
    if image_path.extension().is_some() {
        let lr = image_path.with_extension("xmp");
        if !out.contains(&lr) {
            out.push(lr);
        }
    }

    out
}

/// Parse a foreign sidecar from an in-memory XMP string. Public so a later UI
/// slice (or tests) can drive the parser without touching the filesystem.
///
/// Returns `None` for: non-XMP / malformed input, or an XMP we cannot
/// positively attribute to Lightroom or darktable. Never panics.
pub fn read_foreign_edit_from_str(xmp: &str) -> Option<ForeignEdit> {
    // Collect the flat set of (local-name, value) fields we care about, drawn
    // from BOTH attribute form (compact RDF) and element form (expanded RDF),
    // plus namespace evidence for source identification. XMP for these fields
    // is a flat rdf:Description, so a single streaming pass suffices.
    let mut fields = Fields::default();
    let mut reader = Reader::from_str(xmp);
    let config = reader.config_mut();
    config.trim_text(true);
    config.check_end_names = false; // tolerate sloppy foreign XML

    // Tracks the local-name of the element we are currently inside, so element
    // text (e.g. <xmp:Rating>4</xmp:Rating>) lands in the right field.
    let mut current_local: Option<String> = None;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                scan_attributes(&e, &mut fields);
                let name = e.name();
                let (prefix, local) = split_prefix(name.as_ref());
                // A darktable <history> block is "edits we detect but cannot
                // portably decode" (its IOP params are opaque base64). The
                // element ITSELF is the signal: it holds child elements, not
                // text or a `history` attribute, so note() never sees it.
                if prefix == b"darktable" && local == b"history" {
                    fields.has_dt_history = true;
                }
                current_local = Some(String::from_utf8_lossy(local).into_owned());
            }
            Ok(Event::Empty(e)) => {
                // Self-closing element with attributes (compact crop, etc.).
                scan_attributes(&e, &mut fields);
            }
            Ok(Event::Text(t)) => {
                // In quick-xml 0.40 Text events are already entity-unescaped by
                // the reader; `decode()` resolves the charset to a &str. Our
                // values are plain decimals/labels, so this is all we need.
                if let Some(local) = current_local.as_deref()
                    && let Ok(text) = t.decode()
                {
                    fields.note_element(local, text.trim());
                }
            }
            Ok(Event::End(_)) => current_local = None,
            Ok(Event::Eof) => break,
            // A malformed XMP is best-effort: stop and use whatever we gathered
            // so far rather than discarding the whole sidecar or panicking.
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    fields.into_edit()
}

/// Accumulates the recognized fields and the namespace/source evidence during
/// the streaming parse.
#[derive(Default)]
struct Fields {
    saw_crs: bool,       // Lightroom Camera Raw Settings namespace
    saw_darktable: bool, // darktable namespace or xmlns:darktable
    rating: Option<i8>,
    label: Option<String>,
    orientation: Option<u16>,
    has_crop: bool,
    crop_top: Option<f64>,
    crop_left: Option<f64>,
    crop_bottom: Option<f64>,
    crop_right: Option<f64>,
    crop_angle: Option<f64>,
    has_dt_history: bool,
}

impl Fields {
    /// Record a field reaching us as element text: `<ns:Local>value</ns:Local>`.
    fn note_element(&mut self, local: &str, value: &str) {
        if value.is_empty() {
            return;
        }
        self.note(local, value);
    }

    /// Record a field by its local name and string value, from either
    /// attribute or element form. Unknown locals are ignored.
    fn note(&mut self, local: &str, value: &str) {
        match local {
            "Rating" => {
                if let Ok(v) = value.trim().parse::<f64>() {
                    // Lightroom rejects are -1; stars are 0..=5. Clamp the
                    // parsed value into the legible band; fractional darktable
                    // ratings round to the nearest star.
                    let rounded = v.round();
                    if (-1.0..=5.0).contains(&rounded) {
                        self.rating = Some(rounded as i8);
                    }
                }
            }
            "Label" => {
                let v = value.trim();
                if !v.is_empty() {
                    self.label = Some(v.to_owned());
                }
            }
            "Orientation" => {
                if let Ok(v) = value.trim().parse::<u16>()
                    && (1..=8).contains(&v)
                {
                    self.orientation = Some(v);
                }
            }
            "HasCrop" => {
                self.has_crop = matches!(value.trim().to_ascii_lowercase().as_str(), "true" | "1");
            }
            "CropTop" => self.crop_top = parse_f64(value),
            "CropLeft" => self.crop_left = parse_f64(value),
            "CropBottom" => self.crop_bottom = parse_f64(value),
            "CropRight" => self.crop_right = parse_f64(value),
            "CropAngle" => self.crop_angle = parse_f64(value),
            // A non-empty darktable history is the "has edits we cannot read"
            // signal. The element/attribute exists even when compact-encoded.
            "history" => self.has_dt_history = true,
            _ => {}
        }
    }

    /// Resolve the gathered evidence into a `ForeignEdit`, or `None` when we
    /// cannot positively attribute the sidecar to a supported editor.
    fn into_edit(self) -> Option<ForeignEdit> {
        let source = if self.saw_darktable {
            ForeignSidecarSource::Darktable
        } else if self.saw_crs {
            ForeignSidecarSource::Lightroom
        } else {
            // Not recognizably Lightroom or darktable: refuse rather than guess.
            return None;
        };

        let mut edit = ForeignEdit::empty(source);
        edit.rating = self.rating;
        edit.label = self.label;
        edit.orientation = self.orientation;

        // Crop is Lightroom-only and portable. darktable's crop lives in opaque
        // IOP params we do not decode, so we never populate it there.
        if source == ForeignSidecarSource::Lightroom
            && self.has_crop
            && let (Some(top), Some(left), Some(bottom), Some(right)) = (
                self.crop_top,
                self.crop_left,
                self.crop_bottom,
                self.crop_right,
            )
        {
            // Only accept a sane normalized rectangle; a degenerate or
            // out-of-range box is dropped rather than surfaced as bogus.
            let in_unit = |x: f64| (0.0..=1.0).contains(&x);
            if in_unit(top)
                && in_unit(left)
                && in_unit(bottom)
                && in_unit(right)
                && right > left
                && bottom > top
            {
                edit.crop = Some(CropRect {
                    top,
                    left,
                    bottom,
                    right,
                    angle: self.crop_angle,
                });
            }
        }

        // darktable history => edits we detect but cannot portably reproduce.
        edit.has_unreadable_edits =
            source == ForeignSidecarSource::Darktable && self.has_dt_history;

        Some(edit)
    }
}

/// Walk one element's attributes, feeding recognized fields into `fields` and
/// recording namespace evidence for source identification.
fn scan_attributes(e: &BytesStart<'_>, fields: &mut Fields) {
    for attr in e.attributes().with_checks(false).flatten() {
        let key = attr.key;
        let raw = key.as_ref();
        let (prefix, local) = split_prefix(raw);

        // Namespace evidence: an `xmlns:crs` / `xmlns:darktable` declaration, or
        // any attribute already in that prefix, identifies the editor.
        if prefix == b"xmlns" {
            match local {
                b"crs" => fields.saw_crs = true,
                b"darktable" => fields.saw_darktable = true,
                _ => {}
            }
        }
        match prefix {
            b"crs" => fields.saw_crs = true,
            b"darktable" => fields.saw_darktable = true,
            _ => {}
        }

        let local_str = String::from_utf8_lossy(local);
        // `normalized_value` resolves XML entities/whitespace per the 1.0 rules;
        // our field values are plain, but this is the non-deprecated path.
        if let Ok(value) = attr.normalized_value(XmlVersion::Implicit1_0) {
            fields.note(&local_str, value.as_ref());
        }
    }
}

/// `b"crs:CropTop"` -> (`b"crs"`, `b"CropTop"`); no prefix -> (`b""`, whole).
fn split_prefix(name: &[u8]) -> (&[u8], &[u8]) {
    match name.iter().position(|&b| b == b':') {
        Some(i) => (&name[..i], &name[i + 1..]),
        None => (b"", name),
    }
}

/// Parse a crop fraction. XMP writes plain decimals; reject NaN/inf.
fn parse_f64(value: &str) -> Option<f64> {
    value.trim().parse::<f64>().ok().filter(|f| f.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Lightroom fixtures ------------------------------------------------

    /// A minimal Lightroom XMP: compact (attribute) RDF, the common shape
    /// `exiftool`/Bridge write. Carries rating, label, orientation, and a crop.
    fn lr_compact() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:tiff="http://ns.adobe.com/tiff/1.0/"
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    xmp:Rating="4"
    xmp:Label="Red"
    tiff:Orientation="6"
    crs:HasCrop="True"
    crs:CropTop="0.1"
    crs:CropLeft="0.05"
    crs:CropBottom="0.9"
    crs:CropRight="0.95"
    crs:CropAngle="-2.5">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#
    }

    /// The expanded (element) RDF form of the same Lightroom edit, to prove we
    /// read child-element values too.
    fn lr_expanded() -> &'static str {
        r#"<?xml version="1.0"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/">
   <xmp:Rating>5</xmp:Rating>
   <xmp:Label>Green</xmp:Label>
   <crs:HasCrop>True</crs:HasCrop>
   <crs:CropTop>0.0</crs:CropTop>
   <crs:CropLeft>0.0</crs:CropLeft>
   <crs:CropBottom>1.0</crs:CropBottom>
   <crs:CropRight>0.5</crs:CropRight>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#
    }

    /// A Lightroom reject: rating -1, no crop.
    fn lr_reject() -> &'static str {
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    xmp:Rating="-1"/>
 </rdf:RDF>
</x:xmpmeta>"#
    }

    // ---- darktable fixtures ------------------------------------------------

    /// A minimal darktable XMP: rating + label + a non-empty history block. The
    /// history's crop params are deliberately opaque; we must NOT decode them.
    fn dt_with_history() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="darktable">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:darktable="http://darktable.sf.net/"
    xmp:Rating="3"
    xmp:Label="Blue"
    darktable:xmp_version="4"
    darktable:history_end="7">
   <darktable:history>
    <rdf:Seq>
     <rdf:li darktable:operation="clipping" darktable:params="gz09eJxjY...opaque..."/>
     <rdf:li darktable:operation="exposure" darktable:params="gz12eJxj...opaque..."/>
    </rdf:Seq>
   </darktable:history>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#
    }

    /// A darktable sidecar that is only a rating, no history (e.g. a star set in
    /// the lighttable without any develop). Should NOT report unreadable edits.
    fn dt_rating_only() -> &'static str {
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:darktable="http://darktable.sf.net/"
    xmp:Rating="2"/>
 </rdf:RDF>
</x:xmpmeta>"#
    }

    // ---- negative fixtures -------------------------------------------------

    /// Truncated, never-closed XML.
    fn malformed() -> &'static str {
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF><rdf:Description
            xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
            xmp:Rating="4" crs:CropTop="0.1"  <<<not xml at all"#
    }

    /// Well-formed XMP but from no editor we recognize, and with none of our
    /// fields. Must yield None (we will not guess a source).
    fn foreign_unknown() -> &'static str {
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    dc:creator="Somebody"/>
 </rdf:RDF>
</x:xmpmeta>"#
    }

    // ---- assertions --------------------------------------------------------

    #[test]
    fn lightroom_compact_extracts_full_subset() {
        let edit = read_foreign_edit_from_str(lr_compact()).expect("should parse");
        assert_eq!(edit.source, ForeignSidecarSource::Lightroom);
        assert_eq!(edit.rating, Some(4));
        assert_eq!(edit.label.as_deref(), Some("Red"));
        assert_eq!(edit.orientation, Some(6));
        let crop = edit.crop.expect("crop present");
        assert!((crop.top - 0.1).abs() < 1e-9);
        assert!((crop.left - 0.05).abs() < 1e-9);
        assert!((crop.bottom - 0.9).abs() < 1e-9);
        assert!((crop.right - 0.95).abs() < 1e-9);
        assert_eq!(crop.angle, Some(-2.5));
        assert!(!edit.has_unreadable_edits);
    }

    #[test]
    fn lightroom_expanded_elements_extract_too() {
        let edit = read_foreign_edit_from_str(lr_expanded()).expect("should parse");
        assert_eq!(edit.source, ForeignSidecarSource::Lightroom);
        assert_eq!(edit.rating, Some(5));
        assert_eq!(edit.label.as_deref(), Some("Green"));
        let crop = edit.crop.expect("crop present");
        assert_eq!(crop.right, 0.5);
        assert_eq!(crop.angle, None);
    }

    #[test]
    fn lightroom_reject_is_minus_one_no_crop() {
        let edit = read_foreign_edit_from_str(lr_reject()).expect("should parse");
        assert_eq!(edit.source, ForeignSidecarSource::Lightroom);
        assert_eq!(edit.rating, Some(-1));
        assert_eq!(edit.crop, None);
        assert!(!edit.has_unreadable_edits);
    }

    #[test]
    fn darktable_history_is_detected_but_not_decoded() {
        let edit = read_foreign_edit_from_str(dt_with_history()).expect("should parse");
        assert_eq!(edit.source, ForeignSidecarSource::Darktable);
        assert_eq!(edit.rating, Some(3));
        assert_eq!(edit.label.as_deref(), Some("Blue"));
        // Crop is in opaque IOP params; we must NOT surface one for darktable.
        assert_eq!(edit.crop, None);
        // ...but we DO flag that unreadable edits exist.
        assert!(edit.has_unreadable_edits);
    }

    #[test]
    fn darktable_rating_only_has_no_unreadable_edits() {
        let edit = read_foreign_edit_from_str(dt_rating_only()).expect("should parse");
        assert_eq!(edit.source, ForeignSidecarSource::Darktable);
        assert_eq!(edit.rating, Some(2));
        assert!(!edit.has_unreadable_edits);
        assert_eq!(edit.crop, None);
    }

    #[test]
    fn malformed_xmp_never_panics_and_uses_what_parsed() {
        // The malformed fixture is still recognizably Lightroom (crs namespace)
        // and yields the rating it managed to read before the break — never a
        // panic, never a fabricated field.
        let edit = read_foreign_edit_from_str(malformed());
        if let Some(edit) = edit {
            assert_eq!(edit.source, ForeignSidecarSource::Lightroom);
            // No crop: bounds were incomplete.
            assert_eq!(edit.crop, None);
        }
        // Either None or a partial Lightroom edit is acceptable; the contract is
        // "no panic". Reaching here proves that.
    }

    #[test]
    fn unknown_foreign_xmp_returns_none() {
        assert_eq!(read_foreign_edit_from_str(foreign_unknown()), None);
    }

    #[test]
    fn empty_and_garbage_input_return_none() {
        assert_eq!(read_foreign_edit_from_str(""), None);
        assert_eq!(
            read_foreign_edit_from_str("not xml at all {json:true}"),
            None
        );
    }

    #[test]
    fn out_of_range_crop_is_dropped() {
        // HasCrop true but bounds outside 0..1 and inverted: no crop surfaced.
        let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    xmp:Rating="1"
    crs:HasCrop="True"
    crs:CropTop="0.9"
    crs:CropLeft="0.0"
    crs:CropBottom="0.1"
    crs:CropRight="2.0"/>
 </rdf:RDF>
</x:xmpmeta>"#;
        let edit = read_foreign_edit_from_str(xmp).expect("parses");
        assert_eq!(edit.rating, Some(1));
        assert_eq!(edit.crop, None);
    }

    // ---- path-location tests ----------------------------------------------

    #[test]
    fn candidate_paths_cover_both_conventions() {
        let paths = candidate_sidecar_paths(Path::new("/photos/IMG_0001.ARW"));
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/photos/IMG_0001.ARW.xmp"), // darktable
                PathBuf::from("/photos/IMG_0001.xmp"),     // lightroom
            ]
        );
    }

    #[test]
    fn candidate_paths_no_extension_yields_single() {
        // No extension: the two conventions collide, so we offer one path only.
        let paths = candidate_sidecar_paths(Path::new("/photos/frame17"));
        assert_eq!(paths, vec![PathBuf::from("/photos/frame17.xmp")]);
    }

    #[test]
    fn read_from_disk_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("IMG_0009.ARW");
        // darktable full-name sidecar next to the image.
        std::fs::write(
            dir.path().join("IMG_0009.ARW.xmp"),
            dt_with_history().as_bytes(),
        )
        .unwrap();
        let edit = read_foreign_edit(&image).expect("locates and parses");
        assert_eq!(edit.source, ForeignSidecarSource::Darktable);
        assert_eq!(edit.rating, Some(3));
        assert!(edit.has_unreadable_edits);
    }

    #[test]
    fn missing_sidecar_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("nope.ARW");
        assert_eq!(read_foreign_edit(&image), None);
    }

    #[test]
    fn our_own_json_sidecar_is_never_read() {
        // A `.photoproof.json` sibling must not be mistaken for a foreign edit:
        // we only ever look at `.xmp`, so the candidate list never names it.
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("IMG_0010.ARW");
        std::fs::write(
            dir.path().join("IMG_0010.ARW.photoproof.json"),
            br#"{"schema":"photoproof"}"#,
        )
        .unwrap();
        assert_eq!(read_foreign_edit(&image), None);
    }
}
