//! The Clock seam (spec/CAPTURE.md §1).
//!
//! All binding, linking, and idle decisions happen on the **capture clock**
//! (host monotonic) and are only *recorded* as wall-clock timestamps. The
//! engine takes this seam everywhere; tests drive a [`FakeClock`] — a test
//! that needs real time to pass is wrong (BUILD-LOOP P6.1).

use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::id::UtcMillis;

/// Capture-clock + wall-clock access. Monotonic time is milliseconds from
/// an arbitrary per-process origin; only differences are meaningful.
pub trait Clock {
    /// Capture clock (monotonic), ms.
    fn mono_ms(&self) -> u64;
    /// Wall clock (UTC) — for *recording* `ts`, never for decisions.
    fn wall(&self) -> UtcMillis;
}

/// Production clock: `std::time::Instant` anchored at construction plus the
/// system wall clock.
#[derive(Debug, Clone)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn mono_ms(&self) -> u64 {
        self.origin.elapsed().as_millis() as u64
    }

    fn wall(&self) -> UtcMillis {
        UtcMillis::now()
    }
}

/// Deterministic test clock. `advance` moves the monotonic and wall clocks
/// together (the §1 norm); `skew_wall` moves only the wall clock (clock
/// regressions never affect binding/idle decisions).
#[derive(Debug, Clone)]
pub struct FakeClock {
    state: Arc<Mutex<FakeState>>,
}

#[derive(Debug)]
struct FakeState {
    mono_ms: u64,
    wall_ms: i64,
}

impl FakeClock {
    /// Starts at monotonic 0 and the given wall epoch ms.
    pub fn new(wall_epoch_ms: i64) -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeState {
                mono_ms: 0,
                wall_ms: wall_epoch_ms,
            })),
        }
    }

    pub fn advance(&self, ms: u64) {
        let mut s = self.state.lock().expect("fake clock mutex");
        s.mono_ms += ms;
        s.wall_ms += ms as i64;
    }

    /// Move ONLY the wall clock (NTP step / regression simulation).
    pub fn skew_wall(&self, delta_ms: i64) {
        self.state.lock().expect("fake clock mutex").wall_ms += delta_ms;
    }
}

impl Clock for FakeClock {
    fn mono_ms(&self) -> u64 {
        self.state.lock().expect("fake clock mutex").mono_ms
    }

    fn wall(&self) -> UtcMillis {
        UtcMillis::from_epoch_ms(self.state.lock().expect("fake clock mutex").wall_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_clock_advances_both_domains_together() {
        let c = FakeClock::new(1_000_000);
        c.advance(250);
        assert_eq!(c.mono_ms(), 250);
        assert_eq!(c.wall().epoch_ms(), 1_000_250);
    }

    #[test]
    fn wall_skew_never_moves_the_capture_clock() {
        let c = FakeClock::new(1_000_000);
        c.advance(100);
        c.skew_wall(-60_000);
        assert_eq!(c.mono_ms(), 100, "monotonic time is immune to wall steps");
        assert_eq!(c.wall().epoch_ms(), 940_100);
    }
}
