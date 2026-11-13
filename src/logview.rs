//! The app's own log, on the phone.
//!
//! A viewer that runs on someone else's phone has no cable and no `adb`: when
//! it starts slowly, or a face will not parse, the evidence otherwise never
//! leaves the device. So every line the app logs is also kept in a ring buffer
//! and can be read from the bar's menu — the last few hundred lines, with the
//! milliseconds since start in front of them.
//!
//! The logger is a tee: `android_logger` still gets everything (logcat works
//! exactly as before), and the ring buffer gets a copy for the screen.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use vello::kurbo::{Affine, Rect};
use vello::peniko::{Color, Fill};
use vello::Scene;

use crate::text;

/// How many lines are kept. A cold start is a dozen; a download is a few
/// hundred; anything longer than this is not worth a finger on the screen.
const KEPT: usize = 400;
/// Height of one row, in dp.
const ROW_H: f64 = 17.0;
/// Font size of a row, in dp.
const PX: f64 = 10.5;
/// Height of the panel's title strip, in dp.
const HEAD_H: f64 = 46.0;

static RING: OnceLock<Mutex<VecDeque<Row>>> = OnceLock::new();
/// Bumped on every line, so the panel notices there is something new.
static GENERATION: AtomicU64 = AtomicU64::new(0);

struct Row {
    text: String,
    level: log::Level,
}

fn ring() -> &'static Mutex<VecDeque<Row>> {
    RING.get_or_init(|| Mutex::new(VecDeque::with_capacity(KEPT)))
}

/// Keep one line for the screen.
fn push(level: log::Level, line: String) {
    // A log line that is wider than the phone is a log line nobody reads.
    let line = if line.chars().count() > 300 {
        line.chars().take(300).collect::<String>() + "…"
    } else {
        line
    };
    let mut guard = match ring().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if guard.len() == KEPT {
        guard.pop_front();
    }
    guard.push_back(Row { text: line, level });
    GENERATION.fetch_add(1, Ordering::Release);
}

/// How many lines have ever been written: the panel redraws when it changes.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

/// How many lines are kept right now.
pub fn count() -> usize {
    let guard = match ring().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.len()
}

/// Every kept line, oldest first.
pub fn lines() -> Vec<(String, log::Level)> {
    let guard = match ring().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard
        .iter()
        .map(|row| (row.text.clone(), row.level))
        .collect()
}

/// The logger: everything to logcat, a copy to the ring buffer.
pub struct Tee {
    inner: Box<dyn log::Log>,
}

impl Tee {
    pub fn new(inner: Box<dyn log::Log>) -> Tee {
        Tee { inner }
    }
}

impl log::Log for Tee {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        self.inner.enabled(metadata)
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.inner.enabled(record.metadata()) {
            return;
        }
        self.inner.log(record);
        push(
            record.level(),
            format!(
                "{:>6.2}s {} {}",
                crate::boot_ms() / 1000.0,
                match record.level() {
                    log::Level::Error => "E",
                    log::Level::Warn => "W",
                    log::Level::Info => "I",
                    log::Level::Debug => "D",
                    log::Level::Trace => "T",
                },
                record.args()
            ),
        );
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

/// The log panel: the page area, a title strip, and the lines under it.
#[derive(Clone, Copy, Debug)]
pub struct Panel {
    pub area: Rect,
    pub header: Rect,
    pub list: Rect,
    pub row_h: f64,
}

impl Panel {
    pub fn new(width: f64, bar_top: f64, scale: f64) -> Panel {
        let area = Rect::new(0.0, 0.0, width, bar_top.max(0.0));
        let head_h = (HEAD_H * scale).min(area.height() * 0.25);
        Panel {
            area,
            header: Rect::new(0.0, 0.0, width, head_h),
            list: Rect::new(0.0, head_h, width, area.height()),
            row_h: ROW_H * scale,
        }
    }

    pub fn max_scroll(&self, rows: usize, scale: f64) -> f64 {
        let content = rows as f64 * self.row_h + 12.0 * scale;
        (content - self.list.height()).max(0.0)
    }

    pub fn scroll(&self, scroll: f64, rows: usize, scale: f64) -> f64 {
        scroll.clamp(0.0, self.max_scroll(rows, scale))
    }
}

/// Draw the panel, scrolled to `scroll` px down the list.
pub fn draw(scene: &mut Scene, panel: &Panel, lines: &[(String, log::Level)], scroll: f64, scale: f64) {
    if panel.area.height() <= 0.0 {
        return;
    }
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgba8(14, 14, 17, 250),
        None,
        &panel.area,
    );
    text::draw(
        scene,
        "Log",
        16.0 * scale,
        text::baseline(panel.header.center().y, 17.0 * scale),
        17.0 * scale,
        Color::from_rgba8(232, 234, 242, 255),
    );
    let hint = "hold menu to close";
    let hint_px = 10.5 * scale;
    let hint_w = text::width(hint, hint_px);
    text::draw(
        scene,
        hint,
        panel.area.width() - 14.0 * scale - hint_w,
        text::baseline(panel.header.center().y, hint_px),
        hint_px,
        Color::from_rgba8(140, 142, 154, 255),
    );
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgba8(255, 255, 255, 18),
        None,
        &Rect::new(
            0.0,
            panel.header.y1 - 1.0,
            panel.area.width(),
            panel.header.y1,
        ),
    );

    if lines.is_empty() {
        text::draw(
            scene,
            "nothing logged yet",
            14.0 * scale,
            text::baseline(panel.list.y0 + panel.row_h, PX * scale),
            PX * scale,
            Color::from_rgba8(150, 152, 164, 255),
        );
        return;
    }

    let px = PX * scale;
    let first = (scroll / panel.row_h).floor().max(0.0) as usize;
    let visible = (panel.list.height() / panel.row_h).ceil() as usize + 2;
    for index in first..(first + visible).min(lines.len()) {
        let (line, level) = &lines[index];
        let top = panel.list.y0 + index as f64 * panel.row_h - scroll;
        if top + panel.row_h < panel.list.y0 - 1.0 || top > panel.list.y1 + 1.0 {
            continue;
        }
        let colour = match level {
            log::Level::Error => Color::from_rgba8(255, 150, 140, 255),
            log::Level::Warn => Color::from_rgba8(240, 206, 130, 255),
            _ => Color::from_rgba8(196, 199, 210, 255),
        };
        let line = text::fit(line, px, panel.area.width() - 22.0 * scale);
        text::draw(
            scene,
            &line,
            11.0 * scale,
            text::baseline(top + panel.row_h * 0.5, px),
            px,
            colour,
        );
    }

    // A thumb on the right edge, so a screenful of lines reads as a window
    // onto more of them.
    let max = panel.max_scroll(lines.len(), scale);
    if max > 0.0 {
        let track = Rect::new(
            panel.area.width() - 3.0 * scale,
            panel.list.y0,
            panel.area.width(),
            panel.list.y1,
        );
        let share = panel.list.height() / (panel.list.height() + max);
        let len = (track.height() * share).max(24.0 * scale);
        let y = track.y0 + (track.height() - len) * (scroll / max);
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(255, 255, 255, 46),
            None,
            &Rect::new(track.x0, y, track.x1, y + len),
        );
    }
}
