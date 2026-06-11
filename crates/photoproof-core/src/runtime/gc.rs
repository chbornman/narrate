//! Model updates, versioning, GC (spec/RUNTIME.md §11).
//!
//! - Upgrades are explicit manifest bumps; the OLD model keeps serving
//!   until the new one is **verified** (checksums pass + the process
//!   reaches Ready once).
//! - Embedder GC is additionally blocked until RETRIEVAL answers the
//!   "may I GC model X?" core-bus query (stub answers here; the real
//!   reindex is M3).
//! - **Never delete during an active pass**: GC takes the same scheduler
//!   lock as background passes; a model with in-flight or queued work is
//!   untouchable (§13.9).
//! - Orphaned directories are LISTED for settings ("reclaim N GB"),
//!   never silently deleted.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::bus::{RuntimeBus, RuntimeEvent};
use super::manifest::Manifest;
use super::scheduler::{PassScheduler, SlotBackend};
use crate::capture::Clock;

/// The RETRIEVAL reindex seam (§11: old vectors stay queryable until the
/// reindex over the new model_id completes — RETRIEVAL owns that rule;
/// the runtime only asks).
pub trait ReindexGate: Send {
    fn may_gc(&self, model_id: &str) -> bool;
}

/// Stub gate for P6.2 (real RETRIEVAL wiring is M3).
pub struct StubReindexGate {
    pub allow: HashSet<String>,
}

impl ReindexGate for StubReindexGate {
    fn may_gc(&self, model_id: &str) -> bool {
        self.allow.contains(model_id)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum GcDecision {
    Deleted { bytes_reclaimed: u64 },
    Held { reason: String },
}

pub struct GcCoordinator {
    models_dir: PathBuf,
    bus: RuntimeBus,
    /// New-model verification: checksums passed AND the process reached
    /// Ready once (§11).
    verified_ready: HashSet<String>,
}

impl GcCoordinator {
    pub fn new(models_dir: PathBuf, bus: RuntimeBus) -> Self {
        Self {
            models_dir,
            bus,
            verified_ready: HashSet::new(),
        }
    }

    /// Record that a model's checksums verified AND its process reached
    /// Ready once (for in-process embedders: one successful session
    /// load — the seam caller decides what "Ready once" means).
    pub fn note_verified_ready(&mut self, model_id: &str) {
        self.verified_ready.insert(model_id.to_owned());
    }

    /// Attempt to GC `old_model_id`, replaced by `replacement_id`.
    /// `is_embedder_role` routes through the reindex gate.
    pub fn try_gc_old_model<C: Clock, B: SlotBackend>(
        &mut self,
        old_model_id: &str,
        replacement_id: &str,
        is_embedder_role: bool,
        gate: &dyn ReindexGate,
        scheduler: &mut PassScheduler<C, B>,
    ) -> GcDecision {
        let held = |reason: String, bus: &RuntimeBus| {
            bus.publish(RuntimeEvent::GcResolved {
                model_id: old_model_id.to_owned(),
                allowed: false,
                reason: reason.clone(),
            });
            GcDecision::Held { reason }
        };
        // (a) The old model keeps serving until the NEW one is verified.
        if !self.verified_ready.contains(replacement_id) {
            return held(
                format!("replacement {replacement_id} not verified (checksums + one Ready)"),
                &self.bus,
            );
        }
        // (b) Embedders: RETRIEVAL must answer yes over the bus.
        if is_embedder_role && !gate.may_gc(old_model_id) {
            return held(
                "reindex pending: RETRIEVAL answered no".to_owned(),
                &self.bus,
            );
        }
        // (c) The scheduler lock: in-flight/queued work = untouchable
        //     (§13.9). The hold also pauses background starts while we
        //     delete.
        if !scheduler.try_gc_hold(old_model_id) {
            return held("model has in-flight or queued work".to_owned(), &self.bus);
        }
        let dir = self.models_dir.join(old_model_id);
        let bytes = dir_bytes(&dir);
        let result = std::fs::remove_dir_all(&dir);
        scheduler.release_gc_hold();
        match result {
            Ok(()) => {
                self.bus.publish(RuntimeEvent::GcResolved {
                    model_id: old_model_id.to_owned(),
                    allowed: true,
                    reason: "verified + gated + idle".into(),
                });
                GcDecision::Deleted {
                    bytes_reclaimed: bytes,
                }
            }
            Err(e) => GcDecision::Held {
                reason: format!("delete failed: {e}"),
            },
        }
    }

    /// §11: orphaned directories (no manifest entry) are surfaced for
    /// settings ("reclaim N GB"), never silently deleted.
    pub fn list_orphans(&self, manifest: &Manifest) -> Vec<(String, u64)> {
        let Ok(entries) = std::fs::read_dir(&self.models_dir) else {
            return Vec::new();
        };
        let mut orphans = Vec::new();
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if manifest.model(&name).is_none() {
                orphans.push((name, dir_bytes(&entry.path())));
            }
        }
        orphans.sort();
        orphans
    }
}

fn dir_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| {
            let p = e.path();
            if p.is_dir() {
                dir_bytes(&p)
            } else {
                e.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}
