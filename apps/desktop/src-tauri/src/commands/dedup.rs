//! Tier-1 near-duplicate detection command (DESIGN-DEDUP-AND-SIMILARITY.md
//! §"Tier 1"). One thin command over `Library::find_near_duplicates`: given a
//! grid scope and a Hamming threshold, return GROUPS of images whose 64-bit
//! perceptual hashes are transitively within the threshold of one another.
//!
//! SCOPE reuses the graph lens' `GraphScope` + `enumerate_scope` verbatim, so
//! the "Duplicates" scope the design doc anticipates resolves to the SAME image
//! set the grid / graph already show for a folder / collection / the whole
//! library. Exact byte-identical files already collapse to one image_hash
//! upstream (BLAKE3, K13); this surfaces the NEAR-dups (re-encodes, resizes,
//! light edits) that share a look but not bytes.
//!
//! GRACEFUL by construction: an image still awaiting its preview pass has no
//! perceptual hash yet and is simply skipped — never an error and never
//! mis-grouped as all-zero. An empty scope returns an empty list.

use photoproof_core::ContentHash;
use photoproof_core::tuning::tuning;
use serde::Serialize;

use super::graph::{GraphScope, enumerate_scope};
use super::{S, run_blocking};
use crate::command_work::CommandClass;
use crate::error::{CmdError, CmdResult};

/// One near-duplicate group on the wire: the member image hashes (lowercase
/// BLAKE3 hex), sorted, and a convenience `count`. The frontend renders these
/// as near-dup stacks / a cull-review list (the design doc's "142 near-dups in
/// 38 groups; keep the sharpest"); the count saves it a `.length`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateGroupDto {
    pub image_hashes: Vec<String>,
    pub count: usize,
}

/// Find near-duplicate groups in a scope.
///
/// `hamming_threshold` is OPTIONAL: omitted, it falls back to the file-
/// overridable `[dedup].hamming_threshold` (default 8 / 64), which the design
/// doc insists must be calibrated empirically — so the default lives in
/// `tuning.toml`, not as a magic number here. A caller may pass an explicit
/// value to sweep the threshold live (e.g. a "looseness" slider).
#[tauri::command]
pub async fn find_near_duplicates(
    app: S<'_>,
    scope: GraphScope,
    hamming_threshold: Option<u32>,
) -> CmdResult<Vec<DuplicateGroupDto>> {
    let app = app.inner().clone();
    // Pull the calibrated default OUTSIDE the blocking closure (cheap global
    // read); an explicit arg wins.
    let threshold = hamming_threshold.unwrap_or(tuning().dedup.hamming_threshold);
    // The scope can be the whole library (tens of thousands) and the grouping
    // is O(n²), so run it off the async runtime's cooperative threads (the
    // find_similar / os.rs spawn_blocking precedent) — a big library must not
    // stall the UI loop.
    run_blocking(
        app,
        "dedup.find-near-duplicates",
        CommandClass::Read,
        move |app| {
            app.touch()?;
            let scope_hashes = enumerate_scope(app, &scope)?;
            // Resolve the hex strings to typed ContentHash for the core call. A
            // malformed hash from the scope reader is a hard bug, not user input,
            // so surface it rather than silently drop.
            let typed: Vec<ContentHash> = scope_hashes
                .iter()
                .map(|h| {
                    ContentHash::from_hex(h)
                        .map_err(|e| CmdError::Invalid(format!("bad scope hash: {e}")))
                })
                .collect::<CmdResult<_>>()?;
            let groups = app
                .library
                .find_near_duplicates(&typed, threshold)
                .map_err(|e| CmdError::Invalid(format!("near-dup scan: {e}")))?;
            Ok(groups
                .into_iter()
                .map(|g| DuplicateGroupDto {
                    count: g.image_hashes.len(),
                    image_hashes: g.image_hashes,
                })
                .collect())
        },
    )
    .await
}
