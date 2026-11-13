//! The address bar: type a URL, open the PDF it points at.
//!
//! It sits at the top of the window rather than the bottom, because the system
//! keyboard covers the bottom half of the screen, and it is drawn into the same
//! vello scene as everything else.
//!
//! The text arrives from the framework's own field: [`crate::field`] puts a
//! real `EditText` over the pill this module draws, so an input method can
//! compose into it (autocorrect, swipe typing, emoji, dictation, paste, cursor
//! keys, selection). The bar draws the pill and the buttons; the field draws
//! what is inside it, which is why the text and the caret below are skipped
//! when `View::native` is set.

use vello::kurbo::{Affine, BezPath, Cap, Join, Rect, Stroke};
use vello::peniko::{Color, Fill};
use vello::Scene;

use crate::text;

/// Height of the bar, in dp.
const BAR_H: f64 = 60.0;
/// Height of the status line under it, in dp.
const STATUS_H: f64 = 26.0;
/// Height of the text field, in dp.
const FIELD_H: f64 = 40.0;
/// Font size of the URL, in dp.
pub const URL_PX: f64 = 15.0;

/// Where the bar's parts ended up for one window size.
#[derive(Clone, Copy, Debug)]
pub struct Panel {
    /// The whole bar, including the status line.
    pub area: Rect,
    /// The text field.
    pub field: Rect,
    /// The "open" button at the right of the field.
    pub go: Rect,
    /// The close cross, right of the button.
    pub close: Rect,
    /// Where the status line is drawn.
    pub status: Rect,
    pub scale: f64,
}

impl Panel {
    /// Lay the bar out across the top of a `width` x `height` window, below
    /// `inset` pixels of display cutout or status bar. With the window let into
    /// the cutout (`shortEdges`), y = 0 is behind the camera, and a text field
    /// there is a text field nobody can read.
    pub fn new(width: f64, height: f64, scale: f64, inset: f64) -> Panel {
        let inset = inset.max(0.0);
        let bar_h = (BAR_H * scale).min(height.max(1.0));
        let status_h = (STATUS_H * scale).min((height - inset - bar_h).max(0.0));
        let pad = 8.0 * scale;
        let field_h = (FIELD_H * scale).min(bar_h).max(8.0);
        let top = inset + (bar_h - field_h) * 0.5;
        let close_w = 34.0 * scale;
        let go_w = 52.0 * scale;
        let field_x1 = (width - pad - close_w - go_w - pad * 0.5).max(pad + 1.0);
        Panel {
            area: Rect::new(0.0, inset, width, inset + bar_h + status_h),
            field: Rect::new(pad, top, field_x1, top + field_h),
            go: Rect::new(field_x1 + pad * 0.5, top, field_x1 + pad * 0.5 + go_w, top + field_h),
            close: Rect::new(
                (field_x1 + pad * 0.5 + go_w + pad * 0.5).min(width),
                top,
                width - pad * 0.5,
                top + field_h,
            ),
            status: Rect::new(pad, inset + bar_h, width, inset + bar_h + status_h),
            scale,
        }
    }
}

/// What the bar shows.
pub struct View<'a> {
    pub text: &'a str,
    /// A line under the bar: what is happening, or what went wrong.
    pub status: Option<&'a str>,
    /// Whether a document is being opened: the button turns into a spinner-ish
    /// dot and is not worth pressing again.
    pub busy: bool,
    /// The framework's own `EditText` sits on the field, text and caret and
    /// all: the bar then draws the pill and nothing inside it.
    pub native: bool,
}

/// Draw the bar over the top of the page.
pub fn draw(scene: &mut Scene, panel: &Panel, view: &View) {
    let scale = panel.scale;
    // Above the bar is the display cutout strip: pure black, so the camera
    // hole sits on black and the bar's own edge is a visible line. Drawn first,
    // and reaching to y = 0, because the window is let into the cutout.
    if panel.area.y0 > 0.0 {
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(0, 0, 0, 255),
            None,
            &Rect::new(0.0, 0.0, panel.area.x1, panel.area.y0),
        );
    }
    // The bar itself, opaque enough to read against a white page.
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgba8(24, 24, 30, 255),
        None,
        &Rect::new(0.0, panel.area.y0, panel.area.x1, panel.area.y1),
    );
    // The field.
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgba8(42, 42, 49, 255),
        None,
        &panel.field.to_rounded_rect(9.0 * scale),
    );
    let px = URL_PX * scale;
    let baseline = text::baseline(panel.field.center().y, px);
    let left = panel.field.x0 + 12.0 * scale;
    let right = panel.field.x1 - 10.0 * scale;
    if view.native {
        // The framework's field draws its own text, hint, caret and selection
        // over this pill.
    } else if view.text.is_empty() {
        text::draw(
            scene,
            "https://example.com/file.pdf",
            left,
            baseline,
            px,
            Color::from_rgba8(126, 128, 140, 255),
        );
    } else {
        // Keep the end of a long URL in sight: the caret is where typing
        // happens, and the scheme is the least interesting part of it.
        let full = text::width(view.text, px);
        let room = (right - left).max(1.0);
        let shift = (full + 2.0 * scale - room).max(0.0);
        text::draw(
            scene,
            view.text,
            left - shift,
            baseline,
            px,
            Color::from_rgba8(236, 237, 244, 255),
        );
        let caret_x = (left + full - shift + 1.0).min(right);
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(122, 156, 240, 255),
            None,
            &Rect::new(
                caret_x.max(left),
                panel.field.y0 + 8.0 * scale,
                caret_x.max(left) + 2.0 * scale,
                panel.field.y1 - 8.0 * scale,
            ),
        );
    }

    // The open button: a filled pill with an arrow leaving a tray, or a dot
    // while a document is on its way.
    let lit = if view.busy {
        Color::from_rgba8(70, 88, 130, 255)
    } else {
        Color::from_rgba8(122, 156, 240, 255)
    };
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        lit,
        None,
        &panel.go.to_rounded_rect(9.0 * scale),
    );
    let c = panel.go.center();
    if view.busy {
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(220, 226, 244, 255),
            None,
            &vello::kurbo::Circle::new(c, 4.0 * scale),
        );
    } else {
        // One chevron arrow, one path: a shaft and a head, round caps and
        // joins. Drawn as two paths with square ends it read as a stray corner
        // over a dash.
        let ink = Color::from_rgba8(16, 20, 30, 255);
        let pen = Stroke::new(2.4 * scale).with_caps(Cap::Round).with_join(Join::Round);
        let mut arrow = BezPath::new();
        arrow.move_to((c.x - 7.0 * scale, c.y));
        arrow.line_to((c.x + 5.0 * scale, c.y));
        arrow.move_to((c.x + 0.0 * scale, c.y - 5.5 * scale));
        arrow.line_to((c.x + 5.5 * scale, c.y));
        arrow.line_to((c.x + 0.0 * scale, c.y + 5.5 * scale));
        scene.stroke(&pen, Affine::IDENTITY, ink, None, &arrow);
    }

    // The close cross.
    let x = panel.close.center();
    let arm = 6.5 * scale;
    let mut cross = BezPath::new();
    cross.move_to((x.x - arm, x.y - arm));
    cross.line_to((x.x + arm, x.y + arm));
    cross.move_to((x.x + arm, x.y - arm));
    cross.line_to((x.x - arm, x.y + arm));
    scene.stroke(
        &Stroke::new(2.0 * scale),
        Affine::IDENTITY,
        Color::from_rgba8(180, 182, 196, 255),
        None,
        &cross,
    );

    // The status line, if there is anything to say.
    if let Some(status) = view.status {
        let px = 12.5 * scale;
        text::draw(
            scene,
            status,
            panel.status.x0,
            text::baseline(panel.status.center().y, px),
            px,
            Color::from_rgba8(158, 162, 178, 255),
        );
    }
}
