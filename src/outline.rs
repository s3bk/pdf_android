//! The document outline: read once when the file is opened, drawn as a drawer
//! over the page, tapped to jump.
//!
//! The PDF side is a linked tree — `/Outlines` `/First`, then `/Next` along a
//! level and `/First` again for the children — and an item points at a page in
//! one of two ways: `/Dest` (usually an array whose first element is the page
//! reference) or `/A << /S /GoTo /D … >>`. Named destinations go through the
//! catalogue's `/Dests` dictionary; anything else is shown but goes nowhere.

use std::collections::HashMap;

use pdf::file::{File, NoCache, NoLog};
use pdf::object::{Action, ActionType, MaybeNamedDest, OutlineItem, PlainRef, Ref, Resolve};
use pdf::primitive::Primitive;

use vello::kurbo::Rect;
use vello::peniko::{Color, Fill};
use vello::Scene;

use crate::text;

/// One line of the drawer.
#[derive(Clone, Debug)]
pub struct Row {
    pub title: String,
    /// Page index (0-based) this line jumps to, if it could be resolved.
    pub page: Option<usize>,
    /// Nesting level: 0 for a top-level bookmark.
    pub depth: u8,
}

/// Height of one row, in dp.
const ROW_H: f64 = 44.0;
/// Height of the drawer's title strip, in dp.
const HEAD_H: f64 = 54.0;
/// How far each nesting level indents, in dp.
const INDENT: f64 = 16.0;
/// Font size of a row's title, in dp.
const TITLE_PX: f64 = 15.0;
/// Font size of the page number at the right of a row, in dp.
const PAGENO_PX: f64 = 13.0;

// ---------------------------------------------------------------------------
// reading the outline out of the file

/// Every bookmark in the document, depth first, in reading order. Empty when
/// the file has no `/Outlines` (most exported PDFs do not).
pub fn read<B: pdf::backend::Backend>(file: &File<B, NoCache, NoCache, NoLog>) -> Vec<Row> {
    let mut rows = Vec::new();
    let resolver = file.resolver();
    let pages: Vec<PlainRef> = file.pages().flatten().map(|page| page.get_ref()).collect();
    let catalog = file.get_root();
    let Some(outlines) = catalog.outlines.as_ref() else {
        return rows;
    };
    let Some(first) = outlines.first else {
        return rows;
    };
    // Named destinations live either in the catalogue's `/Dests` dictionary
    // (PDF 1.1) or in the `/Names /Dests` name tree (1.2 onward, and what
    // almost every modern writer emits). Read both, once, into one map.
    let mut named: HashMap<String, Option<usize>> = HashMap::new();
    if let Some(dict) = catalog.dests.as_deref() {
        for (name, dest) in dict.iter() {
            let page = from_dest(dest, &resolver, &pages, &named);
            named.insert(name.as_str().to_string(), page);
        }
    }
    if let Some(names) = catalog.names.as_deref() {
        if let Some(tree) = names.dests.as_ref() {
            let _ = tree.walk(&resolver, &mut |key, dest| {
                let page = dest
                    .as_ref()
                    .and_then(|d| d.page)
                    .and_then(|r| index_of(&pages, r.get_inner()));
                named.insert(key.to_string_lossy(), page);
            });
        }
    }
    let dests = &named;
    walk(&resolver, &pages, dests, first, 0, &mut rows);
    let with_page = rows.iter().filter(|r| r.page.is_some()).count();
    log::info!(
        "outline: {} rows, {with_page} with a page, {} named destinations",
        rows.len(),
        named.len()
    );
    rows
}

fn walk(
    resolve: &impl Resolve,
    pages: &[PlainRef],
    dests: &HashMap<String, Option<usize>>,
    first: Ref<OutlineItem>,
    depth: u8,
    out: &mut Vec<Row>,
) {
    let mut next = Some(first);
    // A broken file can point an item at itself; stop after a sane number.
    let mut seen = 0;
    while let Some(r) = next {
        seen += 1;
        if seen > 4000 {
            break;
        }
        let Ok(item) = resolve.get(r) else {
            break;
        };
        out.push(Row {
            title: item
                .title
                .as_ref()
                .map(|t| t.to_string_lossy().trim().to_string())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| "(untitled)".into()),
            page: page_of(&item, resolve, pages, dests),
            depth,
        });
        if depth < 4 {
            if let Some(child) = item.first {
                walk(resolve, pages, dests, child, depth + 1, out);
            }
        }
        next = item.next;
    }
}

fn page_of(
    item: &OutlineItem,
    resolve: &impl Resolve,
    pages: &[PlainRef],
    dests: &HashMap<String, Option<usize>>,
) -> Option<usize> {
    if let Some(dest) = item.dest.as_ref() {
        if let Some(page) = from_dest(dest, resolve, pages, dests) {
            return Some(page);
        }
    }
    let action: &Action = item.action.as_ref()?;
    if !matches!(action.s, ActionType::GoTo) {
        return None;
    }
    match action.d.as_ref()? {
        MaybeNamedDest::Direct(dest) => {
            let page = dest.page?;
            index_of(pages, page.get_inner())
        }
        MaybeNamedDest::Named(name) => {
            let key = name.to_string_lossy();
            dests.get(&key).copied().flatten()
        }
    }
}

/// Resolve one `/Dest` value — an array, a name, or a string naming a
/// destination in `/Dests` — to a page index.
fn from_dest(
    dest: &Primitive,
    resolve: &impl Resolve,
    pages: &[PlainRef],
    named: &HashMap<String, Option<usize>>,
) -> Option<usize> {
    match dest {
        Primitive::Array(array) => match array.first()? {
            Primitive::Reference(r) => index_of(pages, *r),
            Primitive::Integer(n) => Some((*n).max(0) as usize).filter(|n| *n < pages.len()),
            _ => None,
        },
        Primitive::Name(name) => named.get(name.as_str()).copied().flatten(),
        Primitive::String(s) => named.get(&s.to_string_lossy()).copied().flatten(),
        Primitive::Reference(r) => {
            let target = resolve.resolve(*r).ok()?;
            from_dest(&target, resolve, pages, named)
        }
        Primitive::Dictionary(dict) => {
            // << /D [ … ] >>, or a destination stored in a dict
            let inner = dict.get("D")?;
            from_dest(inner, resolve, pages, named)
        }
        _ => None,
    }
}

fn index_of(pages: &[PlainRef], r: PlainRef) -> Option<usize> {
    pages.iter().position(|p| *p == r)
}

// ---------------------------------------------------------------------------
// the drawer

/// The drawer's geometry for a window of `width` x `height` whose page area
/// ends at `bar_top`. It covers the page, not the bar: the bar's buttons stay
/// reachable while it is open.
#[derive(Clone, Copy, Debug)]
pub struct Panel {
    pub area: Rect,
    pub header: Rect,
    pub list: Rect,
    /// Height of one row in pixels.
    pub row_h: f64,
    /// Indent per nesting level, in pixels.
    pub indent: f64,
}

impl Panel {
    pub fn new(width: f64, bar_top: f64, scale: f64) -> Panel {
        let area = Rect::new(0.0, 0.0, width, bar_top.max(0.0));
        let head_h = (HEAD_H * scale).min(area.height() * 0.25);
        let header = Rect::new(0.0, 0.0, width, head_h);
        let list = Rect::new(0.0, head_h, width, area.height());
        Panel {
            area,
            header,
            list,
            row_h: ROW_H * scale,
            indent: INDENT * scale,
        }
    }

    /// How far the list can be scrolled before its last row sits at the bottom.
    pub fn max_scroll(&self, rows: usize, scale: f64) -> f64 {
        let content = rows as f64 * self.row_h + 12.0 * scale;
        (content - self.list.height()).max(0.0)
    }

    pub fn scroll(&self, scroll: f64, rows: usize, scale: f64) -> f64 {
        scroll.clamp(0.0, self.max_scroll(rows, scale))
    }

    /// Which row a tap at (`x`, `y`) selects.
    pub fn row_at(&self, rows: &[Row], scroll: f64, x: f64, y: f64) -> Option<usize> {
        if !self.list.contains((x, y)) {
            return None;
        }
        let offset = scroll + (y - self.list.y0);
        let index = (offset / self.row_h).floor();
        if index < 0.0 {
            return None;
        }
        let index = index as usize;
        (index < rows.len()).then_some(index)
    }
}

/// Draw the drawer. `current` is the page on screen, so its row stands out.
pub fn draw(
    scene: &mut Scene,
    rows: &[Row],
    panel: &Panel,
    scroll: f64,
    scale: f64,
    current: usize,
) {
    if panel.area.height() <= 0.0 {
        return;
    }
    // The sheet behind the rows.
    scene.fill(
        Fill::NonZero,
        vello::kurbo::Affine::IDENTITY,
        Color::from_rgba8(20, 20, 24, 248),
        None,
        &panel.area,
    );
    // Title strip.
    text::draw(
        scene,
        "Contents",
        16.0 * scale,
        text::baseline(panel.header.center().y, 18.0 * scale),
        18.0 * scale,
        Color::from_rgba8(232, 234, 242, 255),
    );
    scene.fill(
        Fill::NonZero,
        vello::kurbo::Affine::IDENTITY,
        Color::from_rgba8(255, 255, 255, 18),
        None,
        &Rect::new(
            0.0,
            panel.header.y1 - 1.0,
            panel.area.width(),
            panel.header.y1,
        ),
    );

    if rows.is_empty() {
        text::draw(
            scene,
            "This document has no outline.",
            16.0 * scale,
            text::baseline(panel.list.y0 + panel.row_h * 0.5, 15.0 * scale),
            15.0 * scale,
            Color::from_rgba8(150, 152, 164, 255),
        );
        return;
    }

    // Only the rows the viewer can see: the drawer can hold thousands.
    let first = (scroll / panel.row_h).floor().max(0.0) as usize;
    let visible = (panel.list.height() / panel.row_h).ceil() as usize + 2;
    for index in first..(first + visible).min(rows.len()) {
        let row = &rows[index];
        let top = panel.list.y0 + index as f64 * panel.row_h - scroll;
        let rect = Rect::new(panel.area.x0, top, panel.area.x1, top + panel.row_h);
        if rect.y1 < panel.list.y0 - 1.0 || rect.y0 > panel.list.y1 + 1.0 {
            continue;
        }
        if row.page == Some(current) {
            scene.fill(
                Fill::NonZero,
                vello::kurbo::Affine::IDENTITY,
                Color::from_rgba8(122, 156, 240, 46),
                None,
                &rect,
            );
        }
        let x = 16.0 * scale + row.depth as f64 * panel.indent;
        let page_no = row
            .page
            .map(|p| (p + 1).to_string())
            .unwrap_or_else(|| "—".into());
        let page_w = text::width(&page_no, PAGENO_PX * scale);
        let room = panel.area.width() - x - page_w - 32.0 * scale;
        let title = text::fit(&row.title, TITLE_PX * scale, room.max(20.0));
        let baseline = text::baseline(rect.center().y, TITLE_PX * scale);
        let colour = match row.page {
            Some(_) if row.page == Some(current) => Color::from_rgba8(214, 226, 255, 255),
            Some(_) => Color::from_rgba8(224, 226, 234, 255),
            None => Color::from_rgba8(132, 134, 146, 255),
        };
        text::draw(scene, &title, x, baseline, TITLE_PX * scale, colour);
        text::draw(
            scene,
            &page_no,
            panel.area.width() - 16.0 * scale - page_w,
            text::baseline(rect.center().y, PAGENO_PX * scale),
            PAGENO_PX * scale,
            Color::from_rgba8(138, 140, 152, 255),
        );
        // Hairline between rows, dim enough to read as paper grain.
        if index + 1 < rows.len() {
            scene.fill(
                Fill::NonZero,
                vello::kurbo::Affine::IDENTITY,
                Color::from_rgba8(255, 255, 255, 12),
                None,
                &Rect::new(x, rect.y1 - 1.0, panel.area.x1, rect.y1),
            );
        }
    }

    // A thumb on the right edge while there is more than a screenful.
    let max = panel.max_scroll(rows.len(), scale);
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
            vello::kurbo::Affine::IDENTITY,
            Color::from_rgba8(255, 255, 255, 46),
            None,
            &Rect::new(track.x0, y, track.x1, y + len),
        );
    }
}
