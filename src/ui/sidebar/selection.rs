//! The registered files picked together in the sidebar, so one remove takes
//! them all: a click picks one file, Shift+click the run from the last
//! picked to here, Cmd+click (Ctrl elsewhere) adds or drops one.

/// File ids, in the order they were picked, and the file a Shift+click
/// measures its run from.
#[derive(Default)]
pub(super) struct FileSelection {
    picked: Vec<i64>,
    anchor: Option<i64>,
}

impl FileSelection {
    /// A click on file `id`; `order` is every file's id as the list shows them.
    pub(super) fn click(&mut self, id: i64, order: &[i64], shift: bool, toggle: bool) {
        let anchor = self.anchor.filter(|anchor| order.contains(anchor));
        match (shift, anchor) {
            (true, Some(anchor)) => {
                let (Some(from), Some(to)) = (
                    order.iter().position(|&x| x == anchor),
                    order.iter().position(|&x| x == id),
                ) else {
                    return;
                };
                let run = &order[from.min(to)..=from.max(to)];
                if toggle {
                    for &x in run {
                        if !self.picked.contains(&x) {
                            self.picked.push(x);
                        }
                    }
                } else {
                    self.picked = run.to_vec();
                }
            }
            _ if toggle => {
                if let Some(at) = self.picked.iter().position(|&x| x == id) {
                    self.picked.remove(at);
                } else {
                    self.picked.push(id);
                }
                self.anchor = Some(id);
            }
            _ => {
                self.picked = vec![id];
                self.anchor = Some(id);
            }
        }
    }

    pub(super) fn contains(&self, id: i64) -> bool {
        self.picked.contains(&id)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.picked.is_empty()
    }

    pub(super) fn clear(&mut self) {
        self.picked.clear();
        self.anchor = None;
    }

    /// Forget files the list no longer shows.
    pub(super) fn retain(&mut self, order: &[i64]) {
        self.picked.retain(|id| order.contains(id));
        self.anchor = self.anchor.filter(|anchor| order.contains(anchor));
    }

    /// The files an action on file `id` acts on, in list order: every picked
    /// file when `id` is one of them, else `id` alone.
    pub(super) fn targets(&self, id: i64, order: &[i64]) -> Vec<i64> {
        if !self.contains(id) {
            return vec![id];
        }
        order
            .iter()
            .copied()
            .filter(|x| self.contains(*x))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::FileSelection;

    const ORDER: [i64; 5] = [10, 11, 12, 13, 14];

    #[test]
    fn a_click_picks_one_file() {
        let mut s = FileSelection::default();
        s.click(11, &ORDER, false, false);
        s.click(13, &ORDER, false, false);
        assert_eq!(s.targets(13, &ORDER), [13]);
        assert!(!s.contains(11));
    }

    #[test]
    fn shift_picks_the_run_from_the_last_click_either_way() {
        let mut s = FileSelection::default();
        s.click(12, &ORDER, false, false);
        s.click(14, &ORDER, true, false);
        assert_eq!(s.targets(12, &ORDER), [12, 13, 14]);
        // The run is measured from the same file, so it can shrink or flip.
        s.click(10, &ORDER, true, false);
        assert_eq!(s.targets(12, &ORDER), [10, 11, 12]);
    }

    #[test]
    fn shift_with_nothing_picked_picks_one() {
        let mut s = FileSelection::default();
        s.click(13, &ORDER, true, false);
        assert_eq!(s.targets(13, &ORDER), [13]);
    }

    #[test]
    fn toggle_adds_and_drops_and_moves_the_anchor() {
        let mut s = FileSelection::default();
        s.click(10, &ORDER, false, false);
        s.click(12, &ORDER, false, true);
        assert_eq!(s.targets(10, &ORDER), [10, 12]);
        s.click(10, &ORDER, false, true);
        assert_eq!(s.targets(12, &ORDER), [12]);
        // The run starts at the file last clicked, even one just dropped.
        s.click(13, &ORDER, false, true);
        s.click(14, &ORDER, true, true);
        assert_eq!(s.targets(12, &ORDER), [12, 13, 14]);
    }

    #[test]
    fn an_action_outside_the_picked_files_acts_on_its_own_row() {
        let mut s = FileSelection::default();
        s.click(10, &ORDER, false, false);
        s.click(11, &ORDER, true, false);
        assert_eq!(s.targets(14, &ORDER), [14]);
    }

    #[test]
    fn removed_files_leave_the_selection() {
        let mut s = FileSelection::default();
        s.click(10, &ORDER, false, false);
        s.click(12, &ORDER, true, false);
        s.retain(&[11, 13, 14]);
        assert_eq!(s.targets(11, &[11, 13, 14]), [11]);
        s.retain(&[]);
        assert!(s.is_empty());
    }
}
