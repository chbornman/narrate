//! Connector trait boundaries (spec/RUNTIME.md §4).
//!
//! Every external capability — transcription, embedding, language model,
//! vector store — is a trait with swappable implementations. M1 defines the
//! traits; nothing implements them until M2+.
