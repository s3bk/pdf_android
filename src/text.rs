//! Real glyphs for the few words of UI there are: the page number in the bar,
//! the outline drawer's titles and its page numbers.
//!
//! The outlines come from the `font` crate that `pdf_render` already parses
//! document fonts with — same parser, same revision — so the APK carries no
//! second text stack, just one font file (Inter, variable weight; the default
//! instance is the regular one).
//!
//! Shapes are cached per glyph in *em* units, y down, and placed by a plain
//! affine, so drawing a line of text is a handful of `Scene::fill` calls.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use font::pathfinder_impl::{flatten, OutlineEncoder};
use font::{Font, FontVariant, GlyphId};
use pdf_render::vello_backend::outline_to_bez;
use vello::kurbo::{Affine, BezPath};
use vello::peniko::{Color, Fill};
use vello::Scene;

/// The UI face: Inter at its default instance.
const INTER: &[u8] = include_bytes!("../assets/Inter.ttf");

/// If the bundled face will not parse, borrow one the platform ships: bar text
/// is numbers, and a viewer with no numbers is worse than one in Roboto.
const SYSTEM_FONTS: &[&str] = &[
    "/system/fonts/Roboto-Regular.ttf",
    "/system/fonts/NotoSans-Regular.ttf",
    "/system/fonts/DroidSans.ttf",
];

struct Ui {
    font: FontVariant<OutlineEncoder>,
    /// Glyph outlines in em units, y down.
    em: HashMap<u32, BezPath>,
    /// Glyph space -> em: x scale, and y scale (negated: the fonts are y-up,
    /// the scene is y-down, and `font_matrix` itself does not flip).
    sx: f64,
    sy: f64,
    /// Glyph space -> em: translation.
    tx: f64,
    ty: f64,
}

impl Ui {
    fn new() -> Option<Ui> {
        let started = std::time::Instant::now();
        let mut encoder = OutlineEncoder;
        let font: FontVariant<OutlineEncoder> = match font::parse(&INTER[..], &mut encoder) {
            Ok(font) => font,
            Err(e) => {
                // The bundled face is the one that should parse; if it does
                // not, the phone's own font is the difference between a bar
                // with numbers and a bar with nothing.
                log::error!("ui font: {e:?}; trying the system font");
                match SYSTEM_FONTS
                    .iter()
                    .find_map(|path| std::fs::read(path).ok())
                    .and_then(|bytes| font::parse(&bytes[..], &mut encoder).ok())
                {
                    Some(font) => font,
                    None => return None,
                }
            }
        };
        let m = font.font_matrix();
        log::info!(
            "ui font {:?}, {} glyphs, in {:.0} ms",
            font.name().full_name.as_deref().unwrap_or("?"),
            font.num_glyphs(),
            started.elapsed().as_secs_f64() * 1000.0
        );
        Some(Ui {
            font,
            em: HashMap::new(),
            sx: m.m11() as f64,
            sy: -(m.m22() as f64),
            tx: m.m13() as f64,
            ty: m.m23() as f64,
        })
    }

    /// The outline of `chars` in em units, cached. Unknown characters fall back
    /// to the space advance (nothing to draw).
    fn path(&mut self, gid: GlyphId) -> Option<&BezPath> {
        let key = gid.0;
        if !self.em.contains_key(&key) {
            let path = match flatten(&self.font, gid) {
                Some(outline) => {
                    let mut bez = outline_to_bez(&outline);
                    // glyph space (y up, font units) -> em (y down)
                    bez.apply_affine(Affine::new([self.sx, 0.0, 0.0, self.sy, self.tx, self.ty]));
                    bez
                }
                None => BezPath::new(),
            };
            self.em.insert(key, path);
        }
        self.em.get(&key)
    }

    /// One glyph's advance in em units.
    fn advance(&self, gid: GlyphId) -> f64 {
        self.font
            .glyph(gid)
            .map(|g| g.metrics.advance as f64 * self.sx)
            .unwrap_or(0.0)
    }

    fn gid(&self, ch: char) -> Option<GlyphId> {
        self.font.gid_for_unicode_codepoint(ch as u32)
    }

    fn run(&mut self, text: &str, px: f64) -> (Vec<(u32, f64)>, f64) {
        let mut out = Vec::with_capacity(text.len());
        let mut pen = 0.0;
        let mut last: Option<GlyphId> = None;
        for ch in text.chars() {
            let Some(gid) = self.gid(ch) else {
                pen += 0.28; // a missing character still takes room
                last = None;
                continue;
            };
            if let Some(left) = last {
                pen += self.font.kerning(left, gid) as f64 * self.sx;
            }
            out.push((gid.0, pen * px));
            pen += self.advance(gid);
            last = Some(gid);
        }
        (out, pen * px)
    }
}

static UI: OnceLock<Mutex<Option<Ui>>> = OnceLock::new();
static STARTED: OnceLock<()> = OnceLock::new();
/// Bumped when the font finishes parsing, so a frame that was drawn without
/// text can be rebuilt with it.
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Called on the parsing thread once the font is there: the window asks for a
/// new frame. Inter takes seconds to parse on a phone, and a viewer that waits
/// for its own bar text before showing the page looks broken.
static WAKE: Mutex<Option<Arc<dyn Fn() + Send + Sync>>> = Mutex::new(None);

/// Give the text module a way to ask for a redraw.
pub fn set_wake(f: impl Fn() + Send + Sync + 'static) {
    if let Ok(mut slot) = WAKE.lock() {
        *slot = Some(Arc::new(f));
    }
}

fn with<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    let slot = UI.get_or_init(|| Mutex::new(None));
    // Parsing happens once, off the frame: the first frames simply have no
    // text, and the redraw the parser asks for brings it in.
    STARTED.get_or_init(|| {
        std::thread::spawn(|| {
            let parsed = Ui::new();
            if let Ok(mut guard) = UI.get_or_init(|| Mutex::new(None)).lock() {
                *guard = parsed;
            }
            GENERATION.fetch_add(1, Ordering::Release);
            let wake = WAKE.lock().ok().and_then(|slot| slot.clone());
            if let Some(wake) = wake {
                wake();
            }
        });
    });
    let mut guard = match slot.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.as_mut().map(f)
}

/// How many times the font has become available. `0` means "not yet".
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

/// Width of `text` at `px` pixels per em.
pub fn width(text: &str, px: f64) -> f64 {
    with(|ui| ui.run(text, px).1).unwrap_or(0.0)
}

/// The baseline that puts `px`-high text in the middle of a line centred on
/// `mid`. Inter's caps sit about 0.72 em above the baseline, its descender 0.24
/// below; centring on the cap height reads best for UI text.
pub fn baseline(mid: f64, px: f64) -> f64 {
    mid + px * 0.36
}

/// Draw `text` with its left edge at `x` and its baseline at `baseline`, and
/// return the advance actually used.
pub fn draw(scene: &mut Scene, text: &str, x: f64, baseline: f64, px: f64, color: Color) -> f64 {
    with(|ui| {
        let (run, advance) = ui.run(text, px);
        for (key, dx) in run {
            let transform = Affine::new([px, 0.0, 0.0, px, x + dx, baseline]);
            let Some(path) = ui.path(GlyphId(key)) else {
                continue;
            };
            if path.elements().is_empty() {
                continue;
            }
            scene.fill(Fill::NonZero, transform, color, None, path);
        }
        advance
    })
    .unwrap_or(0.0)
}

/// `text`, shortened with an ellipsis if it is wider than `max`.
pub fn fit(text: &str, px: f64, max: f64) -> String {
    if width(text, px) <= max {
        return text.to_string();
    }
    let ellipsis = "…";
    let room = (max - width(ellipsis, px)).max(0.0);
    let mut out = String::new();
    let mut used = 0.0;
    for ch in text.chars() {
        let w = width(&ch.to_string(), px);
        if used + w > room {
            break;
        }
        out.push(ch);
        used += w;
    }
    // Cut at a word boundary if that does not eat too much.
    if let Some(space) = out.rfind(' ') {
        if space as f64 > out.len() as f64 * 0.6 {
            out.truncate(space);
        }
    }
    out.push_str(ellipsis);
    out
}

/// Draw `text` centred on `cx` and return its width.
pub fn draw_centred(
    scene: &mut Scene,
    text: &str,
    cx: f64,
    baseline: f64,
    px: f64,
    color: Color,
) -> f64 {
    let w = width(text, px);
    draw(scene, text, cx - w / 2.0, baseline, px, color);
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_and_fits() {
        let w = width("756", 15.0);
        assert!(w > 10.0 && w < 40.0, "{w}");
        let long = "Chapter 4 — The Long Way Home And Further Still";
        let short = fit(long, 14.0, 120.0);
        assert!(short.ends_with('…'), "{short}");
        assert!(width(&short, 14.0) <= 121.0, "{short}");
    }
}
