//! Attention/engagement heatmap commands (DESIGN-ATTENTION-HEATMAP.md).
//!
//! Three thin wrappers over the core's dwell telemetry + intensity query:
//!   - `record_dwell`  — report one finished focus episode (the frontend sends
//!     the RAW episode; the tier rate + 60 s cap are applied in the backend, so
//!     tuning stays authoritative — DESIGN §"dwell capture").
//!   - `image_intensity` — per-scope, normalized 0..1 engagement; recency-
//!     weighted unless `all_time` (the UI "All-time" toggle).
//!   - `clear_dwell` — wipe the local dwell telemetry (the "clear attention
//!     data" reset). Annotation counts are the user's journal, never touched.
//!
//! Dwell is local, machine-observed telemetry kept OUTSIDE the journal (K14);
//! it never crosses into a sidecar.

use photoproof_core::{DwellSource, UtcMillis};

use super::{S, admit, parse_hash};
use crate::command_work::CommandClass;
use crate::error::{CmdError, CmdResult};

/// One normalized intensity for an image (wire shape). Mirrors core's
/// `ImageIntensity` with serde field names the frontend reads.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageIntensityDto {
    pub hash: String,
    pub intensity: f64,
}

/// Map the wire `source` string to the core tier. Only "look" / "grid" are
/// valid; anything else is a frontend bug surfaced as an error rather than
/// silently miscredited.
fn parse_source(source: &str) -> CmdResult<DwellSource> {
    match source {
        "look" => Ok(DwellSource::Look),
        "grid" => Ok(DwellSource::Grid),
        other => Err(CmdError::Invalid(format!("bad dwell source: {other}"))),
    }
}

/// Record one finished focus episode (DESIGN §"dwell capture"). The frontend
/// reports the RAW episode (hash, source, elapsed_ms); the tier rate and the
/// 60 s per-episode cap are applied in the store from the heatmap tuning
/// config. Light + fire-and-forget on the frontend side.
#[tauri::command]
pub fn record_dwell(app: S<'_>, hash: String, source: String, elapsed_ms: i64) -> CmdResult<()> {
    let _permit = admit(app.inner(), "heatmap.record-dwell", CommandClass::Mutation)?;
    app.touch()?;
    let h = parse_hash(&hash)?;
    let src = parse_source(&source)?;
    app.store
        .record_dwell(&h, src, elapsed_ms, UtcMillis::now())?;
    Ok(())
}

/// Per-scope, normalized engagement intensity (DESIGN §"Rendering"). `all_time`
/// false (the default) is recency-weighted ("what am I working on now"); true is
/// flat all-time ("what mattered most ever"). Returns one entry per input hash,
/// in input order.
#[tauri::command]
pub fn image_intensity(
    app: S<'_>,
    hashes: Vec<String>,
    all_time: bool,
) -> CmdResult<Vec<ImageIntensityDto>> {
    let _permit = admit(app.inner(), "heatmap.image-intensity", CommandClass::Read)?;
    app.touch()?;
    // Parse all hashes up front; a malformed hash is a frontend bug, not a
    // silently-dropped cell.
    let parsed = hashes
        .iter()
        .map(|h| parse_hash(h))
        .collect::<CmdResult<Vec<_>>>()?;
    let scores = app
        .store
        .image_intensity(&parsed, all_time, UtcMillis::now())?;
    Ok(scores
        .into_iter()
        .map(|s| ImageIntensityDto {
            hash: s.image.as_str().to_owned(),
            intensity: s.intensity,
        })
        .collect())
}

/// Wipe the local dwell telemetry (Settings → "Clear attention data"). Resolves
/// to the number of per-image rows removed. Annotation counts are untouched.
#[tauri::command]
pub fn clear_dwell(app: S<'_>) -> CmdResult<usize> {
    let _permit = admit(app.inner(), "heatmap.clear-dwell", CommandClass::Mutation)?;
    app.touch()?;
    Ok(app.store.clear_dwell()?)
}
