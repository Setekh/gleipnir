//! What a debugger shows in the editor: breakpoints and Run icons in the gutter, the line
//! execution stopped on, and values beside the code. The editor knows nothing of debugging; it
//! asks a provider each frame and reports clicks in the gutter as events.

use gpui::App;
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BreakpointKind {
    Line,
    /// Has a condition or a hit count.
    Conditional,
    /// Logs instead of stopping.
    Log,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BreakpointMark {
    pub line: usize,
    pub kind: BreakpointKind,
    pub enabled: bool,
    /// Whether a running session could place it; `None` with nothing running.
    pub verified: Option<bool>,
}

pub trait DecorationProvider {
    fn breakpoints(&self, cx: &App) -> Vec<BreakpointMark>;
    /// The line execution is stopped on, and whether it's the innermost frame rather than a caller.
    fn execution_line(&self, cx: &App) -> Option<(usize, bool)>;
    /// Lines with an entry point that can be run from the gutter.
    fn run_lines(&self, cx: &App) -> Vec<usize>;
    /// Text to show after lines, like the values of the variables on them, for the lines given.
    fn inline_values(&self, lines: Range<usize>, cx: &App) -> Vec<(usize, String)>;
}

/// Where in the gutter a click landed, with decorations shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GutterZone {
    /// The marks column or the line numbers: toggles a breakpoint.
    Marks,
    /// The column between the numbers and the text, where Run icons are.
    Run,
}

/// The width the marks column adds to the gutter's left.
pub(crate) const MARKS_WIDTH: f32 = 14.0;
