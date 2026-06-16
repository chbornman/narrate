//! Embedding ingest passes: `text-embedding` (annotation chunks) and
//! `image-embedding` (CLIP image vectors), through the existing versioned
//! pass queue (LIBRARY §10 / DECISIONS L4).
//!
//! Contract: spec/RETRIEVAL.md §3 — "Embedding is a versioned backfill
//! pass (LIBRARY.md mechanics)". Mechanics reused unchanged: the queue IS
//! the `pending` rows; `running -> pending` on startup; versioned re-runs
//! by `pass_version`. Staleness is `vectors.inputs_hash` (§1.2): the pass
//! recomputes each unit's hash and skips fresh rows, so a re-pended pass
//! after re-annotation re-embeds exactly what changed. Session-level
//! remarks (zero image targets — outside any image's queue row) are swept
//! directly at the top of each drain, so the §1.1 indexable set is fully
//! covered.
//!
//! Degraded posture: with no embedder configured the drain claims nothing
//! and the rows sit pending — idle, NotConfigured-style, never errors —
//! matching the runtime's degraded contract. The embedder models are pinned
//! and downloadable post-B73; the live ort connector that fills these rows
//! lands in P7.4 L3/L4. Everything here ships mock-verified until then.
//!
//! B69 (retrieval stays additive): the image_clip signal is built
//! unconditionally beside the text signal; journal coverage never retires
//! it.

use std::path::Path;

use photoproof_connectors::embedder::{DecodedImage, Embedder};
use photoproof_connectors::vector_store::{VecKey, VecKind, VecSpace, VecUnit};
use rusqlite::{Connection, params};

use super::preview::GENERATOR_VERSION;
use super::{ArtifactKind, Library, LibraryError, QueueOptions, QueueReport, ingest};
use crate::retrieval::{
    ChunkContext, PpvecStore, VecMeta, chunk_folded_text, image_inputs_hash, inputs_hash,
};

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
        // Model-aware re-pend: if the configured embedder differs from the model
        // that last completed a pass (or a legacy NULL row), re-pend it so the
        // library re-embeds under the new model. Pass completion is otherwise
        // model-blind — swapping the CLIP/text model would leave every pass
        // `done` and topic affinities would silently score against a vector space
        // the new model never wrote (the fp16 CLIP default regression).
        {
            let conn = self.db.lock().expect("poisoned");
            if let Some(clip) = rig.clip {
                ingest::repend_passes_for_model(
                    &conn,
                    ingest::PassName::ImageEmbedding,
                    clip.model_id(),
                )?;
            }
            if let Some(text) = rig.text {
                ingest::repend_passes_for_model(
                    &conn,
                    ingest::PassName::TextEmbedding,
                    text.model_id(),
                )?;
            }
        }
        // Physical hygiene first: rows the events engine marked deleted
        // (revision/retraction/redaction — RETRIEVAL §1.1) get their flat
        // file bytes zeroed and their metadata reclaimed before any new
        // work. Redactions were already zeroed synchronously at redact
        // time (§13.5); this sweep is the idempotent backstop and the
        // reclaim path.
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

        // Session-level remarks (zero image targets) ARE indexable (§1.1;
        // they surface via the §5.4 session_hits list) but the queue is
        // keyed per image, so no ingest_passes row can ever reach them.
        // Sweep them directly each drain instead — cheap when fresh, since
        // the §1.2 inputs_hash check skips unchanged chunks without
        // touching the embedder. The sweep is UNBOUNDED (every stale
        // session-level remark in one turn — a first backfill over a journal
        // with hundreds of voice remarks is minutes of ort CPU), so it must
        // honor `cancel` per remark just like the per-item loop below:
        // arming the mic (capture_live) preempts it instead of monopolizing
        // the machine the politeness rule reserves for capture.
        if let Some(embedder) = rig.text {
            self.embed_sessionlevel_text(embedder, rig.vectors, opts, &mut report)?;
            if report.cancelled {
                return Ok(report);
            }
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
                // Embedding reads cached previews / folded text, not the original
                // file, so it runs even while the source volume is offline: no
                // online-path requirement (false).
                ingest::claim_next_of(&conn, self.now(), &allowed, false)?
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
        // event_fts body (EVENTS §5.4/§6.3): reading it back from FTS
        // keeps the two indexes on one indexable set (§1.1). The
        // session-level complement (zero targets, unreachable from any
        // image's queue row) is swept by `embed_sessionlevel_text`.
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

        for (event_id, body, annotated_date) in roots {
            let folder = {
                let conn = self.db.lock().expect("poisoned");
                folder_for_event(&conn, &event_id)?
            };
            let ctx = ChunkContext {
                date: Some(annotated_date),
                folder,
                // Collections store lands in P7.3.
                collection: None,
            };
            if let Some(err) =
                self.embed_event_chunks(embedder, vectors, &space, &event_id, &body, &ctx)?
            {
                // Transient (§10.5): backoff then error after 3 attempts.
                let conn = self.db.lock().expect("poisoned");
                ingest::mark_failed(&conn, item, &err, true, self.now())?;
                report.transient_retries += 1;
                return Ok(());
            }
        }

        let conn = self.db.lock().expect("poisoned");
        // Record the text embedder's model_id so a later model swap re-pends
        // this pass (model-aware completion; see ingest::mark_done_with_model).
        ingest::mark_done_with_model(&conn, item, embedder.model_id(), self.now())?;
        report.done += 1;
        Ok(())
    }

    /// Chunk + embed ONE event's folded text into `space`, skipping fresh
    /// chunks (§1.2 staleness) and dropping the stale re-chunking tail.
    /// Shared by the per-image pass and the session-level sweep.
    /// `Ok(Some(msg))` is a transient failure (embedder or vector-store
    /// IO): nothing was marked complete, so the next run retries; the
    /// caller decides whether a queue row records the failure. Failures
    /// are returned, not propagated, so one bad item can never abort the
    /// whole drain (which would also skip every later pass AND the
    /// end-of-drain compaction).
    fn embed_event_chunks<TE: Embedder>(
        &self,
        embedder: &TE,
        vectors: &PpvecStore,
        space: &VecSpace,
        event_id: &str,
        body: &str,
        ctx: &ChunkContext,
    ) -> Result<Option<String>, LibraryError> {
        let chunks = chunk_folded_text(body, ctx);
        for chunk in &chunks {
            let key = VecKey {
                space: space.clone(),
                unit: VecUnit::AnnotationChunk {
                    event_id: event_id.to_string(),
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
                Err(e) => return Ok(Some(format!("embedder: {e}"))),
            };
            if let Err(e) = vectors.upsert_with_meta(
                &key,
                &embedding,
                &VecMeta {
                    inputs_hash: hash,
                    char_start: Some(chunk.char_start),
                    char_end: Some(chunk.char_end),
                },
            ) {
                return Ok(Some(format!("vector-store: {e}")));
            }
        }
        // Re-chunking tail: a shorter folded text leaves stale
        // higher-index chunk rows behind; zero + drop them.
        if let Err(e) = vectors.drop_chunks_from(space, event_id, chunks.len() as u32) {
            return Ok(Some(format!("vector-store: {e}")));
        }
        Ok(None)
    }

    /// Embed every session-level remark (zero image targets, §1.1). No
    /// queue row drives this — the per-image queue cannot address an event
    /// with no image — so it runs at the top of every drain; the
    /// inputs_hash staleness check makes the steady state a no-op. A
    /// revision marks the old rows deleted (events engine), which reads as
    /// stale here and re-embeds; redactions are zeroed at redact time and
    /// reclaimed by the sweep.
    fn embed_sessionlevel_text<TE: Embedder>(
        &self,
        embedder: &TE,
        vectors: &PpvecStore,
        opts: &QueueOptions,
        report: &mut QueueReport,
    ) -> Result<(), LibraryError> {
        let space = VecSpace {
            vec_kind: VecKind::AnnotationChunk,
            model_id: embedder.model_id().to_string(),
        };
        let roots: Vec<(String, String, String)> = {
            let conn = self.db.lock().expect("poisoned");
            let mut stmt = conn.prepare_cached(
                "SELECT m.root_event_id, f.body, substr(e.ts, 1, 10)
                 FROM fts_map m
                 JOIN event_fts f ON f.rowid = m.fts_rowid
                 JOIN annotation_events e ON e.id = m.root_event_id
                 WHERE NOT EXISTS (SELECT 1 FROM event_targets t
                                   WHERE t.event_id = m.root_event_id)
                 ORDER BY m.root_event_id",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.collect::<Result<_, _>>()?
        };
        for (event_id, body, annotated_date) in roots {
            // Honor cancellation between remarks: this sweep is unbounded, so
            // an armed mic (capture_live, wired as `cancel` by the drain) must
            // be able to preempt it mid-sweep — fresh chunks already no-op via
            // the inputs_hash check, so the cost is only in stale ones, which
            // we stop embedding the moment the flag flips. The half-swept rows
            // stay stale and the next idle drain resumes them.
            if let Some(cancel) = &opts.cancel
                && cancel.load(std::sync::atomic::Ordering::Relaxed)
            {
                report.cancelled = true;
                return Ok(());
            }
            let ctx = ChunkContext {
                date: Some(annotated_date),
                // Zero targets => no folder; collections land in P7.3.
                folder: None,
                collection: None,
            };
            if self
                .embed_event_chunks(embedder, vectors, &space, &event_id, &body, &ctx)?
                .is_some()
            {
                // Transient; nothing was marked fresh, so the next drain
                // retries. There is no queue row to record the error on.
                report.transient_retries += 1;
                return Ok(());
            }
        }
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

        // Staleness over (image identity, embedder model, preview-generator
        // version) — NOT the raw preview bytes. WHY: a preview regen produces
        // different bytes for the same picture, so a byte hash re-embedded the
        // whole library on every regen (self-heal 3B). This recipe skips when
        // the same image is already embedded under the same model + the same
        // preview-pipeline version, and only re-embeds when the generator
        // version bumps (the pixels genuinely changed).
        let hash = image_inputs_hash(
            item.image_hash.as_str(),
            embedder.model_id(),
            GENERATOR_VERSION,
        );
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
            // Already fresh under THIS model — record the model so the row is
            // recognized as current (and not re-pended again next drain).
            ingest::mark_done_with_model(&conn, item, embedder.model_id(), self.now())?;
            report.done += 1;
            return Ok(());
        }

        // Read + decode the preview pixels only now that we know we must embed.
        let bytes = std::fs::read(&path)?;
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
        // Geometry to the CLIP square HERE in core (PLAN-P7.4 decision 3: the
        // connector takes a DecodedImage already preprocessed BY CORE to
        // 378x378). The Display/Thumb artifact is full-resolution (2560/512
        // edge); the real OrtEmbedder::embed_image hard-rejects anything that
        // is not exactly 378x378, so without this every image-embedding row
        // would fail and retry forever once the live connector is wired. The
        // mock accepts any size, which is why the unit suite never caught it.
        let decoded = super::preprocess_clip_image(&decoded);
        let embedding = match pollster::block_on(embedder.embed_image(&decoded)) {
            Ok(e) => e,
            Err(e) => {
                let conn = self.db.lock().expect("poisoned");
                ingest::mark_failed(&conn, item, &format!("embedder: {e}"), true, self.now())?;
                report.transient_retries += 1;
                return Ok(());
            }
        };
        if let Err(e) = vectors.upsert_with_meta(
            &key,
            &embedding,
            &VecMeta {
                inputs_hash: hash,
                char_start: None,
                char_end: None,
            },
        ) {
            // Store IO is transient (disk hiccup; torn files self-heal on
            // the next write): one bad write must not abort the whole
            // drain and starve every other pass plus compaction.
            let conn = self.db.lock().expect("poisoned");
            ingest::mark_failed(&conn, item, &format!("vector-store: {e}"), true, self.now())?;
            report.transient_retries += 1;
            return Ok(());
        }
        let conn = self.db.lock().expect("poisoned");
        // Record the CLIP embedder's model_id so a model swap re-pends this pass.
        ingest::mark_done_with_model(&conn, item, embedder.model_id(), self.now())?;
        report.done += 1;
        Ok(())
    }
}

/// Folder for the tiny-chunk context prefix (§2), resolved per EVENT: the
/// lexicographically smallest folder across all the event's targets'
/// active paths. A multi-target event stores ONE chunk row (§1.2), so the
/// prefix — an `inputs_hash` input — must not depend on which image's pass
/// claims the event first: §2 requires the prefix be deterministic so
/// rebuild byte-equality (§13.8) holds. The spec does not pick WHICH
/// folder a multi-folder event gets; smallest-sorted is the stable,
/// obvious rule.
fn folder_for_event(conn: &Connection, event_id: &str) -> Result<Option<String>, LibraryError> {
    let mut stmt = conn.prepare_cached(
        "SELECT p.rel_path FROM event_targets t
         JOIN paths p ON p.image_hash = t.image_hash AND p.state = 'active'
         WHERE t.event_id = ?1",
    )?;
    let rows = stmt.query_map([event_id], |r| r.get::<_, String>(0))?;
    let mut best: Option<String> = None;
    for rel in rows {
        let folder = Path::new(&rel?)
            .parent()
            .map(|d| d.to_string_lossy().replace('\\', "/"))
            .filter(|d| !d.is_empty());
        if let Some(f) = folder
            && best.as_ref().is_none_or(|b| f < *b)
        {
            best = Some(f);
        }
    }
    Ok(best)
}
