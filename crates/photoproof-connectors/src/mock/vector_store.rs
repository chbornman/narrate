//! In-memory mock `VectorStore`.
//!
//! Brute-force exact search over non-deleted rows (dot product — vectors
//! are L2-normalized, so cosine), deterministic tie-break by `vector_id`.
//! Models the RETRIEVAL §1.3 lifecycle faithfully enough for retrieval
//! tests: upsert-replace, logical delete, physical scrub (zeroed bytes
//! remain observable via `row`), and compaction dropping dead rows while
//! live ids survive.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::embedder::Embedding;
use crate::vector_store::{
    VecHit, VecKey, VecKind, VecSpace, VecUnit, VectorStore, VectorStoreError, VectorStoreResult,
};

pub struct MockVectorStore {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    spaces: HashMap<(VecKind, String), Space>,
    next_id: i64,
}

struct Space {
    dims: usize,
    rows: Vec<Row>,
}

struct Row {
    id: i64,
    unit: VecUnit,
    vector: Vec<f32>,
    deleted: bool,
}

/// Test-visible snapshot of one stored row.
#[derive(Debug, Clone, PartialEq)]
pub struct MockRow {
    pub vector_id: i64,
    pub vector: Vec<f32>,
    pub deleted: bool,
}

impl Default for MockVectorStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MockVectorStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                spaces: HashMap::new(),
                next_id: 1,
            }),
        }
    }

    /// Number of non-deleted rows in `space` (0 if the space is unknown).
    pub fn live_count(&self, space: &VecSpace) -> usize {
        let inner = self.inner.lock().unwrap();
        inner
            .spaces
            .get(&space_key(space))
            .map_or(0, |s| s.rows.iter().filter(|r| !r.deleted).count())
    }

    /// Snapshot of the row at `key`, if it exists (deleted rows included —
    /// this is how tests observe scrub-zeroed bytes).
    pub fn row(&self, key: &VecKey) -> Option<MockRow> {
        let inner = self.inner.lock().unwrap();
        let space = inner.spaces.get(&space_key(&key.space))?;
        space
            .rows
            .iter()
            .find(|r| r.unit == key.unit)
            .map(|r| MockRow {
                vector_id: r.id,
                vector: r.vector.clone(),
                deleted: r.deleted,
            })
    }
}

fn space_key(space: &VecSpace) -> (VecKind, String) {
    (space.vec_kind, space.model_id.clone())
}

fn check_embedding(space: &VecSpace, dims: usize, v: &Embedding) -> VectorStoreResult<()> {
    if v.model_id != space.model_id {
        return Err(VectorStoreError::ModelMismatch {
            expected: space.model_id.clone(),
            got: v.model_id.clone(),
        });
    }
    if v.vector.len() != dims {
        return Err(VectorStoreError::DimensionMismatch {
            space: space.clone(),
            expected: dims,
            got: v.vector.len(),
        });
    }
    Ok(())
}

impl VectorStore for MockVectorStore {
    fn upsert(&self, key: VecKey, v: &Embedding) -> VectorStoreResult<()> {
        let mut inner = self.inner.lock().unwrap();
        let id = inner.next_id;
        let space = inner
            .spaces
            .entry(space_key(&key.space))
            .or_insert_with(|| Space {
                dims: v.vector.len(),
                rows: Vec::new(),
            });
        check_embedding(&key.space, space.dims, v)?;
        if let Some(row) = space.rows.iter_mut().find(|r| r.unit == key.unit) {
            row.vector = v.vector.clone();
            row.deleted = false;
        } else {
            space.rows.push(Row {
                id,
                unit: key.unit,
                vector: v.vector.clone(),
                deleted: false,
            });
            inner.next_id += 1;
        }
        Ok(())
    }

    fn search(
        &self,
        query: &Embedding,
        space: VecSpace,
        k: usize,
    ) -> VectorStoreResult<Vec<VecHit>> {
        let inner = self.inner.lock().unwrap();
        let Some(stored) = inner.spaces.get(&space_key(&space)) else {
            return Ok(Vec::new());
        };
        check_embedding(&space, stored.dims, query)?;
        let mut hits: Vec<VecHit> = stored
            .rows
            .iter()
            .filter(|r| !r.deleted)
            .map(|r| VecHit {
                vector_id: r.id,
                score: dot(&query.vector, &r.vector),
            })
            .collect();
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.vector_id.cmp(&b.vector_id))
        });
        hits.truncate(k);
        Ok(hits)
    }

    fn mark_deleted(&self, key: VecKey) -> VectorStoreResult<()> {
        self.with_row(key, |row| row.deleted = true)
    }

    fn scrub(&self, key: VecKey) -> VectorStoreResult<()> {
        self.with_row(key, |row| {
            row.vector.fill(0.0); // physical zero (redaction)
            row.deleted = true;
        })
    }

    fn compact(&self, space: VecSpace) -> VectorStoreResult<()> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(stored) = inner.spaces.get_mut(&space_key(&space)) {
            stored.rows.retain(|r| !r.deleted);
        }
        Ok(())
    }
}

impl MockVectorStore {
    fn with_row(&self, key: VecKey, f: impl FnOnce(&mut Row)) -> VectorStoreResult<()> {
        let mut inner = self.inner.lock().unwrap();
        let row = inner
            .spaces
            .get_mut(&space_key(&key.space))
            .and_then(|s| s.rows.iter_mut().find(|r| r.unit == key.unit));
        match row {
            Some(row) => {
                f(row);
                Ok(())
            }
            None => Err(VectorStoreError::NotFound(key)),
        }
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
