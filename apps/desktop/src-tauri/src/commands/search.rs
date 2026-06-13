//! Search command (RETRIEVAL §4 / §5.4 — M1 contract) — moved verbatim
//! from the old commands.rs (FOUNDATIONS split).

use super::S;
use crate::error::CmdResult;
use crate::search_types::{Filter, FusionWeightsWire, SearchResults};
use crate::search_wire::SearchMode;

/// M1 search over the journal (RETRIEVAL §4 engine, packet P3.1).
///
/// A new keystroke's query interrupts any in-flight statement first
/// (§4 search-as-you-type); the interrupted call surfaces as an error
/// its (stale) caller discards.
///
/// `mode` (M3 search-as-scope, Phase 1) picks the lane: omitted ⇒ `Auto`
/// (today's behavior, so every existing caller is unchanged); `lexical`
/// FORCES the M1 keyword rig even on a warm machine — the as-you-type path
/// passes it to stay under the <100 ms budget (RETRIEVAL §13.1); `semantic`
/// runs the full hybrid rig (the commit-on-Enter lane).
///
/// `weights` + `include_debug` (Phase 3 — the ⚙ "Ranking signals" popover):
/// both optional and SEMANTIC-LANE ONLY. `weights` overrides the fusion's
/// per-signal weights (an unchecked signal arrives as `0.0`, excluded);
/// omitted, the rig fuses with the B75 defaults exactly as before.
/// `include_debug` lights up `ImageResult::debug` so the popover can SHOW each
/// result's per-signal contribution while open. Neither reaches the lexical
/// keystroke lane, so the <100 ms budget is untouched.
#[tauri::command]
pub fn search(
    app: S<'_>,
    query: String,
    filters: Vec<Filter>,
    mode: Option<String>,
    weights: Option<FusionWeightsWire>,
    include_debug: Option<bool>,
) -> CmdResult<SearchResults> {
    app.touch()?;
    app.searcher.interrupt();
    let mode = SearchMode::from_wire(mode.as_deref())?;
    let results = crate::search_wire::run_search(
        &app,
        query,
        filters,
        mode,
        weights,
        include_debug.unwrap_or(false),
    )?;
    *app.last_search.lock().expect("last_search mutex") = Some(results.query.clone());
    Ok(results)
}

/// Default neighbor count for "more like this" when the caller passes none.
/// One screenful of a typical grid; the frontend may override.
const DEFAULT_SIMILAR_LIMIT: usize = 60;

/// "More like this" (B69 retrieval-stays-additive): the nearest OTHER images
/// to `hash` by visual (CLIP image) similarity, in descending-similarity
/// order with the query image itself excluded. The frontend renders these as
/// a `similar` grid scope — the same in-place result set the committed-query
/// scope produces — so this returns just the ranked hashes; `list_images`
/// enriches them on the frontend (the query scope's exact two-step shape).
///
/// Reuses the existing PPVEC brute-force similarity search the S4 hybrid path
/// uses (no second kNN), over the image's OWN stored vector. Graceful when
/// the index is empty or this image was never embedded (a fresh/mock machine):
/// it returns an empty list, never an error — the mechanism is correct before
/// any embedding pass has run.
#[tauri::command]
pub async fn find_similar(
    app: S<'_>,
    hash: String,
    limit: Option<usize>,
) -> CmdResult<Vec<String>> {
    let app = app.inner().clone();
    let limit = limit.unwrap_or(DEFAULT_SIMILAR_LIMIT);
    // The brute-force scan + a metadata read can take real time on a large
    // space; do it off the async runtime's cooperative threads (the os.rs
    // spawn_blocking precedent) so a big library can't stall the UI loop.
    tauri::async_runtime::spawn_blocking(move || {
        app.touch()?;
        let pairs = app
            .vectors
            .similar_images(&hash, limit)
            .map_err(|e| crate::error::CmdError::Invalid(format!("similarity search: {e}")))?;
        // Pass through the backend's similarity order verbatim — relevance is
        // the order, exactly like the query scope's fused order.
        Ok(pairs.into_iter().map(|(h, _score)| h).collect())
    })
    .await
    .map_err(|e| crate::error::CmdError::Invalid(format!("task join: {e}")))?
}
