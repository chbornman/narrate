//! Identity primitives: `ContentHash` (BLAKE3-256) and `EventId` (ULID).
//!
//! Contract: spec/EVENTS.md §1.

use std::fmt;

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum IdError {
    #[error("content hash must be 64 lowercase hex characters")]
    InvalidContentHash,
    #[error("event id must be a 26-character Crockford-base32 ULID")]
    InvalidEventId,
}

/// BLAKE3-256 of a file's bytes, stored as 64 lowercase hex characters.
///
/// Uppercase or mixed-case input is rejected, never silently normalized
/// (spec/EVENTS.md §1.1: a silent normalizer hides corrupted producers).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentHash(String);

impl ContentHash {
    pub fn from_hex(s: &str) -> Result<Self, IdError> {
        if s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            Ok(Self(s.to_owned()))
        } else {
            Err(IdError::InvalidContentHash)
        }
    }

    pub fn from_bytes_of(content: &[u8]) -> Self {
        Self(blake3::hash(content).to_hex().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_rejects_uppercase() {
        let upper = "B3A91C0D5E7F20146AA8C3D9E1F5B2640C7D8E9F1A2B3C4D5E6F708192A3B4C5";
        assert_eq!(
            ContentHash::from_hex(upper),
            Err(IdError::InvalidContentHash)
        );
        assert!(ContentHash::from_hex(&upper.to_lowercase()).is_ok());
    }

    #[test]
    fn content_hash_round_trips_bytes() {
        let h = ContentHash::from_bytes_of(b"photoproof");
        assert_eq!(h.as_str().len(), 64);
        assert!(ContentHash::from_hex(h.as_str()).is_ok());
    }
}
