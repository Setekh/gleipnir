//! Cursors, selections, and how positions follow edits.

use crate::{AppliedEdit, Buffer, TextEdit};
use std::ops::Range;

/// Which side of an insertion a position sticks to, when the insertion lands exactly on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bias {
    Left,
    Right,
}

/// Moves a position through one edit. Everything anchored to text uses this: other views'
/// selections, and later breakpoints and diagnostics.
pub fn transform_offset(offset: usize, edit: &AppliedEdit, bias: Bias) -> usize {
    if offset < edit.start_byte {
        offset
    } else if offset == edit.start_byte {
        // Only a pure insertion can push a position; at the start of a replaced range, the
        // position stays before the new text.
        if edit.old_end_byte == edit.start_byte && bias == Bias::Right {
            edit.new_end_byte
        } else {
            edit.start_byte
        }
    } else if offset >= edit.old_end_byte {
        offset - edit.old_end_byte + edit.new_end_byte
    } else {
        // Inside text that was replaced.
        match bias {
            Bias::Left => edit.start_byte,
            Bias::Right => edit.new_end_byte,
        }
    }
}

/// Maps a position through a sorted batch of edits, all expressed in pre-batch coordinates. This
/// lets a command work out selections before it applies the batch.
pub fn map_offset_through_edits(offset: usize, edits: &[TextEdit], bias: Bias) -> usize {
    let mut delta = 0isize;
    for edit in edits {
        let new_len = edit.text.len() as isize;
        let old_len = edit.range.len() as isize;
        if offset < edit.range.start {
            break;
        }
        if offset == edit.range.start {
            if edit.range.is_empty() && bias == Bias::Right {
                delta += new_len;
                continue;
            }
            break;
        }
        if offset < edit.range.end {
            let base = edit.range.start as isize + delta;
            return match bias {
                Bias::Left => base as usize,
                Bias::Right => (base + new_len) as usize,
            };
        }
        delta += new_len - old_len;
    }
    (offset as isize + delta) as usize
}

/// A cursor, or a selection when `anchor != head`. Both are byte offsets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
    /// The display column vertical movement is aiming for. Moving through a short line clips the
    /// cursor, but the next long line should return to this column.
    pub goal: Option<usize>,
}

impl Selection {
    pub fn cursor(offset: usize) -> Self {
        Self {
            anchor: offset,
            head: offset,
            goal: None,
        }
    }

    pub fn new(anchor: usize, head: usize) -> Self {
        Self {
            anchor,
            head,
            goal: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    pub fn is_reversed(&self) -> bool {
        self.head < self.anchor
    }

    /// Follows one edit. A non-empty selection doesn't grow to swallow text typed at its edges.
    pub fn transform(&self, edit: &AppliedEdit) -> Self {
        if self.is_empty() {
            return Self::cursor(transform_offset(self.head, edit, Bias::Right));
        }
        let range = self.range();
        let start = transform_offset(range.start, edit, Bias::Right);
        let end = transform_offset(range.end, edit, Bias::Left).max(start);
        if self.is_reversed() {
            Self::new(end, start)
        } else {
            Self::new(start, end)
        }
    }
}

/// All of a view's selections. Always holds at least one, sorted and non-overlapping, so a single
/// cursor is not a special case anywhere.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionSet {
    items: Vec<Selection>,
    /// The most recently added selection. Scrolling and the IME follow this one.
    primary: usize,
}

impl Default for SelectionSet {
    fn default() -> Self {
        Self::cursor(0)
    }
}

impl SelectionSet {
    pub fn cursor(offset: usize) -> Self {
        Self::single(Selection::cursor(offset))
    }

    pub fn single(selection: Selection) -> Self {
        Self {
            items: vec![selection],
            primary: 0,
        }
    }

    /// Builds a normalized set. `primary` indexes into `items` as given. Falls back to a cursor at
    /// the start of the document if `items` is empty.
    pub fn from_vec(items: Vec<Selection>, primary: usize) -> Self {
        if items.is_empty() {
            return Self::default();
        }
        let primary = primary.min(items.len() - 1);
        let mut set = Self { items, primary };
        set.normalize();
        set
    }

    pub fn as_slice(&self) -> &[Selection] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn iter(&self) -> impl Iterator<Item = &Selection> {
        self.items.iter()
    }

    pub fn primary(&self) -> Selection {
        self.items[self.primary]
    }

    pub fn primary_index(&self) -> usize {
        self.primary
    }

    /// Whether any selection covers text, as opposed to all being bare cursors.
    pub fn has_non_empty(&self) -> bool {
        self.items.iter().any(|s| !s.is_empty())
    }

    /// Adds a selection and makes it primary. If it merges into an existing one, the merged
    /// selection becomes primary.
    pub fn push(&mut self, selection: Selection) {
        self.items.push(selection);
        self.primary = self.items.len() - 1;
        self.normalize();
    }

    /// Removes the primary selection, making the one before it primary. Does nothing when it's
    /// the only selection, since a view always has at least one.
    pub fn remove_primary(&mut self) {
        if self.items.len() > 1 {
            self.items.remove(self.primary);
            self.primary = self.primary.saturating_sub(1);
        }
    }

    pub fn collapse_to_primary(&mut self) {
        let primary = self.items[self.primary];
        self.items = vec![primary];
        self.primary = 0;
    }

    /// Rewrites every selection, then restores the sorted, non-overlapping invariant.
    pub fn update(&mut self, mut f: impl FnMut(Selection) -> Selection) {
        for item in &mut self.items {
            *item = f(*item);
        }
        self.normalize();
    }

    /// Follows edits made through another view or a reload, in the order they were applied.
    pub fn transform(&mut self, edits: &[AppliedEdit]) {
        self.update(|mut selection| {
            for edit in edits {
                selection = selection.transform(edit);
            }
            selection
        });
    }

    pub fn clip(&mut self, buffer: &Buffer) {
        self.update(|s| Selection {
            anchor: buffer.clip(s.anchor),
            head: buffer.clip(s.head),
            goal: s.goal,
        });
    }

    fn normalize(&mut self) {
        let primary = self.items[self.primary];
        self.items.sort_by_key(|s| (s.range().start, s.range().end));

        let mut merged: Vec<Selection> = Vec::with_capacity(self.items.len());
        for selection in self.items.drain(..) {
            match merged.last_mut() {
                Some(last) if should_merge(last, &selection) => {
                    let range = last.range().start..last.range().end.max(selection.range().end);
                    let reversed = last.is_reversed() || selection.is_reversed();
                    let goal = last.goal;
                    *last = if reversed {
                        Selection::new(range.end, range.start)
                    } else {
                        Selection::new(range.start, range.end)
                    };
                    last.goal = goal;
                }
                _ => merged.push(selection),
            }
        }
        self.items = merged;

        let target = primary.range();
        self.primary = self
            .items
            .iter()
            .position(|s| s.range().start <= target.start && target.end <= s.range().end)
            .unwrap_or(0);
    }
}

/// Overlapping selections merge, and so does a bare cursor touching anything. Two non-empty
/// selections that merely touch stay separate, so that selecting each `aa` in `aaaa` works.
fn should_merge(a: &Selection, b: &Selection) -> bool {
    let (a, b) = (a.range(), b.range());
    a.end > b.start || (a.end == b.start && (a.is_empty() || b.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Point, Utf16Point};

    fn edit(start: usize, old_end: usize, new_end: usize) -> AppliedEdit {
        AppliedEdit {
            start_byte: start,
            old_end_byte: old_end,
            new_end_byte: new_end,
            start: Point::default(),
            old_end: Point::default(),
            new_end: Point::default(),
            start_utf16: Utf16Point::default(),
            old_end_utf16: Utf16Point::default(),
            new_text: String::new(),
        }
    }

    #[test]
    fn positions_before_an_edit_stay_and_after_it_shift() {
        let e = edit(10, 15, 12);
        assert_eq!(transform_offset(9, &e, Bias::Right), 9);
        assert_eq!(transform_offset(15, &e, Bias::Left), 12);
        assert_eq!(transform_offset(20, &e, Bias::Left), 17);
    }

    #[test]
    fn bias_decides_which_side_of_an_insertion_a_position_lands() {
        let insert = edit(10, 10, 13);
        assert_eq!(transform_offset(10, &insert, Bias::Left), 10);
        assert_eq!(transform_offset(10, &insert, Bias::Right), 13);
    }

    #[test]
    fn positions_inside_deleted_text_collapse_to_the_edit() {
        let delete = edit(10, 20, 10);
        assert_eq!(transform_offset(15, &delete, Bias::Left), 10);
        assert_eq!(transform_offset(15, &delete, Bias::Right), 10);
        let replace = edit(10, 20, 12);
        assert_eq!(transform_offset(15, &replace, Bias::Right), 12);
        // The start of replaced text stays put regardless of bias.
        assert_eq!(transform_offset(10, &replace, Bias::Right), 10);
    }

    #[test]
    fn mapping_through_a_batch_accumulates_earlier_edits() {
        let edits = vec![TextEdit::new(2..2, "XX"), TextEdit::new(5..7, "")];
        assert_eq!(map_offset_through_edits(0, &edits, Bias::Right), 0);
        assert_eq!(map_offset_through_edits(2, &edits, Bias::Right), 4);
        assert_eq!(map_offset_through_edits(2, &edits, Bias::Left), 2);
        assert_eq!(map_offset_through_edits(5, &edits, Bias::Right), 7);
        assert_eq!(map_offset_through_edits(6, &edits, Bias::Right), 7);
        assert_eq!(map_offset_through_edits(9, &edits, Bias::Right), 9);
    }

    #[test]
    fn a_selection_does_not_grow_when_text_is_inserted_at_its_edges() {
        let s = Selection::new(5, 10);
        assert_eq!(s.transform(&edit(5, 5, 7)), Selection::new(7, 12));
        assert_eq!(s.transform(&edit(10, 10, 12)), Selection::new(5, 10));
    }

    #[test]
    fn a_reversed_selection_stays_reversed_through_a_transform() {
        let s = Selection::new(10, 5);
        assert_eq!(s.transform(&edit(0, 0, 2)), Selection::new(12, 7));
    }

    #[test]
    fn a_selection_whose_text_is_deleted_becomes_a_cursor() {
        let s = Selection::new(5, 10);
        assert!(s.transform(&edit(3, 12, 3)).is_empty());
    }

    #[test]
    fn selections_are_kept_sorted() {
        let set = SelectionSet::from_vec(vec![Selection::cursor(9), Selection::cursor(2)], 0);
        assert_eq!(set.as_slice()[0].head, 2);
        assert_eq!(set.primary().head, 9);
    }

    #[test]
    fn overlapping_selections_merge_and_primary_follows() {
        let set = SelectionSet::from_vec(vec![Selection::new(0, 5), Selection::new(3, 8)], 1);
        assert_eq!(set.len(), 1);
        assert_eq!(set.primary().range(), 0..8);
    }

    #[test]
    fn identical_cursors_merge() {
        let set = SelectionSet::from_vec(vec![Selection::cursor(4), Selection::cursor(4)], 0);
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn a_cursor_touching_a_selection_merges_but_two_touching_selections_do_not() {
        let set = SelectionSet::from_vec(vec![Selection::new(0, 4), Selection::cursor(4)], 0);
        assert_eq!(set.len(), 1);
        let set = SelectionSet::from_vec(vec![Selection::new(0, 2), Selection::new(2, 4)], 0);
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn push_makes_the_new_selection_primary() {
        let mut set = SelectionSet::cursor(10);
        set.push(Selection::cursor(3));
        assert_eq!(set.primary().head, 3);
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn the_last_selection_cannot_be_removed() {
        let mut set = SelectionSet::cursor(1);
        set.remove_primary();
        assert_eq!(set.len(), 1);
        set.push(Selection::cursor(5));
        set.remove_primary();
        assert_eq!(set.primary().head, 1);
    }

    #[test]
    fn building_from_nothing_gives_a_cursor_at_the_start() {
        let set = SelectionSet::from_vec(vec![], 3);
        assert_eq!(set.primary(), Selection::cursor(0));
    }

    #[test]
    fn selection_range_is_ordered_regardless_of_direction() {
        let s = Selection::new(9, 3);
        assert_eq!(s.range(), 3..9);
        assert!(s.is_reversed());
        assert!(!s.is_empty());
    }
}
