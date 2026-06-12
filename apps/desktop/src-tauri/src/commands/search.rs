//! Search command (RETRIEVAL §4 / §5.4 — M1 contract) — moved verbatim
//! from the old commands.rs (FOUNDATIONS split).

use super::S;
use crate::error::CmdResult;
use crate::search_types::{Filter, SearchResults};

/// M1 search over the journal (RETRIEVAL §4 engine, packet P3.1).
///
/// A new keystroke's query interrupts any in-flight statement first
/// (§4 search-as-you-type); the interrupted call surfaces as an error
/// its (stale) caller discards.
#[tauri::command]
pub fn search(app: S<'_>, query: String, filters: Vec<Filter>) -> CmdResult<SearchResults> {
    app.touch()?;
    app.searcher.interrupt();
    let results = crate::search_wire::run_search(&app, query, filters)?;
    *app.last_search.lock().expect("last_search mutex") = Some(results.query.clone());
    Ok(results)
}
