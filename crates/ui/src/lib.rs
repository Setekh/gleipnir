//! Shared look and widgets. GPUI provides primitives only, so everything a panel needs is here.

pub mod list;
pub mod text_input;
pub mod theme;

use gpui::{Pixels, ShapedLine};

pub use list::ListNav;
pub use text_input::{TextInput, TextInputEvent, bind_keys as bind_text_input_keys};
pub use theme::{Theme, init_theme};

/// The character boundary nearest `x` in a shaped line. GPUI's `closest_index_for_x` compares each
/// glyph with the one before it, so past the start of the last glyph there's nothing left to
/// compare with and it always returns the end. This corrects that case.
pub fn closest_index(line: &ShapedLine, x: Pixels) -> usize {
    let index = line.closest_index_for_x(x);
    if index == line.len()
        && let Some((last, _)) = line.text.char_indices().next_back()
    {
        let start = line.x_for_index(last);
        if x - start < line.width() - x {
            return last;
        }
    }
    index
}
