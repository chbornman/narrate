//! Write-scope snapshots and the scope ring buffer (spec/CAPTURE.md §3).
//!
//! Scope derives mechanically from the reported target list; every change
//! pushes a snapshot into a ring of **1024 entries or 120 s**, whichever
//! fills first. `scope_at(t_mono)` answers the binding rule's lookup (§5):
//! the snapshot with the greatest `captured_at_mono ≤ t_mono`.

use std::collections::VecDeque;

use crate::id::{ContentHash, UtcMillis};

/// CAPTURE §3.1 ring capacity.
pub const RING_CAPACITY: usize = 1024;
/// CAPTURE §3.1 ring window, ms.
pub const RING_WINDOW_MS: u64 = 120_000;
/// Indicator preview hashes: first ≤ 3 (CAPTURE §11).
pub const PREVIEW_HASHES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    Single,
    Multi,
    Session,
}

impl ScopeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ScopeKind::Single => "single",
            ScopeKind::Multi => "multi",
            ScopeKind::Session => "session",
        }
    }

    /// Scope derives mechanically from the target count (§3): 0 → session,
    /// 1 → single, N ≥ 2 → multi. Shared by the ring push and by the
    /// in-flight target union (engine `set_scope`) so a snapshot whose
    /// targets grew across an image swap reports the right kind.
    pub fn from_target_count(n: usize) -> Self {
        match n {
            0 => ScopeKind::Session,
            1 => ScopeKind::Single,
            _ => ScopeKind::Multi,
        }
    }
}

/// CAPTURE §3.1 `ScopeSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeSnapshot {
    pub kind: ScopeKind,
    /// Ordered: multi target order = selection order (`event_targets.position`).
    pub targets: Vec<ContentHash>,
    /// Capture clock (decisions happen here).
    pub captured_at_mono: u64,
    /// Diagnostics only (§3.1).
    pub captured_at: UtcMillis,
}

impl ScopeSnapshot {
    pub fn view(&self) -> ScopeView {
        ScopeView {
            kind: self.kind,
            count: self.targets.len(),
            preview_hashes: self.targets.iter().take(PREVIEW_HASHES).cloned().collect(),
        }
    }
}

/// CAPTURE §11 `ScopeView` — what the indicator renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeView {
    pub kind: ScopeKind,
    pub count: usize,
    pub preview_hashes: Vec<ContentHash>,
}

/// Result of a `scope_at` lookup. `predated` = the asked-for instant fell
/// before the oldest retained snapshot (should be impossible; the caller
/// logs a debug-panel warning — §3.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeLookup {
    pub snapshot: ScopeSnapshot,
    pub predated: bool,
}

/// The §3.1 ring. In-memory, per-process, never persisted. The current
/// snapshot is always retained, whatever its age.
#[derive(Debug)]
pub struct ScopeRing {
    ring: VecDeque<ScopeSnapshot>,
}

impl ScopeRing {
    /// Starts with a session-scope snapshot at the given instant (the scope
    /// is *always* exactly one of single/multi/session — §3).
    pub fn new(now_mono: u64, now_wall: UtcMillis) -> Self {
        let mut ring = VecDeque::with_capacity(RING_CAPACITY);
        ring.push_back(ScopeSnapshot {
            kind: ScopeKind::Session,
            targets: Vec::new(),
            captured_at_mono: now_mono,
            captured_at: now_wall,
        });
        Self { ring }
    }

    /// Scope derives mechanically (§3): 0 targets → session, 1 → single,
    /// N ≥ 2 → multi (selection order kept). Returns the pushed snapshot.
    pub fn push(
        &mut self,
        targets: Vec<ContentHash>,
        now_mono: u64,
        now_wall: UtcMillis,
    ) -> &ScopeSnapshot {
        let kind = ScopeKind::from_target_count(targets.len());
        if self.ring.len() == RING_CAPACITY {
            self.ring.pop_front();
        }
        self.ring.push_back(ScopeSnapshot {
            kind,
            targets,
            captured_at_mono: now_mono,
            captured_at: now_wall,
        });
        // 120 s window: evict entries older than the window, but always
        // keep at least the newest (the current scope must stay answerable).
        while self.ring.len() > 1
            && self
                .ring
                .front()
                .is_some_and(|s| now_mono.saturating_sub(s.captured_at_mono) > RING_WINDOW_MS)
        {
            self.ring.pop_front();
        }
        self.ring.back().expect("just pushed")
    }

    /// §3.1 lookup: the snapshot with the greatest `captured_at_mono ≤
    /// t_mono`; if `t_mono` predates the oldest entry, the oldest is used
    /// and `predated` is set (caller logs the debug warning).
    pub fn scope_at(&self, t_mono: u64) -> ScopeLookup {
        let mut best: Option<&ScopeSnapshot> = None;
        for s in &self.ring {
            if s.captured_at_mono <= t_mono {
                best = Some(s);
            } else {
                break; // captured_at_mono is non-decreasing in push order
            }
        }
        match best {
            Some(s) => ScopeLookup {
                snapshot: s.clone(),
                predated: false,
            },
            None => ScopeLookup {
                snapshot: self.ring.front().expect("ring never empty").clone(),
                predated: true,
            },
        }
    }

    pub fn current(&self) -> &ScopeSnapshot {
        self.ring.back().expect("ring never empty")
    }

    /// Snapshot history, oldest first (debug panel Capture tab).
    pub fn history(&self) -> impl Iterator<Item = &ScopeSnapshot> {
        self.ring.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u8) -> ContentHash {
        ContentHash::from_bytes_of(&[n])
    }

    fn wall(ms: i64) -> UtcMillis {
        UtcMillis::from_epoch_ms(ms)
    }

    #[test]
    fn kind_derives_mechanically_and_order_is_selection_order() {
        let mut r = ScopeRing::new(0, wall(0));
        assert_eq!(r.current().kind, ScopeKind::Session);
        r.push(vec![h(1)], 10, wall(10));
        assert_eq!(r.current().kind, ScopeKind::Single);
        let order = vec![h(9), h(2), h(5)];
        r.push(order.clone(), 20, wall(20));
        assert_eq!(r.current().kind, ScopeKind::Multi);
        assert_eq!(r.current().targets, order);
    }

    #[test]
    fn scope_at_returns_greatest_snapshot_at_or_before_t() {
        let mut r = ScopeRing::new(0, wall(0));
        r.push(vec![h(1)], 100, wall(100)); // A at 100
        r.push(vec![h(2)], 900, wall(900)); // B at 900
        assert_eq!(r.scope_at(99).snapshot.kind, ScopeKind::Session);
        assert_eq!(r.scope_at(100).snapshot.targets, vec![h(1)]);
        assert_eq!(r.scope_at(899).snapshot.targets, vec![h(1)]);
        assert_eq!(r.scope_at(900).snapshot.targets, vec![h(2)]);
        assert_eq!(r.scope_at(5000).snapshot.targets, vec![h(2)]);
    }

    #[test]
    fn predating_the_oldest_entry_flags_and_answers_the_oldest() {
        let mut r = ScopeRing::new(500, wall(500));
        r.push(vec![h(1)], 600, wall(600));
        let look = r.scope_at(10);
        assert!(look.predated);
        assert_eq!(look.snapshot.kind, ScopeKind::Session);
    }

    #[test]
    fn ring_caps_at_1024_entries() {
        let mut r = ScopeRing::new(0, wall(0));
        for i in 0..(RING_CAPACITY as u64 + 10) {
            r.push(vec![h((i % 250) as u8)], i, wall(i as i64));
        }
        assert_eq!(r.history().count(), RING_CAPACITY);
    }

    #[test]
    fn ring_evicts_past_the_120_s_window_but_keeps_the_newest() {
        let mut r = ScopeRing::new(0, wall(0));
        r.push(vec![h(1)], 1_000, wall(1_000));
        // A push far in the future evicts everything older than 120 s …
        r.push(vec![h(2)], 500_000, wall(500_000));
        assert_eq!(r.history().count(), 1);
        assert_eq!(r.current().targets, vec![h(2)]);
        // … and a lookup before it answers (flagged) from the oldest kept.
        let look = r.scope_at(0);
        assert!(look.predated);
        assert_eq!(look.snapshot.targets, vec![h(2)]);
    }
}
