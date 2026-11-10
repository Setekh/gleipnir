//! The log's graph, as IntelliJ draws it: each commit a dot in a coloured lane, with lines down to
//! its parents, so branches can be seen forking off and merging back.
//!
//! Lanes are worked out from the commits in the order the log lists them, children before
//! parents. A lane waits for the commit named next in it; a commit takes the lane waiting for it,
//! or a free one if nothing is, and its first parent carries on in that lane. Other parents of a
//! merge join the lane already waiting for them, or start one. Lanes that end leave a gap the
//! ones to their right close up, leaning over in the row where they move.

/// How far down a row a line starts or ends: at the top edge, the dot, or the bottom edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Top,
    Middle,
    Bottom,
}

/// A straight line within one row, from one lane to another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphLine {
    pub from: (usize, Level),
    pub to: (usize, Level),
    /// Which colour: an index into a palette, which it wraps around.
    pub color: usize,
}

/// One commit's row of the graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphRow {
    /// The lane the commit's dot is in.
    pub lane: usize,
    pub color: usize,
    pub lines: Vec<GraphLine>,
    /// How many lanes the row uses, for how wide to draw it.
    pub width: usize,
}

#[derive(Clone)]
struct Lane<'a> {
    /// The commit this lane goes on to.
    waiting_for: &'a str,
    color: usize,
}

/// The graph for commits listed children first, each with its parents' hashes. Parents that
/// aren't in the list, as past the end of a long log, keep their lanes open to the bottom.
pub fn graph<'a>(commits: impl IntoIterator<Item = (&'a str, &'a [String])>) -> Vec<GraphRow> {
    let mut lanes: Vec<Option<Lane<'a>>> = Vec::new();
    let mut next_color = 0;
    let mut new_color = || {
        next_color += 1;
        next_color - 1
    };
    let mut rows = Vec::new();
    for (hash, parents) in commits {
        let before = lanes.clone();
        let mut lines = Vec::new();
        let waiting = |l: &Option<Lane>| l.as_ref().is_some_and(|l| l.waiting_for == hash);
        let (lane, color) = match before.iter().position(waiting) {
            Some(lane) => (lane, before[lane].as_ref().map_or(0, |l| l.color)),
            // A branch's newest commit: nothing above leads to it.
            None => (free_lane(&mut lanes), new_color()),
        };
        // Every lane waiting for this commit ends at its dot.
        for (index, waiting_lane) in before.iter().enumerate() {
            if let Some(l) = waiting_lane
                && l.waiting_for == hash
            {
                lines.push(GraphLine {
                    from: (index, Level::Top),
                    to: (lane, Level::Middle),
                    color: l.color,
                });
                lanes[index] = None;
            }
        }
        if let Some(first) = parents.first() {
            lanes[lane] = Some(Lane {
                waiting_for: first,
                color,
            });
            lines.push(GraphLine {
                from: (lane, Level::Middle),
                to: (lane, Level::Bottom),
                color,
            });
        }
        for parent in parents.iter().skip(1) {
            let existing = lanes
                .iter()
                .position(|l| l.as_ref().is_some_and(|l| l.waiting_for == parent));
            let (target, line_color) = match existing {
                Some(index) => (index, lanes[index].as_ref().map_or(0, |l| l.color)),
                None => {
                    let index = free_lane(&mut lanes);
                    let color = new_color();
                    lanes[index] = Some(Lane {
                        waiting_for: parent,
                        color,
                    });
                    (index, color)
                }
            };
            lines.push(GraphLine {
                from: (lane, Level::Middle),
                to: (target, Level::Bottom),
                color: line_color,
            });
        }
        // Lanes going past this commit.
        for (index, passing) in before.iter().enumerate() {
            if let Some(l) = passing
                && l.waiting_for != hash
            {
                lines.push(GraphLine {
                    from: (index, Level::Top),
                    to: (index, Level::Bottom),
                    color: l.color,
                });
            }
        }
        // Close up the gaps lanes that ended left: what's at the bottom edge moves over.
        let mut moved = vec![0; lanes.len()];
        let mut kept = 0;
        for (index, l) in lanes.iter().enumerate() {
            moved[index] = kept;
            if l.is_some() {
                kept += 1;
            }
        }
        for line in &mut lines {
            if line.to.1 == Level::Bottom {
                line.to.0 = moved[line.to.0];
            }
        }
        lanes.retain(Option::is_some);
        let width = lines
            .iter()
            .flat_map(|l| [l.from.0, l.to.0])
            .chain([lane])
            .max()
            .map_or(1, |m| m + 1);
        rows.push(GraphRow {
            lane,
            color,
            lines,
            width,
        });
    }
    rows
}

/// The leftmost lane nothing's in, adding one if they're all taken.
fn free_lane(lanes: &mut Vec<Option<Lane>>) -> usize {
    match lanes.iter().position(Option::is_none) {
        Some(index) => index,
        None => {
            lanes.push(None);
            lanes.len() - 1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Level::*;

    fn rows(commits: &[(&str, &[&str])]) -> Vec<GraphRow> {
        let owned: Vec<(String, Vec<String>)> = commits
            .iter()
            .map(|(h, p)| (h.to_string(), p.iter().map(|p| p.to_string()).collect()))
            .collect();
        graph(owned.iter().map(|(h, p)| (h.as_str(), p.as_slice())))
    }

    fn line(from: (usize, Level), to: (usize, Level), color: usize) -> GraphLine {
        GraphLine { from, to, color }
    }

    /// Each row as its dot's lane and its lines, sorted, for comparing.
    fn shape(rows: &[GraphRow]) -> Vec<(usize, Vec<GraphLine>)> {
        rows.iter()
            .map(|r| {
                let mut lines = r.lines.clone();
                lines.sort_by_key(|l| (l.from.0, l.to.0, l.color));
                (r.lane, lines)
            })
            .collect()
    }

    #[test]
    fn a_straight_history_is_one_lane() {
        let g = rows(&[("c", &["b"]), ("b", &["a"]), ("a", &[])]);
        assert!(
            g.iter()
                .all(|r| r.lane == 0 && r.color == 0 && r.width == 1)
        );
        assert_eq!(g[0].lines, [line((0, Middle), (0, Bottom), 0)]);
        assert_eq!(
            g[1].lines,
            [
                line((0, Top), (0, Middle), 0),
                line((0, Middle), (0, Bottom), 0)
            ]
        );
        // The first commit has nothing below it.
        assert_eq!(g[2].lines, [line((0, Top), (0, Middle), 0)]);
    }

    #[test]
    fn a_merge_forks_a_lane_that_joins_back_where_the_branch_started() {
        // m merges side branch s into b; both started from a.
        let g = rows(&[("m", &["b", "s"]), ("s", &["a"]), ("b", &["a"]), ("a", &[])]);
        assert_eq!(
            shape(&g),
            [
                (
                    0,
                    vec![
                        line((0, Middle), (0, Bottom), 0),
                        line((0, Middle), (1, Bottom), 1),
                    ]
                ),
                (
                    1,
                    vec![
                        line((0, Top), (0, Bottom), 0),
                        line((1, Top), (1, Middle), 1),
                        line((1, Middle), (1, Bottom), 1),
                    ]
                ),
                (
                    0,
                    vec![
                        line((0, Top), (0, Middle), 0),
                        line((0, Middle), (0, Bottom), 0),
                        line((1, Top), (1, Bottom), 1),
                    ]
                ),
                // Both lanes end at a, the side one leaning in.
                (
                    0,
                    vec![
                        line((0, Top), (0, Middle), 0),
                        line((1, Top), (0, Middle), 1),
                    ]
                ),
            ]
        );
        assert_eq!(g.iter().map(|r| r.width).collect::<Vec<_>>(), [2, 2, 2, 2]);
        assert_eq!(g[1].color, 1);
    }

    #[test]
    fn a_merge_joins_a_lane_already_waiting_for_its_parent() {
        // t is a branch tip whose parent s is also merged into m.
        let g = rows(&[
            ("t", &["s"]),
            ("m", &["b", "s"]),
            ("s", &["a"]),
            ("b", &["a"]),
            ("a", &[]),
        ]);
        // m's merge line goes into t's lane rather than starting a third.
        assert_eq!(g[1].lane, 1);
        assert!(g[1].lines.contains(&line((1, Middle), (0, Bottom), 0)));
        assert!(g.iter().all(|r| r.width <= 2), "{g:?}");
        // s ends the lane waiting for it and carries on in its own.
        assert_eq!(g[2].lane, 0);
    }

    #[test]
    fn branches_that_end_leave_a_gap_the_others_close() {
        // Three tips, x, y and z, made from a, each in its own lane; y ends first at its
        // own root r, and z's lane moves left into the gap.
        let g = rows(&[
            ("x", &["a"]),
            ("y", &["r"]),
            ("z", &["a"]),
            ("r", &[]),
            ("a", &[]),
        ]);
        assert_eq!(
            g.iter().map(|r| r.lane).collect::<Vec<_>>(),
            [0, 1, 2, 1, 0]
        );
        // At r, the lane from z leans over from 2 to 1.
        assert!(g[3].lines.contains(&line((2, Top), (1, Bottom), 2)));
        assert_eq!(g[3].width, 3);
        // At a, both remaining lanes end.
        assert_eq!(g[4].lines.len(), 2);
        assert_eq!(g[4].width, 2);
    }

    #[test]
    fn many_parallel_branches_get_lanes_of_their_own_and_different_colors() {
        let tips: Vec<String> = (0..12).map(|i| format!("tip{i}")).collect();
        let base = vec!["base".to_string()];
        let mut commits: Vec<(&str, &[String])> =
            tips.iter().map(|t| (t.as_str(), base.as_slice())).collect();
        commits.push(("base", &[]));
        let g = graph(commits);
        assert_eq!(
            g.iter().take(12).map(|r| r.lane).collect::<Vec<_>>(),
            (0..12).collect::<Vec<_>>()
        );
        let colors: std::collections::HashSet<usize> = g.iter().take(12).map(|r| r.color).collect();
        assert_eq!(colors.len(), 12);
        // All twelve meet at the base.
        assert_eq!(g[12].lane, 0);
        assert_eq!(g[12].lines.len(), 12);
        assert!(
            g[12]
                .lines
                .iter()
                .all(|l| l.to == (0, Middle) && l.from.1 == Top)
        );
    }

    #[test]
    fn parents_past_the_end_of_the_log_keep_their_lanes_open() {
        let g = rows(&[("m", &["p1", "p2"]), ("p1", &["old"])]);
        assert_eq!(g[1].lane, 0);
        assert!(g[1].lines.contains(&line((0, Middle), (0, Bottom), 0)));
        assert!(g[1].lines.contains(&line((1, Top), (1, Bottom), 1)));
    }

    #[test]
    fn a_long_log_is_quick() {
        // A thousand commits, with a branch merged every ten.
        let mut commits = Vec::new();
        for i in 0..1000 {
            let hash = format!("c{i}");
            let mut parents = vec![format!("c{}", i + 1)];
            if i % 10 == 0 {
                parents.push(format!("s{i}"));
                commits.push((hash, parents));
                commits.push((format!("s{i}"), vec![format!("c{}", i + 1)]));
            } else {
                commits.push((hash, parents));
            }
        }
        let started = std::time::Instant::now();
        let g = graph(commits.iter().map(|(h, p)| (h.as_str(), p.as_slice())));
        assert_eq!(g.len(), 1100);
        assert!(g.iter().all(|r| r.width <= 2));
        assert!(started.elapsed() < std::time::Duration::from_millis(200));
    }
}
