//! Write-scope tracker (CAPTURE §3): the UI reports selection/view changes;
//! this derives the scope mechanically and echoes it back. The UI performs
//! no scope logic of its own (spec/UI.md §3.4).
//!
//! M1 carries no voice pipeline, but the snapshot history ring exists already
//! (CAPTURE §3.1: 1024 entries) so the debug panel's Capture tab can show
//! every snapshot, and so the P6.1 capture engine plugs into a shape it
//! recognizes.

use std::collections::VecDeque;

use photoproof_core::{ContentHash, UtcMillis};

use crate::dto::ScopeView;

/// CAPTURE §3.1 ring capacity (the 120 s clause needs the capture clock,
/// which arrives with P6.1; the entry cap is enforced here).
pub const RING_CAPACITY: usize = 1024;

/// Preview hashes carried on the echoed scope (CAPTURE §11: first ≤ 3).
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
}

#[derive(Debug, Clone)]
pub struct ScopeSnapshot {
    pub kind: ScopeKind,
    /// Ordered: multi target order = selection order (CAPTURE §3).
    pub targets: Vec<ContentHash>,
    /// Diagnostics only (debug panel Capture tab; CAPTURE §3.1).
    #[cfg_attr(not(feature = "debug-panel"), allow(dead_code))]
    pub captured_at: UtcMillis,
}

impl ScopeSnapshot {
    pub fn view(&self) -> ScopeView {
        ScopeView {
            kind: self.kind.as_str(),
            count: self.targets.len(),
            preview_hashes: self
                .targets
                .iter()
                .take(PREVIEW_HASHES)
                .map(|h| h.as_str().to_owned())
                .collect(),
        }
    }
}

pub struct ScopeTracker {
    current: ScopeSnapshot,
    history: VecDeque<ScopeSnapshot>,
}

impl Default for ScopeTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ScopeTracker {
    pub fn new() -> Self {
        let initial = ScopeSnapshot {
            kind: ScopeKind::Session,
            targets: Vec::new(),
            captured_at: UtcMillis::now(),
        };
        let mut history = VecDeque::with_capacity(RING_CAPACITY);
        history.push_back(initial.clone());
        Self {
            current: initial,
            history,
        }
    }

    /// Scope derives mechanically from the reported target list (CAPTURE §3):
    /// 0 targets → session, 1 → single, N ≥ 2 → multi (selection order kept).
    pub fn set(&mut self, targets: Vec<ContentHash>, now: UtcMillis) -> &ScopeSnapshot {
        let kind = match targets.len() {
            0 => ScopeKind::Session,
            1 => ScopeKind::Single,
            _ => ScopeKind::Multi,
        };
        let snap = ScopeSnapshot {
            kind,
            targets,
            captured_at: now,
        };
        if self.history.len() == RING_CAPACITY {
            self.history.pop_front();
        }
        self.history.push_back(snap.clone());
        self.current = snap;
        &self.current
    }

    pub fn current(&self) -> &ScopeSnapshot {
        &self.current
    }

    /// Snapshot history, oldest first (debug panel Capture tab).
    #[cfg_attr(not(feature = "debug-panel"), allow(dead_code))]
    pub fn history(&self) -> impl Iterator<Item = &ScopeSnapshot> {
        self.history.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u8) -> ContentHash {
        ContentHash::from_bytes_of(&[n])
    }

    #[test]
    fn kind_derives_mechanically_from_target_count() {
        let mut t = ScopeTracker::new();
        assert_eq!(t.current().kind, ScopeKind::Session);
        t.set(vec![h(1)], UtcMillis::from_epoch_ms(1));
        assert_eq!(t.current().kind, ScopeKind::Single);
        t.set(vec![h(1), h(2), h(3)], UtcMillis::from_epoch_ms(2));
        assert_eq!(t.current().kind, ScopeKind::Multi);
        t.set(vec![], UtcMillis::from_epoch_ms(3));
        assert_eq!(t.current().kind, ScopeKind::Session);
    }

    #[test]
    fn multi_target_order_is_selection_order() {
        let mut t = ScopeTracker::new();
        let order = vec![h(9), h(2), h(5)];
        t.set(order.clone(), UtcMillis::from_epoch_ms(1));
        assert_eq!(t.current().targets, order);
    }

    #[test]
    fn view_echoes_count_and_first_three_preview_hashes() {
        let mut t = ScopeTracker::new();
        let targets: Vec<ContentHash> = (0..5).map(h).collect();
        t.set(targets.clone(), UtcMillis::from_epoch_ms(1));
        let v = t.current().view();
        assert_eq!(v.kind, "multi");
        assert_eq!(v.count, 5);
        assert_eq!(
            v.preview_hashes,
            targets
                .iter()
                .take(3)
                .map(|x| x.as_str().to_owned())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn session_scope_view_renders_zero_targets() {
        let t = ScopeTracker::new();
        let v = t.current().view();
        assert_eq!((v.kind, v.count), ("session", 0));
        assert!(v.preview_hashes.is_empty());
    }

    #[test]
    fn history_ring_caps_at_capacity() {
        let mut t = ScopeTracker::new();
        for i in 0..(RING_CAPACITY + 10) {
            t.set(vec![h((i % 250) as u8)], UtcMillis::from_epoch_ms(i as i64));
        }
        assert_eq!(t.history().count(), RING_CAPACITY);
        // Newest snapshot is the current one.
        let last = t.history().last().unwrap();
        assert_eq!(last.captured_at, t.current().captured_at);
    }
}
