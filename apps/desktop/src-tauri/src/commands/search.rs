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
