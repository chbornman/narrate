//! The capture audio ring buffer (spec/CAPTURE.md §7).
//!
//! No audio is ever written to disk in v1: capture audio lives ONLY here —
//! an in-memory ring, **60 s at 16 kHz mono f32** (~3.8 MB). A segment's
//! audio is discard-eligible at finalization + 5 s ("discard" = overwrite
//! eligibility, not a deletion job); on disarm, quit, or fatal ASR error
//! the whole buffer zeroes immediately.

/// 16 kHz mono f32 (CAPTURE §6.2 — capture resamples in-app).
pub const SAMPLE_RATE: u32 = 16_000;
/// 60 s ring (CAPTURE §7).
pub const RING_SECONDS: usize = 60;
/// Finalization + 5 s safety window before a segment's audio is
/// overwrite-eligible (CAPTURE §7).
pub const DISCARD_ELIGIBLE_AFTER_MS: u64 = 5_000;

/// The §7 ring. Holds samples only; never serialized, never persisted.
pub struct AudioRing {
    samples: Vec<f32>,
    write_pos: usize,
    /// Total samples ever written (occupancy = min(total, capacity)).
    total_written: u64,
    /// Newest `finalization + 5 s` eligibility instants, capture clock.
    eligibility: Vec<u64>,
}

impl AudioRing {
    pub fn new() -> Self {
        Self {
            samples: vec![0.0; RING_SECONDS * SAMPLE_RATE as usize],
            write_pos: 0,
            total_written: 0,
            eligibility: Vec::new(),
        }
    }

    pub fn capacity(&self) -> usize {
        self.samples.len()
    }

    pub fn occupied(&self) -> usize {
        self.total_written.min(self.capacity() as u64) as usize
    }

    /// Append samples, overwriting the oldest when full (ring semantics —
    /// the 60 s window IS the retention policy).
    pub fn write(&mut self, samples: &[f32]) {
        for &s in samples {
            self.samples[self.write_pos] = s;
            self.write_pos = (self.write_pos + 1) % self.samples.len();
        }
        self.total_written += samples.len() as u64;
    }

    /// A segment finalized at `now_mono`: its audio becomes overwrite-
    /// eligible at `now_mono + 5 s` (§7's immediate-retry safety window).
    pub fn note_finalized(&mut self, now_mono: u64) {
        self.eligibility.push(now_mono + DISCARD_ELIGIBLE_AFTER_MS);
    }

    /// Whether every finalized segment's safety window has elapsed.
    pub fn all_discard_eligible(&self, now_mono: u64) -> bool {
        self.eligibility.iter().all(|&at| now_mono >= at)
    }

    /// §7: zero the WHOLE buffer immediately (disarm, quit, fatal error).
    pub fn zero(&mut self) {
        self.samples.fill(0.0);
        self.write_pos = 0;
        self.total_written = 0;
        self.eligibility.clear();
    }

    /// Test hook: no non-zero sample anywhere in the buffer.
    pub fn is_zeroed(&self) -> bool {
        self.total_written == 0 && self.samples.iter().all(|&s| s == 0.0)
    }
}

impl Default for AudioRing {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_holds_exactly_60_seconds_and_wraps() {
        let mut r = AudioRing::new();
        assert_eq!(r.capacity(), 960_000);
        r.write(&vec![0.5; 960_000]);
        assert_eq!(r.occupied(), 960_000);
        r.write(&[0.25; 16_000]); // one more second wraps
        assert_eq!(r.occupied(), 960_000, "occupancy is capped at 60 s");
        assert_eq!(r.samples[0], 0.25, "oldest second was overwritten");
    }

    #[test]
    fn zero_clears_every_sample_immediately() {
        let mut r = AudioRing::new();
        r.write(&[0.7; 48_000]);
        assert!(!r.is_zeroed());
        r.zero();
        assert!(r.is_zeroed());
    }

    #[test]
    fn discard_eligibility_is_finalization_plus_5_s() {
        let mut r = AudioRing::new();
        r.note_finalized(10_000);
        assert!(!r.all_discard_eligible(14_999));
        assert!(r.all_discard_eligible(15_000));
    }
}
