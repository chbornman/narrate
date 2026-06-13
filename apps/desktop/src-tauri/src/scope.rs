//! Write-scope tracker (CAPTURE §3): the UI reports selection/view changes;
//! the core's scope ring (photoproof_core::capture::scope — P6.1) derives
//! the scope mechanically and the shell echoes it back. The UI performs no
//! scope logic of its own (spec/UI.md §3.4), and the shell duplicates no
//! ring truth: this is a thin clock-holding adapter over the core ring.

use photoproof_core::ContentHash;
use photoproof_core::capture::{
    Clock, ScopeRing, ScopeSnapshot, ScopeSubject, SubjectKind, SystemClock,
};

use crate::dto::ScopeView;

/// Core scope snapshot → the wire `ScopeView` (CAPTURE §11: first ≤ 3
/// preview hashes; DESIGN-VOICE-SUBJECTS.md subject name when targeting a
/// collection/topic note log).
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
        subject: v.subject.map(|k| match k {
            SubjectKind::Collection => "collection",
            SubjectKind::Topic => "topic",
        }),
        subject_name: v.subject_name,
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

    /// DESIGN-VOICE-SUBJECTS.md: report a selection/view change that MAY name
    /// a non-image subject (collection/topic). The echoed scope carries the
    /// subject + its display name so the indicator can render "noting: <name>".
    pub fn set_with_subject(
        &mut self,
        targets: Vec<ContentHash>,
        subject: Option<ScopeSubject>,
        subject_name: Option<String>,
    ) -> ScopeView {
        let (m, w) = (self.clock.mono_ms(), self.clock.wall());
        view_of(self.ring.push_scope(targets, subject, subject_name, m, w))
    }

    pub fn current(&self) -> &ScopeSnapshot {
        self.ring.current()
    }

    pub fn current_view(&self) -> ScopeView {
        view_of(self.ring.current())
    }

    /// Snapshot history, oldest first (debug panel Capture tab).
    #[cfg_attr(not(any(feature = "debug-panel", debug_assertions)), allow(dead_code))]
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

    /// Image-only scope push (the subject-less common case) for the tests.
    fn set(t: &mut ScopeTracker, targets: Vec<ContentHash>) -> ScopeView {
        t.set_with_subject(targets, None, None)
    }

    #[test]
    fn kind_derives_mechanically_from_target_count() {
        let mut t = ScopeTracker::new();
        assert_eq!(t.current_view().kind, "session");
        assert_eq!(set(&mut t, vec![h(1)]).kind, "single");
        assert_eq!(set(&mut t, vec![h(1), h(2), h(3)]).kind, "multi");
        assert_eq!(set(&mut t, vec![]).kind, "session");
    }

    #[test]
    fn multi_target_order_is_selection_order() {
        let mut t = ScopeTracker::new();
        let order = vec![h(9), h(2), h(5)];
        set(&mut t, order.clone());
        assert_eq!(t.current().targets, order);
    }

    #[test]
    fn view_echoes_count_and_first_three_preview_hashes() {
        let mut t = ScopeTracker::new();
        let targets: Vec<ContentHash> = (0..5).map(h).collect();
        let v = set(&mut t, targets.clone());
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
