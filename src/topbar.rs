//! The strip in the display cutout's safe area: which document is open, and how
//! much of it has arrived.
//!
//! The window is drawn edge to edge, so on a phone with a camera notch the top
//! `top_inset` pixels are a band that nothing else uses: the page starts below
//! it. Putting the title and the download numbers there means the reader can
//! tell what they are looking at — and how much of it is still coming — without
//! giving up a line of the page.
//!
//! The strip belongs to the same chrome as the bar at the bottom and goes away
//! with it (see `App::bars_off`); it is drawn only while the bar is.

use vello::kurbo::Rect;
use vello::peniko::{Color, Fill};
use vello::Scene;

use crate::text;

/// Text size, in dp: small enough to be a caption, big enough to read.
const PX: f64 = 11.0;
/// Padding at the left and right ends of the strip, in dp.
const PAD: f64 = 12.0;

/// Draw the strip over the top `top_inset` pixels.
///
/// `title` is the document's name; `stats` its download numbers. Either may be
/// empty. The camera is in the middle of the strip, so the title is confined to
/// the left half and the numbers are right-aligned against the right edge.
pub fn draw(scene: &mut Scene, width: f64, top_inset: f64, scale: f64, title: &str, stats: &str) {
    if top_inset < 8.0 * scale {
        // No cutout and no status bar: there is no band to put anything in, and
        // the page starts at the top of the window.
        return;
    }
    let h = top_inset;
    // A plate, not a translucent wash: the page under it would make the small
    // text hard to read, and this way the strip reads as part of the chrome.
    scene.fill(
        Fill::NonZero,
        vello::kurbo::Affine::IDENTITY,
        Color::from_rgba8(22, 22, 26, 255),
        None,
        &Rect::new(0.0, 0.0, width, h),
    );
    scene.fill(
        Fill::NonZero,
        vello::kurbo::Affine::IDENTITY,
        Color::from_rgba8(44, 44, 52, 255),
        None,
        &Rect::new(0.0, h - 1.0, width, h),
    );

    let px = PX * scale;
    let y = text::baseline(h * 0.5, px);
    let pad = PAD * scale;
    // Room on each side of the camera: the notch sits in the middle of the
    // strip and text under it would be invisible.
    let room = (width * 0.5 - pad * 2.0).max(0.0);
    if !title.is_empty() {
        let fitted = text::fit(title, px, room);
        text::draw(
            scene,
            &fitted,
            pad,
            y,
            px,
            Color::from_rgba8(226, 228, 238, 255),
        );
    }
    if !stats.is_empty() {
        let fitted = text::fit(stats, px, room);
        let w = text::width(&fitted, px);
        text::draw(
            scene,
            &fitted,
            (width - pad - w).max(pad),
            y,
            px,
            Color::from_rgba8(146, 149, 162, 255),
        );
    }
}
