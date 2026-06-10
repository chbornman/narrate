//! Sidecar truth layer: canonical `.photoproof.json` serialization, debounced
//! writer, overflow store, session journals, redaction propagation, export,
//! rebuild-from-sidecars.
//!
//! Contract: spec/SIDECARS.md. Owned by work packet P2.1.
//!
//! - `doc`: the sidecar document model + §4 validation (§3, §5)
//! - `serialize`: §3.5 canonical pretty/compact byte forms (S1)
//! - `paths`: placement and naming (§2, §7.2, §8.1, §9.3, §12.1)
//! - `writer`: debounce state machine + atomic mtime-stable replace (§9)
//! - `engine`: the single write chokepoint — flush routing, reconciliation,
//!   redaction propagation, overflow migration (§7–§11)
//! - `export`: one-click export + manifest (§12.1–§12.2)
//! - `rebuild`: rebuild-from-sidecars, the proof of K11 (§12.3)

mod doc;
mod engine;
mod export;
mod paths;
mod rebuild;
mod serialize;
mod writer;

pub use doc::{
    FORMAT_MANIFEST, FORMAT_SIDECAR, ImageSnapshot, ParsedSidecar, SIDECAR_SCHEMA_VERSION,
    SessionEntry, SessionMeta, SidecarDoc, SidecarEntry, SidecarParseError, parse_sidecar,
};
pub use engine::{
    AdjacentLocation, FlushOutcome, ImageLocator, IntegrityReport, MemoryLocator, RedactionReceipt,
    SidecarEngine, SidecarError, UNKNOWN_DEVICE_ID,
};
pub use export::{ExportReport, MANIFEST_SCHEMA_VERSION, VolumeInfo};
pub use paths::{
    SIDECAR_SUFFIX, default_export_dir_name, fanout_rel_path, is_sidecar_filename, is_temp_orphan,
    overflow_dir, overflow_path, session_journal_path, sessions_dir, sidecar_path_for_image,
};
pub use rebuild::{RebuildOptions, rebuild_from_sidecars};
pub use serialize::{compact_timestamp, to_compact_canonical, to_pretty_canonical};
pub use writer::{
    DEBOUNCE_CAP_MS, DEBOUNCE_QUIET_MS, Debouncer, Fault, FaultPoint, SIMULATED_KILL, WriteOutcome,
    clean_orphan_temps, write_atomic, write_atomic_faulty,
};
