//! Write-scope tracker (CAPTURE §3): the UI reports selection/view changes;
//! the core's scope ring (photoproof_core::capture::scope — P6.1) derives
//! the scope mechanically and the shell echoes it back. The UI performs no
//! scope logic of its own (spec/UI.md §3.4), and the shell duplicates no
//! ring truth: this is a thin clock-holding adapter over the core ring.

use photoproof_core::ContentHash;
use photoproof_core::capture::{Clock, ScopeRing, ScopeSnapshot, SystemClock};

use crate::dto::ScopeView;

/// Core scope snapshot → the wire `ScopeView` (CAPTURE §11: first ≤ 3
/// preview hashes).
pub fn view_of(s: &ScopeSnapshot) -> ScopeView {
    let v = s.view();
    ScopeView {
        kind: v.kind.as_str(),
        count: v.count,
        preview_hashes: v
            .preview_hashes
            .iter()
            .map(|h| h.as_str().to_owned())
            .collect(),
    }
}

pub struct ScopeTracker {
    clock: SystemClock,
    ring: ScopeRing,
}

impl Default for ScopeTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ScopeTracker {
    pub fn new() -> Self {
        let clock = SystemClock::new();
        let ring = ScopeRing::new(clock.mono_ms(), clock.wall());
        Self { clock, ring }
    }

    /// Report a selection/view change; echoes the derived scope (CAPTURE §3).
    pub fn set(&mut self, targets: Vec<ContentHash>) -> ScopeView {
        let (m, w) = (self.clock.mono_ms(), self.clock.wall());
        view_of(self.ring.push(targets, m, w))
    }

    pub fn current(&self) -> &ScopeSnapshot {
        self.ring.current()
    }

    pub fn current_view(&self) -> ScopeView {
        view_of(self.ring.current())
    }

    /// Snapshot history, oldest first (debug panel Capture tab).
    #[cfg_attr(not(feature = "debug-panel"), allow(dead_code))]
    pub fn history(&self) -> impl Iterator<Item = &ScopeSnapshot> {
        self.ring.history()
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
        assert_eq!(t.current_view().kind, "session");
        assert_eq!(t.set(vec![h(1)]).kind, "single");
        assert_eq!(t.set(vec![h(1), h(2), h(3)]).kind, "multi");
        assert_eq!(t.set(vec![]).kind, "session");
    }

    #[test]
    fn multi_target_order_is_selection_order() {
        let mut t = ScopeTracker::new();
        let order = vec![h(9), h(2), h(5)];
        t.set(order.clone());
        assert_eq!(t.current().targets, order);
    }

    #[test]
    fn view_echoes_count_and_first_three_preview_hashes() {
        let mut t = ScopeTracker::new();
        let targets: Vec<ContentHash> = (0..5).map(h).collect();
        let v = t.set(targets.clone());
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
        let v = t.current_view();
        assert_eq!((v.kind, v.count), ("session", 0));
        assert!(v.preview_hashes.is_empty());
    }
}
