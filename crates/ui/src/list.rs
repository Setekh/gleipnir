use gpui::{ScrollStrategy, UniformListScrollHandle};

/// Keyboard selection for a virtual list, for use with GPUI's `uniform_list`. Only visible rows
/// are ever built, so a list of a hundred thousand files costs the same as a list of ten.
#[derive(Clone, Default)]
pub struct ListNav {
    selected: Option<usize>,
    count: usize,
    scroll: UniformListScrollHandle,
}

impl ListNav {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn scroll_handle(&self) -> &UniformListScrollHandle {
        &self.scroll
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub fn count(&self) -> usize {
        self.count
    }

    /// Updates the item count, keeping the selection in range, and selects the first item if
    /// there wasn't a selection and now there can be.
    pub fn set_count(&mut self, count: usize) {
        self.count = count;
        self.selected = match self.selected {
            _ if count == 0 => None,
            Some(selected) => Some(selected.min(count - 1)),
            None => Some(0),
        };
    }

    pub fn select(&mut self, index: usize) {
        if index < self.count {
            self.selected = Some(index);
            self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
        }
    }

    /// Moves the selection without scrolling to it, for when the list changed under the user
    /// rather than the user moving.
    pub fn set_selected(&mut self, index: usize) {
        if index < self.count {
            self.selected = Some(index);
        }
    }

    /// Moves the selection, wrapping at the ends.
    pub fn select_next(&mut self) {
        if self.count > 0 {
            self.select(self.selected.map_or(0, |s| (s + 1) % self.count));
        }
    }

    pub fn select_previous(&mut self) {
        if self.count > 0 {
            self.select(
                self.selected
                    .map_or(self.count - 1, |s| (s + self.count - 1) % self.count),
            );
        }
    }

    /// Moves by a page, stopping at the ends, which is what's expected when paging.
    pub fn select_by(&mut self, delta: isize) {
        if self.count > 0 {
            let current = self.selected.unwrap_or(0) as isize;
            self.select((current + delta).clamp(0, self.count as isize - 1) as usize);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nav(count: usize) -> ListNav {
        let mut nav = ListNav::new();
        nav.set_count(count);
        nav
    }

    #[test]
    fn the_first_item_is_selected_once_there_are_items() {
        assert_eq!(nav(3).selected(), Some(0));
        assert_eq!(nav(0).selected(), None);
    }

    #[test]
    fn next_and_previous_wrap_around() {
        let mut list = nav(3);
        list.select_previous();
        assert_eq!(list.selected(), Some(2));
        list.select_next();
        assert_eq!(list.selected(), Some(0));
    }

    #[test]
    fn paging_stops_at_the_ends() {
        let mut list = nav(10);
        list.select_by(100);
        assert_eq!(list.selected(), Some(9));
        list.select_by(-100);
        assert_eq!(list.selected(), Some(0));
    }

    #[test]
    fn shrinking_keeps_the_selection_in_range() {
        let mut list = nav(10);
        list.select(9);
        list.set_count(4);
        assert_eq!(list.selected(), Some(3));
        list.set_count(0);
        assert_eq!(list.selected(), None);
    }

    #[test]
    fn navigating_an_empty_list_does_nothing() {
        let mut list = nav(0);
        list.select_next();
        list.select_previous();
        list.select_by(3);
        list.select(0);
        assert_eq!(list.selected(), None);
    }
}
