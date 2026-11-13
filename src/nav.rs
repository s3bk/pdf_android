//! The bar along the bottom: contents, previous, next, the page number and a
//! progress track, drawn straight into the same vello `Scene` as the page.
//!
//! Everything here is in device pixels; `scale` is winit's scale factor, so the
//! sizes below are density-independent pixels.

use vello::kurbo::{Affine, BezPath, Rect};
use vello::peniko::{Color, Fill};
use vello::Scene;

use crate::text;

/// Height of the bar, in dp.
const BAR_H: f64 = 56.0;
/// Width of each of the three buttons, in dp. They are as tall as the bar.
const BUTTON_W: f64 = 56.0;
/// Half the side of the page-turn triangles, in dp.
const TRIANGLE: f64 = 10.0;
/// Font size of "page / pages", in dp.
const PAGE_PX: f64 = 14.0;

/// Which button a tap landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Prev,
    Next,
    Menu,
    Search,
    /// Open a PDF from a URL.
    Url,
}

/// Where the bar and its buttons ended up for one window size.
#[derive(Clone, Copy, Debug)]
pub struct Nav {
    /// The whole bar, at the bottom of the window.
    pub bar: Rect,
    /// The previous-page button.
    pub prev: Rect,
    /// The next-page button.
    pub next: Rect,
    /// The contents (outline) button.
    pub menu: Rect,
    /// The search button, just left of the next-page button.
    pub search: Rect,
    /// The address button, left of the search button.
    pub url: Rect,
}

impl Nav {
    /// Which button a tap at (`x`, `y`) hit, if any.
    pub fn hit(&self, x: f64, y: f64) -> Option<Hit> {
        if self.menu.contains((x, y)) {
            Some(Hit::Menu)
        } else if self.next.contains((x, y)) {
            Some(Hit::Next)
        } else if self.search.contains((x, y)) {
            Some(Hit::Search)
        } else if self.url.contains((x, y)) {
            Some(Hit::Url)
        } else if self.prev.contains((x, y)) {
            Some(Hit::Prev)
        } else {
            None
        }
    }
}

/// Lay the bar out for a `width` x `height` window whose bottom `bottom_inset`
/// pixels are covered by the system navigation bar / gesture pill. The bar is
/// painted all the way down (so the inset strip is not paper), but its buttons
/// stay above the inset.
pub fn layout(width: f64, height: f64, scale: f64, bottom_inset: f64) -> Nav {
    // Never let a bogus inset eat the window.
    let bottom = (height - bottom_inset.clamp(0.0, height / 4.0)).max(0.0);
    let bar_h = (BAR_H * scale).min(bottom);
    let button_w = (BUTTON_W * scale).max(1.0);
    let top = bottom - bar_h;
    let menu = Rect::new(0.0, top, button_w.min(width), bottom);
    let prev = Rect::new(
        button_w.min(width),
        top,
        (2.0 * button_w).min(width),
        bottom,
    );
    Nav {
        bar: Rect::new(0.0, top, width, height),
        prev,
        next: Rect::new((width - button_w).max(0.0), top, width, bottom),
        search: Rect::new(
            (width - 2.0 * button_w).max(0.0),
            top,
            (width - button_w).max(0.0),
            bottom,
        ),
        url: Rect::new(
            (width - 3.0 * button_w).max(0.0),
            top,
            (width - 2.0 * button_w).max(0.0),
            bottom,
        ),
        menu,
    }
}

/// Draw the bar for `page` (0-based) of `pages`. `has_outline` dims the
/// contents button when the document has no bookmarks; `open` highlights it
/// while the drawer is up.
pub fn draw(
    scene: &mut Scene,
    width: f64,
    height: f64,
    scale: f64,
    bottom_inset: f64,
    page: usize,
    pages: usize,
    has_outline: bool,
    open: bool,
    searching: bool,
    addressing: bool,
) {
    let nav = layout(width, height, scale, bottom_inset);
    let top = nav.bar.y0;
    let bottom = nav.prev.y1;
    let mid = (top + bottom) / 2.0;
    let at_first = page == 0;
    let at_last = pages == 0 || page + 1 >= pages;

    // The bar itself, and a hairline above it so a white page does not bleed in.
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgba8(0, 0, 0, 60),
        None,
        &Rect::new(0.0, top - 1.0, width, top),
    );
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgba8(22, 22, 26, 255),
        None,
        &nav.bar,
    );

    // Progress: a 3dp track along the very top of the bar, filled up to the
    // current page.
    let track_h = (3.0 * scale).max(1.0);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgba8(58, 58, 66, 255),
        None,
        &Rect::new(0.0, top, width, top + track_h),
    );
    if pages > 0 {
        let fraction = (page + 1) as f64 / pages as f64;
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(122, 156, 240, 255),
            None,
            &Rect::new(0.0, top, width * fraction, top + track_h),
        );
    }

    let lit = Color::from_rgba8(235, 235, 242, 255);
    let dim = Color::from_rgba8(84, 84, 92, 255);

    // The two page buttons: a triangle each, dimmed at the ends of the document.
    let half = TRIANGLE * scale;
    let prev_c = nav.prev.center();
    let next_c = nav.next.center();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        if at_first { dim } else { lit },
        None,
        &triangle(prev_c.x + half * 0.45, mid, half, false),
    );
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        if at_last { dim } else { lit },
        None,
        &triangle(next_c.x - half * 0.45, mid, half, true),
    );

    // The contents button: three rows of "bullet, line", the usual outline mark.
    let menu_c = nav.menu.center();
    let mark = if !has_outline {
        dim
    } else if open {
        Color::from_rgba8(122, 156, 240, 255)
    } else {
        lit
    };
    let row_gap = 6.0 * scale;
    let bullet = (3.0 * scale).max(1.0);
    let line_w = 11.0 * scale;
    for i in 0..3 {
        let y = menu_c.y + (i as f64 - 1.0) * row_gap;
        let x = menu_c.x - 9.0 * scale;
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            mark,
            None,
            &Rect::new(x, y - bullet / 2.0, x + bullet, y + bullet / 2.0),
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            mark,
            None,
            &Rect::new(
                x + bullet * 2.5,
                y - bullet / 2.0,
                x + bullet * 2.5 + line_w,
                y + bullet / 2.0,
            ),
        );
    }

    // The search button: a magnifier, lit while the panel is up.
    let search_c = nav.search.center();
    let lens = 7.0 * scale;
    let ring =
        vello::kurbo::Circle::new((search_c.x - 1.5 * scale, search_c.y - 1.5 * scale), lens);
    scene.stroke(
        &vello::kurbo::Stroke::new((2.0 * scale).max(1.0)),
        Affine::IDENTITY,
        if searching {
            Color::from_rgba8(122, 156, 240, 255)
        } else {
            lit
        },
        None,
        &ring,
    );
    let mut handle = BezPath::new();
    handle.move_to((
        search_c.x - 1.5 * scale + lens * 0.72,
        search_c.y - 1.5 * scale + lens * 0.72,
    ));
    handle.line_to((
        search_c.x - 1.5 * scale + lens * 1.55,
        search_c.y - 1.5 * scale + lens * 1.55,
    ));
    scene.stroke(
        &vello::kurbo::Stroke::new((2.0 * scale).max(1.0)),
        Affine::IDENTITY,
        if searching {
            Color::from_rgba8(122, 156, 240, 255)
        } else {
            lit
        },
        None,
        &handle,
    );

    // The address button: a globe, lit while the address bar is up. It is how
    // a document that is not on the phone gets in.
    let globe_c = nav.url.center();
    let globe_r = 8.0 * scale;
    let tone = if addressing {
        Color::from_rgba8(122, 156, 240, 255)
    } else {
        lit
    };
    scene.stroke(
        &vello::kurbo::Stroke::new((1.8 * scale).max(1.0)),
        Affine::IDENTITY,
        tone,
        None,
        &vello::kurbo::Circle::new(globe_c, globe_r),
    );
    let mut equator = BezPath::new();
    equator.move_to((globe_c.x - globe_r, globe_c.y));
    equator.line_to((globe_c.x + globe_r, globe_c.y));
    scene.stroke(
        &vello::kurbo::Stroke::new((1.4 * scale).max(1.0)),
        Affine::IDENTITY,
        tone,
        None,
        &equator,
    );
    // The meridian: an ellipse seen edge-on, so it reads as a globe and not as
    // a circle with a line through it.
    let mut meridian = BezPath::new();
    meridian.move_to((globe_c.x, globe_c.y - globe_r));
    meridian.curve_to(
        (globe_c.x + globe_r * 0.62, globe_c.y - globe_r * 0.55),
        (globe_c.x + globe_r * 0.62, globe_c.y + globe_r * 0.55),
        (globe_c.x, globe_c.y + globe_r),
    );
    meridian.curve_to(
        (globe_c.x - globe_r * 0.62, globe_c.y + globe_r * 0.55),
        (globe_c.x - globe_r * 0.62, globe_c.y - globe_r * 0.55),
        (globe_c.x, globe_c.y - globe_r),
    );
    scene.stroke(
        &vello::kurbo::Stroke::new((1.4 * scale).max(1.0)),
        Affine::IDENTITY,
        tone,
        None,
        &meridian,
    );

    // "page / pages", centred between the buttons.
    if pages > 0 {
        let px = PAGE_PX * scale;
        text::draw_centred(
            scene,
            &format!("{} / {}", page + 1, pages),
            width / 2.0,
            text::baseline(mid, px),
            px,
            Color::from_rgba8(214, 214, 222, 255),
        );
    }
}

/// A left- or right-pointing triangle, `half` from the centre to each corner.
fn triangle(cx: f64, cy: f64, half: f64, rightwards: bool) -> BezPath {
    let (tip, back) = if rightwards {
        (cx + half, cx - half * 0.8)
    } else {
        (cx - half, cx + half * 0.8)
    };
    let mut path = BezPath::new();
    path.move_to((tip, cy));
    path.line_to((back, cy - half));
    path.line_to((back, cy + half));
    path.close_path();
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_test_buttons_only() {
        let nav = layout(1080.0, 2400.0, 3.0, 0.0);
        assert_eq!(nav.hit(540.0, 2385.0), None); // middle of the bar
        assert_eq!(nav.hit(84.0, 2385.0), Some(Hit::Menu));
        assert_eq!(nav.hit(252.0, 2385.0), Some(Hit::Prev));
        assert_eq!(nav.hit(990.0, 2385.0), Some(Hit::Next));
    }

    #[test]
    fn inset_keeps_buttons_up() {
        let nav = layout(1080.0, 2400.0, 3.0, 63.0);
        assert!((nav.bar.y1 - 2400.0).abs() < 0.01);
        assert!((nav.next.y1 - (2400.0 - 63.0)).abs() < 0.01);
    }

    #[test]
    fn a_narrow_window_does_not_invert_the_buttons() {
        let nav = layout(100.0, 200.0, 3.0, 0.0);
        assert!(nav.prev.x1 <= nav.next.x0 + 0.01);
    }
}
