//! A tab that shows an image instead of text: raster formats and SVG, fitted to the tab until it's
//! zoomed, over a checkerboard so transparency shows. It follows the file as it changes on disk,
//! and an SVG, being text too, can be opened in an editor from here.

use crate::Workspace;
use gleipnir_ui::Theme;
use gpui::{
    App, Bounds, Context, FocusHandle, Focusable, ImgResourceLoader, KeyBinding, MouseButton,
    Pixels, RenderImage, Resource, ScrollEvent, ScrollHandle, SharedString, Task, WeakEntity,
    Window, actions, canvas, div, fill, img, point, prelude::*, px, size,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

actions!(
    image_view,
    [
        /// Zooms the image in a step.
        ZoomIn,
        /// Zooms the image out a step.
        ZoomOut,
        /// Shows the image at its own size, a pixel of it to a pixel on screen.
        ActualSize,
        /// Fits the image to the tab, never past its own size.
        ZoomToFit,
        /// Opens an SVG's text in an editor.
        OpenAsText,
    ]
);

/// The zoom levels stepped through, as fractions of the image's own size.
const ZOOM_STEPS: &[f32] = &[
    0.05, 0.1, 0.25, 0.33, 0.5, 0.67, 0.75, 1., 1.25, 1.5, 2., 3., 4., 6., 8., 12., 16., 32.,
];
const CHECKER: f32 = 8.;
/// How often the file is looked at again, for changes made outside.
const POLL: Duration = Duration::from_secs(1);

/// Whether a file opens as an image: whatever GPUI can draw, by extension.
pub fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            gpui::Img::extensions()
                .iter()
                .any(|known| known.eq_ignore_ascii_case(ext))
        })
}

fn is_svg(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
}

pub fn bind_keys(cx: &mut App) {
    let context = Some("ImageView");
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-=", ZoomIn, context),
        KeyBinding::new("cmd-+", ZoomIn, context),
        KeyBinding::new("cmd--", ZoomOut, context),
        KeyBinding::new("cmd-0", ActualSize, context),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-=", ZoomIn, context),
        KeyBinding::new("ctrl-+", ZoomIn, context),
        KeyBinding::new("ctrl--", ZoomOut, context),
        KeyBinding::new("ctrl-0", ActualSize, context),
    ]);
}

pub struct ImageView {
    path: PathBuf,
    focus_handle: FocusHandle,
    workspace: WeakEntity<Workspace>,
    /// The zoom, or none to fit the tab.
    zoom: Option<f32>,
    scroll: ScrollHandle,
    /// The file's modification time and size when last looked at.
    disk: Option<(SystemTime, u64)>,
    _poll: Task<()>,
}

impl ImageView {
    pub fn new(path: PathBuf, workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                if this.update(cx, |this, cx| this.check_disk(cx)).is_err() {
                    break;
                }
            }
        });
        Self {
            disk: disk_state(&path),
            path,
            focus_handle: cx.focus_handle(),
            workspace,
            zoom: None,
            scroll: ScrollHandle::new(),
            _poll: poll,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The zoom, or none when fitted to the tab.
    pub fn zoom(&self) -> Option<f32> {
        self.zoom
    }

    fn resource(&self) -> Resource {
        Resource::Path(Arc::from(self.path.as_path()))
    }

    /// Drops the decoded image when the file changed, so the next frame reads it again.
    fn check_disk(&mut self, cx: &mut Context<Self>) {
        let now = disk_state(&self.path);
        if now != self.disk {
            self.disk = now;
            cx.remove_asset::<ImgResourceLoader>(&self.resource());
            cx.notify();
        }
    }

    /// How much the image is scaled now: its zoom, or what fitting it to the tab takes.
    fn scale(&self, image: Bounds<Pixels>) -> f32 {
        self.zoom.unwrap_or_else(|| {
            let viewport = self.scroll.bounds().size;
            if image.size.width <= px(0.) || image.size.height <= px(0.) {
                return 1.;
            }
            (viewport.width / image.size.width)
                .min(viewport.height / image.size.height)
                .min(1.)
        })
    }

    fn step(&mut self, image: Option<Bounds<Pixels>>, larger: bool, cx: &mut Context<Self>) {
        let current = image.map_or(1., |image| self.scale(image));
        let next = if larger {
            ZOOM_STEPS.iter().copied().find(|z| *z > current + 0.001)
        } else {
            ZOOM_STEPS
                .iter()
                .rev()
                .copied()
                .find(|z| *z < current - 0.001)
        };
        if let Some(next) = next {
            self.zoom = Some(next);
            cx.notify();
        }
    }

    fn open_as_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.path.clone();
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |workspace, cx| {
                workspace.open_as_text(path, window, cx).detach();
            });
        }
    }
}

fn disk_state(path: &Path) -> Option<(SystemTime, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

fn image_size(image: &RenderImage) -> Bounds<Pixels> {
    let size = image.size(0);
    Bounds::new(
        point(px(0.), px(0.)),
        gpui::size(px(size.width.0 as f32), px(size.height.0 as f32)),
    )
}

fn format_bytes(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.),
        b => format!("{:.1} MB", b as f64 / (1024. * 1024.)),
    }
}

impl Focusable for ImageView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ImageView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let loaded = window.use_asset::<ImgResourceLoader>(&self.resource(), cx);
        let image = loaded.as_ref().and_then(|l| l.as_ref().ok()).cloned();
        let natural = image.as_deref().map(image_size);
        // Fitting needs the tab's size, known once it's been laid out.
        if self.zoom.is_none() && self.scroll.bounds().size.width <= px(0.) {
            cx.on_next_frame(window, |_, _, cx| cx.notify());
        }
        let scale = natural.map(|n| self.scale(n));

        let format = self
            .path
            .extension()
            .map(|e| e.to_string_lossy().to_uppercase())
            .unwrap_or_default();
        let mut info = vec![format];
        if let Some(natural) = natural {
            info.push(format!(
                "{}×{}",
                f32::from(natural.size.width) as u32,
                f32::from(natural.size.height) as u32
            ));
        }
        if let Some((_, bytes)) = self.disk {
            info.push(format_bytes(bytes));
        }
        let zoom_label: SharedString = match (self.zoom, scale) {
            (None, Some(scale)) => format!("Fit ({:.0}%)", scale * 100.).into(),
            (_, Some(scale)) => format!("{:.0}%", scale * 100.).into(),
            _ => "".into(),
        };

        let button = |id: &'static str, label: &'static str, theme: &Theme| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .px(px(8.))
                .py(px(2.))
                .rounded(px(4.))
                .cursor_pointer()
                .text_color(theme.text_muted)
                .hover(|s| s.bg(theme.hover).text_color(theme.text))
                .child(label)
        };
        let toolbar = div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .h(px(30.))
            .px(px(10.))
            .bg(theme.surface)
            .border_b_1()
            .border_color(theme.border)
            .text_size(px(12.))
            .child(
                div()
                    .debug_selector(|| "image-info".into())
                    .text_color(theme.text_muted)
                    .child(info.join(" · ")),
            )
            .child(div().flex_1())
            .child(
                div()
                    .debug_selector(|| "image-zoom".into())
                    .text_color(theme.text_muted)
                    .px(px(6.))
                    .child(zoom_label),
            )
            .child(
                button("image-zoom-out", "−", &theme)
                    .on_click(cx.listener(move |this, _, _, cx| this.step(natural, false, cx))),
            )
            .child(
                button("image-zoom-in", "+", &theme)
                    .on_click(cx.listener(move |this, _, _, cx| this.step(natural, true, cx))),
            )
            .child(
                button("image-actual-size", "1:1", &theme).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.zoom = Some(1.);
                        cx.notify();
                    },
                )),
            )
            .child(
                button("image-fit", "Fit", &theme).on_click(cx.listener(|this, _, _, cx| {
                    this.zoom = None;
                    cx.notify();
                })),
            )
            .when(is_svg(&self.path), |bar| {
                bar.child(
                    button("image-open-as-text", "Edit as Text", &theme)
                        .on_click(cx.listener(|this, _, window, cx| this.open_as_text(window, cx))),
                )
            });

        let content = match (&loaded, image, natural, scale) {
            (_, Some(image), Some(natural), Some(scale)) => {
                let shown = size(natural.size.width * scale, natural.size.height * scale);
                let (light, dark) = (theme.surface_raised, theme.background);
                div()
                    // A margin of auto centres it while it fits, and lets it scroll once it doesn't.
                    .m_auto()
                    .flex_none()
                    .w(shown.width)
                    .h(shown.height)
                    .relative()
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |bounds, _, window, _| paint_checker(bounds, light, dark, window),
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(img(gpui::ImageSource::Render(image)).absolute().size_full())
                    .into_any_element()
            }
            (Some(Err(error)), ..) => div()
                .m_auto()
                .text_color(theme.text_muted)
                .child(format!("Can't show this image: {error}"))
                .into_any_element(),
            _ => div().into_any_element(),
        };

        div()
            .key_context("ImageView")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(move |this, _: &ZoomIn, _, cx| this.step(natural, true, cx)))
            .on_action(cx.listener(move |this, _: &ZoomOut, _, cx| this.step(natural, false, cx)))
            .on_action(cx.listener(|this, _: &ActualSize, _, cx| {
                this.zoom = Some(1.);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ZoomToFit, _, cx| {
                this.zoom = None;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenAsText, window, cx| {
                if is_svg(&this.path) {
                    this.open_as_text(window, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| window.focus(&this.focus_handle, cx)),
            )
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.editor_background)
            .child(toolbar)
            .child(
                div()
                    .id("image-viewport")
                    .debug_selector(|| "image-viewport".into())
                    .track_scroll(&self.scroll)
                    .overflow_scroll()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .p(px(16.))
                    // Ctrl (Cmd on macOS) with the wheel zooms, as in browsers.
                    .on_scroll(cx.listener(move |this, event: &ScrollEvent, _, cx| {
                        if !event.modifiers.secondary(cx) {
                            return;
                        }
                        let up = event.delta.pixel_delta(px(16.)).y > px(0.);
                        this.step(natural, up, cx);
                        cx.stop_propagation();
                    }))
                    .child(content),
            )
    }
}

/// Squares of two colours behind the image, only where they can be seen.
fn paint_checker(bounds: Bounds<Pixels>, light: gpui::Hsla, dark: gpui::Hsla, window: &mut Window) {
    window.paint_quad(fill(bounds, dark));
    let visible = bounds.intersect(&window.content_mask().bounds);
    if visible.size.width <= px(0.) || visible.size.height <= px(0.) {
        return;
    }
    let cell = px(CHECKER);
    let first_col = ((visible.left() - bounds.left()) / cell).floor() as i64;
    let last_col = ((visible.right() - bounds.left()) / cell).ceil() as i64;
    let first_row = ((visible.top() - bounds.top()) / cell).floor() as i64;
    let last_row = ((visible.bottom() - bounds.top()) / cell).ceil() as i64;
    for row in first_row..last_row {
        for col in first_col..last_col {
            if (row + col) % 2 != 0 {
                continue;
            }
            let origin = point(
                bounds.left() + cell * col as f32,
                bounds.top() + cell * row as f32,
            );
            let square = Bounds::new(origin, size(cell, cell)).intersect(&bounds);
            window.paint_quad(fill(square, light));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_are_known_by_extension_in_any_case() {
        assert!(is_image(Path::new("a/logo.png")));
        assert!(is_image(Path::new("icon.SVG")));
        assert!(is_image(Path::new("photo.JPeG")));
        assert!(!is_image(Path::new("main.rs")));
        assert!(!is_image(Path::new("Makefile")));
        assert!(is_svg(Path::new("x.svg")));
        assert!(!is_svg(Path::new("x.png")));
    }

    #[test]
    fn sizes_read_as_people_write_them() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(3 * 1024 * 1024), "3.0 MB");
    }
}
