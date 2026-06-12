//! Embedding ingest passes: `text-embedding` (annotation chunks) and
//! `image-embedding` (CLIP image vectors), through the existing versioned
//! pass queue (LIBRARY §10 / DECISIONS L4).
//!
//! Contract: spec/RETRIEVAL.md §3 — "Embedding is a versioned backfill
//! pass (LIBRARY.md mechanics)". Mechanics reused unchanged: the queue IS
//! the `pending` rows; `running -> pending` on startup; versioned re-runs
//! by `pass_version`. Staleness is `vectors.inputs_hash` (§1.2): the pass
//! recomputes each unit's hash and skips fresh rows, so a re-pended pass
//! after re-annotation re-embeds exactly what changed.
//!
//! Degraded posture: with no embedder configured the drain claims nothing
//! and the rows sit pending — idle, NotConfigured-style, never errors —
//! matching the runtime's degraded contract. The embedders themselves are
//! not downloadable yet (manifest unpinned until spike session 2);
//! everything here ships mock-verified.
//!
//! B69 (retrieval stays additive): the image_clip signal is built
//! unconditionally beside the text signal; journal coverage never retires
//! it.

use std::path::Path;

use photoproof_connectors::embedder::{DecodedImage, Embedder};
use photoproof_connectors::vector_store::{VecKey, VecKind, VecSpace, VecUnit};
use rusqlite::{Connection, OptionalExtension, params};

use super::{ArtifactKind, Library, LibraryError, QueueOptions, QueueReport, ingest};
use crate::retrieval::{ChunkContext, PpvecStore, VecMeta, chunk_folded_text, inputs_hash};

/// The embedding drain's collaborators. `None` embedders leave their pass
/// rows untouched (idle); the PPVEC store is always required — it also
/// hosts the physical-zero sweep for redacted vectors.
pub struct EmbeddingRig<'a, TE: Embedder, CE: Embedder = TE> {
    /// The text embedder (annotation chunks; DECISIONS X3).
    pub text: Option<&'a TE>,
    /// The CLIP embedder (image vectors; DECISIONS X3).
    pub clip: Option<&'a CE>,
    pub vectors: &'a PpvecStore,
}

impl Library {
    /// Ensure embedding pass rows exist for every known image (idempotent;
    /// `done` rows are never re-pended here — that is the event-append
    /// hook's job). RETRIEVAL §3: embedding is a versioned BACKFILL pass,
    /// so rows appear when the backfill is scheduled, not at M1 ingest —
    /// M1's drain-to-empty invariant stays intact.
    pub fn enqueue_embedding_backfill(
        &self,
        text: bool,
        clip: bool,
    ) -> Result<usize, LibraryError> {
        let now = self.now().to_rfc3339();
        let conn = self.db.lock().expect("poisoned");
        let mut created = 0usize;
        for (enabled, pass) in [
            (text, ingest::PassName::TextEmbedding),
            (clip, ingest::PassName::ImageEmbedding),
        ] {
            if !enabled {
                continue;
            }
            created += conn.execute(
                "INSERT INTO ingest_passes
                   (image_hash, pass_name, pass_version, model_id, state, priority,
                    attempts, error, enqueued_at, started_at, completed_at, not_before)
                 SELECT image_hash, ?1, ?2, NULL, 'pending', ?3, 0, NULL, ?4,
                        NULL, NULL, NULL
                 FROM images
                 WHERE true
                 ON CONFLICT(image_hash, pass_name, pass_version) DO NOTHING",
                params![
                    pass.as_str(),
                    ingest::PASS_VERSION,
                    ingest::PRIORITY_GPU,
                    now
                ],
            )?;
        }
        Ok(created)
    }

    /// Drain the embedding queue. Sequential by design: GPU/model passes
    /// run at concurrency 1 (LIBRARY §10.3); claims honor queue priority
    /// and only cover passes whose embedder is configured.
    pub fn process_embedding_queue<TE: Embedder, CE: Embedder>(
        &self,
        rig: &EmbeddingRig<'_, TE, CE>,
        opts: &QueueOptions,
    ) -> Result<QueueReport, LibraryError> {
        let mut report = QueueReport::default();
        self.enqueue_embedding_backfill(rig.text.is_some(), rig.clip.is_some())?;
        // Physical hygiene first: rows the events engine marked deleted
        // (revision/retraction/redaction — RETRIEVAL §1.1) get their flat
        // file bytes zeroed before any new work, so a redaction never
        // outlives the next drain.
        rig.vectors.sweep_dead()?;

        let mut allowed = Vec::new();
        if rig.text.is_some() {
            allowed.push(ingest::PassName::TextEmbedding);
        }
        if rig.clip.is_some() {
            allowed.push(ingest::PassName::ImageEmbedding);
        }
        if allowed.is_empty() {
            // NotConfigured: rows sit pending, no errors, nothing claimed.
            return Ok(report);
        }

        loop {
            if let Some(cancel) = &opts.cancel
                && cancel.load(std::sync::atomic::Ordering::Relaxed)
            {
                report.cancelled = true;
                break;
            }
            if let Some(max) = opts.max_items
                && report.processed >= max
            {
                break;
            }
            let item = {
                let conn = self.db.lock().expect("poisoned");
                ingest::claim_next_of(&conn, self.now(), &allowed)?
            };
            let Some(item) = item else { break };
            report.processed += 1;
            match item.pass {
                ingest::PassName::TextEmbedding => {
                    let embedder = rig.text.expect("claimed only when configured");
                    self.run_text_embedding_pass(&item, embedder, rig.vectors, &mut report)?;
                }
                ingest::PassName::ImageEmbedding => {
                    let embedder = rig.clip.expect("claimed only when configured");
                    self.run_image_embedding_pass(&item, embedder, rig.vectors, &mut report)?;
                }
                _ => unreachable!("claim_next_of returns only allowed passes"),
            }
        }

        // Compaction by the §1.3 thresholds, scheduled here because this
        // IS the background pass that touches the spaces.
        for embedder_space in [
            rig.text.map(|e| VecSpace {
                vec_kind: VecKind::AnnotationChunk,
                model_id: e.model_id().to_string(),
            }),
            rig.clip.map(|e| VecSpace {
                vec_kind: VecKind::ImageClip,
                model_id: e.model_id().to_string(),
            }),
        ]
        .into_iter()
        .flatten()
        {
            rig.vectors.compact_if_needed(&embedder_space)?;
        }
        Ok(report)
    }

    /// `text-embedding` for one image: chunk + embed every live indexable
    /// event targeting it (§1.1 rules, §2 chunking), skipping chunks whose
    /// `inputs_hash` is fresh.
    fn run_text_embedding_pass<TE: Embedder>(
        &self,
        item: &ingest::QueueItem,
        embedder: &TE,
        vectors: &PpvecStore,
        report: &mut QueueReport,
    ) -> Result<(), LibraryError> {
        let space = VecSpace {
            vec_kind: VecKind::AnnotationChunk,
            model_id: embedder.model_id().to_string(),
        };
        // The folded text of live, unscrubbed remark roots IS the
        // event_fts body (EVENTS §5.4/§6.3) — the indexable set for FTS
        // and vectors is identical by construction (§1.1), so reading it
        // back from FTS guarantees the two indexes can never disagree.
        let roots: Vec<(String, String, String)> = {
            let conn = self.db.lock().expect("poisoned");
            let mut stmt = conn.prepare_cached(
                "SELECT m.root_event_id, f.body, substr(e.ts, 1, 10)
                 FROM fts_map m
                 JOIN event_fts f ON f.rowid = m.fts_rowid
                 JOIN event_targets t ON t.event_id = m.root_event_id
                 JOIN annotation_events e ON e.id = m.root_event_id
                 WHERE t.image_hash = ?1
                 ORDER BY m.root_event_id",
            )?;
            let rows = stmt.query_map([item.image_hash.as_str()], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
            rows.collect::<Result<_, _>>()?
        };
        let folder = {
            let conn = self.db.lock().expect("poisoned");
            folder_name(&conn, item.image_hash.as_str())?
        };

        for (event_id, body, annotated_date) in roots {
            let ctx = ChunkContext {
                date: Some(annotated_date),
                folder: folder.clone(),
                // Collections store lands in P7.3.
                collection: None,
            };
            let chunks = chunk_folded_text(&body, &ctx);
            for chunk in &chunks {
                let key = VecKey {
                    space: space.clone(),
                    unit: VecUnit::AnnotationChunk {
                        event_id: event_id.clone(),
                        chunk_index: chunk.index,
                    },
                };
                let hash = inputs_hash(chunk.embed_text.as_bytes());
                if let Some((existing, deleted)) = vectors.row_inputs_hash(&key)?
                    && !deleted
                    && existing == hash
                {
                    continue; // fresh — the staleness check (§1.2)
                }
                let embedding = match pollster::block_on(embedder.embed_text(&chunk.embed_text)) {
                    Ok(e) => e,
                    Err(e) => {
                        // Model IO failures are transient by default
                        // (§10.5): backoff then error after 3 attempts.
                        let conn = self.db.lock().expect("poisoned");
                        ingest::mark_failed(
                            &conn,
                            item,
                            &format!("embedder: {e}"),
                            true,
                            self.now(),
                        )?;
                        report.transient_retries += 1;
                        return Ok(());
                    }
                };
                vectors.upsert_with_meta(
                    &key,
                    &embedding,
                    &VecMeta {
                        inputs_hash: hash,
                        char_start: Some(chunk.char_start),
                        char_end: Some(chunk.char_end),
                    },
                )?;
            }
            // Re-chunking tail: a shorter folded text leaves stale
            // higher-index chunk rows behind; zero + drop them.
            vectors.drop_chunks_from(&space, &event_id, chunks.len() as u32)?;
        }

        let conn = self.db.lock().expect("poisoned");
        ingest::mark_done(&conn, item, self.now())?;
        report.done += 1;
        Ok(())
    }

    /// `image-embedding` for one image: embed the cached preview pixels
    /// (§3 item 3) via the CLIP embedder's image tower.
    fn run_image_embedding_pass<CE: Embedder>(
        &self,
        item: &ingest::QueueItem,
        embedder: &CE,
        vectors: &PpvecStore,
        report: &mut QueueReport,
    ) -> Result<(), LibraryError> {
        // Prefer the display artifact (more pixels than CLIP needs is
        // fine; thumb is the fallback). No artifact yet says nothing about
        // the image — the preview pass simply has not run; defer without
        // burning attempts, like an offline volume.
        let artifact = [ArtifactKind::Display, ArtifactKind::Thumb]
            .into_iter()
            .find_map(|kind| {
                let path = super::artifact_path(self.cache_dir(), &item.image_hash, kind);
                path.exists().then_some(path)
            });
        let Some(path) = artifact else {
            let conn = self.db.lock().expect("poisoned");
            ingest::defer(&conn, item, "preview-not-ready", self.now())?;
            report.transient_retries += 1;
            return Ok(());
        };

        let bytes = std::fs::read(&path)?;
        let hash = inputs_hash(&bytes);
        let space = VecSpace {
            vec_kind: VecKind::ImageClip,
            model_id: embedder.model_id().to_string(),
        };
        let key = VecKey {
            space,
            unit: VecUnit::Image {
                image_hash: item.image_hash.as_str().to_string(),
            },
        };
        if let Some((existing, deleted)) = vectors.row_inputs_hash(&key)?
            && !deleted
            && existing == hash
        {
            let conn = self.db.lock().expect("poisoned");
            ingest::mark_done(&conn, item, self.now())?;
            report.done += 1;
            return Ok(());
        }

        let decoded = match image::load_from_memory(&bytes) {
            Ok(img) => {
                let rgb = img.to_rgb8();
                DecodedImage {
                    width: rgb.width(),
                    height: rgb.height(),
                    rgb8: rgb.into_raw(),
                }
            }
            Err(e) => {
                // A torn artifact regenerates (preview writes are atomic;
                // doctor re-pends vanished ones) — transient, not a
                // strike against the image.
                let conn = self.db.lock().expect("poisoned");
                ingest::mark_failed(&conn, item, &format!("decode: {e}"), true, self.now())?;
                report.transient_retries += 1;
                return Ok(());
            }
        };
        let embedding = match pollster::block_on(embedder.embed_image(&decoded)) {
            Ok(e) => e,
            Err(e) => {
                let conn = self.db.lock().expect("poisoned");
                ingest::mark_failed(&conn, item, &format!("embedder: {e}"), true, self.now())?;
                report.transient_retries += 1;
                return Ok(());
            }
        };
        vectors.upsert_with_meta(
            &key,
            &embedding,
            &VecMeta {
                inputs_hash: hash,
                char_start: None,
                char_end: None,
            },
        )?;
        let conn = self.db.lock().expect("poisoned");
        ingest::mark_done(&conn, item, self.now())?;
        report.done += 1;
        Ok(())
    }
}

/// Folder name for the tiny-chunk context prefix (§2): the directory part
/// of the image's active path, when one exists.
fn folder_name(conn: &Connection, image_hash: &str) -> Result<Option<String>, LibraryError> {
    let rel: Option<String> = conn
        .query_row(
            "SELECT rel_path FROM paths
             WHERE image_hash = ?1 AND state = 'active'
             ORDER BY path_id LIMIT 1",
            [image_hash],
            |r| r.get(0),
        )
        .optional()?;
    Ok(rel.and_then(|p| {
        Path::new(&p)
            .parent()
            .map(|d| d.to_string_lossy().replace('\\', "/"))
            .filter(|d| !d.is_empty())
    }))
}
