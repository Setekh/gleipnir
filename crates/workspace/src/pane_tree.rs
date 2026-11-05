//! The arrangement of split editor panes, as a pure tree. Generic over what's in a pane, so all the
//! splitting and closing logic can be tested without creating any views.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    /// Side by side.
    Horizontal,
    /// One above the other.
    Vertical,
}

/// Panes can't be dragged smaller than this fraction of their split.
pub const MIN_RATIO: f32 = 0.1;

#[derive(Clone, Debug, PartialEq)]
pub enum PaneTree<T> {
    Pane(T),
    /// Always has at least two children, whose ratios sum to 1.
    Split {
        axis: Axis,
        children: Vec<PaneTree<T>>,
        ratios: Vec<f32>,
    },
}

impl<T: PartialEq + Clone> PaneTree<T> {
    /// The panes in reading order: left to right, top to bottom.
    pub fn panes(&self) -> Vec<T> {
        let mut panes = Vec::new();
        self.collect(&mut panes);
        panes
    }

    fn collect(&self, panes: &mut Vec<T>) {
        match self {
            Self::Pane(pane) => panes.push(pane.clone()),
            Self::Split { children, .. } => children.iter().for_each(|c| c.collect(panes)),
        }
    }

    /// Splits `target`, placing `new` after it. A split in the same direction as the one it's
    /// already in adds a sibling, rather than nesting, so three panes in a row are one level deep.
    /// Returns false if `target` isn't in the tree.
    pub fn split(&mut self, target: &T, new: T, axis: Axis) -> bool {
        match self {
            Self::Pane(pane) => {
                if pane != target {
                    return false;
                }
                let existing = pane.clone();
                *self = Self::Split {
                    axis,
                    children: vec![Self::Pane(existing), Self::Pane(new)],
                    ratios: vec![0.5, 0.5],
                };
                true
            }
            Self::Split {
                axis: own_axis,
                children,
                ratios,
            } => {
                if *own_axis == axis
                    && let Some(index) = children
                        .iter()
                        .position(|c| matches!(c, Self::Pane(p) if p == target))
                {
                    // The new pane takes half of the space of the one it came from.
                    let half = ratios[index] / 2.;
                    ratios[index] = half;
                    ratios.insert(index + 1, half);
                    children.insert(index + 1, Self::Pane(new));
                    return true;
                }
                children.iter_mut().any(|c| {
                    if c.contains(target) {
                        c.split(target, new.clone(), axis)
                    } else {
                        false
                    }
                })
            }
        }
    }

    pub fn contains(&self, target: &T) -> bool {
        match self {
            Self::Pane(pane) => pane == target,
            Self::Split { children, .. } => children.iter().any(|c| c.contains(target)),
        }
    }

    /// Removes a pane, giving its space to its siblings, and collapsing any split left with one
    /// child. Returns false if it's the only pane, as there must always be one.
    pub fn remove(&mut self, target: &T) -> bool {
        let Self::Split {
            children, ratios, ..
        } = self
        else {
            return false;
        };

        let direct = children
            .iter()
            .position(|c| matches!(c, Self::Pane(p) if p == target));
        let removed = if let Some(index) = direct {
            children.remove(index);
            let freed = ratios.remove(index);
            let remaining: f32 = ratios.iter().sum();
            for ratio in ratios.iter_mut() {
                *ratio += freed * (*ratio / remaining);
            }
            true
        } else {
            children
                .iter_mut()
                .any(|c| c.contains(target) && c.remove(target))
        };

        if removed && children.len() == 1 {
            let only = children.remove(0);
            *self = only;
        }
        removed
    }

    /// Moves the divider after child `index` of the split at `path`, keeping both sides above the
    /// minimum. `delta` is a fraction of the whole split.
    pub fn resize(&mut self, path: &[usize], index: usize, delta: f32) {
        let Self::Split {
            children, ratios, ..
        } = self
        else {
            return;
        };
        if let Some((first, rest)) = path.split_first() {
            if let Some(child) = children.get_mut(*first) {
                child.resize(rest, index, delta);
            }
            return;
        }
        if index + 1 >= ratios.len() {
            return;
        }
        let total = ratios[index] + ratios[index + 1];
        let left = (ratios[index] + delta).clamp(MIN_RATIO, (total - MIN_RATIO).max(MIN_RATIO));
        ratios[index] = left;
        ratios[index + 1] = total - left;
    }

    /// The pane after or before `target` in reading order, wrapping around.
    pub fn neighbour(&self, target: &T, forward: bool) -> Option<T> {
        let panes = self.panes();
        let index = panes.iter().position(|p| p == target)?;
        let next = if forward {
            index + 1
        } else {
            index + panes.len() - 1
        };
        Some(panes[next % panes.len()].clone())
    }

    /// Builds the same shape over different contents, dropping any pane `f` declines. Used to
    /// restore a saved layout whose files may have gone away.
    pub fn filter_map<U>(&self, f: &mut impl FnMut(&T) -> Option<U>) -> Option<PaneTree<U>> {
        match self {
            Self::Pane(pane) => f(pane).map(PaneTree::Pane),
            Self::Split {
                axis,
                children,
                ratios,
            } => {
                let mut kept_children = Vec::new();
                let mut kept_ratios = Vec::new();
                for (child, ratio) in children.iter().zip(ratios) {
                    if let Some(child) = child.filter_map(f) {
                        kept_children.push(child);
                        kept_ratios.push(*ratio);
                    }
                }
                match kept_children.len() {
                    0 => None,
                    1 => kept_children.pop(),
                    _ => {
                        let total: f32 = kept_ratios.iter().sum();
                        kept_ratios.iter_mut().for_each(|r| *r /= total);
                        Some(PaneTree::Split {
                            axis: *axis,
                            children: kept_children,
                            ratios: kept_ratios,
                        })
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Tree = PaneTree<u32>;

    fn assert_valid(tree: &Tree) {
        if let Tree::Split {
            children, ratios, ..
        } = tree
        {
            assert!(
                children.len() >= 2,
                "a split with one child should have collapsed"
            );
            assert_eq!(children.len(), ratios.len());
            let total: f32 = ratios.iter().sum();
            assert!((total - 1.).abs() < 0.001, "ratios sum to {total}");
            children.iter().for_each(assert_valid);
        }
    }

    #[test]
    fn the_first_split_makes_two_even_panes() {
        let mut tree = Tree::Pane(1);
        assert!(tree.split(&1, 2, Axis::Horizontal));
        assert_eq!(tree.panes(), [1, 2]);
        assert!(matches!(&tree, Tree::Split { ratios, .. } if ratios == &[0.5, 0.5]));
    }

    #[test]
    fn splitting_again_the_same_way_adds_a_sibling() {
        let mut tree = Tree::Pane(1);
        tree.split(&1, 2, Axis::Horizontal);
        tree.split(&1, 3, Axis::Horizontal);
        assert_eq!(tree.panes(), [1, 3, 2]);
        let Tree::Split {
            children, ratios, ..
        } = &tree
        else {
            panic!()
        };
        assert_eq!(children.len(), 3);
        assert_eq!(
            ratios,
            &[0.25, 0.25, 0.5],
            "only the split pane gives up space"
        );
        assert_valid(&tree);
    }

    #[test]
    fn splitting_the_other_way_nests() {
        let mut tree = Tree::Pane(1);
        tree.split(&1, 2, Axis::Horizontal);
        tree.split(&2, 3, Axis::Vertical);
        assert_eq!(tree.panes(), [1, 2, 3]);
        let Tree::Split { axis, children, .. } = &tree else {
            panic!()
        };
        assert_eq!(*axis, Axis::Horizontal);
        assert!(matches!(
            &children[1],
            Tree::Split {
                axis: Axis::Vertical,
                ..
            }
        ));
    }

    #[test]
    fn splitting_something_missing_does_nothing() {
        let mut tree = Tree::Pane(1);
        assert!(!tree.split(&9, 2, Axis::Horizontal));
        assert_eq!(tree, Tree::Pane(1));
    }

    #[test]
    fn the_last_pane_cannot_be_removed() {
        let mut tree = Tree::Pane(1);
        assert!(!tree.remove(&1));
    }

    #[test]
    fn removing_one_of_two_collapses_the_split() {
        let mut tree = Tree::Pane(1);
        tree.split(&1, 2, Axis::Horizontal);
        assert!(tree.remove(&1));
        assert_eq!(tree, Tree::Pane(2));
    }

    #[test]
    fn removing_shares_the_space_among_siblings() {
        let mut tree = Tree::Pane(1);
        tree.split(&1, 2, Axis::Horizontal);
        tree.split(&2, 3, Axis::Horizontal);
        tree.remove(&2);
        assert_valid(&tree);
        assert_eq!(tree.panes(), [1, 3]);
    }

    #[test]
    fn removing_from_a_nested_split_collapses_it_into_its_parent() {
        let mut tree = Tree::Pane(1);
        tree.split(&1, 2, Axis::Horizontal);
        tree.split(&2, 3, Axis::Vertical);
        assert!(tree.remove(&3));
        assert_valid(&tree);
        let Tree::Split { children, .. } = &tree else {
            panic!()
        };
        assert_eq!(children[1], Tree::Pane(2));
    }

    #[test]
    fn removing_something_missing_does_nothing() {
        let mut tree = Tree::Pane(1);
        tree.split(&1, 2, Axis::Horizontal);
        assert!(!tree.remove(&9));
        assert_eq!(tree.panes(), [1, 2]);
    }

    #[test]
    fn resizing_moves_space_between_neighbours_only() {
        let mut tree = Tree::Pane(1);
        tree.split(&1, 2, Axis::Horizontal);
        tree.split(&2, 3, Axis::Horizontal);
        tree.resize(&[], 0, 0.1);
        let Tree::Split { ratios, .. } = &tree else {
            panic!()
        };
        assert!((ratios[0] - 0.6).abs() < 0.001);
        assert!((ratios[1] - 0.15).abs() < 0.001);
        assert!((ratios[2] - 0.25).abs() < 0.001);
        assert_valid(&tree);
    }

    #[test]
    fn resizing_stops_at_the_minimum() {
        let mut tree = Tree::Pane(1);
        tree.split(&1, 2, Axis::Horizontal);
        tree.resize(&[], 0, 5.);
        let Tree::Split { ratios, .. } = &tree else {
            panic!()
        };
        assert!((ratios[1] - MIN_RATIO).abs() < 0.001);
        tree.resize(&[], 0, -5.);
        let Tree::Split { ratios, .. } = &tree else {
            panic!()
        };
        assert!((ratios[0] - MIN_RATIO).abs() < 0.001);
        assert_valid(&tree);
    }

    #[test]
    fn resizing_reaches_nested_splits_by_path() {
        let mut tree = Tree::Pane(1);
        tree.split(&1, 2, Axis::Horizontal);
        tree.split(&2, 3, Axis::Vertical);
        tree.resize(&[1], 0, 0.2);
        let Tree::Split {
            children, ratios, ..
        } = &tree
        else {
            panic!()
        };
        assert_eq!(ratios, &[0.5, 0.5], "the outer split is untouched");
        let Tree::Split { ratios, .. } = &children[1] else {
            panic!()
        };
        assert!((ratios[0] - 0.7).abs() < 0.001);
    }

    #[test]
    fn a_bad_resize_path_is_ignored() {
        let mut tree = Tree::Pane(1);
        tree.split(&1, 2, Axis::Horizontal);
        let before = tree.clone();
        tree.resize(&[7], 0, 0.2);
        tree.resize(&[], 9, 0.2);
        assert_eq!(tree, before);
    }

    #[test]
    fn neighbours_wrap_around() {
        let mut tree = Tree::Pane(1);
        tree.split(&1, 2, Axis::Horizontal);
        tree.split(&2, 3, Axis::Vertical);
        assert_eq!(tree.neighbour(&1, true), Some(2));
        assert_eq!(tree.neighbour(&3, true), Some(1));
        assert_eq!(tree.neighbour(&1, false), Some(3));
        assert_eq!(tree.neighbour(&9, true), None);
        assert_eq!(Tree::Pane(1).neighbour(&1, true), Some(1));
    }

    #[test]
    fn filtering_collapses_what_is_left_and_rebalances() {
        let mut tree = Tree::Pane(1);
        tree.split(&1, 2, Axis::Horizontal);
        tree.split(&2, 3, Axis::Horizontal);
        let kept = tree
            .filter_map(&mut |p| (*p != 2).then_some(*p * 10))
            .unwrap();
        assert_eq!(kept.panes(), [10, 30]);
        assert_valid(&kept);

        let one = tree.filter_map(&mut |p| (*p == 3).then_some(*p)).unwrap();
        assert_eq!(one, Tree::Pane(3));
        assert_eq!(tree.filter_map::<u32>(&mut |_| None), None);
    }

    #[test]
    fn any_sequence_of_splits_and_removals_stays_valid() {
        // A fixed pseudo-random walk, enough to cover deep nesting in both directions.
        let mut tree = Tree::Pane(0);
        let mut next = 1u32;
        let mut seed = 0x2545f491u32;
        for _ in 0..2000 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let panes = tree.panes();
            let target = panes[seed as usize % panes.len()];
            if seed.is_multiple_of(3) && panes.len() > 1 {
                assert!(tree.remove(&target));
            } else if panes.len() < 40 {
                let axis = if seed.is_multiple_of(2) {
                    Axis::Horizontal
                } else {
                    Axis::Vertical
                };
                assert!(tree.split(&target, next, axis));
                next += 1;
            }
            assert_valid(&tree);
            let panes = tree.panes();
            let mut unique = panes.clone();
            unique.sort();
            unique.dedup();
            assert_eq!(unique.len(), panes.len(), "a pane was duplicated or lost");
        }
    }
}
