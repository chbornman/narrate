//! Stage 1 of the §5 pipeline: the NL parse — prompt construction against
//! the `LanguageModel` seam, the §5.1 LLM output JSON, and the validation
//! firewall that turns it into a typed [`ParsedQuery`].
//!
//! Contract: spec/RETRIEVAL.md §5.1. Every clause validates independently;
//! a clause the firewall rejects is **dropped** with a reason (debug-panel
//! visible), never a query failure. Collection names resolve per §10.3
//! (normalized Jaro-Winkler, threshold 0.80, status tiebreak). A wholly
//! undeserializable response, a model error, or a parse over the latency
//! budget falls back to the whole raw query (§5.1) — handled by the caller
//! in `hybrid`; this module only says *whether* the response made sense.

use photoproof_connectors::llm::{ChatMessage, ChatRequest, Lane, Role};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::id::UtcMillis;

use super::{
    CollectionRef, Comparison, DateField, DateRange, DroppedClause, Filter, PathMatch,
    RelativeRange, SearchError, Season, StringMatch, VolumeFilter,
};

/// §10.3 normative threshold for fuzzy collection-name resolution.
pub(crate) const COLLECTION_MATCH_THRESHOLD: f64 = 0.80;

// ---------------------------------------------------------------------------
// Grounding (the small lists the prompt carries; also the validation vocab)
// ---------------------------------------------------------------------------

/// One collections-store row, as resolution and grounding need it.
pub(crate) struct CollectionRow {
    pub id: String,
    pub name: String,
    /// 'active' | 'shelved' | 'done'.
    pub status: String,
    pub updated_ts: String,
}

/// Grounding lists (§5.1 prompt strategy) — also the hallucination
/// firewall's vocabulary: a camera/lens the library has never seen, or a
/// collection that resolves below threshold, drops its clause.
pub(crate) struct Grounding {
    pub collections: Vec<CollectionRow>,
    pub cameras: Vec<String>,
    pub lenses: Vec<String>,
    pub roots: Vec<String>,
}

/// §5.1: "distinct camera/lens strings from EXIF (≤ 40 each, most-used
/// first)".
const VOCAB_LIMIT: usize = 40;

pub(crate) fn load_grounding(conn: &Connection) -> Result<Grounding, SearchError> {
    let collections = {
        let mut stmt = conn
            .prepare_cached("SELECT id, name, status, updated_ts FROM collections ORDER BY name")?;
        let rows = stmt.query_map([], |r| {
            Ok(CollectionRow {
                id: r.get(0)?,
                name: r.get(1)?,
                status: r.get(2)?,
                updated_ts: r.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    let strings = |sql: &str| -> Result<Vec<String>, SearchError> {
        let mut stmt = conn.prepare_cached(sql)?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    };
    let cameras = strings(&format!(
        "SELECT camera_model FROM images WHERE camera_model IS NOT NULL
         GROUP BY camera_model ORDER BY COUNT(*) DESC, camera_model LIMIT {VOCAB_LIMIT}"
    ))?;
    let lenses = strings(&format!(
        "SELECT lens_model FROM images WHERE lens_model IS NOT NULL
         GROUP BY lens_model ORDER BY COUNT(*) DESC, lens_model LIMIT {VOCAB_LIMIT}"
    ))?;
    let roots =
        strings("SELECT display_name FROM roots WHERE state = 'active' ORDER BY display_name")?;
    Ok(Grounding {
        collections,
        cameras,
        lenses,
        roots,
    })
}

// ---------------------------------------------------------------------------
// §10.3 — fuzzy collection-name resolution
// ---------------------------------------------------------------------------

/// Lowercase + collapse internal whitespace, so "quiet  Hours" and
/// "Quiet Hours" compare equal before the similarity metric runs.
fn normalize_name(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn status_rank(status: &str) -> u8 {
    // §10.3 tiebreak order: active > shelved > done.
    match status {
        "active" => 0,
        "shelved" => 1,
        _ => 2,
    }
}

/// Resolve `raw` against the collections store. `Ok` carries the resolved
/// ref; `Err` carries the drop reason (§5.1: below threshold → dropped
/// with debug visibility).
pub(crate) fn resolve_collection(
    rows: &[CollectionRow],
    raw: &str,
) -> Result<CollectionRef, String> {
    let needle = normalize_name(raw);
    let mut best: Option<(f64, &CollectionRow)> = None;
    for row in rows {
        let sim = strsim::jaro_winkler(&needle, &normalize_name(&row.name));
        let better = match &best {
            None => true,
            Some((bs, br)) => {
                // Similarity first; ties by status (active > shelved >
                // done), then updated_ts (most recent), then id for
                // determinism.
                sim > *bs
                    || (sim == *bs
                        && (
                            status_rank(&row.status),
                            std::cmp::Reverse(row.updated_ts.as_str()),
                            row.id.as_str(),
                        ) < (
                            status_rank(&br.status),
                            std::cmp::Reverse(br.updated_ts.as_str()),
                            br.id.as_str(),
                        ))
            }
        };
        if better {
            best = Some((sim, row));
        }
    }
    match best {
        Some((sim, row)) if sim >= COLLECTION_MATCH_THRESHOLD => {
            let resolved = ulid::Ulid::from_string(&row.id)
                .map_err(|e| format!("collection id '{}' is not a ULID: {e}", row.id))?;
            Ok(CollectionRef {
                raw: raw.to_owned(),
                resolved: Some(resolved),
            })
        }
        Some((sim, row)) => Err(format!(
            "no collection \u{2265} {COLLECTION_MATCH_THRESHOLD:.2}: best '{}' {sim:.2}",
            row.name
        )),
        None => Err(format!(
            "no collection \u{2265} {COLLECTION_MATCH_THRESHOLD:.2}: no collections exist"
        )),
    }
}

// ---------------------------------------------------------------------------
// Prompt construction (§5.1 — wording iterable, structure normative)
// ---------------------------------------------------------------------------

pub(crate) fn build_request(raw_query: &str, grounding: &Grounding, now: UtcMillis) -> ChatRequest {
    let today = &now.to_rfc3339()[..10];
    let mut system = String::new();
    system.push_str(
        "You translate a photographer's search into structured filters plus a \
         semantic remainder, as JSON.\n",
    );
    system.push_str(&format!("Today's date is {today} (UTC).\n"));
    system.push_str(
        "Anything not expressible as a filter goes into `semantic` verbatim, \
         preserving the user's wording. Set `visual: true` only when the query \
         describes what is IN the picture (objects, colors, composition), not \
         what the photographer said or felt about it. Emit only fields you are \
         certain of; omit, never guess.\n",
    );
    let list = |title: &str, items: &[String]| -> String {
        if items.is_empty() {
            String::new()
        } else {
            format!("{title}: {}\n", items.join(", "))
        }
    };
    system.push_str(&list(
        "Collections",
        &grounding
            .collections
            .iter()
            .map(|c| format!("{} ({})", c.name, c.status))
            .collect::<Vec<_>>(),
    ));
    system.push_str(&list("Cameras", &grounding.cameras));
    system.push_str(&list("Lenses", &grounding.lenses));
    system.push_str(&list("Roots", &grounding.roots));

    // 4–6 few-shot pairs (§5.1): relative season date, collection fuzzy
    // name, rating + camera combo, pure-semantic, explicitly visual.
    let shots: [(&str, Value); 5] = [
        (
            "barns from last winter",
            json!({
                "filters": [{ "type": "date", "field": "captured",
                              "relative": { "unit": "season", "season": "winter", "n": 1 } }],
                "semantic": "barns", "visual": false
            }),
        ),
        (
            "the quiet hours picks rated 4 or up",
            json!({
                "filters": [{ "type": "collection", "name": "quiet hours" },
                            { "type": "rating", "op": "gte", "value": 4 }],
                "semantic": "picks", "visual": false
            }),
        ),
        (
            "shots on the x-t5 I marked with strokes",
            json!({
                "filters": [{ "type": "camera", "value": "x-t5" },
                            { "type": "has_strokes", "value": true }],
                "semantic": null, "visual": false
            }),
        ),
        (
            "that mournful, heavy feeling I kept coming back to",
            json!({
                "filters": [],
                "semantic": "that mournful, heavy feeling I kept coming back to",
                "visual": false
            }),
        ),
        (
            "red barn against snow",
            json!({
                "filters": [],
                "semantic": "red barn against snow", "visual": true
            }),
        ),
    ];
    let mut messages = vec![ChatMessage {
        role: Role::System,
        content: system,
    }];
    for (q, a) in shots {
        messages.push(ChatMessage {
            role: Role::User,
            content: q.to_owned(),
        });
        messages.push(ChatMessage {
            role: Role::Assistant,
            content: a.to_string(),
        });
    }
    messages.push(ChatMessage {
        role: Role::User,
        content: raw_query.to_owned(),
    });

    ChatRequest {
        messages,
        max_tokens: 512,
        temperature: 0.0,
        json_schema: Some(output_schema()),
        priority: Lane::Interactive,
    }
}

/// The §5.1 LLM output JSONSchema (compact notation made mechanical),
/// enforced via constrained decoding where the backend supports it.
fn output_schema() -> Value {
    let date = json!({
        "type": "object",
        "properties": {
            "type": { "const": "date" },
            "field": { "enum": ["captured", "annotated"] },
            "absolute": {
                "type": "object",
                "properties": {
                    "start": { "type": ["string", "null"] },
                    "end":   { "type": ["string", "null"] }
                },
                "additionalProperties": false
            },
            "relative": {
                "type": "object",
                "properties": {
                    "unit": { "enum": ["days","weeks","months","years","season","month","year"] },
                    "n": { "type": "integer" },
                    "season": { "enum": ["spring","summer","autumn","winter"] },
                    "month": { "type": "integer", "minimum": 1, "maximum": 12 },
                    "year": { "type": "integer" }
                },
                "required": ["unit"],
                "additionalProperties": false
            }
        },
        "required": ["type"],
        "additionalProperties": false
    });
    let simple = |t: &str, key: &str, val: Value| {
        json!({
            "type": "object",
            "properties": { "type": { "const": t }, key: val },
            "required": ["type", key],
            "additionalProperties": false
        })
    };
    json!({
        "type": "object",
        "properties": {
            "filters": {
                "type": "array",
                "items": { "anyOf": [
                    date,
                    simple("camera", "value", json!({ "type": "string" })),
                    simple("lens", "value", json!({ "type": "string" })),
                    simple("folder", "value", json!({ "type": "string" })),
                    json!({
                        "type": "object",
                        "properties": {
                            "type": { "const": "rating" },
                            "op": { "enum": ["eq", "gte", "lte"] },
                            "value": { "type": "integer", "minimum": 0, "maximum": 5 }
                        },
                        "required": ["type", "op", "value"],
                        "additionalProperties": false
                    }),
                    simple("collection", "name", json!({ "type": "string" })),
                    simple("volume", "value", json!({ "enum": ["online", "offline"] })),
                    simple("has_strokes", "value", json!({ "type": "boolean" })),
                    simple("source", "values", json!({
                        "type": "array",
                        "items": { "enum": ["voice", "typed", "pencil"] }
                    })),
                ] }
            },
            "semantic": { "type": ["string", "null"] },
            "visual": { "type": "boolean" }
        },
        "required": ["filters", "semantic", "visual"],
        "additionalProperties": false
    })
}

// ---------------------------------------------------------------------------
// Validation firewall (§5.1)
// ---------------------------------------------------------------------------

/// A validated parse: typed filters, the rejects, and the remainder.
pub(crate) struct ValidatedParse {
    pub filters: Vec<Filter>,
    pub dropped: Vec<DroppedClause>,
    pub semantic: Option<String>,
    pub visual: bool,
}

/// Validate the model's response text. `None` = wholly undeserializable
/// (the §5.1 fallback trigger); otherwise every filter clause validates
/// independently and rejects land in `dropped` with reasons.
pub(crate) fn validate_response(text: &str, grounding: &Grounding) -> Option<ValidatedParse> {
    let top: Value = serde_json::from_str(text.trim()).ok()?;
    let obj = top.as_object()?;
    let clauses = obj.get("filters")?.as_array()?.clone();
    let semantic = match obj.get("semantic")? {
        Value::Null => None,
        Value::String(s) if s.trim().is_empty() => None,
        Value::String(s) => Some(s.trim().to_owned()),
        _ => return None,
    };
    let visual = obj.get("visual")?.as_bool()?;

    let mut filters = Vec::new();
    let mut dropped = Vec::new();
    for clause in clauses {
        match validate_clause(&clause, grounding) {
            Ok(f) => filters.push(f),
            Err(reason) => dropped.push(DroppedClause {
                raw: clause,
                reason,
            }),
        }
    }
    Some(ValidatedParse {
        filters,
        dropped,
        semantic,
        visual,
    })
}

/// Deserialize one clause with per-variant `deny_unknown_fields` — the
/// "hallucination firewall": unknown type, unknown field, out-of-range
/// value, vocabulary miss, unresolvable collection name → `Err(reason)`.
fn validate_clause(clause: &Value, grounding: &Grounding) -> Result<Filter, String> {
    let obj = clause
        .as_object()
        .ok_or_else(|| "filter clause is not an object".to_owned())?;
    let ty = obj
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| "filter clause has no string `type`".to_owned())?
        .to_owned();
    // The discriminant is consumed here; the per-variant structs see only
    // the payload fields (deny_unknown_fields rejects anything extra).
    let mut payload = obj.clone();
    payload.remove("type");
    let payload = Value::Object(payload);

    match ty.as_str() {
        "date" => validate_date(payload),
        "camera" => {
            let w: ValueClause = de(payload)?;
            vocab_match(&grounding.cameras, &w.value, "camera")?;
            Ok(Filter::Camera(StringMatch::Contains(w.value)))
        }
        "lens" => {
            let w: ValueClause = de(payload)?;
            vocab_match(&grounding.lenses, &w.value, "lens")?;
            Ok(Filter::Lens(StringMatch::Contains(w.value)))
        }
        "folder" => {
            let w: ValueClause = de(payload)?;
            if w.value.trim().is_empty() {
                return Err("folder value is empty".into());
            }
            // The parser emits a folder NAME fragment, not a rooted path
            // (chips construct Subtree refs from the actual tree).
            Ok(Filter::Folder(PathMatch::NameContains(w.value)))
        }
        "rating" => {
            let w: RatingClause = de(payload)?;
            if w.value > 5 {
                return Err(format!("rating {} out of range 0..=5", w.value));
            }
            Ok(Filter::Rating(match w.op.as_str() {
                "eq" => Comparison::Eq(w.value),
                "gte" => Comparison::Gte(w.value),
                "lte" => Comparison::Lte(w.value),
                other => return Err(format!("unknown rating op '{other}'")),
            }))
        }
        "collection" => {
            let w: NameClause = de(payload)?;
            resolve_collection(&grounding.collections, &w.name).map(Filter::Collection)
        }
        "volume" => {
            let w: ValueClause = de(payload)?;
            Ok(Filter::Volume(match w.value.as_str() {
                "online" => VolumeFilter::Online,
                "offline" => VolumeFilter::Offline,
                other => return Err(format!("unknown volume '{other}'")),
            }))
        }
        "has_strokes" => {
            let w: BoolClause = de(payload)?;
            Ok(Filter::HasStrokes(w.value))
        }
        "source" => {
            let w: SourceClause = de(payload)?;
            if w.values.is_empty() {
                return Err("source list is empty".into());
            }
            let mut sources = Vec::with_capacity(w.values.len());
            for s in &w.values {
                sources.push(
                    crate::event::Source::parse(s)
                        .filter(|s| *s != crate::event::Source::System)
                        .ok_or_else(|| format!("unknown source '{s}'"))?,
                );
            }
            Ok(Filter::Source(sources))
        }
        other => Err(format!("unknown filter type '{other}'")),
    }
}

fn de<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, String> {
    serde_json::from_value(v).map_err(|e| e.to_string())
}

/// §5.1: camera/lens validate by case-insensitive containment against the
/// EXIF vocabulary; matching nothing drops the clause.
fn vocab_match(vocab: &[String], value: &str, what: &str) -> Result<(), String> {
    let needle = value.to_lowercase();
    if needle.trim().is_empty() {
        return Err(format!("{what} value is empty"));
    }
    if vocab.iter().any(|v| v.to_lowercase().contains(&needle)) {
        Ok(())
    } else {
        Err(format!("{what} '{value}' matches nothing in the library"))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ValueClause {
    value: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NameClause {
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BoolClause {
    value: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RatingClause {
    op: String,
    value: u8,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceClause {
    values: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DateClause {
    #[serde(default)]
    field: Option<String>,
    #[serde(default)]
    absolute: Option<AbsoluteClause>,
    #[serde(default)]
    relative: Option<RelativeClause>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AbsoluteClause {
    #[serde(default)]
    start: Option<String>,
    #[serde(default)]
    end: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RelativeClause {
    unit: String,
    #[serde(default)]
    n: Option<u32>,
    #[serde(default)]
    season: Option<String>,
    #[serde(default)]
    month: Option<u8>,
    #[serde(default)]
    year: Option<i32>,
}

fn validate_date(payload: Value) -> Result<Filter, String> {
    let w: DateClause = de(payload)?;
    let field = match w.field.as_deref() {
        // §5.1: default Captured.
        None | Some("captured") => DateField::Captured,
        Some("annotated") => DateField::Annotated,
        Some(other) => return Err(format!("unknown date field '{other}'")),
    };
    let range = match (w.absolute, w.relative) {
        (Some(a), None) => {
            let start = a.start.as_deref().map(parse_llm_ts).transpose()?;
            let end = a.end.as_deref().map(parse_llm_ts).transpose()?;
            if start.is_none() && end.is_none() {
                return Err("absolute date range has neither start nor end".into());
            }
            DateRange::Absolute { start, end }
        }
        (None, Some(r)) => DateRange::Relative(validate_relative(r)?),
        _ => return Err("date filter needs exactly one of absolute/relative".into()),
    };
    Ok(Filter::Date { field, range })
}

fn validate_relative(r: RelativeClause) -> Result<RelativeRange, String> {
    let n = || -> Result<u32, String> {
        match r.n {
            Some(n) if n >= 1 => Ok(n),
            Some(n) => Err(format!("relative n {n} must be >= 1")),
            None => Err("relative range needs n".into()),
        }
    };
    Ok(match r.unit.as_str() {
        "days" => RelativeRange::LastDays(n()?),
        "weeks" => RelativeRange::LastWeeks(n()?),
        "months" => RelativeRange::LastMonths(n()?),
        "years" => RelativeRange::LastYears(n()?),
        "season" => RelativeRange::Season {
            season: match r.season.as_deref() {
                Some("spring") => Season::Spring,
                Some("summer") => Season::Summer,
                Some("autumn") => Season::Autumn,
                Some("winter") => Season::Winter,
                Some(other) => return Err(format!("unknown season '{other}'")),
                None => return Err("season range needs a season".into()),
            },
            // "last winter" = n: 1; an omitted n means the most recent one.
            years_ago: r.n.unwrap_or(1),
        },
        "month" => {
            let month = r.month.ok_or("month range needs month")?;
            if !(1..=12).contains(&month) {
                return Err(format!("month {month} out of range 1..=12"));
            }
            RelativeRange::Month {
                month,
                year: r.year,
            }
        }
        "year" => RelativeRange::Year(r.year.ok_or("year range needs year")?),
        other => Err(format!("unknown relative unit '{other}'"))?,
    })
}

/// Lenient timestamp reading for model output: the canonical millisecond
/// form, the second-precision RFC 3339 form, or a bare date. Anything else
/// drops the clause (unparseable date — §5.1 firewall).
fn parse_llm_ts(s: &str) -> Result<UtcMillis, String> {
    let s = s.trim();
    if let Ok(t) = UtcMillis::parse(s) {
        return Ok(t);
    }
    if s.len() == 20
        && s.ends_with('Z')
        && let Ok(t) = UtcMillis::parse(&format!("{}.000Z", &s[..19]))
    {
        return Ok(t);
    }
    if s.len() == 10
        && let Ok(t) = UtcMillis::parse(&format!("{s}T00:00:00.000Z"))
    {
        return Ok(t);
    }
    Err(format!("unparseable date '{s}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_grounding() -> Grounding {
        Grounding {
            collections: Vec::new(),
            cameras: vec!["FUJIFILM X-T5".into()],
            lenses: Vec::new(),
            roots: Vec::new(),
        }
    }

    #[test]
    fn lenient_timestamps_parse_or_drop() {
        assert!(parse_llm_ts("2026-01-14T21:08:11.000Z").is_ok());
        assert!(parse_llm_ts("2026-01-14T21:08:11Z").is_ok());
        assert_eq!(
            parse_llm_ts("2026-01-14").unwrap().to_rfc3339(),
            "2026-01-14T00:00:00.000Z"
        );
        assert!(parse_llm_ts("January 14th").is_err());
    }

    #[test]
    fn unknown_fields_drop_the_clause() {
        let g = no_grounding();
        let v: Value = serde_json::json!({ "type": "camera", "value": "x-t5", "extra": 1 });
        assert!(validate_clause(&v, &g).is_err());
        let v: Value = serde_json::json!({ "type": "camera", "value": "x-t5" });
        assert!(validate_clause(&v, &g).is_ok());
    }

    #[test]
    fn collection_resolution_honors_threshold_and_status_tiebreak() {
        let rows = vec![
            CollectionRow {
                id: ulid::Ulid::new().to_string(),
                name: "Quiet Hours".into(),
                status: "done".into(),
                updated_ts: "2026-01-01T00:00:00.000Z".into(),
            },
            CollectionRow {
                id: ulid::Ulid::new().to_string(),
                name: "Quiet Hours".into(),
                status: "active".into(),
                updated_ts: "2026-01-01T00:00:00.000Z".into(),
            },
        ];
        let r = resolve_collection(&rows, "quiet hours").unwrap();
        assert_eq!(r.resolved.unwrap().to_string(), rows[1].id);
        // Far-off names drop with the best candidate named in the reason.
        let err = resolve_collection(&rows, "quieter melancholic series").unwrap_err();
        assert!(err.contains("Quiet Hours"), "{err}");
        assert!(err.contains("\u{2265} 0.80"), "{err}");
    }
}
