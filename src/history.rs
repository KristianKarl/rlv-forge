pub struct History<T> {
    undo: Vec<T>,
    redo: Vec<T>,
    group: Option<u64>,
}

impl<T> Default for History<T> {
    fn default() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            group: None,
        }
    }
}

impl<T: PartialEq> History<T> {
    pub fn end_group(&mut self) {
        self.group = None;
    }

    pub fn record(&mut self, before: T, after: &T, group: Option<u64>) {
        if &before == after {
            if group.is_none() {
                self.group = None;
            }
            return;
        }
        if group.is_none() || self.group != group {
            self.undo.push(before);
        }
        self.group = group;
        self.redo.clear();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self, current: T) -> Option<T> {
        let previous = self.undo.pop()?;
        self.redo.push(current);
        self.group = None;
        Some(previous)
    }

    pub fn redo(&mut self, current: T) -> Option<T> {
        let next = self.redo.pop()?;
        self.undo.push(current);
        self.group = None;
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_round_trip_and_new_edits_discard_redo() {
        let mut history = History::<i32>::default();
        history.record(0, &1, None);
        history.record(1, &2, None);
        assert_eq!(history.undo(2), Some(1));
        assert_eq!(history.undo(1), Some(0));
        assert_eq!(history.redo(0), Some(1));
        history.record(1, &3, None);
        assert!(!history.can_redo());
        assert_eq!(history.undo(3), Some(1));
    }

    #[test]
    fn drag_and_text_gestures_are_single_actions() {
        let mut history = History::<i32>::default();
        history.record(0, &1, Some(7));
        history.record(1, &2, Some(7));
        history.record(2, &3, Some(7));
        assert_eq!(history.undo(3), Some(0));
        assert_eq!(history.redo(0), Some(3));
        history.record(3, &3, None);
        history.record(3, &4, Some(7));
        assert_eq!(history.undo(4), Some(3));
    }
}
