//! The window's frame on Linux, where the app draws it, as on macOS, rather than the compositor:
//! the minimise, maximise and close buttons sit in the title bar, in the order the desktop is set
//! to use, and a window that isn't tiled gets a border, a shadow and edges that resize it.
//!
//! `GLEIPNIR_WINDOW_DECORATIONS=server` asks the compositor for its own title bar instead.

use gleipnir_ui::{Icon, Theme};
use gpui::{
    AnyElement, App, Bounds, CursorStyle, Decorations, Edges, Global, Hitbox, HitboxBehavior,
    IntoElement, MouseButton, Pixels, Point, ResizeEdge, Size, Stateful, Tiling, Window,
    WindowDecorations, canvas, div, point, prelude::*, px, svg,
};

/// The band outside the window's border that holds its shadow and resizes it.
const SHADOW: Pixels = px(10.);
const ROUNDING: Pixels = px(8.);

/// What to ask the compositor for when opening a window.
pub fn requested() -> Option<WindowDecorations> {
    if cfg!(target_os = "macos") {
        return None;
    }
    match std::env::var("GLEIPNIR_WINDOW_DECORATIONS").as_deref() {
        Ok("server") => Some(WindowDecorations::Server),
        _ => Some(WindowDecorations::Client),
    }
}

/// Whether the app draws the window's buttons itself: wherever it asked to decorate the window.
/// Some compositors answer that they will, and then draw no title bar at all, like Hyprland, so
/// their answer can't decide it; one that does draw a title bar honours the request.
pub fn draws_buttons(_window: &Window) -> bool {
    requested() == Some(WindowDecorations::Client)
}

/// Space between the window's edges and its content: the shadow, on each side that isn't tiled.
pub fn content_insets(window: &Window) -> Edges<Pixels> {
    match window.window_decorations() {
        Decorations::Client { tiling } if !cfg!(target_os = "macos") => {
            let side = |tiled: bool| if tiled { px(0.) } else { SHADOW };
            Edges {
                top: side(tiling.top),
                right: side(tiling.right),
                bottom: side(tiling.bottom),
                left: side(tiling.left),
            }
        }
        _ => Edges::default(),
    }
}

struct HoveredEdge(Option<ResizeEdge>);
impl Global for HoveredEdge {}

/// Frames the window's content: with decorations from the compositor this adds nothing.
pub fn frame(content: impl IntoElement, theme: &Theme, window: &mut Window) -> Stateful<gpui::Div> {
    let decorations = if cfg!(target_os = "macos") {
        Decorations::Server
    } else {
        window.window_decorations()
    };
    let Decorations::Client { tiling } = decorations else {
        window.set_client_inset(px(0.));
        return div().id("window-frame").size_full().child(content);
    };
    window.set_client_inset(SHADOW);
    let radius = |rounded: bool| if rounded { ROUNDING } else { px(0.) };
    let corners = gpui::Corners {
        top_left: radius(!tiling.top && !tiling.left),
        top_right: radius(!tiling.top && !tiling.right),
        bottom_left: radius(!tiling.bottom && !tiling.left),
        bottom_right: radius(!tiling.bottom && !tiling.right),
    };
    window.set_client_corner_radii(corners);
    let resizable = window.is_resizable() && !tiling.is_tiled();
    let insets = content_insets(window);

    let inner = div()
        .size_full()
        .overflow_hidden()
        .cursor(CursorStyle::Arrow)
        .rounded_tl(corners.top_left)
        .rounded_tr(corners.top_right)
        .rounded_bl(corners.bottom_left)
        .rounded_br(corners.bottom_right)
        .border_color(theme.border)
        .when(!tiling.top, |d| d.border_t_1())
        .when(!tiling.bottom, |d| d.border_b_1())
        .when(!tiling.left, |d| d.border_l_1())
        .when(!tiling.right, |d| d.border_r_1())
        .when(!tiling.is_tiled(), |d| {
            d.shadow(vec![
                gpui::BoxShadow::new(px(0.), px(0.), gpui::black().opacity(0.4))
                    .blur_radius(SHADOW / 2.),
            ])
        })
        // The content's own moves shouldn't reach the frame's resize handling.
        .on_mouse_move(|_, _, cx| cx.stop_propagation())
        .child(content);

    div()
        .id("window-frame")
        .size_full()
        .bg(gpui::transparent_black())
        .pt(insets.top)
        .pr(insets.right)
        .pb(insets.bottom)
        .pl(insets.left)
        .when(resizable, |d| {
            d.on_mouse_move(move |event, window, cx| {
                let edge = resize_edge(event.position, window_size(window), tiling);
                if cx.try_global::<HoveredEdge>().map(|e| e.0) != Some(edge) {
                    cx.set_global(HoveredEdge(edge));
                    window.refresh();
                }
            })
            .on_mouse_down(MouseButton::Left, move |event, window, _| {
                if let Some(edge) = resize_edge(event.position, window_size(window), tiling) {
                    window.start_window_resize(edge);
                }
            })
        })
        .child(inner)
        .when(resizable, |d| {
            // A cursor over the band that says which way it resizes.
            d.child(
                canvas(
                    |_, window, _| {
                        let whole = Bounds::new(point(px(0.), px(0.)), window_size(window));
                        window.insert_hitbox(whole, HitboxBehavior::Normal)
                    },
                    move |_, hitbox: Hitbox, window, _| {
                        let at = window.mouse_position();
                        if let Some(edge) = resize_edge(at, window_size(window), tiling) {
                            window.set_cursor_style(resize_cursor(edge), &hitbox);
                        }
                    },
                )
                .size_full()
                .absolute(),
            )
        })
}

fn window_size(window: &Window) -> Size<Pixels> {
    window.window_bounds().get_bounds().size
}

fn resize_cursor(edge: ResizeEdge) -> CursorStyle {
    match edge {
        ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
        ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
        ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
        ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
    }
}

/// Which edge or corner a point in the shadow band resizes, if any. Corners take a little more
/// room than the band, as they're hard to hit otherwise.
fn resize_edge(at: Point<Pixels>, window: Size<Pixels>, tiling: Tiling) -> Option<ResizeEdge> {
    let corner = SHADOW * 1.5;
    if Bounds::new(point(px(0.), px(0.)), window)
        .inset(corner)
        .contains(&at)
    {
        return None;
    }
    let near_left = at.x < corner;
    let near_right = at.x > window.width - corner;
    let near_top = at.y < corner;
    let near_bottom = at.y > window.height - corner;
    let corner_edge = match (near_top, near_bottom, near_left, near_right) {
        (true, _, true, _) if !tiling.top => Some(ResizeEdge::TopLeft),
        (true, _, _, true) if !tiling.top => Some(ResizeEdge::TopRight),
        (_, true, true, _) if !tiling.bottom => Some(ResizeEdge::BottomLeft),
        (_, true, _, true) if !tiling.bottom => Some(ResizeEdge::BottomRight),
        _ => None,
    };
    if corner_edge.is_some() {
        return corner_edge;
    }
    if !tiling.top && at.y < SHADOW {
        Some(ResizeEdge::Top)
    } else if !tiling.bottom && at.y > window.height - SHADOW {
        Some(ResizeEdge::Bottom)
    } else if !tiling.left && at.x < SHADOW {
        Some(ResizeEdge::Left)
    } else if !tiling.right && at.x > window.width - SHADOW {
        Some(ResizeEdge::Right)
    } else {
        None
    }
}

/// A window button, as the title bar draws it. Elsewhere than Linux only Close is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "freebsd")),
    expect(dead_code)
)]
pub enum WindowButton {
    Minimize,
    Maximize,
    Close,
}

/// The buttons for each end of the title bar, as the desktop orders them (GNOME's
/// `button-layout`, which other desktops follow too), less those the compositor can't do.
pub fn buttons(window: &Window, cx: &App) -> (Vec<WindowButton>, Vec<WindowButton>) {
    if !draws_buttons(window) {
        return (Vec::new(), Vec::new());
    }
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        let layout = cx
            .button_layout()
            .unwrap_or_else(gpui::WindowButtonLayout::linux_default);
        let controls = window.window_controls();
        let side = |buttons: [Option<gpui::WindowButton>; gpui::MAX_BUTTONS_PER_SIDE]| {
            buttons
                .into_iter()
                .flatten()
                .filter_map(|button| match button {
                    gpui::WindowButton::Minimize => {
                        controls.minimize.then_some(WindowButton::Minimize)
                    }
                    gpui::WindowButton::Maximize => {
                        controls.maximize.then_some(WindowButton::Maximize)
                    }
                    gpui::WindowButton::Close => Some(WindowButton::Close),
                })
                .collect()
        };
        (side(layout.left), side(layout.right))
    }
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    {
        let _ = cx;
        (Vec::new(), vec![WindowButton::Close])
    }
}

/// One button; `on_close` decides whether the window may close, as there may be unsaved work.
pub fn button(
    button: WindowButton,
    theme: &Theme,
    window: &Window,
    on_close: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    let (id, icon) = match button {
        WindowButton::Minimize => ("window-minimize", Icon::WindowMinimize),
        WindowButton::Maximize if window.is_maximized() => ("window-maximize", Icon::WindowRestore),
        WindowButton::Maximize => ("window-maximize", Icon::WindowMaximize),
        WindowButton::Close => ("window-close", Icon::WindowClose),
    };
    // Close turns red, as it does on the desktops this follows.
    let close = button == WindowButton::Close;
    let background = if close { theme.error } else { theme.hover };
    div()
        .id(id)
        .group(id)
        .debug_selector(move || id.into())
        .size(px(28.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .cursor_pointer()
        .hover(move |s| s.bg(background))
        .child(
            svg()
                .path(icon.path())
                .size(px(16.))
                .text_color(theme.text_muted)
                .when(close, |s| {
                    s.group_hover(id, |s| s.text_color(gpui::white()))
                }),
        )
        // Pressing a button mustn't start moving the window, as pressing the bar does.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, cx| match button {
            WindowButton::Minimize => window.minimize_window(),
            WindowButton::Maximize => window.zoom_window(),
            WindowButton::Close => on_close(window, cx),
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::size;

    const WINDOW: Size<Pixels> = size(px(800.), px(600.));

    #[test]
    fn the_band_resizes_and_corners_take_both_directions() {
        let floating = Tiling::default();
        let at = |x: f32, y: f32| resize_edge(point(px(x), px(y)), WINDOW, floating);
        assert_eq!(at(400., 300.), None);
        assert_eq!(at(400., 3.), Some(ResizeEdge::Top));
        assert_eq!(at(400., 597.), Some(ResizeEdge::Bottom));
        assert_eq!(at(3., 300.), Some(ResizeEdge::Left));
        assert_eq!(at(797., 300.), Some(ResizeEdge::Right));
        assert_eq!(at(3., 3.), Some(ResizeEdge::TopLeft));
        assert_eq!(at(797., 597.), Some(ResizeEdge::BottomRight));
        assert_eq!(at(12., 3.), Some(ResizeEdge::TopLeft));
        // Inside the border, but within the corner's extra room, nothing along an edge.
        assert_eq!(at(400., 12.), None);
    }

    #[test]
    fn tiled_edges_dont_resize() {
        let tiled = Tiling::tiled();
        let at = |x: f32, y: f32| resize_edge(point(px(x), px(y)), WINDOW, tiled);
        assert_eq!(at(400., 3.), None);
        assert_eq!(at(3., 3.), None);
        let left_half = Tiling {
            top: true,
            bottom: true,
            left: true,
            right: false,
        };
        let at = |x: f32, y: f32| resize_edge(point(px(x), px(y)), WINDOW, left_half);
        assert_eq!(at(797., 300.), Some(ResizeEdge::Right));
        assert_eq!(at(3., 300.), None);
    }
}
