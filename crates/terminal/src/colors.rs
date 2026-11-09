//! The terminal's colors: the 16 programs name come from the theme, unless a program has set
//! them itself; the other 240 are xterm's color cube and grays; and colors given exactly are
//! used as they are.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use gleipnir_ui::Theme;
use gpui::{Hsla, Rgba};

fn from_rgb(rgb: Rgb) -> Hsla {
    Rgba {
        r: rgb.r as f32 / 255.,
        g: rgb.g as f32 / 255.,
        b: rgb.b as f32 / 255.,
        a: 1.,
    }
    .into()
}

fn to_rgb(color: Hsla) -> Rgb {
    let rgba = Rgba::from(color);
    let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    Rgb {
        r: byte(rgba.r),
        g: byte(rgba.g),
        b: byte(rgba.b),
    }
}

/// One of xterm's 256 colors past the first 16: a 6×6×6 cube, then 24 grays.
fn xterm_color(index: usize) -> Rgb {
    if (16..232).contains(&index) {
        let i = index - 16;
        let level = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u8 };
        Rgb {
            r: level(i / 36),
            g: level(i / 6 % 6),
            b: level(i % 6),
        }
    } else {
        let gray = (8 + (index.saturating_sub(232)) * 10) as u8;
        Rgb {
            r: gray,
            g: gray,
            b: gray,
        }
    }
}

/// The color at an index of the 256, or of the named ones past them, with the theme's own.
pub(crate) fn rgb_for_index(index: usize, theme: &Theme) -> Rgb {
    match index {
        0..16 => to_rgb(theme.terminal[index]),
        16..256 => xterm_color(index),
        i if i == NamedColor::Background as usize => to_rgb(theme.editor_background),
        i if i == NamedColor::Cursor as usize => to_rgb(theme.editor_cursor),
        _ => to_rgb(theme.text),
    }
}

/// What a color in a cell is, in the theme and with what the program has set.
pub fn resolve(color: Color, colors: &Colors, theme: &Theme) -> Hsla {
    let indexed = |index: usize| match colors[index] {
        Some(rgb) => from_rgb(rgb),
        None if index < 16 => theme.terminal[index],
        None => from_rgb(xterm_color(index)),
    };
    match color {
        Color::Spec(rgb) => from_rgb(rgb),
        Color::Indexed(index) => indexed(index as usize),
        Color::Named(named) => {
            let index = named as usize;
            if index < 16 {
                return indexed(index);
            }
            let set = colors[named].map(from_rgb);
            match named {
                NamedColor::Background => set.unwrap_or(theme.editor_background),
                NamedColor::Cursor => set.unwrap_or(theme.editor_cursor),
                NamedColor::Foreground | NamedColor::BrightForeground => set.unwrap_or(theme.text),
                NamedColor::DimForeground => set.unwrap_or(theme.text_muted),
                // The dim versions of the 16, darker by a third.
                dim => {
                    let base = dim as usize - NamedColor::DimBlack as usize;
                    let mut color = indexed(base.min(7));
                    color.l *= 0.66;
                    color
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cube_and_grays_are_xterms() {
        assert_eq!(xterm_color(16), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(xterm_color(196), Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(
            xterm_color(231),
            Rgb {
                r: 255,
                g: 255,
                b: 255
            }
        );
        assert_eq!(xterm_color(232), Rgb { r: 8, g: 8, b: 8 });
        assert_eq!(
            xterm_color(255),
            Rgb {
                r: 238,
                g: 238,
                b: 238
            }
        );
    }

    #[test]
    fn named_colors_are_the_themes_unless_a_program_set_them() {
        let theme = Theme::dark();
        let mut colors = Colors::default();
        let red = resolve(Color::Named(NamedColor::Red), &colors, &theme);
        assert_eq!(red, theme.terminal[1]);
        assert_eq!(
            resolve(Color::Named(NamedColor::Foreground), &colors, &theme),
            theme.text
        );
        colors[1] = Some(Rgb { r: 255, g: 0, b: 0 });
        let set = resolve(Color::Indexed(1), &colors, &theme);
        assert_eq!(Rgba::from(set).r, 1.);
    }
}
