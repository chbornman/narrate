//! Coordinator wiring: the IPC search DTOs (`search_types`, RETRIEVAL §5.1
//! JSON shapes) mapped onto `photoproof_core::search` (packet P3.1) and the
//! engine's §5.4 results mapped back. No business logic — every semantic
//! lives in the engine; this module only translates encodings.
//!
//! Filters are hard constraints: a DTO this layer cannot translate is an
//! error, never a silent drop (RETRIEVAL §5.1 firewall discipline).

use photoproof_connectors::OrtEmbedder;
use photoproof_connectors::vector_store::VectorStore;
use photoproof_core::search::{
    self as core_search, CollectionRef, Comparison, DateField, DateRange, Filter as CoreFilter,
    PathMatch, RelativeRange as CoreRelativeRange, SearchError, Searcher, Season, StringMatch,
    VolumeFilter,
};
use photoproof_core::{Source, UtcMillis};

use crate::error::{CmdError, CmdResult};
use crate::search_types as dto;

impl From<SearchError> for CmdError {
    fn from(e: SearchError) -> Self {
        CmdError::Invalid(e.to_string())
    }
}

/// Which lane the `search` command runs (M3 search-as-scope, Phase 1).
///
/// The frontend's as-you-type path is held to a <100 ms budget (RETRIEVAL
/// §13.1). On a warm machine the auto lane (below) would pay vector latency
/// on EVERY keystroke; `Lexical` lets the live path FORCE the M1 keyword
/// rig even when embedders are ready — that is the guardrail mechanism.
/// `Semantic` is the commit-on-Enter full-hybrid lane. `Auto` preserves
/// today's behavior exactly (keyword when no embedder is ready, hybrid when
/// one is) and is the default for callers that pass no mode (the debug
/// search command, older tests) so this arg can never silently change them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchMode {
    /// Today's behavior: keyword when degraded, hybrid when embedders warm.
    #[default]
    Auto,
    /// Force the M1 keyword-only rig — the <100 ms as-you-type lane.
    Lexical,
    /// The full hybrid rig — the commit-on-Enter lane.
    Semantic,
}

impl SearchMode {
    /// Parse the wire string. `None`/absent ⇒ `Auto` (back-compat default).
    /// An unknown string is a hard error: a typo in the lane name must not
    /// silently fall back and quietly bust the keystroke budget.
    pub fn from_wire(s: Option<&str>) -> CmdResult<Self> {
        Ok(match s {
            None | Some("auto") => SearchMode::Auto,
            Some("lexical") => SearchMode::Lexical,
            Some("semantic") => SearchMode::Semantic,
            Some(other) => return Err(invalid(format!("unknown search mode '{other}'"))),
        })
    }
}

/// Execute a search and translate the result contract for IPC.
///
/// Runs the §5 hybrid pipeline. P7.4 decision 6: when an embedder is ready
/// the rig is fed from the `EmbedderHost` + the PPVEC store — query-time
/// text embedding for S1/S3, S4 CLIP query embedding (B69 always-votes).
/// **ABSOLUTE INVARIANT:** with NO embedder ready the call goes through the
/// `keyword_only_rig()` path UNCHANGED — byte-identical to today's M1
/// keyword search (the existing search tests pin this). The parse LLM seam
/// stays `None` in this lane (out of scope); NL parse wires in later.
pub fn run_search(
    app: &crate::state::App,
    raw: String,
    filters: Vec<dto::Filter>,
    mode: SearchMode,
) -> CmdResult<dto::SearchResults> {
    // The lexical lane forces the keyword rig REGARDLESS of embedder warmth
    // — this is the <100 ms guardrail: a warm machine must not pay vector
    // latency on the as-you-type path (RETRIEVAL §13.1). Identical to the
    // degraded branch below; routed first so warmth can never override it.
    if mode == SearchMode::Lexical {
        return run_search_keyword(&app.searcher, raw, filters);
    }

    let text = app.runtime.embedders.text();
    let clip = app.runtime.embedders.clip();

    if text.is_none() && clip.is_none() {
        // Degraded posture: no embedder ready ⇒ the exact M1 keyword path,
        // byte-for-byte (`run_search_keyword`). This branch must never
        // diverge from P7.2 behavior — the search tests pin it. (Auto and
        // Semantic both land here when nothing is warm — the rig is the
        // same degenerate keyword pipeline either way.)
        return run_search_keyword(&app.searcher, raw, filters);
    }

    // Semantic rig: the ready embedders + the vector store. `llm: None`
    // keeps the degenerate parse (no NL parse this lane). An embedder
    // present but its space empty still yields no vector hits — the signals
    // are additive (B69), never error.
    let core_filters: Vec<CoreFilter> = filters
        .iter()
        .map(filter_to_core)
        .collect::<CmdResult<_>>()?;
    let rig = core_search::HybridRig::<core_search::NoModel, OrtEmbedder, OrtEmbedder> {
        llm: None,
        text: text.as_deref(),
        clip: clip.as_deref(),
        vectors: Some(app.vectors.as_ref() as &dyn VectorStore),
    };
    let results = app.searcher.hybrid_search(
        &raw,
        &core_filters,
        &rig,
        &core_search::HybridOptions::default(),
    )?;
    Ok(results_to_dto(results, filters))
}

/// The M1 keyword-only path (the shipping degraded posture): the §5 hybrid
/// pipeline with every model slot `None`, which the engine documents as the
/// degenerate case of itself — byte-identical to the P7.2 search, plus
/// §10.3 collection-chip resolution (no model required). The degraded
/// branch of `run_search` and the search tests both go through here.
fn run_search_keyword(
    searcher: &Searcher,
    raw: String,
    filters: Vec<dto::Filter>,
) -> CmdResult<dto::SearchResults> {
    let core_filters: Vec<CoreFilter> = filters
        .iter()
        .map(filter_to_core)
        .collect::<CmdResult<_>>()?;
    let results = searcher.hybrid_search(
        &raw,
        &core_filters,
        &core_search::keyword_only_rig(),
        &core_search::HybridOptions::default(),
    )?;
    Ok(results_to_dto(results, filters))
}

fn filter_to_core(f: &dto::Filter) -> CmdResult<CoreFilter> {
    Ok(match f {
        dto::Filter::Date {
            field,
            absolute,
            relative,
        } => {
            let field = match field.as_str() {
                "captured" => DateField::Captured,
                "annotated" => DateField::Annotated,
                other => return Err(invalid(format!("unknown date field '{other}'"))),
            };
            let range = match (absolute, relative) {
                (Some(a), None) => DateRange::Absolute {
                    start: a.start.as_deref().map(parse_ts).transpose()?,
                    end: a.end.as_deref().map(parse_ts).transpose()?,
                },
                (None, Some(r)) => DateRange::Relative(relative_to_core(r)?),
                _ => {
                    return Err(invalid(
                        "date filter needs exactly one of absolute/relative".into(),
                    ));
                }
            };
            CoreFilter::Date { field, range }
        }
        // M1 chips carry free text: case-insensitive containment (the
        // engine owns the comparison semantics either way).
        dto::Filter::Camera { value } => CoreFilter::Camera(StringMatch::Contains(value.clone())),
        dto::Filter::Lens { value } => CoreFilter::Lens(StringMatch::Contains(value.clone())),
        dto::Filter::Folder { value } => CoreFilter::Folder(PathMatch::Subtree(value.clone())),
        dto::Filter::Root { value } => CoreFilter::Root(value.clone()),
        dto::Filter::Rating { op, value } => CoreFilter::Rating(match op.as_str() {
            "eq" => Comparison::Eq(*value),
            "gte" => Comparison::Gte(*value),
            "lte" => Comparison::Lte(*value),
            other => return Err(invalid(format!("unknown rating op '{other}'"))),
        }),
        // M3 filter; forwarded so the engine's UnsupportedFilter error
        // (hard-constraint discipline) reaches the UI verbatim.
        dto::Filter::Collection { name } => CoreFilter::Collection(CollectionRef {
            raw: name.clone(),
            resolved: None,
        }),
        dto::Filter::Volume { value } => CoreFilter::Volume(match value.as_str() {
            "online" => VolumeFilter::Online,
            "offline" => VolumeFilter::Offline,
            other => return Err(invalid(format!("unknown volume filter '{other}'"))),
        }),
        dto::Filter::HasStrokes { value } => CoreFilter::HasStrokes(*value),
        dto::Filter::Source { values } => CoreFilter::Source(
            values
                .iter()
                .map(|s| match s.as_str() {
                    "voice" => Ok(Source::Voice),
                    "typed" => Ok(Source::Typed),
                    "pencil" => Ok(Source::Pencil),
                    "system" => Ok(Source::System),
                    other => Err(invalid(format!("unknown source '{other}'"))),
                })
                .collect::<CmdResult<_>>()?,
        ),
    })
}

fn relative_to_core(r: &dto::RelativeRange) -> CmdResult<CoreRelativeRange> {
    let n = || r.n.ok_or_else(|| invalid("relative range needs n".into()));
    Ok(match r.unit.as_str() {
        "days" => CoreRelativeRange::LastDays(n()?),
        "weeks" => CoreRelativeRange::LastWeeks(n()?),
        "months" => CoreRelativeRange::LastMonths(n()?),
        "years" => CoreRelativeRange::LastYears(n()?),
        "season" => CoreRelativeRange::Season {
            season: match r.season.as_deref() {
                Some("spring") => Season::Spring,
                Some("summer") => Season::Summer,
                Some("autumn") | Some("fall") => Season::Autumn,
                Some("winter") => Season::Winter,
                other => return Err(invalid(format!("unknown season {other:?}"))),
            },
            years_ago: r.n.unwrap_or(1),
        },
        "month" => CoreRelativeRange::Month {
            month: r
                .month
                .ok_or_else(|| invalid("month range needs month".into()))?,
            year: r.year,
        },
        "year" => CoreRelativeRange::Year(
            r.year
                .ok_or_else(|| invalid("year range needs year".into()))?,
        ),
        other => return Err(invalid(format!("unknown relative unit '{other}'"))),
    })
}

fn results_to_dto(r: core_search::SearchResults, echo: Vec<dto::Filter>) -> dto::SearchResults {
    dto::SearchResults {
        query: dto::QueryEcho {
            raw: r.query.raw,
            filters: echo,
            dropped: r
                .query
                .parsed
                .dropped
                .into_iter()
                .map(|d| dto::DroppedClause {
                    raw: d.raw,
                    reason: d.reason,
                })
                .collect(),
            fallback: r.query.parsed.fallback,
        },
        images: r.images.into_iter().map(image_to_dto).collect(),
        session_hits: r
            .session_hits
            .into_iter()
            .map(|h| dto::SessionHit {
                session_id: h.session_id.to_string(),
                quote: quote_to_dto(h.quote),
            })
            .collect(),
    }
}

fn image_to_dto(i: core_search::ImageResult) -> dto::ImageResult {
    dto::ImageResult {
        image_hash: i.image_hash.to_string(),
        preview: i.preview.image_hash.to_string(),
        score: i.score,
        provenance: match i.provenance {
            core_search::Provenance::Quote(q) => dto::Provenance::Quote(quote_to_dto(q)),
            core_search::Provenance::Stroke {
                event_id,
                session_id,
                ts,
            } => dto::Provenance::Stroke {
                event_id: event_id.to_string(),
                session_id: session_id.to_string(),
                ts: ts.to_string(),
            },
            core_search::Provenance::VisualMatch => dto::Provenance::VisualMatch,
            core_search::Provenance::FilterOnly => dto::Provenance::FilterOnly,
        },
        last_annotated_ts: i.last_annotated_ts.map(|t| t.to_string()),
        debug: i.debug.map(|d| dto::DebugScores {
            per_signal: d
                .per_signal
                .into_iter()
                .map(|(sig, rank, score)| (format!("{sig:?}"), rank, score))
                .collect(),
            fused: d.fused,
        }),
    }
}

fn quote_to_dto(q: core_search::Quote) -> dto::Quote {
    dto::Quote {
        event_id: q.event_id.to_string(),
        session_id: q.session_id.to_string(),
        ts: q.ts.to_string(),
        source: q.source.as_str().to_owned(),
        text: q.text,
        char_start: q.char_start,
        char_end: q.char_end,
        highlights: q.highlights,
        linked_stroke: q.linked_stroke.map(|id| id.to_string()),
    }
}

fn parse_ts(s: &str) -> CmdResult<UtcMillis> {
    UtcMillis::parse(s).map_err(|e| invalid(format!("bad timestamp '{s}': {e}")))
}

fn invalid(msg: String) -> CmdError {
    CmdError::Invalid(msg)
}

#[cfg(test)]
mod tests {
    use photoproof_core::{ContentHash, EventDraft, EventStore, RemarkSource, SessionContext};

    use super::*;

    fn ctx() -> SessionContext {
        SessionContext {
            app_version: "0.0.0-test".into(),
            device_id: "a".repeat(32),
            root_context: None,
        }
    }

    /// End-to-end across the seam: events appended through the store come
    /// back through the engine as Quote provenance in the IPC DTO shape.
    #[test]
    fn wired_search_returns_quote_provenance_dto() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("journal.db");
        let store = EventStore::open(&db).unwrap();
        let sid = store.open_session(ctx()).unwrap();
        let target = ContentHash::from_bytes_of(b"wired-search-image");
        store
            .append(
                &sid,
                EventDraft::Remark {
                    source: RemarkSource::Typed,
                    text: "the fog swallowing the barn, keep this one".into(),
                    targets: vec![target.clone()],
                },
                None,
            )
            .unwrap();

        let searcher = Searcher::open(&db).unwrap();
        let out = run_search_keyword(&searcher, "fog ba".into(), Vec::new()).unwrap();
        assert_eq!(out.images.len(), 1);
        assert_eq!(out.images[0].image_hash, target.to_string());
        match &out.images[0].provenance {
            dto::Provenance::Quote(q) => {
                assert!(q.text.contains("fog"));
                assert!(!q.highlights.is_empty());
                assert_eq!(q.source, "typed");
            }
            other => panic!("expected Quote provenance, got {other:?}"),
        }
        assert!(out.session_hits.is_empty());
    }

    /// P7.2: a collection chip resolves against the collections store
    /// (§10.3, no model required) and constrains to current members; an
    /// unresolvable name is a hard error — chips are typed user intent,
    /// and silently un-constraining one would broaden results invisibly.
    #[test]
    fn collection_chip_resolves_through_the_hybrid_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("journal.db");
        let store = EventStore::open(&db).unwrap();
        let sid = store.open_session(ctx()).unwrap();
        let member = ContentHash::from_bytes_of(b"member-image");
        let outsider = ContentHash::from_bytes_of(b"outsider-image");
        for target in [&member, &outsider] {
            store
                .append(
                    &sid,
                    EventDraft::Remark {
                        source: RemarkSource::Typed,
                        text: "fog on the water".into(),
                        targets: vec![target.clone()],
                    },
                    None,
                )
                .unwrap();
        }
        let collections = photoproof_core::collections::Collections::open(&db, dir.path()).unwrap();
        let quiet = collections
            .create("Quiet Hours", "", photoproof_core::UtcMillis::now())
            .unwrap();
        collections
            .add_images(
                &quiet.id,
                std::slice::from_ref(&member),
                photoproof_core::UtcMillis::now(),
            )
            .unwrap();

        let searcher = Searcher::open(&db).unwrap();
        let chip = dto::Filter::Collection {
            name: "quiet hours".into(),
        };
        let out = run_search_keyword(&searcher, "fog".into(), vec![chip]).unwrap();
        assert_eq!(out.images.len(), 1);
        assert_eq!(out.images[0].image_hash, member.to_string());
        assert!(out.query.dropped.is_empty());

        let bad = dto::Filter::Collection {
            name: "zzz qqq".into(),
        };
        let err = run_search_keyword(&searcher, "fog".into(), vec![bad]).unwrap_err();
        assert!(
            err.to_string().contains("'zzz qqq' not found"),
            "unresolvable chip errors instead of broadening: {err}"
        );
    }

    /// The `mode` wire arg (M3 search-as-scope, Phase 1): an omitted mode
    /// is `Auto` (back-compat — every pre-Phase-1 caller is unchanged), the
    /// two named lanes parse, and a typo is a HARD error (never a silent
    /// fallback that could bust the keystroke budget by running the wrong
    /// lane). This is the parse half of the guardrail; the routing half
    /// (lexical forcing the keyword rig regardless of embedder warmth) is
    /// the early return at the top of `run_search`.
    #[test]
    fn search_mode_parses_from_wire_strictly() {
        assert_eq!(SearchMode::from_wire(None).unwrap(), SearchMode::Auto);
        assert_eq!(
            SearchMode::from_wire(Some("auto")).unwrap(),
            SearchMode::Auto
        );
        assert_eq!(
            SearchMode::from_wire(Some("lexical")).unwrap(),
            SearchMode::Lexical
        );
        assert_eq!(
            SearchMode::from_wire(Some("semantic")).unwrap(),
            SearchMode::Semantic
        );
        assert!(SearchMode::from_wire(Some("hybrid")).is_err());
        assert!(SearchMode::from_wire(Some("")).is_err());
    }

    /// Filter DTO → core AST translation: untranslatable input errors,
    /// never silently drops (hard-constraint discipline).
    #[test]
    fn filter_translation_is_strict() {
        assert!(
            filter_to_core(&dto::Filter::Rating {
                op: "gte".into(),
                value: 3
            })
            .is_ok()
        );
        assert!(
            filter_to_core(&dto::Filter::Rating {
                op: "≥".into(),
                value: 3
            })
            .is_err()
        );
        assert!(
            filter_to_core(&dto::Filter::Volume {
                value: "nearline".into()
            })
            .is_err()
        );
        assert!(
            filter_to_core(&dto::Filter::Date {
                field: "captured".into(),
                absolute: None,
                relative: None,
            })
            .is_err()
        );
        let winter = filter_to_core(&dto::Filter::Date {
            field: "captured".into(),
            absolute: None,
            relative: Some(dto::RelativeRange {
                unit: "season".into(),
                n: None,
                season: Some("winter".into()),
                month: None,
                year: None,
            }),
        })
        .unwrap();
        assert_eq!(
            winter,
            CoreFilter::Date {
                field: DateField::Captured,
                range: DateRange::Relative(CoreRelativeRange::Season {
                    season: Season::Winter,
                    years_ago: 1
                })
            }
        );
    }
}
