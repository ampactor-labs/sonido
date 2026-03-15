//! Undo/redo history for parameter changes.
//!
//! [`UndoHistory`] tracks parameter mutations as a linear buffer with a
//! cursor. Mutations during an active gesture are grouped — `begin_gesture`
//! opens a group, `end_gesture` closes it. Undo/redo operate at the group
//! boundary: a single Ctrl+Z undoes the entire drag rather than each
//! individual sample.
//!
//! # Invariants
//!
//! - `position` ≤ `buffer.len()`.
//! - Pushing while `position < buffer.len()` truncates the redo tail.
//! - Maximum history depth is [`MAX_HISTORY`] entries. The oldest entry is
//!   dropped when the limit is reached.
//! - `begin_gesture` / `end_gesture` must be balanced. Nested calls are a
//!   no-op (only one group can be open at a time).

/// Maximum number of history entries (each entry is a [`Mutation`]).
pub const MAX_HISTORY: usize = 100;

/// A single parameter mutation — records the slot, parameter index, and the
/// old and new values so it can be reversed or reapplied.
#[derive(Clone, Debug, PartialEq)]
pub struct Mutation {
    /// Effect slot index in the graph engine.
    pub slot: usize,
    /// Parameter index within the effect.
    pub param: usize,
    /// Parameter value before the mutation.
    pub old: f32,
    /// Parameter value after the mutation.
    pub new: f32,
}

impl Mutation {
    /// Create a new mutation record.
    pub fn new(slot: usize, param: usize, old: f32, new: f32) -> Self {
        Self {
            slot,
            param,
            old,
            new,
        }
    }
}

/// Linear undo/redo history with gesture grouping.
///
/// # Invariants
/// - `position` is always ≤ `buffer.len()`.
/// - All entries with index < `position` are "past" (can be undone).
/// - All entries with index ≥ `position` are "future" (can be redone).
#[derive(Clone, Debug, Default)]
pub struct UndoHistory {
    /// Stored mutation groups. Each entry is a non-empty list of mutations
    /// that form a single logical operation (e.g., a parameter drag).
    buffer: Vec<Vec<Mutation>>,
    /// Current position in history (points one past the last applied group).
    position: usize,
    /// Accumulator for in-progress gesture mutations.
    gesture_buf: Vec<Mutation>,
    /// Whether a gesture is currently open.
    gesture_active: bool,
}

impl UndoHistory {
    /// Create a new empty undo history.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a single mutation into the history.
    ///
    /// If a gesture is active, the mutation is accumulated into the current
    /// group and not yet committed to the buffer.
    ///
    /// If no gesture is active, the mutation is committed immediately as a
    /// single-mutation group. Pushing truncates the redo tail.
    pub fn push(&mut self, mutation: Mutation) {
        if self.gesture_active {
            self.gesture_buf.push(mutation);
        } else {
            self.commit(vec![mutation]);
        }
    }

    /// Open a gesture group.
    ///
    /// All subsequent [`push`](Self::push) calls accumulate into this group
    /// until [`end_gesture`](Self::end_gesture) is called. Nested calls are a
    /// no-op.
    pub fn begin_gesture(&mut self) {
        if !self.gesture_active {
            self.gesture_active = true;
            self.gesture_buf.clear();
        }
    }

    /// Close a gesture group and commit its accumulated mutations.
    ///
    /// If the gesture buffer is empty (e.g., the user clicked without
    /// dragging), nothing is committed. Calling `end_gesture` without a
    /// matching `begin_gesture` is a no-op.
    pub fn end_gesture(&mut self) {
        if self.gesture_active {
            self.gesture_active = false;
            let group = core::mem::take(&mut self.gesture_buf);
            if !group.is_empty() {
                self.commit(group);
            }
        }
    }

    /// Returns `true` if a gesture group is currently open.
    pub fn gesture_active(&self) -> bool {
        self.gesture_active
    }

    /// Undo the most recently committed group.
    ///
    /// Returns `Some(group)` with the mutations that need to be reversed
    /// (i.e., apply `old` values). Returns `None` if nothing to undo.
    pub fn undo(&mut self) -> Option<Vec<Mutation>> {
        if self.position == 0 {
            return None;
        }
        self.position -= 1;
        Some(self.buffer[self.position].clone())
    }

    /// Redo the next committed group.
    ///
    /// Returns `Some(group)` with the mutations that need to be reapplied
    /// (i.e., apply `new` values). Returns `None` if nothing to redo.
    pub fn redo(&mut self) -> Option<Vec<Mutation>> {
        if self.position >= self.buffer.len() {
            return None;
        }
        let group = self.buffer[self.position].clone();
        self.position += 1;
        Some(group)
    }

    /// Returns `true` if there are operations that can be undone.
    pub fn can_undo(&self) -> bool {
        self.position > 0
    }

    /// Returns `true` if there are operations that can be redone.
    pub fn can_redo(&self) -> bool {
        self.position < self.buffer.len()
    }

    /// Clear all history.
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.position = 0;
        self.gesture_active = false;
        self.gesture_buf.clear();
    }

    /// Number of committed history entries.
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Returns `true` if there are no committed history entries.
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Commit a group to the buffer.
    ///
    /// Truncates the redo tail and enforces the max history limit.
    fn commit(&mut self, group: Vec<Mutation>) {
        // Truncate redo tail.
        self.buffer.truncate(self.position);

        // Enforce max history.
        if self.buffer.len() >= MAX_HISTORY {
            self.buffer.remove(0);
            if self.position > 0 {
                self.position -= 1;
            }
        }

        self.buffer.push(group);
        self.position = self.buffer.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(slot: usize, param: usize, old: f32, new: f32) -> Mutation {
        Mutation::new(slot, param, old, new)
    }

    #[test]
    fn initial_state() {
        let h = UndoHistory::new();
        assert!(!h.can_undo());
        assert!(!h.can_redo());
        assert!(h.is_empty());
    }

    #[test]
    fn push_single() {
        let mut h = UndoHistory::new();
        h.push(m(0, 0, 0.0, 1.0));
        assert!(h.can_undo());
        assert!(!h.can_redo());
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn undo_returns_mutation() {
        let mut h = UndoHistory::new();
        h.push(m(0, 0, 0.0, 1.0));
        let group = h.undo().unwrap();
        assert_eq!(group.len(), 1);
        assert_eq!(group[0].old, 0.0);
        assert_eq!(group[0].new, 1.0);
        assert!(!h.can_undo());
        assert!(h.can_redo());
    }

    #[test]
    fn redo_reapplies() {
        let mut h = UndoHistory::new();
        h.push(m(0, 0, 0.0, 1.0));
        h.undo();
        let group = h.redo().unwrap();
        assert_eq!(group[0].new, 1.0);
        assert!(h.can_undo());
        assert!(!h.can_redo());
    }

    #[test]
    fn push_truncates_redo_tail() {
        let mut h = UndoHistory::new();
        h.push(m(0, 0, 0.0, 1.0));
        h.push(m(0, 0, 1.0, 2.0));
        h.undo(); // undo second
        h.push(m(0, 0, 1.0, 3.0)); // new branch — should truncate
        assert_eq!(h.len(), 2);
        assert!(!h.can_redo());
    }

    #[test]
    fn gesture_groups_mutations() {
        let mut h = UndoHistory::new();
        h.begin_gesture();
        h.push(m(0, 0, 0.0, 0.5));
        h.push(m(0, 0, 0.5, 1.0));
        h.end_gesture();

        assert_eq!(h.len(), 1);
        let group = h.undo().unwrap();
        assert_eq!(group.len(), 2);
    }

    #[test]
    fn gesture_empty_does_not_commit() {
        let mut h = UndoHistory::new();
        h.begin_gesture();
        h.end_gesture();
        assert!(h.is_empty());
    }

    #[test]
    fn nested_begin_gesture_is_noop() {
        let mut h = UndoHistory::new();
        h.begin_gesture();
        h.push(m(0, 0, 0.0, 1.0));
        h.begin_gesture(); // nested — no-op
        h.push(m(0, 0, 1.0, 2.0)); // still in first gesture
        h.end_gesture();
        assert_eq!(h.len(), 1);
        let group = h.undo().unwrap();
        assert_eq!(group.len(), 2);
    }

    #[test]
    fn max_history_enforced() {
        let mut h = UndoHistory::new();
        for i in 0..MAX_HISTORY + 5 {
            h.push(m(0, 0, i as f32, (i + 1) as f32));
        }
        assert!(h.len() <= MAX_HISTORY);
    }

    #[test]
    fn clear_resets_all() {
        let mut h = UndoHistory::new();
        h.push(m(0, 0, 0.0, 1.0));
        h.clear();
        assert!(h.is_empty());
        assert!(!h.can_undo());
        assert!(!h.can_redo());
    }

    #[test]
    fn undo_empty_returns_none() {
        let mut h = UndoHistory::new();
        assert!(h.undo().is_none());
    }

    #[test]
    fn redo_empty_returns_none() {
        let mut h = UndoHistory::new();
        assert!(h.redo().is_none());
    }
}
