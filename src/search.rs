//! Finding words in the document.
//!
//! The text index is built lazily, on a background thread, the first time the
//! owner opens the search panel: no PDF reader that ships as a phone app can
//! afford to read 800 pages of content streams at start-up, but once the index
//! is there (a few seconds) a query over it is instant. Pages are indexed
//! starting at the one on screen and working outward, so the page you were
//! reading answers first.
//!
//! Text comes out of `pdf_render`'s [`Tracer`], the same backend `pdf_text`
//! uses: it walks the page's content stream, loads the fonts (with their
//! `/ToUnicode` maps) and hands back the spans it would have drawn. Nothing is
//! rasterised.
//!
//! The query is typed into the framework's own field: [`crate::field`] puts a
//! real `EditText` over the pill this panel draws, so an input method can
//! compose into it (autocorrect, swipe typing, emoji, dictation, paste, cursor
//! keys, selection). The key rows below are only the fallback for a build
//! without that field: winit's Android backend on its own has no IME support at
//! all (`set_ime_allowed` there is an empty function and no `InputConnection` is
//! ever created), and a hand-drawn QWERTY was the only way to type into a
//! `NativeActivity` before the field existed.

use std::cmp::Ordering;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering as Atomic};
use std::sync::{Arc, Mutex, OnceLock};

use pdf::file::{File, FileOptions, NoCache, NoLog};
use pdf_render::rangecache::{coalesce, render_with_ranges, Borrowed, LazyReader, RangeCache};
use pdf_render::tracer::{DrawItem, NullEncoder, TraceCache, Tracer};
use pdf_render::{render_page, Transform2F};
use vello::kurbo::{Affine, Rect, Stroke};
use vello::peniko::{Color, Fill};
use vello::Scene;

use crate::text;

/// Height of one search result, in dp.
const ROW_H: f64 = 46.0;
/// Height of the query field's strip, in dp.
const HEAD_H: f64 = 56.0;
/// Font size of the query, in dp.
pub const QUERY_PX: f64 = 17.0;
/// Height of one key, in dp.
const KEY_H: f64 = 44.0;
/// Gap between keys, in dp.
const KEY_GAP: f64 = 5.0;

// ---------------------------------------------------------------------------
// the index

/// One page's text, plus the same text lower-cased (spaces collapsed) and a map
/// from every lower-case byte back to where it came from, so a hit can be
/// shown in the document's own casing.
struct Page {
    text: String,
    lower: String,
    map: Vec<u32>,
}

impl Page {
    fn new(text: String) -> Page {
        let mut lower = String::with_capacity(text.len());
        let mut map: Vec<u32> = Vec::with_capacity(text.len());
        let mut space = false;
        for (i, ch) in text.char_indices() {
            if ch.is_whitespace() {
                if !space && !lower.is_empty() {
                    lower.push(' ');
                    map.push(i as u32);
                    space = true;
                }
                continue;
            }
            space = false;
            let from = lower.len();
            for lc in ch.to_lowercase() {
                lower.push(lc);
            }
            for _ in from..lower.len() {
                map.push(i as u32);
            }
        }
        Page { text, lower, map }
    }
}

struct Index {
    pages: Vec<Option<Page>>,
    /// The document this index belongs to, as `file:<path>` or `url:<url>`; a
    /// new document replaces it.
    key: Option<String>,
    built: usize,
    done: bool,
}

static INDEX: OnceLock<Mutex<Index>> = OnceLock::new();
/// Bumped every time a page joins the index, so the panel can tell its hits
/// were computed against a shorter index.
static GENERATION: AtomicU64 = AtomicU64::new(0);
static WAKE: Mutex<Option<Arc<dyn Fn() + Send + Sync>>> = Mutex::new(None);

fn index() -> &'static Mutex<Index> {
    INDEX.get_or_init(|| {
        Mutex::new(Index {
            pages: Vec::new(),
            key: None,
            built: 0,
            done: true,
        })
    })
}

/// Give the index a way to ask for a redraw when a page lands.
pub fn set_wake(f: impl Fn() + Send + Sync + 'static) {
    if let Ok(mut slot) = WAKE.lock() {
        *slot = Some(Arc::new(f));
    }
}

/// How many pages have joined the index so far. `0` when it has not started.
pub fn generation() -> u64 {
    GENERATION.load(Atomic::Acquire)
}

/// (pages indexed, pages in the document)
pub fn progress() -> (usize, usize) {
    match index().lock() {
        Ok(index) => (index.built, index.pages.len()),
        Err(poisoned) => {
            let index = poisoned.into_inner();
            (index.built, index.pages.len())
        }
    }
}

/// Mark the index finished: whatever stopped the walk — the last page, a
/// missing font bundle, a panic — the panel must stop claiming it is still
/// indexing and show what it has.
fn finish() {
    let mut index = match index().lock() {
        Ok(index) => index,
        Err(poisoned) => poisoned.into_inner(),
    };
    index.done = true;
    drop(index);
    GENERATION.fetch_add(1, Atomic::Release);
    wake();
}

/// Calls [`finish`] when the indexing thread leaves, however it leaves.
struct Finish;

impl Drop for Finish {
    fn drop(&mut self) {
        finish();
    }
}

/// Start indexing `path` in the background. Called once, when the panel first
/// opens; a second call for the same document does nothing.
pub fn start(path: PathBuf, pages: usize, focus: usize) {
    let key = format!("file:{}", path.display());
    let Some(order) = begin(key, pages, focus) else {
        return;
    };

    std::thread::spawn(move || {
        let _finish = Finish;
        let file: File<Vec<u8>, NoCache, NoCache, NoLog> =
            match FileOptions::uncached()
                .parse_options(crate::page_options())
                .open_owned(&path) {
                Ok(file) => file,
                Err(e) => {
                    log::error!("search: cannot reopen {}: {e:?}", path.display());
                    if let Ok(mut index) = index().lock() {
                        index.done = true;
                    }
                    GENERATION.fetch_add(1, Atomic::Release);
                    wake();
                    return;
                }
            };
        let dir = match pdf_render::MemDir::parse_bundle(crate::FONT_BUNDLE) {
            Ok(dir) => dir,
            Err(e) => {
                log::error!("search: font bundle: {e:?}");
                finish();
                return;
            }
        };
        let cache: TraceCache<NullEncoder> =
            TraceCache::with_standard(pdf_render::StandardCache::with_dir(dir));
        for page in order {
            let text = extract(&file, page as u32, &cache);
            {
                let mut index = match index().lock() {
                    Ok(index) => index,
                    Err(poisoned) => poisoned.into_inner(),
                };
                if index.pages.len() <= page {
                    break;
                }
                index.pages[page] = Some(Page::new(text));
                index.built += 1;
                if index.built == index.pages.len() {
                    index.done = true;
                    log::info!("search: index complete ({} pages)", index.built);
                }
            }
            GENERATION.fetch_add(1, Atomic::Release);
            wake();
        }
    });
}

/// Reset the index for a new document and hand out the pages in the order they
/// should be indexed: the page on screen first, then the nearest either side.
/// `None` when this document is already indexed (or already being indexed).
fn begin(key: String, pages: usize, focus: usize) -> Option<Vec<usize>> {
    {
        let mut index = match index().lock() {
            Ok(index) => index,
            Err(poisoned) => poisoned.into_inner(),
        };
        if index.key.as_deref() == Some(key.as_str()) {
            return None;
        }
        index.pages = (0..pages).map(|_| None).collect();
        index.key = Some(key.clone());
        index.built = 0;
        index.done = pages == 0;
        log::info!("search: indexing {pages} pages of {key}");
    }
    let mut order: Vec<usize> = Vec::with_capacity(pages);
    let focus = focus.min(pages.saturating_sub(1));
    if pages > 0 {
        order.push(focus);
    }
    let mut step = 1usize;
    while order.len() < pages {
        if focus + step < pages {
            order.push(focus + step);
        }
        if step <= focus {
            order.push(focus - step);
        }
        step += 1;
    }
    Some(order)
}

/// Put one page's text into the index and tell the panel.
fn publish(page: usize, text: String) {
    let mut complete = false;
    {
        let mut index = match index().lock() {
            Ok(index) => index,
            Err(poisoned) => poisoned.into_inner(),
        };
        if index.pages.len() <= page {
            return;
        }
        index.pages[page] = Some(Page::new(text));
        index.built += 1;
        if index.built == index.pages.len() {
            index.done = true;
            complete = true;
        }
    }
    if complete {
        log::info!("search: index complete ({} pages)", progress().0);
    }
    GENERATION.fetch_add(1, Atomic::Release);
    wake();
}

/// Index a document that lives at a URL. Same walk as [`start`], but each page
/// is rendered against a byte range cache that the indexing thread fills over
/// HTTP: the bytes one page needed are usually most of what the next needs.
pub fn start_url(url: String, pages: usize, focus: usize) {
    let key = format!("url:{url}");
    let Some(order) = begin(key, pages, focus) else {
        return;
    };
    std::thread::spawn(move || {
        let _finish = Finish;
        let dir = match pdf_render::MemDir::parse_bundle(crate::FONT_BUNDLE) {
            Ok(dir) => dir,
            Err(e) => {
                log::error!("search: font bundle: {e:?}");
                return;
            }
        };
        let cache: TraceCache<NullEncoder> =
            TraceCache::with_standard(pdf_render::StandardCache::with_dir(dir));
        // `head` follows redirects and says where they led: the ranges below
        // must go to the same place the document's own ranges went.
        let (len, url) = match crate::net::head(&url) {
            Ok(head) => match head.len {
                Some(len) => (len as usize, head.url),
                None => {
                    log::error!("search: {url} did not say how big it is");
                    return;
                }
            },
            Err(e) => {
                log::error!("search: {e}");
                return;
            }
        };
        let mut ranges = RangeCache::new(len);
        for page in order {
            let text = render_with_ranges(
                &mut ranges,
                8,
                |reader| {
                    let file: File<Borrowed<'_, LazyReader<'_>>, NoCache, NoCache, NoLog> =
                        File::load_data(
                            Borrowed(reader),
                            b"",
                            // Lenient: see `remote::open`.
                            crate::page_options(),
                            NoCache,
                            NoCache,
                            NoLog,
                        )?;
                    Ok(extract(&file, page as u32, &cache))
                },
                |cache, missing| {
                    for range in coalesce(missing.to_vec(), 32 * 1024) {
                        if range.is_empty() {
                            continue;
                        }
                        let data = crate::net::range(&url, range.start as u64, range.end as u64)?;
                        cache.insert(range.start, data);
                    }
                    Ok(())
                },
            )
            .unwrap_or_else(|e| {
                log::warn!("search: page {}: {e}", page + 1);
                String::new()
            });
            publish(page, text);
        }
    });
}

fn wake() {
    let wake = WAKE.lock().ok().and_then(|slot| slot.clone());
    if let Some(wake) = wake {
        wake();
    }
}

/// One span of text on a page, in page coordinates.
struct Piece {
    x0: f64,
    x1: f64,
    y: f64,
    h: f64,
    text: String,
}

/// The text of one page: spans from the tracer, gathered into lines.
fn extract<B: pdf::backend::Backend>(
    file: &File<B, NoCache, NoCache, NoLog>,
    page: u32,
    cache: &TraceCache<NullEncoder>,
) -> String {
    let page = match file.get_page(page) {
        Ok(page) => page,
        Err(e) => {
            log::warn!("search: page: {e:?}");
            return String::new();
        }
    };
    let resolver = file.resolver();
    let mut clips = Vec::new();
    let mut tracer = Tracer::<NullEncoder>::new(cache, &mut clips);
    if let Err(e) = render_page(&mut tracer, &resolver, &page, Transform2F::default()) {
        log::warn!("search: page content: {e:?}");
    }

    let mut pieces: Vec<Piece> = Vec::new();
    for item in tracer.finish() {
        let DrawItem::Text(span, _) = item else {
            continue;
        };
        if span.text.trim().is_empty() {
            continue;
        }
        // The span's rect is in text space; the transform puts it on the page.
        let r = span.transform * span.rect;
        let h = (r.max_y() - r.min_y()).abs() as f64;
        pieces.push(Piece {
            x0: r.min_x() as f64,
            x1: r.max_x() as f64,
            y: r.min_y() as f64,
            h,
            text: span.text,
        });
    }
    pieces.sort_by(|a, b| {
        a.y.partial_cmp(&b.y)
            .unwrap_or(Ordering::Equal)
            .then(a.x0.partial_cmp(&b.x0).unwrap_or(Ordering::Equal))
    });

    let mut out = String::new();
    let mut row: Vec<&Piece> = Vec::new();
    let (mut row_y, mut row_h) = (0.0f64, 0.0f64);
    for piece in &pieces {
        // Same line while the baselines are within half a line of each other.
        if !row.is_empty() && (piece.y - row_y).abs() > row_h.max(piece.h) * 0.6 {
            flush(&mut out, &mut row, row_h);
            row_y = piece.y;
            row_h = piece.h;
        } else if row.is_empty() {
            row_y = piece.y;
            row_h = piece.h;
        } else {
            row_h = row_h.max(piece.h);
        }
        row.push(piece);
    }
    flush(&mut out, &mut row, row_h);
    out.trim().to_string()
}

/// Append one line of pieces to `out`: a gap wider than a quarter em between
/// two pieces is a word space.
fn flush(out: &mut String, row: &mut Vec<&Piece>, row_h: f64) {
    if row.is_empty() {
        return;
    }
    row.sort_by(|a, b| a.x0.partial_cmp(&b.x0).unwrap_or(Ordering::Equal));
    let em = row_h.max(1.0);
    let mut last_x = f64::NEG_INFINITY;
    for piece in row.iter() {
        if !out.is_empty() && last_x.is_finite() && piece.x0 - last_x > em * 0.22 {
            out.push(' ');
        }
        out.push_str(&piece.text);
        last_x = piece.x1;
    }
    row.clear();
    out.push('\n');
}

// ---------------------------------------------------------------------------
// querying

/// One match: the page it is on and the line it sits in.
#[derive(Clone, Debug)]
pub struct Hit {
    pub page: usize,
    pub snippet: String,
}

/// Search the pages indexed so far. Returns at most `limit` hits (and how many
/// there are in total).
pub fn hits(query: &str, limit: usize) -> (Vec<Hit>, usize) {
    let needle = normalize(query);
    if needle.is_empty() {
        return (Vec::new(), 0);
    }
    let (mut found, mut total) = (Vec::new(), 0usize);
    let index = match index().lock() {
        Ok(index) => index,
        Err(poisoned) => poisoned.into_inner(),
    };
    for (index_of_page, entry) in index.pages.iter().enumerate() {
        let Some(page) = entry else { continue };
        let mut from = 0;
        while let Some(at) = page.lower[from..].find(&needle) {
            let at = from + at;
            total += 1;
            if found.len() < limit {
                found.push(Hit {
                    page: index_of_page,
                    snippet: snippet(page, at, needle.len()),
                });
            }
            // Overlapping matches on the same spot are noise.
            from = at + needle.len().max(1);
            if from >= page.lower.len() {
                break;
            }
        }
    }
    (found, total)
}

/// The same folding the index applies: lower case, whitespace to one space.
fn normalize(query: &str) -> String {
    let mut out = String::with_capacity(query.len());
    let mut space = false;
    for ch in query.chars() {
        if ch.is_whitespace() {
            if !space && !out.is_empty() {
                out.push(' ');
                space = true;
            }
            continue;
        }
        space = false;
        for lc in ch.to_lowercase() {
            out.push(lc);
        }
    }
    out
}

/// The line a match sits in, cut down to something a row can hold.
fn snippet(page: &Page, start: usize, len: usize) -> String {
    let bytes = &page.map;
    if bytes.is_empty() {
        return String::new();
    }
    let from = bytes[start.min(bytes.len() - 1)] as usize;
    let to = (bytes[(start + len).min(bytes.len() - 1)] as usize + 1).min(page.text.len());
    let from = floor_boundary(&page.text, from);
    let to = ceil_boundary(&page.text, to);

    let line_start = page.text[..from].rfind('\n').map_or(0, |i| i + 1);
    let line_end = page.text[to..]
        .find('\n')
        .map_or(page.text.len(), |i| to + i);
    let line = &page.text[line_start..line_end];
    let (rel, rel_end) = (from - line_start, to - line_start);

    const BEFORE: usize = 36;
    const AFTER: usize = 64;
    let start_char = line[..rel]
        .char_indices()
        .rev()
        .nth(BEFORE)
        .map_or(0, |(i, _)| i);
    let end_char = line[rel_end..]
        .char_indices()
        .nth(AFTER)
        .map_or(line.len(), |(i, _)| rel_end + i);
    let mut out = String::new();
    if start_char > 0 {
        out.push('…');
    }
    out.push_str(line[start_char..end_char].trim());
    if end_char < line.len() {
        out.push('…');
    }
    out
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

// ---------------------------------------------------------------------------
// the panel

/// A key on the on-screen keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Ch(char),
    Back,
    Space,
    Done,
}

/// The panel's geometry for a window of `width` x `height` whose page area
/// ends at `bar_top`, with or without the keyboard.
pub struct Panel {
    pub area: Rect,
    /// The query pill.
    pub field: Rect,
    /// The close button, at the right of the field's strip.
    pub close: Rect,
    /// Where results start.
    pub list: Rect,
    /// Height of one result row, in pixels.
    pub row_h: f64,
    pub scale: f64,
    keys: Vec<(Rect, Key)>,
}

impl Panel {
    /// `top` is the height of the display cutout / status bar: the panel starts
    /// below it, because the query field is a text field and a text field under
    /// the camera is a text field nobody can read.
    pub fn new(width: f64, bar_top: f64, top: f64, scale: f64, keyboard: bool) -> Panel {
        let top = top.max(0.0).min(bar_top.max(0.0));
        let area = Rect::new(0.0, top, width, bar_top.max(top));
        let head = (HEAD_H * scale).min(area.height());
        let pad = 8.0 * scale;
        let close_w = 40.0 * scale;
        let field = Rect::new(
            pad,
            top + (head - 40.0 * scale) * 0.5,
            (width - close_w - pad * 1.5).max(pad + 1.0),
            top + (head + 40.0 * scale) * 0.5,
        );
        let close = Rect::new(width - close_w - pad, field.y0, width - pad, field.y1);

        let rows = key_rows();
        let mut keys = Vec::new();
        let mut list_top = top + head;
        if keyboard {
            let key_h = (KEY_H * scale).max(18.0);
            let gap = (KEY_GAP * scale).max(1.0);
            let block = rows.len() as f64 * (key_h + gap) + gap;
            let block = block.min((area.height() - head).max(0.0) * 0.7);
            let row_h = ((block - gap) / rows.len() as f64 - gap).max(8.0);
            for (r, row) in rows.iter().enumerate() {
                let total: usize = row.iter().map(|k| weight(*k)).sum();
                let y = list_top + gap + r as f64 * (row_h + gap);
                let avail = width - 2.0 * pad - gap * (row.len() as f64 - 1.0);
                let unit = avail / total as f64;
                let used: f64 = row
                    .iter()
                    .map(|k| weight(*k) as f64 * unit + gap)
                    .sum::<f64>()
                    - gap;
                let mut x = (width - used) * 0.5;
                for key in row {
                    let w = weight(*key) as f64 * unit;
                    keys.push((Rect::new(x, y, x + w, y + row_h), *key));
                    x += w + gap;
                }
            }
            list_top = top + head + block.min((area.height() - head).max(0.0) * 0.7) + gap;
        }
        let list = Rect::new(0.0, list_top, width, area.height());
        Panel {
            area,
            field,
            close,
            list,
            row_h: ROW_H * scale,
            scale,
            keys,
        }
    }

    /// Which key a tap at (`x`, `y`) hit.
    pub fn key_at(&self, x: f64, y: f64) -> Option<Key> {
        self.keys
            .iter()
            .find(|(rect, _)| rect.contains((x, y)))
            .map(|(_, key)| *key)
    }

    /// How far the results can be scrolled.
    pub fn max_scroll(&self, rows: usize, scale: f64) -> f64 {
        let content = rows as f64 * ROW_H * scale;
        (content - self.list.height()).max(0.0)
    }

    /// Which result a tap at (`x`, `y`) selects.
    pub fn row_at(&self, rows: usize, scroll: f64, x: f64, y: f64) -> Option<usize> {
        if !self.list.contains((x, y)) {
            return None;
        }
        let offset = scroll + (y - self.list.y0);
        let index = (offset / self.row_h).floor();
        if index < 0.0 {
            return None;
        }
        let index = index as usize;
        (index < rows).then_some(index)
    }
}

fn key_rows() -> Vec<Vec<Key>> {
    vec![
        "1234567890".chars().map(Key::Ch).collect(),
        "qwertyuiop".chars().map(Key::Ch).collect(),
        "asdfghjkl".chars().map(Key::Ch).collect(),
        "zxcvbnm".chars().map(Key::Ch).chain([Key::Back]).collect(),
        vec![
            Key::Ch('.'),
            Key::Ch(','),
            Key::Ch('-'),
            Key::Ch('\''),
            Key::Space,
            Key::Done,
        ],
    ]
}

fn weight(key: Key) -> usize {
    match key {
        Key::Space => 3,
        Key::Done | Key::Back => 2,
        _ => 1,
    }
}

/// What the panel shows, gathered by the caller so the index is not locked
/// while a scene is built.
pub struct View<'a> {
    pub query: &'a str,
    pub hits: &'a [Hit],
    pub total: usize,
    pub built: usize,
    pub pages: usize,
    pub scroll: f64,
    pub keyboard: bool,
    /// The framework's own `EditText` sits on the query pill, text and caret
    /// and all: the panel then draws the pill and nothing inside it.
    pub native: bool,
}

/// Draw the panel over the page.
pub fn draw(scene: &mut Scene, panel: &Panel, view: &View) {
    let scale = panel.scale;
    // Above the panel is the display cutout strip: black, so the camera hole
    // sits on black rather than on a slice of the page.
    if panel.area.y0 > 0.0 {
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(0, 0, 0, 255),
            None,
            &Rect::new(0.0, 0.0, panel.area.x1, panel.area.y0),
        );
    }
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgba8(18, 18, 22, 250),
        None,
        &panel.area,
    );

    // The query pill.
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgba8(42, 42, 49, 255),
        None,
        &panel.field.to_rounded_rect(9.0 * scale),
    );
    if view.native {
        // The framework's field draws the query, its hint and its caret.
    } else {
        let px = QUERY_PX * scale;
        let baseline = text::baseline(panel.field.center().y, px);
        let left = panel.field.x0 + 12.0 * scale;
        if view.query.is_empty() {
            text::draw(
                scene,
                "Search the document",
                left,
                baseline,
                px,
                Color::from_rgba8(126, 128, 140, 255),
            );
        } else {
            text::draw(
                scene,
                view.query,
                left,
                baseline,
                px,
                Color::from_rgba8(236, 237, 244, 255),
            );
        }
        // Caret.
        let caret_x = (left + text::width(view.query, px) + 1.0).min(panel.field.x1 - 3.0);
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(122, 156, 240, 255),
            None,
            &Rect::new(
                caret_x,
                panel.field.y0 + 10.0 * scale,
                caret_x + 2.0 * scale,
                panel.field.y1 - 10.0 * scale,
            ),
        );
    }

    // The close cross.
    let c = panel.close.center();
    let arm = 7.0 * scale;
    let mut cross = vello::kurbo::BezPath::new();
    cross.move_to((c.x - arm, c.y - arm));
    cross.line_to((c.x + arm, c.y + arm));
    cross.move_to((c.x + arm, c.y - arm));
    cross.line_to((c.x - arm, c.y + arm));
    scene.stroke(
        &Stroke::new(2.0 * scale),
        Affine::IDENTITY,
        Color::from_rgba8(180, 182, 196, 255),
        None,
        &cross,
    );

    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgba8(255, 255, 255, 18),
        None,
        &Rect::new(
            0.0,
            panel.area.y0 + HEAD_H * scale - 1.0,
            panel.area.x1,
            panel.area.y0 + HEAD_H * scale,
        ),
    );

    // The keyboard.
    for (rect, key) in panel.keys.iter() {
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(44, 44, 53, 255),
            None,
            &rect.to_rounded_rect(6.0 * scale),
        );
        let (label, size) = match key {
            Key::Ch(ch) => (ch.to_string(), 16.0),
            Key::Back => ("del".to_string(), 13.0),
            Key::Space => ("space".to_string(), 12.0),
            Key::Done => ("done".to_string(), 13.0),
        };
        let px = size * scale;
        text::draw_centred(
            scene,
            &label,
            rect.center().x,
            text::baseline(rect.center().y, px),
            px,
            Color::from_rgba8(232, 233, 240, 255),
        );
    }

    // Results.
    let list = panel.list;
    if list.height() <= 0.0 {
        return;
    }
    if view.query.is_empty() {
        let msg = if view.built >= view.pages {
            format!("{} pages indexed", view.pages)
        } else {
            format!("indexing… {} of {} pages", view.built, view.pages)
        };
        text::draw(
            scene,
            &msg,
            list.x0 + 16.0 * scale,
            text::baseline(list.y0 + ROW_H * 0.5 * scale, 14.0 * scale),
            14.0 * scale,
            Color::from_rgba8(150, 152, 164, 255),
        );
        return;
    }

    if view.hits.is_empty() {
        let msg = if view.built >= view.pages {
            "Nothing found.".to_string()
        } else {
            format!(
                "Nothing yet — {} of {} pages indexed",
                view.built, view.pages
            )
        };
        text::draw(
            scene,
            &msg,
            list.x0 + 16.0 * scale,
            text::baseline(list.y0 + ROW_H * 0.5 * scale, 14.0 * scale),
            14.0 * scale,
            Color::from_rgba8(150, 152, 164, 255),
        );
        return;
    }

    let first = (view.scroll / panel.row_h).floor().max(0.0) as usize;
    let visible = (list.height() / panel.row_h).ceil() as usize + 2;
    for index in first..(first + visible).min(view.hits.len()) {
        let hit = &view.hits[index];
        let top = list.y0 + index as f64 * panel.row_h - view.scroll;
        let rect = Rect::new(list.x0, top, list.x1, top + panel.row_h);
        if rect.y1 < list.y0 - 1.0 || rect.y0 > list.y1 + 1.0 {
            continue;
        }
        text::draw(
            scene,
            &format!("{}", hit.page + 1),
            list.x0 + 16.0 * scale,
            text::baseline(rect.y0 + panel.row_h * 0.5, 13.0 * scale),
            13.0 * scale,
            Color::from_rgba8(122, 156, 240, 255),
        );
        let snippet_x = list.x0 + 56.0 * scale;
        let snippet = text::fit(
            &hit.snippet,
            14.0 * scale,
            list.x1 - snippet_x - 16.0 * scale,
        );
        text::draw(
            scene,
            &snippet,
            snippet_x,
            text::baseline(rect.y0 + panel.row_h * 0.5, 14.0 * scale),
            14.0 * scale,
            Color::from_rgba8(216, 218, 226, 255),
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(255, 255, 255, 12),
            None,
            &Rect::new(list.x0 + 16.0 * scale, rect.y1 - 1.0, list.x1, rect.y1),
        );
    }

    // How many, along the bottom edge of the list.
    if view.total > 0 {
        let label = format!(
            "{}{} match{}",
            view.total,
            if view.built < view.pages {
                " so far"
            } else {
                ""
            },
            if view.total == 1 { "" } else { "es" }
        );
        text::draw(
            scene,
            &label,
            list.x0 + 16.0 * scale,
            list.y1 - 8.0 * scale,
            12.0 * scale,
            Color::from_rgba8(140, 142, 156, 255),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_keeps_offsets() {
        let page = Page::new("Hello   Wörld\nnext line".to_string());
        assert_eq!(page.lower, "hello wörld next line");
        assert_eq!(&page.text[page.map[0] as usize..][..5], "Hello");
    }

    #[test]
    fn keys_reach_the_edges() {
        let panel = Panel::new(1080.0, 2000.0, 3.0, true);
        assert!(!panel.keys.is_empty());
        let bottom = panel.keys.iter().map(|(r, _)| r.y1).fold(0.0, f64::max);
        assert!(bottom <= panel.list.y0 + 1.0, "{bottom} {}", panel.list.y0);
        for (rect, _) in &panel.keys {
            assert!(rect.x0 >= -0.01 && rect.x1 <= 1080.01);
        }
    }

    #[test]
    fn query_normalises() {
        assert_eq!(normalize("  Foo   Bar "), "foo bar");
    }
}
