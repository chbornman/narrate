//! Deterministic mock `Embedder`.
//!
//! Default behavior: a vector derived purely from (model_id, kind, input
//! bytes) via FNV-1a + SplitMix64 — same input, same vector, on every run
//! and platform; different inputs are (near-orthogonal) different vectors.
//! For retrieval tests that need *controlled* similarity, exact vectors
//! can be pinned per input with `set_text_embedding` /
//! `set_image_embedding`. All outputs are L2-normalized (RUNTIME §4).

use std::collections::HashMap;
use std::sync::Mutex;

use crate::embedder::{DecodedImage, Embedder, Embedding};
use crate::error::{ConnectorError, ConnectorResult};
use crate::mock::{fnv1a64, splitmix64};

pub struct MockEmbedder {
    model_id: String,
    dims: usize,
    /// X3 split: the TEXT instance does not support `embed_image`.
    supports_images: bool,
    text_overrides: Mutex<HashMap<String, Vec<f32>>>,
    image_overrides: Mutex<HashMap<u64, Vec<f32>>>,
}

impl MockEmbedder {
    /// A text-embedder instance (annotation chunks & summaries):
    /// `embed_image` is unsupported, mirroring RUNTIME §4's contract note.
    pub fn text(model_id: impl Into<String>, dims: usize) -> Self {
        Self::build(model_id, dims, false)
    }

    /// A CLIP-embedder instance: both towers supported.
    pub fn clip(model_id: impl Into<String>, dims: usize) -> Self {
        Self::build(model_id, dims, true)
    }

    fn build(model_id: impl Into<String>, dims: usize, supports_images: bool) -> Self {
        assert!(dims > 0, "MockEmbedder: dims must be > 0");
        Self {
            model_id: model_id.into(),
            dims,
            supports_images,
            text_overrides: Mutex::new(HashMap::new()),
            image_overrides: Mutex::new(HashMap::new()),
        }
    }

    /// Pin the exact vector returned for `text` (L2-normalized on insert).
    /// Panics (test bug) on a dimension mismatch.
    pub fn set_text_embedding(&self, text: impl Into<String>, vector: Vec<f32>) {
        let vector = self.normalized_override(vector);
        self.text_overrides
            .lock()
            .unwrap()
            .insert(text.into(), vector);
    }

    /// Pin the exact vector returned for an image with these pixels.
    pub fn set_image_embedding(&self, img: &DecodedImage, vector: Vec<f32>) {
        let vector = self.normalized_override(vector);
        self.image_overrides
            .lock()
            .unwrap()
            .insert(image_key(img), vector);
    }

    fn normalized_override(&self, mut vector: Vec<f32>) -> Vec<f32> {
        assert_eq!(
            vector.len(),
            self.dims,
            "MockEmbedder: override vector dims mismatch"
        );
        l2_normalize(&mut vector);
        vector
    }

    fn embedding(&self, vector: Vec<f32>) -> Embedding {
        Embedding {
            vector,
            model_id: self.model_id.clone(),
        }
    }

    fn derive(&self, kind: &str, bytes: &[u8]) -> Vec<f32> {
        let seed = fnv1a64(fnv1a64(0, self.model_id.as_bytes()), kind.as_bytes());
        let mut state = fnv1a64(seed, bytes);
        let mut vector: Vec<f32> = (0..self.dims)
            .map(|_| {
                // Uniform in [-1, 1) from the top 53 bits.
                let r = splitmix64(&mut state) >> 11;
                (r as f64 / (1u64 << 52) as f64 - 1.0) as f32
            })
            .collect();
        l2_normalize(&mut vector);
        vector
    }
}

impl Embedder for MockEmbedder {
    async fn embed_text(&self, text: &str) -> ConnectorResult<Embedding> {
        if let Some(vector) = self.text_overrides.lock().unwrap().get(text) {
            return Ok(self.embedding(vector.clone()));
        }
        Ok(self.embedding(self.derive("text", text.as_bytes())))
    }

    async fn embed_image(&self, img: &DecodedImage) -> ConnectorResult<Embedding> {
        if !self.supports_images {
            // The text embedder has no image tower (RUNTIME §4, X3).
            // ConnectorError has no Unsupported variant; 501 Not
            // Implemented is the closest honest mapping (flagged).
            return Err(ConnectorError::Backend {
                status: 501,
                message: format!("embed_image unsupported by text embedder {}", self.model_id),
            });
        }
        if let Some(vector) = self.image_overrides.lock().unwrap().get(&image_key(img)) {
            return Ok(self.embedding(vector.clone()));
        }
        let mut bytes = Vec::with_capacity(img.rgb8.len() + 8);
        bytes.extend_from_slice(&img.width.to_le_bytes());
        bytes.extend_from_slice(&img.height.to_le_bytes());
        bytes.extend_from_slice(&img.rgb8);
        Ok(self.embedding(self.derive("image", &bytes)))
    }

    fn dimensions(&self) -> usize {
        self.dims
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }
}

fn image_key(img: &DecodedImage) -> u64 {
    let mut key = fnv1a64(0, &img.width.to_le_bytes());
    key = fnv1a64(key, &img.height.to_le_bytes());
    fnv1a64(key, &img.rgb8)
}

fn l2_normalize(vector: &mut [f32]) {
    let norm = vector
        .iter()
        .map(|x| f64::from(*x) * f64::from(*x))
        .sum::<f64>()
        .sqrt();
    if norm > 0.0 {
        for x in vector.iter_mut() {
            *x = (f64::from(*x) / norm) as f32;
        }
    }
}
