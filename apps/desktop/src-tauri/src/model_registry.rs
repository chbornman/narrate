//! Durable, in-memory authority for installed model artifacts.
//!
//! `installed.json` is a derived commit record, not proof by itself. Launch
//! validates current-version indexed entries by existence/size before Usable.
//! Any recovery that requires reading multi-GB model payloads (a missing or
//! malformed index, or a manifest-version change) remains dark and pending
//! until a managed post-Usable verification task durably adopts it. Every
//! mutating model operation takes `operation_gate`;
//! this is intentionally stronger than per-model locking because all models
//! share one installed-index file and a read-modify-write on different model
//! ids can otherwise lose an update.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, TryLockError};

use photoproof_core::UtcMillis;
use photoproof_core::runtime::{
    DownloadManager, InstalledRecord, Manifest, ModelEntry, RuntimeBus,
};

#[derive(Debug, Clone, Default)]
struct RegistrySnapshot {
    installed: BTreeMap<String, InstalledRecord>,
    disagreements: BTreeMap<String, String>,
    orphans: Vec<(String, u64)>,
    operations: BTreeMap<String, String>,
    operation_events: BTreeMap<String, ModelOperationEvent>,
    operation_sequence: u64,
    partial_bytes: BTreeMap<String, u64>,
    pending_verification: BTreeSet<String>,
    recovery_commit_needed: bool,
}

pub struct ModelOperationRegistry {
    models_dir: PathBuf,
    operation_gate: Mutex<()>,
    snapshot: Mutex<RegistrySnapshot>,
    bus: RuntimeBus,
    writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelOperationEvent {
    pub attempt_id: String,
    pub sequence: u64,
    pub phase: String,
    pub terminal: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryRecoveryReport {
    pub verified: usize,
    pub rejected: usize,
    pub remaining: usize,
    pub cancelled: bool,
}

impl ModelOperationRegistry {
    pub fn open(models_dir: PathBuf, manifest: &Manifest, bus: RuntimeBus, writable: bool) -> Self {
        let manager = DownloadManager::new(models_dir.clone(), bus.clone());
        let path = models_dir.join("installed.json");
        let mut disagreements = BTreeMap::new();
        let parsed: Result<BTreeMap<String, InstalledRecord>, String> = std::fs::read(&path)
            .map_err(|error| error.to_string())
            .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|error| error.to_string()));

        let (mut installed, recovery_reason) = match parsed {
            Ok(index) => (index, None),
            Err(error) => {
                if path.exists() && writable {
                    let quarantine =
                        models_dir.join(format!("installed.json.corrupt-{}", ulid::Ulid::new()));
                    if let Err(rename_error) = std::fs::rename(&path, &quarantine) {
                        disagreements.insert(
                            "_registry".into(),
                            format!(
                                "installed index was unreadable ({error}) and quarantine failed: \
                                 {rename_error}"
                            ),
                        );
                    } else {
                        disagreements.insert(
                            "_registry".into(),
                            format!(
                                "installed index was unreadable ({error}); preserved at {}",
                                quarantine.display()
                            ),
                        );
                    }
                }
                if !writable {
                    disagreements.insert(
                        "_registry".into(),
                        format!(
                            "installed index could not be read ({error}); this process does not \
                             hold the runtime lock and will not repair it"
                        ),
                    );
                }
                (BTreeMap::new(), Some(error))
            }
        };

        let mut pending_verification = BTreeSet::new();
        let mut recovery_commit_needed = false;
        if recovery_reason.is_some() {
            // A missing/corrupt index has no authority. Adopt only directories
            // whose complete immutable files later hash against the running
            // manifest. Do not perform that potentially multi-GB read before
            // the shell reaches Usable.
            if writable {
                recovery_commit_needed = true;
                for model in &manifest.models {
                    if models_dir.join(&model.id).is_dir() {
                        pending_verification.insert(model.id.clone());
                        disagreements.insert(
                            model.id.clone(),
                            "installed-index recovery is pending managed verification".into(),
                        );
                    }
                }
                disagreements.insert(
                    "_registry".into(),
                    "installed index recovery is pending after the app becomes usable".into(),
                );
                if pending_verification.is_empty() {
                    match manager.replace_installed(&installed) {
                        Ok(()) => {
                            recovery_commit_needed = false;
                            disagreements.remove("_registry");
                        }
                        Err(error) => {
                            disagreements.insert(
                                "_registry".into(),
                                format!("empty installed index could not be committed: {error}"),
                            );
                        }
                    }
                }
            }
        } else {
            // A syntactically valid index is cheap to validate at launch.
            // Exact length proves expected layout for the same manifest.
            // Manifest-version drift requires a full re-hash and therefore
            // stays dark until the managed post-Usable recovery lane.
            let indexed_before_validation = installed.clone();
            installed.retain(|id, record| {
                let Some(model) = manifest.model(id) else {
                    disagreements.insert(
                        id.clone(),
                        "installed index names a model absent from the current manifest".into(),
                    );
                    return false;
                };
                match verify_layout(&models_dir, model) {
                    Ok(()) if record.manifest_version == manifest.manifest_version => true,
                    Ok(()) => {
                        if writable {
                            pending_verification.insert(id.clone());
                            recovery_commit_needed = true;
                            disagreements.insert(
                                id.clone(),
                                "manifest changed; managed verification is pending".into(),
                            );
                        } else {
                            disagreements.insert(
                                id.clone(),
                                "manifest changed but this process cannot verify shared model files"
                                    .into(),
                            );
                        }
                        false
                    }
                    Err(error) => {
                        disagreements.insert(id.clone(), error);
                        false
                    }
                }
            });
            if writable && installed != indexed_before_validation {
                recovery_commit_needed = true;
                if pending_verification.is_empty() {
                    match manager.replace_installed(&installed) {
                        Ok(()) => recovery_commit_needed = false,
                        Err(error) => {
                            disagreements.insert(
                                "_registry".into(),
                                format!("repaired installed index could not be committed: {error}"),
                            );
                        }
                    }
                }
            }

            // Complete but unindexed model directories are deliberately not
            // adopted when the index itself was sound: that disagreement may
            // be a crashed/abandoned install and requires Verify.
            for model in &manifest.models {
                if !installed.contains_key(&model.id)
                    && models_dir.join(&model.id).is_dir()
                    && !disagreements.contains_key(&model.id)
                {
                    disagreements.insert(
                        model.id.clone(),
                        "model directory exists but is absent from installed.json; Verify to adopt \
                         manifest-valid files or discard the partial download"
                            .into(),
                    );
                }
            }
        }

        let orphans = list_orphans(&models_dir, manifest);
        let partial_bytes = manifest
            .models
            .iter()
            .filter(|model| !installed.contains_key(&model.id))
            .filter_map(|model| {
                let bytes = manager.downloaded_bytes(model);
                (bytes > 0).then_some((model.id.clone(), bytes))
            })
            .collect();
        Self {
            models_dir,
            operation_gate: Mutex::new(()),
            writable,
            snapshot: Mutex::new(RegistrySnapshot {
                installed,
                disagreements,
                orphans,
                operations: BTreeMap::new(),
                operation_events: BTreeMap::new(),
                operation_sequence: 0,
                partial_bytes,
                pending_verification,
                recovery_commit_needed,
            }),
            bus,
        }
    }

    pub fn recovery_pending(&self) -> bool {
        let snapshot = self.snapshot.lock().expect("model registry snapshot");
        snapshot.recovery_commit_needed || !snapshot.pending_verification.is_empty()
    }

    /// Verify and durably adopt recovery candidates after the shell is usable.
    ///
    /// The global operation gate prevents a concurrent download/remove/index
    /// commit from being overwritten by the recovered snapshot. Cancellation
    /// publishes no partial index: leaving the recovery source missing or
    /// quarantined guarantees the next launch cannot mistake an incomplete
    /// adoption for a sound, intentionally sparse installed index.
    pub fn recover_pending(
        &self,
        manifest: &Manifest,
        bus: RuntimeBus,
        cancel: &AtomicBool,
    ) -> Result<RegistryRecoveryReport, String> {
        if !self.writable {
            return Err("this process does not own model-registry recovery".into());
        }
        let _operation = self.lock_operation();
        let (candidates, mut installed, commit_needed) = {
            let mut snapshot = self.snapshot.lock().expect("model registry snapshot");
            let candidates = snapshot
                .pending_verification
                .iter()
                .cloned()
                .collect::<Vec<_>>();
            for model_id in &candidates {
                snapshot
                    .operations
                    .insert(model_id.clone(), "verifying".into());
            }
            (
                candidates,
                snapshot.installed.clone(),
                snapshot.recovery_commit_needed,
            )
        };
        if candidates.is_empty() && !commit_needed {
            return Ok(RegistryRecoveryReport {
                verified: 0,
                rejected: 0,
                remaining: 0,
                cancelled: false,
            });
        }

        let manager = DownloadManager::new(self.models_dir.clone(), bus);
        let mut verified = BTreeSet::new();
        let mut rejected = BTreeMap::new();
        let mut processed = BTreeSet::new();
        for model_id in &candidates {
            if cancel.load(Ordering::Acquire) {
                break;
            }
            let Some(model) = manifest.model(model_id) else {
                rejected.insert(
                    model_id.clone(),
                    "model disappeared from the current manifest".into(),
                );
                processed.insert(model_id.clone());
                continue;
            };
            match manager.verify_model(model, cancel) {
                Ok(()) => {
                    installed.insert(
                        model_id.clone(),
                        InstalledRecord {
                            manifest_version: manifest.manifest_version,
                            when: UtcMillis::now().to_rfc3339(),
                        },
                    );
                    verified.insert(model_id.clone());
                    processed.insert(model_id.clone());
                }
                Err(_error) if cancel.load(Ordering::Acquire) => break,
                Err(error) => {
                    rejected.insert(
                        model_id.clone(),
                        format!("unindexed files did not verify: {error}"),
                    );
                    processed.insert(model_id.clone());
                }
            }
        }

        let cancelled = cancel.load(Ordering::Acquire);
        if cancelled {
            let mut snapshot = self.snapshot.lock().expect("model registry snapshot");
            for model_id in &candidates {
                snapshot.operations.remove(model_id);
            }
            snapshot.disagreements.insert(
                "_registry".into(),
                "installed index recovery cancelled; every candidate remains dark".into(),
            );
            return Ok(RegistryRecoveryReport {
                verified: 0,
                rejected: 0,
                remaining: snapshot.pending_verification.len(),
                cancelled: true,
            });
        }
        if let Err(error) = manager.replace_installed(&installed) {
            let mut snapshot = self.snapshot.lock().expect("model registry snapshot");
            for model_id in &candidates {
                snapshot.operations.remove(model_id);
            }
            snapshot.disagreements.insert(
                "_registry".into(),
                format!("recovered installed index could not be committed: {error}"),
            );
            return Err(error.to_string());
        }

        let mut snapshot = self.snapshot.lock().expect("model registry snapshot");
        snapshot.installed = installed;
        for model_id in &processed {
            snapshot.pending_verification.remove(model_id);
            snapshot.operations.remove(model_id);
            snapshot.disagreements.remove(model_id);
        }
        for (model_id, detail) in rejected {
            snapshot.disagreements.insert(model_id, detail);
        }
        for model_id in &candidates {
            snapshot.operations.remove(model_id);
        }
        snapshot.recovery_commit_needed = false;
        if snapshot.pending_verification.is_empty() {
            snapshot.disagreements.remove("_registry");
        } else {
            snapshot.disagreements.insert(
                "_registry".into(),
                "installed index recovery paused; remaining models stay dark".into(),
            );
        }
        Ok(RegistryRecoveryReport {
            verified: verified.len(),
            rejected: processed.len().saturating_sub(verified.len()),
            remaining: snapshot.pending_verification.len(),
            cancelled,
        })
    }

    pub fn lock_operation(&self) -> MutexGuard<'_, ()> {
        self.operation_gate.lock().expect("model operation gate")
    }

    /// Enter model convergence without blocking a lifecycle/status caller
    /// behind a multi-gigabyte download or verification. A missed converge is
    /// harmless: the owned cadence retries it, while a mutation holding the
    /// global gate is guaranteed not to overlap a stale load/unload dispatch.
    pub fn try_lock_operation(&self) -> Option<MutexGuard<'_, ()>> {
        match self.operation_gate.try_lock() {
            Ok(guard) => Some(guard),
            Err(TryLockError::WouldBlock) => None,
            Err(TryLockError::Poisoned(_)) => panic!("model operation gate"),
        }
    }

    pub fn installed(&self) -> BTreeMap<String, InstalledRecord> {
        self.snapshot
            .lock()
            .expect("model registry snapshot")
            .installed
            .clone()
    }

    pub fn disagreement(&self, model_id: &str) -> Option<String> {
        self.snapshot
            .lock()
            .expect("model registry snapshot")
            .disagreements
            .get(model_id)
            .cloned()
    }

    pub fn operation(&self, model_id: &str) -> Option<String> {
        self.snapshot
            .lock()
            .expect("model registry snapshot")
            .operations
            .get(model_id)
            .cloned()
    }

    pub fn last_operation(&self, model_id: &str) -> Option<ModelOperationEvent> {
        self.snapshot
            .lock()
            .expect("model registry snapshot")
            .operation_events
            .get(model_id)
            .cloned()
    }

    pub fn partial_bytes(&self, model_id: &str) -> u64 {
        self.snapshot
            .lock()
            .expect("model registry snapshot")
            .partial_bytes
            .get(model_id)
            .copied()
            .unwrap_or(0)
    }

    pub fn publish_partial_bytes(&self, model_id: &str, bytes: u64) {
        let mut snapshot = self.snapshot.lock().expect("model registry snapshot");
        if bytes == 0 {
            snapshot.partial_bytes.remove(model_id);
        } else {
            snapshot.partial_bytes.insert(model_id.to_owned(), bytes);
        }
    }

    pub fn set_operation(&self, model_id: &str, phase: Option<&str>) {
        let mut snapshot = self.snapshot.lock().expect("model registry snapshot");
        match phase {
            Some(phase) => {
                snapshot
                    .operations
                    .insert(model_id.to_owned(), phase.to_owned());
            }
            None => {
                snapshot.operations.remove(model_id);
            }
        }
    }

    /// Publish one ordered operation transition and retain its latest snapshot.
    /// A terminal transition clears the live-operation owner only after all
    /// durable/in-memory mutation state has settled.
    pub fn publish_operation(
        &self,
        model_id: &str,
        attempt_id: &str,
        phase: &str,
        terminal: bool,
        error: Option<String>,
    ) -> ModelOperationEvent {
        let event = {
            let mut snapshot = self.snapshot.lock().expect("model registry snapshot");
            snapshot.operation_sequence = snapshot.operation_sequence.saturating_add(1);
            let event = ModelOperationEvent {
                attempt_id: attempt_id.to_owned(),
                sequence: snapshot.operation_sequence,
                phase: phase.to_owned(),
                terminal,
                error,
            };
            if terminal {
                snapshot.operations.remove(model_id);
            } else {
                snapshot
                    .operations
                    .insert(model_id.to_owned(), phase.to_owned());
            }
            snapshot
                .operation_events
                .insert(model_id.to_owned(), event.clone());
            event
        };
        self.bus
            .publish(photoproof_core::runtime::RuntimeEvent::ModelOperation {
                model_id: model_id.to_owned(),
                attempt_id: event.attempt_id.clone(),
                sequence: event.sequence,
                phase: event.phase.clone(),
                terminal: event.terminal,
                error: event.error.clone(),
            });
        event
    }

    /// Publish the result of a durable install-index commit. Must be called
    /// while `lock_operation()` is held.
    pub fn publish_installed(&self, model_id: &str, record: InstalledRecord) {
        let mut snapshot = self.snapshot.lock().expect("model registry snapshot");
        snapshot.installed.insert(model_id.to_owned(), record);
        snapshot.disagreements.remove(model_id);
        snapshot.partial_bytes.remove(model_id);
    }

    /// Durably add or replace one installed record from the verified in-memory
    /// authority. The caller must hold `lock_operation()`. Building the next
    /// complete map here (instead of reparsing `installed.json`) is the
    /// no-lost-update seam shared by explicit Verify and future GC/install
    /// callers.
    pub fn commit_installed_locked(
        &self,
        bus: RuntimeBus,
        model_id: &str,
        record: InstalledRecord,
    ) -> Result<(), String> {
        let mut installed = self.installed();
        installed.insert(model_id.to_owned(), record.clone());
        DownloadManager::new(self.models_dir.clone(), bus)
            .replace_installed(&installed)
            .map_err(|error| error.to_string())?;
        self.publish_installed(model_id, record);
        Ok(())
    }

    /// Publish the result of a durable removal commit. Must be called while
    /// `lock_operation()` is held.
    pub fn publish_removed(&self, model_id: &str) {
        let mut snapshot = self.snapshot.lock().expect("model registry snapshot");
        snapshot.installed.remove(model_id);
        snapshot.disagreements.remove(model_id);
        snapshot.partial_bytes.remove(model_id);
    }

    /// Durably remove one installed record from the verified in-memory
    /// authority. Explicit removal and any future production GC caller use
    /// this while holding `lock_operation()` so different model ids cannot
    /// overwrite each other's read-modify-write.
    pub fn commit_removed_locked(&self, bus: RuntimeBus, model_id: &str) -> Result<(), String> {
        let mut installed = self.installed();
        installed.remove(model_id);
        DownloadManager::new(self.models_dir.clone(), bus)
            .replace_installed(&installed)
            .map_err(|error| error.to_string())?;
        self.publish_removed(model_id);
        Ok(())
    }

    pub fn publish_error(&self, model_id: &str, detail: String) {
        self.snapshot
            .lock()
            .expect("model registry snapshot")
            .disagreements
            .insert(model_id.to_owned(), detail);
    }

    pub fn debug_lines(&self) -> Vec<String> {
        let snapshot = self.snapshot.lock().expect("model registry snapshot");
        let mut lines: Vec<String> = snapshot
            .disagreements
            .iter()
            .map(|(id, detail)| format!("model registry disagreement {id}: {detail}"))
            .collect();
        lines.extend(
            snapshot
                .orphans
                .iter()
                .map(|(id, bytes)| format!("orphan model directory {id}: {bytes} bytes")),
        );
        lines
    }

    pub fn models_dir(&self) -> &Path {
        &self.models_dir
    }

    pub fn model_bytes(&self, model_id: &str) -> u64 {
        dir_bytes(&self.models_dir.join(model_id))
    }
}

fn verify_layout(models_dir: &Path, model: &ModelEntry) -> Result<(), String> {
    for file in &model.files {
        let path = models_dir.join(&model.id).join(&file.path);
        let metadata = std::fs::metadata(&path)
            .map_err(|error| format!("installed file {} is unavailable: {error}", file.path))?;
        if !metadata.is_file() || metadata.len() != file.bytes {
            return Err(format!(
                "installed file {} has {} bytes, expected {}",
                file.path,
                metadata.len(),
                file.bytes
            ));
        }
    }
    Ok(())
}

fn list_orphans(models_dir: &Path, manifest: &Manifest) -> Vec<(String, u64)> {
    let Ok(entries) = std::fs::read_dir(models_dir) else {
        return Vec::new();
    };
    let mut orphans = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_dir() && manifest.model(&name).is_none() {
            orphans.push((name, dir_bytes(&entry.path())));
        }
    }
    orphans.sort();
    orphans
}

fn dir_bytes(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            if entry.path().is_dir() {
                dir_bytes(&entry.path())
            } else {
                entry.metadata().map(|metadata| metadata.len()).unwrap_or(0)
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use photoproof_core::runtime::{
        Acceptances, DownloadManager, DownloadPhase, FileEntry, License, Pacer, RuntimeEvent,
    };
    use std::sync::Arc;
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::time::Duration;

    fn manifest(payload: &[u8]) -> Manifest {
        Manifest {
            manifest_version: 7,
            models: vec![
                ModelEntry {
                    id: "good".into(),
                    role: "llm".into(),
                    tiers: vec![1],
                    license: License {
                        name: "test".into(),
                        url: "https://example.test".into(),
                        acceptance_required: false,
                    },
                    total_bytes: payload.len() as u64,
                    files: vec![FileEntry {
                        repo: "https://example.test".into(),
                        revision: "immutable".into(),
                        path: "weights.bin".into(),
                        sha256: photoproof_core::runtime::download::sha256_bytes(payload),
                        bytes: payload.len() as u64,
                    }],
                },
                ModelEntry {
                    id: "bad".into(),
                    role: "llm".into(),
                    tiers: vec![1],
                    license: License {
                        name: "test".into(),
                        url: "https://example.test".into(),
                        acceptance_required: false,
                    },
                    total_bytes: payload.len() as u64,
                    files: vec![FileEntry {
                        repo: "https://example.test".into(),
                        revision: "immutable".into(),
                        path: "weights.bin".into(),
                        sha256: photoproof_core::runtime::download::sha256_bytes(payload),
                        bytes: payload.len() as u64,
                    }],
                },
            ],
        }
    }

    #[test]
    fn missing_index_recovers_only_fully_hash_valid_manifest_files() {
        let temp = tempfile::tempdir().unwrap();
        let models = temp.path().join("models");
        std::fs::create_dir_all(models.join("good")).unwrap();
        std::fs::create_dir_all(models.join("bad")).unwrap();
        std::fs::write(models.join("good/weights.bin"), b"immutable").unwrap();
        std::fs::write(models.join("bad/weights.bin"), b"same-lengt").unwrap();
        let manifest = manifest(b"immutable");

        let registry =
            ModelOperationRegistry::open(models.clone(), &manifest, RuntimeBus::new(), true);

        assert!(
            registry.installed().is_empty(),
            "pre-Usable open must not hash or adopt model payloads"
        );
        assert!(registry.recovery_pending());
        let report = registry
            .recover_pending(&manifest, RuntimeBus::new(), &AtomicBool::new(false))
            .unwrap();
        assert_eq!(report.verified, 1);
        assert_eq!(report.rejected, 1);
        assert_eq!(report.remaining, 0);
        assert!(registry.installed().contains_key("good"));
        assert!(!registry.installed().contains_key("bad"));
        assert!(registry.disagreement("bad").is_some());
        assert!(!registry.recovery_pending());
        let durable: BTreeMap<String, InstalledRecord> =
            serde_json::from_slice(&std::fs::read(models.join("installed.json")).unwrap()).unwrap();
        assert_eq!(durable, registry.installed());
    }

    #[test]
    fn successful_download_emits_exact_seven_phase_attempt_and_one_terminal() {
        struct RegistryPacer<'a> {
            registry: &'a ModelOperationRegistry,
            model_id: &'a str,
            attempt_id: &'a str,
        }
        impl Pacer for RegistryPacer<'_> {
            fn pace(&mut self, _just_transferred: usize) {}

            fn phase(&mut self, phase: DownloadPhase) {
                let phase = match phase {
                    DownloadPhase::Downloading => "downloading",
                    DownloadPhase::Verifying => "verifying",
                    DownloadPhase::Installing => "installing",
                };
                self.registry
                    .publish_operation(self.model_id, self.attempt_id, phase, false, None);
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let models = temp.path().join("models");
        let payload = b"immutable";
        let manifest = manifest(payload);
        let model = manifest.model("good").unwrap();
        std::fs::create_dir_all(models.join("good")).unwrap();
        std::fs::write(models.join("good/weights.bin"), payload).unwrap();
        let bus = RuntimeBus::new();
        let rx = bus.subscribe();
        let registry = ModelOperationRegistry::open(models.clone(), &manifest, bus.clone(), true);
        let attempt_id = "attempt-seven";
        registry.publish_operation("good", attempt_id, "queued", false, None);
        let mut pacer = RegistryPacer {
            registry: &registry,
            model_id: "good",
            attempt_id,
        };
        DownloadManager::new(models, bus.clone())
            .download_model(
                model,
                manifest.manifest_version,
                &Acceptances::default(),
                &mut pacer,
                &AtomicBool::new(false),
                "2026-07-27T00:00:00Z",
            )
            .unwrap();
        let record = DownloadManager::new(registry.models_dir().to_path_buf(), bus)
            .installed()
            .remove("good")
            .unwrap();
        registry.publish_installed("good", record);
        registry.publish_operation("good", attempt_id, "installed", true, None);

        let events = rx
            .try_iter()
            .filter_map(|event| match event {
                RuntimeEvent::ModelOperation {
                    attempt_id,
                    sequence,
                    phase,
                    terminal,
                    ..
                } => Some((attempt_id, sequence, phase, terminal)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            events
                .iter()
                .map(|event| event.2.as_str())
                .collect::<Vec<_>>(),
            [
                "queued",
                "downloading",
                "verifying",
                "installing",
                "installed"
            ]
        );
        assert!(events.iter().all(|event| event.0 == attempt_id));
        assert!(events.windows(2).all(|pair| pair[0].1 < pair[1].1));
        assert_eq!(events.iter().filter(|event| event.3).count(), 1);
        assert_eq!(registry.operation("good"), None);
        let terminal = registry.last_operation("good").unwrap();
        assert!(terminal.terminal);
        assert_eq!(terminal.phase, "installed");
    }

    #[test]
    fn failed_and_cancelled_attempts_each_publish_one_terminal_verdict() {
        let temp = tempfile::tempdir().unwrap();
        let bus = RuntimeBus::new();
        let rx = bus.subscribe();
        let registry = ModelOperationRegistry::open(
            temp.path().join("models"),
            &manifest(b"immutable"),
            bus,
            true,
        );
        for (model_id, attempt_id, terminal, error) in [
            (
                "good",
                "attempt-failed",
                "failed",
                Some("checksum mismatch".to_owned()),
            ),
            ("bad", "attempt-cancelled", "cancelled", None),
        ] {
            registry.publish_operation(model_id, attempt_id, "queued", false, None);
            registry.publish_operation(model_id, attempt_id, "downloading", false, None);
            registry.publish_operation(model_id, attempt_id, terminal, true, error.clone());
        }
        let events = rx
            .try_iter()
            .filter_map(|event| match event {
                RuntimeEvent::ModelOperation {
                    model_id,
                    attempt_id,
                    sequence,
                    phase,
                    terminal,
                    error,
                } => Some((model_id, attempt_id, sequence, phase, terminal, error)),
                _ => None,
            })
            .collect::<Vec<_>>();
        for (model_id, attempt_id, terminal) in [
            ("good", "attempt-failed", "failed"),
            ("bad", "attempt-cancelled", "cancelled"),
        ] {
            let attempt = events
                .iter()
                .filter(|event| event.0 == model_id)
                .collect::<Vec<_>>();
            assert_eq!(
                attempt
                    .iter()
                    .map(|event| event.3.as_str())
                    .collect::<Vec<_>>(),
                ["queued", "downloading", terminal]
            );
            assert!(attempt.iter().all(|event| event.1 == attempt_id));
            assert!(attempt.windows(2).all(|pair| pair[0].2 < pair[1].2));
            assert_eq!(attempt.iter().filter(|event| event.4).count(), 1);
        }
        assert_eq!(
            registry.last_operation("good").unwrap().error.as_deref(),
            Some("checksum mismatch")
        );
        assert_eq!(registry.last_operation("bad").unwrap().error, None);
    }

    #[test]
    fn valid_index_with_missing_file_is_excluded_and_surfaced_not_trusted() {
        let temp = tempfile::tempdir().unwrap();
        let models = temp.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        let indexed = BTreeMap::from([(
            "good".to_owned(),
            InstalledRecord {
                manifest_version: 7,
                when: "2026-07-27T00:00:00Z".into(),
            },
        )]);
        std::fs::write(
            models.join("installed.json"),
            serde_json::to_vec(&indexed).unwrap(),
        )
        .unwrap();

        let registry =
            ModelOperationRegistry::open(models, &manifest(b"immutable"), RuntimeBus::new(), true);

        assert!(!registry.installed().contains_key("good"));
        assert!(
            registry
                .disagreement("good")
                .unwrap()
                .contains("unavailable")
        );
        assert!(
            !registry.recovery_pending(),
            "layout-only rejection repairs the small index synchronously"
        );
        let durable: BTreeMap<String, InstalledRecord> = serde_json::from_slice(
            &std::fs::read(registry.models_dir().join("installed.json")).unwrap(),
        )
        .unwrap();
        assert!(
            !durable.contains_key("good"),
            "a later download must not trust the rejected record by length"
        );
    }

    #[test]
    fn cancelled_recovery_publishes_no_partial_index_or_install() {
        let temp = tempfile::tempdir().unwrap();
        let models = temp.path().join("models");
        std::fs::create_dir_all(models.join("good")).unwrap();
        std::fs::write(models.join("good/weights.bin"), b"immutable").unwrap();
        let manifest = manifest(b"immutable");
        let registry =
            ModelOperationRegistry::open(models.clone(), &manifest, RuntimeBus::new(), true);

        let report = registry
            .recover_pending(&manifest, RuntimeBus::new(), &AtomicBool::new(true))
            .unwrap();

        assert!(report.cancelled);
        assert!(registry.recovery_pending());
        assert!(registry.installed().is_empty());
        assert!(
            !models.join("installed.json").exists(),
            "a cancelled partial proof must stay distinguishable next launch"
        );
    }

    #[test]
    fn manifest_version_hashing_is_deferred_until_managed_recovery() {
        let temp = tempfile::tempdir().unwrap();
        let models = temp.path().join("models");
        std::fs::create_dir_all(models.join("good")).unwrap();
        std::fs::write(models.join("good/weights.bin"), b"immutable").unwrap();
        let indexed = BTreeMap::from([(
            "good".to_owned(),
            InstalledRecord {
                manifest_version: 6,
                when: "2026-07-27T00:00:00Z".into(),
            },
        )]);
        std::fs::write(
            models.join("installed.json"),
            serde_json::to_vec(&indexed).unwrap(),
        )
        .unwrap();
        let manifest = manifest(b"immutable");

        let registry = ModelOperationRegistry::open(models, &manifest, RuntimeBus::new(), true);
        assert!(registry.installed().is_empty());
        assert!(registry.recovery_pending());

        registry
            .recover_pending(&manifest, RuntimeBus::new(), &AtomicBool::new(false))
            .unwrap();
        assert!(registry.installed().contains_key("good"));
        assert!(!registry.recovery_pending());
    }

    #[test]
    fn unknown_directories_are_reported_as_true_orphans() {
        let temp = tempfile::tempdir().unwrap();
        let models = temp.path().join("models");
        std::fs::create_dir_all(models.join("not-in-manifest")).unwrap();
        std::fs::write(models.join("not-in-manifest/blob"), b"orphan").unwrap();

        let registry =
            ModelOperationRegistry::open(models, &manifest(b"immutable"), RuntimeBus::new(), true);

        assert!(
            registry
                .debug_lines()
                .iter()
                .any(|line| line.contains("orphan model directory not-in-manifest: 6 bytes"))
        );
    }

    #[test]
    fn lockless_process_never_repairs_or_quarantines_shared_index() {
        let temp = tempfile::tempdir().unwrap();
        let models = temp.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        let corrupt = b"{truncated";
        std::fs::write(models.join("installed.json"), corrupt).unwrap();

        let registry = ModelOperationRegistry::open(
            models.clone(),
            &manifest(b"immutable"),
            RuntimeBus::new(),
            false,
        );

        assert_eq!(
            std::fs::read(models.join("installed.json")).unwrap(),
            corrupt,
            "a process without the instance lock is read-only"
        );
        assert!(
            registry
                .debug_lines()
                .iter()
                .any(|line| line.contains("does not hold the runtime lock"))
        );
        assert!(
            std::fs::read_dir(&models)
                .unwrap()
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().contains(".corrupt-"))
        );
    }

    #[test]
    fn adversarial_multi_model_operation_matrix_never_loses_an_index_write() {
        let temp = tempfile::tempdir().unwrap();
        let models = temp.path().join("models");
        let bus = RuntimeBus::new();
        let registry = Arc::new(ModelOperationRegistry::open(
            models.clone(),
            &manifest(b"immutable"),
            bus.clone(),
            true,
        ));
        let record = || InstalledRecord {
            manifest_version: 7,
            when: "2026-07-27T00:00:00Z".into(),
        };
        {
            let _operation = registry.lock_operation();
            for id in ["keep", "remove-old", "gc-old"] {
                std::fs::create_dir_all(models.join(id)).unwrap();
                std::fs::write(models.join(id).join("weights.bin"), id.as_bytes()).unwrap();
                registry
                    .commit_installed_locked(bus.clone(), id, record())
                    .unwrap();
            }
        }

        // These are the production phases that can otherwise collide around
        // model files or the shared installed.json RMW. All start together;
        // the active counter proves there was never more than one admitted
        // operation, including the non-index load/unload transitions.
        let operations = [
            ("downloaded", "downloading"),
            ("verified", "verifying"),
            ("loaded", "loading"),
            ("unloaded", "unloading"),
            ("remove-old", "removing"),
            ("gc-old", "garbage-collecting"),
        ];
        let barrier = Arc::new(Barrier::new(operations.len() + 1));
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::new();
        for (model_id, phase) in operations.iter().copied() {
            let registry = Arc::clone(&registry);
            let bus = bus.clone();
            let barrier = Arc::clone(&barrier);
            let active = Arc::clone(&active);
            let max_active = Arc::clone(&max_active);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                let _operation = registry.lock_operation();
                let now_active = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_active.fetch_max(now_active, Ordering::SeqCst);
                registry.set_operation(model_id, Some(phase));
                std::thread::sleep(Duration::from_millis(2));
                match phase {
                    "downloading" | "verifying" => {
                        std::fs::create_dir_all(registry.models_dir().join(model_id)).unwrap();
                        std::fs::write(
                            registry.models_dir().join(model_id).join("weights.bin"),
                            phase.as_bytes(),
                        )
                        .unwrap();
                        registry
                            .commit_installed_locked(
                                bus,
                                model_id,
                                InstalledRecord {
                                    manifest_version: 7,
                                    when: "2026-07-27T00:00:01Z".into(),
                                },
                            )
                            .unwrap();
                    }
                    "removing" | "garbage-collecting" => {
                        std::fs::remove_dir_all(registry.models_dir().join(model_id)).unwrap();
                        registry.commit_removed_locked(bus, model_id).unwrap()
                    }
                    "loading" | "unloading" => {}
                    other => panic!("unexpected operation {other}"),
                }
                registry.set_operation(model_id, None);
                active.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        barrier.wait();
        for worker in workers {
            worker.join().unwrap();
        }

        assert_eq!(
            max_active.load(Ordering::SeqCst),
            1,
            "download/verify/load/unload/remove/GC share one admission authority"
        );
        let expected = BTreeSet::from([
            "downloaded".to_owned(),
            "keep".to_owned(),
            "verified".to_owned(),
        ]);
        assert_eq!(
            registry
                .installed()
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>(),
            expected
        );
        let durable: BTreeMap<String, InstalledRecord> =
            serde_json::from_slice(&std::fs::read(models.join("installed.json")).unwrap()).unwrap();
        assert_eq!(
            durable,
            registry.installed(),
            "the durable index and committed in-memory authority converge exactly"
        );
        assert!(!models.join("remove-old").exists());
        assert!(!models.join("gc-old").exists());
        assert!(
            operations
                .iter()
                .all(|(model_id, _)| registry.operation(model_id).is_none()),
            "no operation may remain live after its terminal commit"
        );
    }
}
