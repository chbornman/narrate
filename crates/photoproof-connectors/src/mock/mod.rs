//! Deterministic mock connectors for downstream work packets
//! (docs/BUILD-LOOP.md: capture state machines test against a scripted
//! mock Transcriber, retrieval against deterministic mock embeddings,
//! supervision against mocks).
//!
//! Design rules, shared by every mock here:
//! - **Scriptable**: behavior is driven by explicit scripts/queues set up
//!   by the test, never by heuristics the test cannot predict.
//! - **Deterministic**: same inputs → byte-identical outputs, across runs
//!   and platforms (hashing is FNV-1a + SplitMix64, not `DefaultHasher`).
//! - **No wall-clock dependence**: all timing is the stream clock implied
//!   by consumed audio; nothing sleeps, nothing reads `Instant::now()`.

mod audio;
mod embedder;
mod llm;
mod reranker;
mod transcriber;
mod vad;
mod vector_store;

pub use audio::{AudioFeed, collect, frames_stream, silent_frame};
pub use embedder::MockEmbedder;
pub use llm::{CaptionCall, MockLanguageModel};
pub use reranker::MockReranker;
pub use transcriber::{MockTranscriber, ScriptEntry, ScriptedEvent};
pub use vad::{MockVad, SpeechSpan};
pub use vector_store::{MockRow, MockVectorStore};

/// FNV-1a 64-bit — stable across runs, platforms, and Rust versions
/// (unlike `std::hash::DefaultHasher`, which guarantees neither).
pub(crate) fn fnv1a64(seed: u64, bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64 ^ seed;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// SplitMix64 PRNG step — deterministic vector material from a hash seed.
pub(crate) fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
