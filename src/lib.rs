//! A native Android PDF viewer: `NativeActivity` + a cdylib, no Java and no
//! gradle. winit owns the window and the input, wgpu draws through Vulkan,
//! vello rasterises, and `pdf_render` turns a page into a vello `Scene`.
//!
//! The document is opened from the intent the launcher handed us (see
//! [`App::open_uri`]): a URL, a file path, or a `content://` URI out of a
//! document provider. With no intent, it falls back to the first PDF that turns
//! up among a few well-known paths (see [`find_document`]).

#[cfg(target_os = "android")]
mod intent;
mod field;
mod logview;
mod nav;
mod net;
mod outline;
mod remote;
mod search;
mod text;
mod topbar;
mod url;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pdf::file::{File, FileOptions, NoCache, NoLog};
use pdf::object::ParseOptions;
use pdf_render::rangecache::{render_with_ranges, Borrowed, LazyReader};
use pdf_render::vello_backend::{OutlineBuilder, VelloBackend};
use pdf_render::{page_bounds, render_page, Cache, MemDir, StandardCache, Transform2F};
use vello::kurbo::{Affine, Rect, Stroke};
use vello::peniko::{Color, Fill};
use vello::util::{RenderContext, RenderSurface};
use vello::{wgpu, AaConfig, RenderParams, Renderer, RendererOptions, Scene};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, TouchPhase, WindowEvent};
use winit::keyboard::{Key, NamedKey};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

/// The standard 14 fonts, as `MemDir::parse_bundle` wants them: a browser has
/// no `$STANDARD_FONTS` and neither has an APK, so they travel in the binary.
const FONT_BUNDLE: &[u8] = include_bytes!("../assets/standard_fonts.bundle");

/// A swipe has to be this long (in pixels) to count as a page turn.
const SWIPE: f64 = 40.0;
/// A horizontal drag longer than this starts sliding the page under the finger.
const DRAG_START: f64 = 12.0;
/// How much of the page width a released swipe has to cover to turn the page.
const COMMIT: f64 = 0.25;
/// How long a released swipe takes to settle, in milliseconds.
const SETTLE_MS: f64 = 220.0;
/// The largest the page can be enlarged to: 6x is enough to read a footnote on
/// a phone, and past it a pinch does nothing rather than losing the page.
const MAX_ZOOM: f64 = 6.0;
/// How long two taps may be apart to count as a double tap, in milliseconds.
const DOUBLE_TAP_MS: u128 = 320;
/// How far apart two taps may be to count as a double tap, in dp.
const DOUBLE_TAP_SLOP: f64 = 36.0;
/// How long the screen may go untouched before the chrome — the strip at the
/// top and the bar at the bottom — gets out of the way. Reading is what the
/// page is for; the buttons come back on the next touch.
const BARS_IDLE: Duration = Duration::from_millis(3500);
/// How long after the page stops moving a frame is drawn for the pose it came
/// to rest at. Until then the last good frame is presented, moved, and a whole
/// page is not rasterised per frame of a gesture.
const SETTLE: Duration = Duration::from_millis(90);
/// How many times a frame that never reached the screen is asked for again,
/// 25 ms apart. Ten seconds of trying: a cold start on a slow phone is a
/// second or two of window that has no size yet, and giving up after eight
/// immediate tries is what left it black.
const RETRIES: u32 = 400;
/// Whether the chrome is up, for the ticker below: nothing else wakes the
/// window for a deadline that passes with no input.
static BARS_UP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// When the native side started. Every boot log line says how far into the
/// start it is, because "black for a few seconds" is a question about *where*
/// those seconds went, and logcat timestamps alone do not say when the
/// framework started loading the library.
static BOOT: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// Milliseconds since the native side started.
pub(crate) fn boot_ms() -> f64 {
    BOOT.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}

/// How the page sits in the page area: fitted (zoom 1) or enlarged under the
/// fingers. The page is laid out once at fit size and the scene is scaled, so
/// zooming costs no re-render and the glyphs stay sharp: vello rasterises the
/// outlines at the transformed size.
#[derive(Clone, Copy, Debug, PartialEq)]
struct View {
    /// Enlargement of the fitted page. 1.0 shows the whole page.
    zoom: f64,
    /// Where the page's top left corner sits in the page area, in physical
    /// pixels. Zero at fit size.
    pan: (f64, f64),
}

impl Default for View {
    fn default() -> View {
        View {
            zoom: 1.0,
            pan: (0.0, 0.0),
        }
    }
}

impl View {
    /// Keep the zoom in range and the page over the area: a zoomed page is
    /// dragged, but never dragged off the screen. `page_h` is how tall the page
    /// is at fit size — the page being fitted to the width, a page taller than
    /// the area scrolls upwards, and one that fits does not move.
    fn clamp(self, width: f64, height: f64, page_h: f64) -> View {
        let zoom = if self.zoom.is_finite() {
            self.zoom.clamp(1.0, MAX_ZOOM)
        } else {
            1.0
        };
        let page_h = if page_h.is_finite() && page_h > 0.0 {
            page_h
        } else {
            height
        };
        let tall = page_h * zoom;
        let pan = (
            self.pan.0.clamp(width * (1.0 - zoom), 0.0),
            if tall > height + 0.5 {
                // Taller than the area: scrolled, but never dragged past the
                // top or the bottom of it.
                self.pan.1.clamp(height - tall, 0.0)
            } else {
                // Short enough to fit: centred in the area, so a page that is
                // not a phone-shaped one sits in the middle rather than under
                // the strip.
                (height - tall) * 0.5
            },
        );
        View { zoom, pan }
    }

    /// Page coordinates to page-area coordinates.
    fn transform(&self) -> Affine {
        Affine::translate(self.pan) * Affine::scale(self.zoom)
    }

    /// The point of the fitted page that is under `at` on the screen.
    fn under(&self, at: (f64, f64)) -> (f64, f64) {
        (
            (at.0 - self.pan.0) / self.zoom,
            (at.1 - self.pan.1) / self.zoom,
        )
    }

    /// Zoom to `zoom` with `at` staying where it is on the screen.
    fn about(&self, zoom: f64, at: (f64, f64)) -> View {
        let (x, y) = self.under(at);
        let zoom = if zoom.is_finite() { zoom } else { self.zoom };
        View {
            zoom,
            pan: (at.0 - x * zoom, at.1 - y * zoom),
        }
    }
}

/// A pinch in progress: the finger distance and midpoint it started at, and the
/// point of the page that was under that midpoint, so it follows the fingers.
#[derive(Clone, Copy, Debug)]
struct Pinch {
    dist: f64,
    /// The page point under the starting midpoint, in fitted-page pixels.
    at: (f64, f64),
    zoom: f64,
}

/// How far apart the first two fingers are, and where their midpoint is.
fn pinch_measure(touches: &[(u64, (f64, f64))]) -> (f64, (f64, f64)) {
    let (_, a) = touches[0];
    let (_, b) = touches[1];
    let dist = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
    (dist, ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5))
}

// ---------------------------------------------------------------------------
// the document

/// An open PDF: the file, the current page, and the render cache (fonts, glyph
/// outlines, decoded images) which is worth keeping across page turns.
struct Doc {
    /// The file itself, or the URL it is being read from.
    source: Source,
    page: usize,
    cache: Cache<OutlineBuilder>,
    /// The bookmarks, read once: they never change while the document is open.
    outline: Vec<outline::Row>,
    /// The last failure a remote page reported, for the status line.
    error: Option<String>,
    /// (page, rounds) for a remote page that is still being fetched, so a page
    /// that can never be rendered stops asking for bytes.
    tries: (usize, u32),
    /// The document's name for the top strip: `/Info`'s title where the file
    /// has one, else the file's name, else the last part of the URL.
    title: String,
}

/// Where a document's bytes come from.
enum Source {
    /// A file on the device, whole in memory.
    File(LocalFile),
    /// A URL, fetched a byte range at a time as pages need them.
    Url(remote::Remote),
}

/// The local file type, written once: `File` is generic over its backend.
type LocalFile = File<Vec<u8>, NoCache, NoCache, NoLog>;

/// The render cache, with the standard 14 fonts the font bundle carries.
fn glyph_cache() -> Result<Cache<OutlineBuilder>, String> {
    let dir = MemDir::parse_bundle(FONT_BUNDLE).map_err(|e| format!("font bundle: {e:?}"))?;
    Ok(Cache::with_standard(
        OutlineBuilder::default(),
        StandardCache::with_dir(dir),
    ))
}

/// What the parser is allowed to do with a page.
///
/// `tolerant` for the structure — a viewer shows what it can rather than
/// refusing the file — and two limits raised far above the parser's own
/// (1 MiB strict, 10 MiB tolerant; 1024 px), because those caps guard against
/// untrusted *structure*, not against pictures: a 12 MP photo is 36 MB once
/// decoded, and a page whose figure is over the cap is drawn without it, which
/// looks like a rendering bug and is one from where the reader sits. A 64 MB
/// stream and an 8192 px side is about what a phone can hold for one image.
fn page_options() -> ParseOptions {
    let mut options = ParseOptions::tolerant();
    options.object_size_limit = 64 * 1024 * 1024;
    options.max_img_dim = 8192;
    options
}

impl Doc {
    fn open(path: &PathBuf) -> Result<Doc, String> {
        let file: LocalFile = FileOptions::uncached()
            .parse_options(page_options())
            .open_owned(path)
            .map_err(|e| format!("{}: {e:?}", path.display()))?;
        let outline = outline::read(&file);
        let title = info_title(&file.trailer)
            .or_else(|| {
                path.file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
            })
            .unwrap_or_default();
        log::info!("opened {} ({} pages)", path.display(), file.num_pages());
        Ok(Doc {
            source: Source::File(file),
            page: 0,
            cache: glyph_cache()?,
            outline,
            error: None,
            tries: (0, 0),
            title,
        })
    }

    /// Start reading a document over HTTP. Returns at once with no pages: the
    /// worker is fetching the head and tail of the file, and [`Doc::poll`] fills
    /// the document in when the structure arrives.
    fn open_url(url: &str) -> Doc {
        log::info!("opening {url}");
        Doc {
            source: Source::Url(remote::Remote::connect(url)),
            page: 0,
            cache: glyph_cache().unwrap_or_else(|e| {
                log::error!("{e}");
                Cache::new(OutlineBuilder::default())
            }),
            outline: Vec::new(),
            error: None,
            tries: (0, 0),
            title: String::new(),
        }
    }

    /// The name to show in the top strip: the document's own title while it has
    /// one, and until then the last part of its address.
    fn title(&self) -> String {
        match &self.source {
            Source::File(_) => self.title.clone(),
            Source::Url(remote) => remote
                .title
                .clone()
                .unwrap_or_else(|| short_name(&remote.url)),
        }
    }

    /// How far along the top strip's download numbers are: for a remote
    /// document, what has crossed the wire against the whole file; for a local
    /// one there is nothing to download, so the strip shows the title alone.
    fn stats(&self) -> String {
        match &self.source {
            Source::File(_) => String::new(),
            Source::Url(remote) => {
                let mut parts = Vec::new();
                if remote.len > 0 {
                    parts.push(format!(
                        "{} of {}",
                        bytes(remote.got),
                        bytes(remote.len as u64)
                    ));
                    parts.push(format!("{:.0}%", remote.fraction() * 100.0));
                }
                parts.join(" · ")
            }
        }
    }

    fn pages(&self) -> usize {
        match &self.source {
            Source::File(file) => file.num_pages() as usize,
            Source::Url(remote) => remote.pages,
        }
    }

    /// The URL this document came from, if it did not come from the device.
    fn url(&self) -> Option<&str> {
        match &self.source {
            Source::File(_) => None,
            Source::Url(remote) => Some(remote.url.as_str()),
        }
    }

    /// Take what a remote document's worker has brought. Returns whether
    /// anything changed that the screen should show.
    fn poll(&mut self) -> bool {
        let Source::Url(remote) = &mut self.source else {
            return false;
        };
        let changed = remote.poll();
        if let Some(rows) = remote.take_outline() {
            self.outline = rows;
        }
        if let Some(e) = remote.error.clone() {
            self.error = Some(e);
        }
        changed
    }

    /// A line about a document that is still arriving, for the bar.
    fn progress(&self) -> Option<String> {
        let Source::Url(remote) = &self.source else {
            return None;
        };
        if let Some(e) = remote.error.as_deref() {
            return Some(e.to_string());
        }
        if remote.pages == 0 {
            return Some(format!("opening {} …", short(&remote.url)));
        }
        if remote.pending > 0 {
            let percent = (remote.fraction() * 100.0).round() as u32;
            return Some(format!(
                "{} — {percent}% of {} KiB",
                short(&remote.url),
                remote.len / 1024
            ));
        }
        None
    }

    /// Go to `page`, if it exists. Returns whether the page actually changed.
    fn jump(&mut self, page: usize) -> bool {
        if page < self.pages() && page != self.page {
            self.page = page;
            true
        } else {
            false
        }
    }

    /// Turn page `index` into a vello scene.
    ///
    /// For a document at a URL this runs one parse pass over the bytes that are
    /// already here. A page whose objects are not all present comes back empty
    /// and the miss is handed to the worker; the frame after the bytes land
    /// gets further, and after a few rounds the page is complete.
    fn scene_page(&mut self, index: usize, width: u32, height: u32) -> Option<(Scene, f64)> {
        let Doc {
            source,
            cache,
            error,
            tries,
            ..
        } = self;
        match source {
            Source::File(file) => sheet(file, index, width, height, cache),
            Source::Url(remote) => {
                // Nothing to parse yet, a request already in flight, or a page
                // that does not exist: draw no page this frame.
                if remote.pages == 0 || index >= remote.pages || remote.pending > 0 {
                    return None;
                }
                if tries.0 != index {
                    *tries = (index, 0);
                }
                // Byte at a time, a page that will never parse would ask for
                // ranges until the battery went flat.
                if tries.1 > 24 {
                    *error = Some(format!("page {} will not render", index + 1));
                    return None;
                }
                if remote.stalled() {
                    // The server claims a range and sends nothing, twice: the
                    // page is not coming, and saying why beats a blank screen.
                    if error.is_none() {
                        *error = Some(format!(
                            "{} answers range requests with nothing",
                            short(&remote.url)
                        ));
                    }
                    return None;
                }
                let mut missing = Vec::new();
                let outcome = render_with_ranges(
                    &mut remote.cache,
                    1,
                    |reader| {
                        let file: File<Borrowed<'_, LazyReader<'_>>, NoCache, NoCache, NoLog> =
                            File::load_data(
                                Borrowed(reader),
                                b"",
                                // Lenient, and generous about image size: see
                                // `crate::page_options`.
                                crate::page_options(),
                                NoCache,
                                NoCache,
                                NoLog,
                            )?;
                        Ok(sheet(&file, index, width, height, cache))
                    },
                    |_, wanted| {
                        missing = wanted.to_vec();
                        Err("waiting for bytes".to_string())
                    },
                );
                if !missing.is_empty() {
                    tries.1 += 1;
                    remote.request(&missing);
                    return None;
                }
                match outcome {
                    Ok(scene) => {
                        *tries = (index, 0);
                        scene
                    }
                    Err(e) => {
                        log::error!("page {}: {e}", index + 1);
                        *error = Some(e);
                        None
                    }
                }
            }
        }
    }
}

/// Render page `index` of `file` onto a sheet: the paper, its shadow and its
/// hairline edge, with the page's own content on top.
///
/// The sheet is the width of the page area, edge to edge: the page is fitted
/// to the width and keeps its proportions, so one taller than the window is
/// scrolled rather than shrunk. The height it comes out at is returned with
/// the scene, because that is what bounds the scroll.
fn sheet<B: pdf::backend::Backend>(
    file: &File<B, NoCache, NoCache, NoLog>,
    index: usize,
    width: u32,
    height: u32,
    cache: &mut Cache<OutlineBuilder>,
) -> Option<(Scene, f64)> {
        let page = file.get_page(index as u32).ok()?;
        let bounds = page_bounds(&page);
        let (mut pw, mut ph) = (bounds.width().max(1.0), bounds.height().max(1.0));
        // A quarter turn swaps the page's footprint.
        if page.rotate() % 180 != 0 {
            std::mem::swap(&mut pw, &mut ph);
        }
        // Fit to the width: the paper touches both edges of the area, and a
        // page taller than the window hangs over the bottom, to be scrolled.
        let scale = width as f32 / pw;
        // `row_major` keeps us clear of pathfinder's Vector2F, which
        // pdf_render gets from a git checkout of its own.
        let transform = Transform2F::row_major(scale, 0.0, 0.0, 0.0, scale, 0.0);
        let paper = Rect::new(
            0.0,
            0.0,
            (pw * scale) as f64,
            (ph * scale) as f64,
        );

        let resolver = file.resolver();
        let mut backend = VelloBackend::new(cache);
        let content = match render_page(&mut backend, &resolver, &page, transform) {
            Ok(_) => backend.finish(),
            Err(e) => {
                log::error!("page {}: {e:?}", index + 1);
                return None;
            }
        };

        // Draw the sheet under the content, so it travels with the page when a
        // swipe carries it sideways.
        let mut scene = Scene::new();
        let unit = (width.min(height) as f64 * 0.008).clamp(0.75, 6.0);
        for (grow, alpha) in [(3.0, 8u8), (2.0, 14), (1.0, 24)] {
            let r = Rect::new(
                paper.x0 - grow * unit,
                paper.y0 - grow * unit * 0.5,
                paper.x1 + grow * unit,
                paper.y1 + grow * unit * 1.5,
            )
            .to_rounded_rect(2.0 * unit);
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgba8(0, 0, 0, alpha),
                None,
                &r,
            );
        }
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(253, 253, 251, 255),
            None,
            &paper,
        );
        scene.append(&content, None);
        // The edge itself: one dark hairline, so the sheet's bounds are exact
        // even where the shadow is faint.
        scene.stroke(
            &Stroke::new((1.0 * unit).max(1.0)),
            Affine::IDENTITY,
            Color::from_rgba8(0, 0, 0, 70),
            None,
            &paper,
        );
        Some((scene, paper.height()))
}

/// A URL short enough for one line of status: the scheme dropped, the middle of
/// a long path left out, the file name kept.
fn short(url: &str) -> String {
    let plain = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let chars: Vec<char> = plain.chars().collect();
    if chars.len() <= 46 {
        return plain.to_string();
    }
    let tail: String = chars[chars.len() - 36..].iter().collect();
    format!("…{tail}")
}

/// The `/Title` out of a document's `/Info` dictionary, tidied for one line of
/// small text: whitespace collapsed, nothing control-ish, and short.
fn info_title(trailer: &pdf::file::Trailer) -> Option<String> {
    let raw = trailer.info_dict.as_ref()?.title.as_ref()?.to_string_lossy();
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_whitespace() {
            if !out.ends_with(' ') {
                out.push(' ');
            }
        } else if !ch.is_control() {
            out.push(ch);
        }
    }
    let out = out.trim().to_string();
    if out.is_empty() {
        return None;
    }
    let mut short: String = out.chars().take(90).collect();
    if short.chars().count() < out.chars().count() {
        short.push('…');
    }
    Some(short)
}

/// The last part of an address, for a document whose `/Info` says nothing: the
/// path's file name, or the host if the path is empty.
fn short_name(url: &str) -> String {
    let plain = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let last = plain.rsplit('/').next().unwrap_or(plain);
    if last.is_empty() {
        return plain.to_string();
    }
    // Drops a query string or fragment that came along with it.
    last.split(['?', '#']).next().unwrap_or(last).to_string()
}

/// A byte count as a phone would say it: 940 B, 264 kB, 2.1 MB.
fn bytes(n: u64) -> String {
    const K: f64 = 1024.0;
    let n = n as f64;
    if n < K {
        format!("{n:.0} B")
    } else if n < K * K {
        format!("{:.0} kB", n / K)
    } else {
        format!("{:.1} MB", n / (K * K))
    }
}

/// Where the document comes from: `$PDF_PATH` if the launcher set it, else the
/// first `*.pdf` in a few places a phone actually has one.
fn find_document(data_dir: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PDF_PATH").map(PathBuf::from) {
        if p.is_file() {
            return Some(p);
        }
    }
    let mut dirs = Vec::new();
    if let Some(dir) = data_dir {
        dirs.push(dir);
    }
    // The app's own external files directory: `adb push` can create it, and no
    // runtime permission is needed to read it back.
    dirs.push(PathBuf::from(
        "/sdcard/Android/data/org.pdf_rs.viewer/files",
    ));
    // The media folder of the same package: a file manager can drop things in
    // here, and unlike `Android/data` the app may read it without a permission.
    dirs.push(PathBuf::from("/sdcard/Android/media/org.pdf_rs.viewer"));
    dirs.push(PathBuf::from("/sdcard/Download"));
    dirs.push(PathBuf::from("/storage/emulated/0/Download"));
    dirs.push(PathBuf::from("/data/local/tmp"));
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            log::info!("no PDFs in {} (cannot read it)", dir.display());
            continue;
        };
        let mut found: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")))
            .collect();
        found.sort();
        if let Some(p) = found.into_iter().next() {
            return Some(p);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// the system bars

/// How many pixels at the bottom of the window the system navigation bar or
/// gesture pill covers. Android only: elsewhere there is no such thing.
#[cfg(target_os = "android")]
fn bottom_inset(window: &Window) -> f64 {
    use winit::platform::android::WindowExtAndroid;
    let height = window.inner_size().height as i32;
    let rect = window.content_rect();
    (height - rect.bottom).max(0) as f64
}

#[cfg(not(target_os = "android"))]
fn bottom_inset(_window: &Window) -> f64 {
    0.0
}

/// How many pixels at the top of the window the display cutout (camera notch)
/// and, if it is showing, the status bar cover. The window is deliberately let
/// into the cutout (`shortEdges`) so the page can fill the screen; anything
/// drawn as a control has to stay below this line.
#[cfg(target_os = "android")]
fn top_inset(window: &Window) -> f64 {
    use winit::platform::android::WindowExtAndroid;
    let height = window.inner_size().height as i32;
    let rect = window.content_rect();
    // The rect is empty until Android first reports one; and a rect that is
    // taller than the window is nonsense, not a 2000-pixel notch.
    if rect.bottom <= 0 || rect.bottom > height * 2 {
        return 0.0;
    }
    rect.top.max(0) as f64
}

#[cfg(not(target_os = "android"))]
fn top_inset(_window: &Window) -> f64 {
    0.0
}

// ---------------------------------------------------------------------------
// the GPU side

/// The chrome outside the page for one frame, gathered by the caller: whether
/// it is up at all, and what the strip in the cutout's safe area says.
struct Chrome<'a> {
    bars: bool,
    title: &'a str,
    stats: &'a str,
}

/// The address bar's contents for one frame, gathered by the caller.
struct Address<'a> {
    text: &'a str,
    status: Option<&'a str>,
    busy: bool,
    /// The text is drawn by the framework's own field on top of the bar, with
    /// its caret and selection: the bar leaves that part empty.
    native: bool,
    /// Bumped by the caller whenever anything above changed.
    rev: u64,
}

/// One line of text on a dark pill, centred just above the navigation bar.
fn note_line(
    scene: &mut Scene,
    width: f64,
    height: f64,
    scale: f64,
    bottom_inset: f64,
    note: &str,
) {
    let nav = nav::layout(width, height, scale, bottom_inset);
    let px = 12.5 * scale;
    let w = (text::width(note, px) + 22.0 * scale).min(width - 8.0 * scale);
    let h = 26.0 * scale;
    let x = (width - w) * 0.5;
    let y1 = nav.bar.y0 - 10.0 * scale;
    let pill = Rect::new(x, y1 - h, x + w, y1);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgba8(18, 18, 22, 235),
        None,
        &pill.to_rounded_rect(h * 0.5),
    );
    text::draw_centred(
        scene,
        note,
        width * 0.5,
        text::baseline(pill.center().y, px),
        px,
        Color::from_rgba8(226, 228, 238, 255),
    );
}

/// Everything wgpu and vello need, tied to one window: the surface, the renderer
/// and the scene of the page that is currently on screen.
struct Gfx {    render_cx: RenderContext,
    surface: RenderSurface<'static>,
    renderer: Renderer,
    /// The current page on its own, cached while nothing moves.
    page: Option<Scene>,
    /// What actually gets rendered: the page, its neighbour mid-swipe, and the
    /// navigation bar on top.
    scene: Scene,
    /// The page either side of the current one, kept for the length of a swipe.
    peek: Option<(usize, Scene)>,
    /// The page changed or the window was resized: the page has to be rebuilt.
    stale: bool,
    /// The composite in `scene` was built with a swipe offset on it.
    slid: bool,
    /// The outline drawer: `Some(scroll)` while it is open. Part of the scene,
    /// so a change here means the composite has to be rebuilt.
    drawer: Option<f64>,
    /// The search panel's revision: the panel holds strings and a scroll
    /// offset, and this is how the GPU side notices they changed.
    find_rev: u64,
    /// Whether the panel was up for the last composite. Opening or closing it
    /// changes the frame even when the revision happens to be the same (an
    /// empty query on a fresh panel does not touch a single hit).
    find_up: bool,
    /// Whether the last composite had a document at all. A frame without one
    /// is still a frame (the canvas and the bar), so going from none to one
    /// has to rebuild it.
    doc_up: bool,
    /// The address bar was up for the last composite, at this revision.
    addr_up: bool,
    addr_rev: u64,
    /// The one-line note shown above the bar: a document still arriving.
    note: String,
    /// The view the composite in `scene` was built with: a zoom or a pan means
    /// the page has to be put down again, even though its scene is unchanged.
    view: View,
    /// The text module's generation when the scene was built: when the UI font
    /// finishes parsing, the bar and the drawer need a new composite to get
    /// their words.
    text_gen: u64,
    /// winit's density: the navigation bar is laid out in dp.
    scale: f64,
    /// The chrome the last composite was built with: whether it was up, and the
    /// height the page was laid out for. A change means a new composite.
    page_h: u32,
    /// How tall the current page is at fit size, in pixels: the page is fitted
    /// to the width, so this is what the vertical scroll is bounded by.
    fit: f64,
    /// The strip's text in the last composite, so a download ticking up gets a
    /// frame of its own.
    chrome_key: String,
    /// Frames logged with their geometry, capped at four.
    dbg: u32,
    /// Slow frames logged, capped so a device that is always slow does not
    /// fill the log.
    slow: u32,
    /// The blit that puts the rendered frame on the swapchain through an
    /// affine. `None` if the pipeline could not be built: the frame is then
    /// copied as it is, without the moving-frame interpolation.
    blit: Blit,
    /// The pose the frame in `surface.target_view` was rasterised at: the last
    /// good frame. While the page moves and the next frame is being drawn, this
    /// one is presented under the new pose instead.
    frame_pose: Option<Pose>,
    /// The pose `scene` was composited at.
    scene_pose: Option<Pose>,
    /// Whether `scene` holds anything composite has built.
    composited: bool,
    /// When the pose last changed: the page is moving right now, so a
    /// re-rasterisation can wait and the last good frame is moved instead.
    moved_at: Option<Instant>,
    /// Which way the neighbour page was put into `scene`, `0` for none: a swipe
    /// that starts needs one composite to get the page sliding in behind.
    peek_turn: i8,
    /// A redraw is already scheduled for the moment the movement settles.
    settle_wake: bool,
    /// The window the surface belongs to: the settle timer asks it for a frame.
    window: Arc<Window>,
    /// The log page: whether it was up in the last composite, how far it was
    /// scrolled, and how many lines had been written then.
    log_open: bool,
    log_scroll: f64,
    log_gen: u64,
}

/// Where the page sits for one frame: the view's zoom and pan, and the swipe
/// offset on top of it.
///
/// A rasterised frame is only valid for the pose it was drawn at, and the
/// difference between two poses is a plain affine. That is what lets the last
/// good frame be presented — moved — while the next one is still being drawn,
/// instead of leaving the page to jump from one finished frame to the next.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Pose {
    view: View,
    dx: f64,
}

impl Pose {
    /// The transform the page content was drawn through.
    fn affine(&self) -> Affine {
        Affine::translate((self.dx, 0.0)) * self.view.transform()
    }

    /// Screen pixels of the frame drawn at `drawn`, mapped to where this pose
    /// puts them.
    fn shift_from(&self, drawn: Pose) -> Affine {
        if *self == drawn {
            return Affine::IDENTITY;
        }
        let m = self.affine() * drawn.affine().inverse();
        let finite = m.as_coeffs().iter().all(|v| v.is_finite());
        if finite {
            m
        } else {
            Affine::IDENTITY
        }
    }
}

/// The canvas the sheet lies on, as the blit's clear colour.
const CANVAS: wgpu::Color = wgpu::Color {
    r: 34.0 / 255.0,
    g: 34.0 / 255.0,
    b: 39.0 / 255.0,
    a: 1.0,
};

/// A full-screen triangle that puts one texture on the swapchain through an
/// affine. Sampling outside the texture is thrown away, so a frame presented
/// under a moved pose does not smear its edge pixels over the window.
const BLIT_WGSL: &str = r#"
struct Uniform {
    row0: vec4<f32>,
    row1: vec4<f32>,
    size: vec4<f32>,
    clip: vec4<f32>,
};

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> u: Uniform;

struct Varying {
    @builtin(position) pos: vec4<f32>,
    @location(0) at: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> Varying {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    let ndc = vec2<f32>(p.x * 2.0 - 1.0, 1.0 - p.y * 2.0);
    return Varying(vec4<f32>(ndc, 0.0, 1.0), p * u.size.xy);
}

@fragment
fn fs(v: Varying) -> @location(0) vec4<f32> {
    let at = vec2<f32>(
        u.row0.x * v.at.x + u.row0.y * v.at.y + u.row0.z,
        u.row1.x * v.at.x + u.row1.y * v.at.y + u.row1.z,
    );
    if (at.x < 0.0 || at.y < 0.0 || at.x > u.size.x || at.y >= u.size.y) {
        discard;
    }
    // The page rectangle only ever shows page: a frame moved under a bar that
    // stayed put must not pull the bar's pixels up into the page.
    if (at.y < u.clip.x || at.y > u.clip.y) {
        discard;
    }
    return textureSample(src, samp, at / u.size.xy);
}
"#;

/// Two uniform slots (48 bytes each, 256 apart) and the pipeline that reads
/// them: slot 0 for the frame moved into the current pose, slot 1 for the
/// frame as it was drawn. The chrome is never interpolated — a bar that slides
/// with the page looks broken — so the two are drawn as separate passes over
/// separate scissor rectangles.
struct Blit {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform: wgpu::Buffer,
}

impl Blit {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Blit {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit"),
            source: wgpu::ShaderSource::Wgsl(BLIT_WGSL.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    // The vertex stage reads u.size to place the triangle in
                    // frame pixels, so this one is visible to both stages.
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("blit"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("blit"),
            size: 2 * 256,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Blit {
            pipeline,
            layout,
            sampler,
            uniform,
        }
    }

    /// The uniform for a frame mapped by `inv` (screen pixels to the pixels of
    /// the frame that was drawn), in slot `slot`. `clip` is the band of the
    /// frame that may be shown: everything else is the chrome, which is never
    /// moved.
    fn slot(
        &self,
        queue: &wgpu::Queue,
        slot: usize,
        inv: Affine,
        width: f64,
        height: f64,
        clip: (f64, f64),
    ) {
        let [a, b, c, d, e, f] = inv.as_coeffs();
        let mut data = [0f32; 16];
        data[0] = a as f32;
        data[1] = c as f32;
        data[2] = e as f32;
        data[4] = b as f32;
        data[5] = d as f32;
        data[6] = f as f32;
        data[8] = width as f32;
        data[9] = height as f32;
        data[12] = clip.0 as f32;
        data[13] = clip.1 as f32;
        let mut bytes = [0u8; 64];
        for (i, v) in data.iter().enumerate() {
            bytes[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        queue.write_buffer(&self.uniform, (slot * 256) as u64, &bytes);
    }

    fn bind(&self, device: &wgpu::Device, src: &wgpu::TextureView, slot: usize) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(src),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.uniform,
                        offset: (slot * 256) as u64,
                        size: wgpu::BufferSize::new(64),
                    }),
                },
            ],
        })
    }
}

/// Present one flat frame, before vello exists: the window shows what it is
/// given, and a surface that has been attached but never presented is black.
fn paint_flat(cx: &RenderContext, surface: &RenderSurface<'static>) {
    let handle = &cx.devices[surface.dev_id];
    let frame = match surface.surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(frame)
        | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
        other => {
            log::warn!("flat frame: {other:?}");
            return;
        }
    };
    let view = frame
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = handle
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("flat"),
        });
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("flat"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(CANVAS),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    handle.queue.submit(Some(encoder.finish()));
    handle.queue.present(frame);
}

impl Gfx {
    fn new(window: Arc<Window>) -> Result<Gfx, String> {
        let size = window.inner_size();
        let (width, height) = (size.width.max(1), size.height.max(1));
        let scale = window.scale_factor();
        log::info!("surface {width}x{height}, scale {scale} at {:.0} ms", boot_ms());

        let mut render_cx = RenderContext::new();
        let surface: RenderSurface<'static> = block_on(render_cx.create_surface(
            window.clone(),
            width,
            height,
            wgpu::PresentMode::AutoVsync,
        ))
        .map_err(|e| format!("surface: {e:?}"))?;
        // Which GPU, on which backend: a phone that falls back to GL is a phone
        // where vello is slow, and that is worth knowing from the log alone.
        {
            let handle = &render_cx.devices[surface.dev_id];
            let info = handle.adapter().get_info();
            log::info!(
                "surface up at {:.0} ms: {:?}/{:?} {}, format {:?}, present {:?}",
                boot_ms(),
                info.backend,
                info.device_type,
                info.name,
                surface.format,
                surface.config.present_mode,
            );
        }

        // Put the paper on the screen before anything expensive is built. An
        // attached surface that has never been presented is black, and building
        // vello's pipelines on a phone GPU takes seconds: those were the black
        // seconds at start. One clear pass costs almost nothing.
        paint_flat(&render_cx, &surface);
        log::info!("flat frame presented at {:.0} ms", boot_ms());

        let device = &render_cx.devices[surface.dev_id];
        // Area anti-aliasing is the one pipeline every mobile GPU manages, and
        // it is all a page of text needs.
        let renderer = Renderer::new(
            &device.device,
            RendererOptions {
                antialiasing_support: vello::AaSupport::area_only(),
                ..Default::default()
            },
        )
        .map_err(|e| format!("renderer: {e:?}"))?;
        log::info!("renderer ready at {:.0} ms", boot_ms());
        let blit = Blit::new(&device.device, surface.format);

        // The surface keeps the window alive on its own (`Surface<'static>`);
        // the clone here is for the settle timer's redraw.
        Ok(Gfx {
            render_cx,
            surface,
            renderer,
            page: None,
            scene: Scene::default(),
            peek: None,
            stale: true,
            slid: false,
            doc_up: false,
            drawer: None,
            find_rev: 0,
            find_up: false,
            addr_up: false,
            addr_rev: 0,
            note: String::new(),
            view: View::default(),
            text_gen: 0,
            scale,
            page_h: 0,
            fit: 0.0,
            chrome_key: String::new(),
            dbg: 0,
            slow: 0,
            blit,
            frame_pose: None,
            scene_pose: None,
            composited: false,
            moved_at: None,
            peek_turn: 0,
            settle_wake: false,
            window,
            log_open: false,
            log_scroll: 0.0,
            log_gen: 0,
        })
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.render_cx
            .resize_surface(&mut self.surface, width, height);
        self.stale = true;
    }

    /// Render one frame. `slide` describes a swipe in progress: the current page
    /// is `dx` pixels off, and `turn` (+1 = next page, -1 = previous) says which
    /// page is sliding in behind it. `drawer` is the outline drawer: whether it
    /// is open and how far it is scrolled. Returns false when nothing reached
    /// the screen (an occluded or outdated surface), so the caller can try again.
    fn draw(
        &mut self,
        mut doc: Option<&mut Doc>,
        bottom_inset: f64,
        top_inset: f64,
        view: View,
        slide: Option<(f64, i8)>,
        drawer: Option<f64>,
        log: Option<f64>,
        find: Option<(search::View, u64)>,
        address: Option<Address<'_>>,
        note: Option<&str>,
        chrome: Chrome<'_>,
    ) -> bool {
        let (width, height) = (self.surface.config.width, self.surface.config.height);
        if width == 0 || height == 0 {
            return false;
        }
        let at = Instant::now();
        let (w, h) = (width as f64, height as f64);
        // The page is laid out for the whole window. The bar and the strip are
        // overlays drawn over it, and a page that changed size or place as they
        // came and went would look like the app losing the reader's spot: so
        // where the page sits is computed without them. `bar_top` is where the
        // bar sits, and only the bar cares about it.
        let page_height = (h as u32).max(1);
        let bar_top = nav::layout(w, h, self.scale, bottom_inset).bar.y0;
        if page_height != self.page_h {
            self.page_h = page_height;
            self.stale = true;
            self.peek = None;
        }
        let view = view.clamp(w, h, self.fit);
        self.view = view;

        // With no document open there is nothing to rasterise, and there is
        // still a frame to draw: the canvas, the bar, and the words that say
        // how to get a document. A window that presents nothing stays black.
        let doc_up = doc.is_some();
        let doc_changed = doc_up != self.doc_up;
        self.doc_up = doc_up;

        // Rasterising a page is the expensive part; keep it while nobody moves.
        let mut rebuilt = false;
        match doc.as_deref_mut() {
            Some(doc) => {
                if self.stale || self.page.is_none() {
                    self.page = match doc.scene_page(doc.page, width, page_height) {
                        Some((scene, fit)) => {
                            self.fit = fit;
                            Some(scene)
                        }
                        None => None,
                    };
                    self.peek = None;
                    self.stale = false;
                    rebuilt = true;
                }
            }
            None => {
                self.page = None;
                self.peek = None;
                self.fit = 0.0;
                // The first frame (and any resize) still composites: the
                // canvas and the bar have to be built and presented.
                rebuilt = self.stale;
                self.stale = false;
            }
        }

        let (dx, turn) = slide.unwrap_or((0.0, 0));
        let moving = turn != 0 && dx.abs() > 0.01;
        // The strip says what the document is and how much of it has arrived:
        // both change while a download runs, and both need a new composite.
        let chrome_key = format!(
            "{}|{}|{}",
            chrome.bars, chrome.title, chrome.stats
        );
        let chrome_moved = chrome_key != self.chrome_key;
        self.chrome_key = chrome_key;
        // The drawer is part of the scene: it has to be rebuilt when it opens,
        // closes or scrolls, not only when the page under it changes.
        let drawer_moved = self.drawer != drawer;
        self.drawer = drawer;
        // The log page: opening, closing, scrolling, or a line that arrived
        // since the last frame all want a new composite. It is a screenful of
        // text, not a page, so it is cheap.
        let log_moved = self.log_open != log.is_some()
            || log.is_some_and(|scroll| scroll != self.log_scroll)
            || (log.is_some() && logview::generation() != self.log_gen);
        self.log_open = log.is_some();
        self.log_scroll = log.unwrap_or(0.0);
        self.log_gen = logview::generation();
        let find_rev = find.as_ref().map_or(0, |(_, rev)| *rev);
        let find_up = find.is_some();
        let find_moved = find_rev != self.find_rev || find_up != self.find_up;
        self.find_rev = find_rev;
        self.find_up = find_up;
        let gen = text::generation();
        let text_arrived = gen != self.text_gen;
        self.text_gen = gen;
        let addr_up = address.is_some();
        let addr_rev = address.as_ref().map_or(0, |a| a.rev);
        let addr_moved = addr_up != self.addr_up || addr_rev != self.addr_rev;
        self.addr_up = addr_up;
        self.addr_rev = addr_rev;
        let note = note.unwrap_or_default().to_string();
        let note_moved = note != self.note;
        self.note = note;
        // A change in the *page* — a new page, the drawer, the bar's words —
        // needs a new composite. A change in the *pose* — the finger moving the
        // page, a pinch — does not: the frame that is already rasterised can be
        // shown moved, which is a quad on the GPU rather than a whole page
        // drawn again, and that is the difference between a page that follows
        // the finger and one that jumps a few times a second.
        let pose = Pose { view, dx };
        let bars = chrome.bars;
        let structural = rebuilt
            || doc_changed
            || drawer_moved
            || log_moved
            || text_arrived
            || find_moved
            || addr_moved
            || note_moved
            || chrome_moved
            || !self.composited
            // The composite has a swipe offset on it: the swipe ended, and the
            // page has to be put back where it rests.
            || self.slid
            // A swipe that starts needs one composite, for the page that slides
            // in behind this one.
            || (moving && self.peek_turn != turn);
        if structural {
            self.composite(
                doc,
                w,
                h,
                page_height,
                bottom_inset,
                top_inset,
                view,
                dx,
                turn,
                log,
                find.map(|(view, _)| view),
                address,
                chrome,
            );
            self.slid = moving;
            self.composited = true;
            self.scene_pose = Some(pose);
        }
        if self.scene_pose != Some(pose) {
            self.moved_at = Some(Instant::now());
        }
        let moving_now = self.moved_at.is_some_and(|at| at.elapsed() < SETTLE);
        // Rasterise when the scene changed, when there is no frame yet, or when
        // the page has come to rest at a pose that is not the one in the frame.
        // While it is still moving the frame that is there is presented, moved.
        let fresh = structural || self.frame_pose.is_none() || !moving_now;
        if fresh {
            self.settle_wake = false;
        } else {
            self.wake_when_settled();
        }

        // `rasterise` wants all of `self`; hand it the scene by value and back.
        let scene = std::mem::take(&mut self.scene);
        let n = scene.encoding().draw_data.len();
        let drawn = if fresh {
            self.rasterise(&scene, pose)
        } else {
            true
        };
        self.scene = scene;
        let shown = drawn && self.present(pose, bar_top, top_inset, bars);
        // A slow frame is what "it is laggy" means, and the numbers say where
        // it went: a rebuild is the page being rasterised again, no rebuild is
        // the GPU still taking its time on a scene that did not change.
        let took = at.elapsed();
        if took > Duration::from_millis(33) && self.slow < 200 {
            self.slow += 1;
            log::warn!(
                "slow frame {:.1} ms: {width}x{height}, scene {n} items, rebuilt {rebuilt}, \
                 moving {moving}, shown {shown}",
                took.as_secs_f64() * 1000.0
            );
        }
        if self.dbg < 4 {
            self.dbg += 1;
            log::info!(
                "draw {}: {width}x{height}, insets {top_inset:.0}/{bottom_inset:.0}, \
                 zoom {:.2}, bar_top {bar_top:.0}, page_h {page_height}, rebuilt {rebuilt}, \
                 moving {moving}, shown {shown}, scene {n} items",
                self.dbg,
                view.zoom,
            );
        }
        shown
    }

    /// Assemble the frame: the neighbour (if any) under the finger, the current
    /// page, then the bar.
    #[allow(clippy::too_many_arguments)]
    fn composite(
        &mut self,
        mut doc: Option<&mut Doc>,
        width: f64,
        height: f64,
        page_height: u32,
        bottom_inset: f64,
        top_inset: f64,
        view: View,
        dx: f64,
        turn: i8,
        log: Option<f64>,
        find: Option<search::View<'_>>,
        address: Option<Address<'_>>,
        chrome: Chrome<'_>,
    ) {
        self.scene.reset();
        // With no document the bar still draws, and the canvas around it says
        // how to get one; the numbers the bars and the drawer want are zero.
        let (page_no, page_count, has_outline) = match doc.as_deref() {
            Some(doc) => (doc.page, doc.pages(), !doc.outline.is_empty()),
            None => (0, 0, false),
        };

        // The paper is clipped to the page area whatever is done to it. At fit
        // size a page fitted to the width is usually taller than the window, and
        // a page panned or zoomed has to stay off the bar and the strip.
        let clip = view.zoom > 1.0001
            || view.pan != (0.0, 0.0)
            || self.fit > page_height as f64 + 0.5;
        if clip {
            let area = Rect::new(0.0, 0.0, width, page_height as f64);
            self.scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                Affine::IDENTITY,
                &area,
            );
        }

        // The neighbour sliding in behind the current page, mid-swipe.
        if turn != 0 && dx.abs() > 0.01 {
            if let Some(doc) = doc.as_deref_mut() {
                let target = doc.page as i64 + turn as i64;
                let target =
                    (target >= 0 && (target as usize) < doc.pages()).then_some(target as usize);
                if let Some(target) = target {
                    if self.peek.as_ref().map(|(page, _)| *page) != Some(target) {
                        self.peek = doc
                            .scene_page(target, width as u32, page_height)
                            .map(|(scene, _)| (target, scene));
                    }
                    if let Some((_, peek)) = self.peek.as_ref() {
                        let x = if turn > 0 { dx + width } else { dx - width };
                        // The neighbour sits where the current page does
                        // vertically: centred, or scrolled the same amount.
                        self.scene
                            .append(peek, Some(Affine::translate((x, view.pan.1))));
                    }
                }
            }
        }

        // Which way the neighbour went into the scene, so the same swipe is not
        // composited again frame after frame.
        self.peek_turn = if turn != 0 && dx.abs() > 0.01 { turn } else { 0 };

        // The page is laid out at fit size and scaled from there, so a zoom is a
        // transform rather than a re-render: vello rasterises the outlines at
        // the transformed size, glyphs and all.
        if let Some(page) = self.page.as_ref() {
            let xform = Affine::translate((dx, 0.0)) * view.transform();
            self.scene.append(page, Some(xform));
        }
        if clip {
            self.scene.pop_layer();
        }
        if let (Some(doc), Some(scroll)) = (doc.as_deref(), self.drawer) {
            let panel = outline::Panel::new(
                width,
                nav::layout(width, height, self.scale, bottom_inset).bar.y0,
                self.scale,
            );
            let scroll = panel.scroll(scroll, doc.outline.len(), self.scale);
            outline::draw(
                &mut self.scene,
                &doc.outline,
                &panel,
                scroll,
                self.scale,
                doc.page,
            );
        }
        if let Some(view) = find.as_ref() {
            let bar_top = nav::layout(width, height, self.scale, bottom_inset).bar.y0;
            let panel = search::Panel::new(width, bar_top, top_inset, self.scale, view.keyboard);
            search::draw(&mut self.scene, &panel, view);
        }
        // Nothing open: say so in the middle of the canvas. Leaving it empty is
        // how the window ends up showing the black it was born with.
        if doc.is_none() && address.is_none() {
            let cx = width * 0.5;
            let cy = page_height as f64 * 0.5;
            let px = 15.0 * self.scale;
            text::draw_centred(
                &mut self.scene,
                "No PDF open",
                cx,
                text::baseline(cy, px),
                px,
                Color::from_rgba8(206, 208, 220, 255),
            );
            let px = 11.5 * self.scale;
            let lines = [
                "the link button below opens a URL",
                "or put a PDF in the app's files folder",
            ];
            for (i, line) in lines.iter().enumerate() {
                let y = cy + 34.0 * self.scale + i as f64 * 24.0 * self.scale;
                text::draw_centred(
                    &mut self.scene,
                    line,
                    cx,
                    text::baseline(y, px),
                    px,
                    Color::from_rgba8(150, 153, 168, 255),
                );
            }
        }
        if chrome.bars {
            nav::draw(
                &mut self.scene,
                width,
                height,
                self.scale,
                bottom_inset,
                page_no,
                page_count,
                has_outline,
                self.drawer.is_some(),
                find.is_some(),
                address.is_some(),
            );
        }
        // The strip in the cutout's safe area: the document's name on the left,
        // its download numbers on the right. With no document open yet, the
        // note says what is on its way.
        if chrome.bars {
            topbar::draw(
                &mut self.scene,
                width,
                top_inset,
                self.scale,
                chrome.title,
                chrome.stats,
            );
        }
        if let Some(address) = address.as_ref() {
            let panel = url::Panel::new(width, height, self.scale, top_inset);
            url::draw(
                &mut self.scene,
                &panel,
                &url::View {
                    text: address.text,
                    status: address.status,
                    busy: address.busy,
                    native: address.native,
                },
            );
        }
        // A document still arriving says so just above the bar, where neither
        // the address bar nor the keyboard covers it — unless the chrome is
        // away, in which case the strip at the top already says it.
        if address.is_none() && !self.note.is_empty() && chrome.bars {
            note_line(
                &mut self.scene,
                width,
                height,
                self.scale,
                bottom_inset,
                &self.note,
            );
        }
        // The log page goes over everything: it is what the reader asked for,
        // and the one thing on this screen that is about the app rather than
        // the document.
        if let Some(scroll) = log {
            let panel = logview::Panel::new(width, page_height as f64, self.scale);
            let lines = logview::lines();
            let scroll = panel.scroll(scroll, lines.len(), self.scale);
            logview::draw(&mut self.scene, &panel, &lines, scroll, self.scale);
        }
    }

    /// Ask for one more frame once the page stops moving, so the sharp one is
    /// drawn for the pose it came to rest at: while the finger is down, frames
    /// are the last good one, moved. One thread per settle, not per frame.
    fn wake_when_settled(&mut self) {
        if self.settle_wake {
            return;
        }
        let window = Arc::clone(&self.window);
        self.settle_wake = true;
        std::thread::spawn(move || {
            std::thread::sleep(SETTLE);
            window.request_redraw();
        });
    }

    /// Rasterise `scene` into the offscreen frame the blit presents. This is
    /// the expensive part, and the frame it leaves behind is the one that is
    /// shown — moved — while the next one is being drawn.
    fn rasterise(&mut self, scene: &Scene, pose: Pose) -> bool {
        let (width, height) = (self.surface.config.width, self.surface.config.height);
        let handle = &self.render_cx.devices[self.surface.dev_id];
        let params = RenderParams {
            // The canvas the sheet lies on: without it a white page has no
            // visible edge at all.
            base_color: Color::from_rgba8(34, 34, 39, 255),
            width,
            height,
            antialiasing_method: AaConfig::Area,
        };
        if let Err(e) = self.renderer.render_to_texture(
            &handle.device,
            &handle.queue,
            scene,
            &self.surface.target_view,
            &params,
        ) {
            log::error!("render: {e:?}");
            return false;
        }
        self.frame_pose = Some(pose);
        true
    }

    /// Put the frame in `surface.target_view` on the screen, in the pose the
    /// page is in now.
    ///
    /// If the frame was rasterised at another pose, the page part of it is
    /// moved to this one rather than waiting for a new rasterisation: the page
    /// follows the finger instead of the screen standing still while a page is
    /// drawn again. The chrome is never moved — a bar that slides with the page
    /// looks broken — so it is drawn from the frame as it was rasterised, over
    /// its own rectangle.
    fn present(&mut self, pose: Pose, page_bottom: f64, top_inset: f64, bars: bool) -> bool {
        let (width, height) = (self.surface.config.width, self.surface.config.height);
        if width == 0 || height == 0 {
            return false;
        }
        let handle = &self.render_cx.devices[self.surface.dev_id];
        let frame = match self.surface.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Outdated => {
                // e.g. the display was rotated while we were not looking
                self.render_cx.configure_surface(&self.surface);
                self.stale = true;
                return false;
            }
            other => {
                log::warn!("surface: {other:?}");
                return false;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let drawn = self.frame_pose;
        // The frame that was just rasterised is copied the way it has always
        // been copied: a page standing still never needs the shader below, and
        // nothing about the moving-frame path can then cost a still frame.
        if drawn.is_some_and(|drawn| drawn == pose) {
            let mut encoder = handle
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("copy"),
                });
            self.surface.blitter.copy(
                &handle.device,
                &mut encoder,
                &self.surface.target_view,
                &view,
            );
            handle.queue.submit(Some(encoder.finish()));
            handle.queue.present(frame);
            return true;
        }
        let mut encoder = handle
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("present"),
            });
        // The page rectangle: the window, less whatever the chrome is covering.
        // The page itself is laid out for the whole window; only the rows the
        // bar and the strip own keep the frame's chrome instead of the page.
        let (page_top, page_bottom) = if bars {
            (
                top_inset.clamp(0.0, height as f64),
                page_bottom.clamp(0.0, height as f64),
            )
        } else {
            (0.0, height as f64)
        };
        if let Some(drawn) = drawn {
            let (w, h) = (width as f64, height as f64);
            // Screen pixels -> pixels of the frame that was drawn.
            let mut moved = pose.shift_from(drawn).inverse();
            if !moved.as_coeffs().iter().all(|v| v.is_finite()) {
                moved = Affine::IDENTITY;
            }
            self.blit.slot(&handle.queue, 0, moved, w, h, (page_top, page_bottom));
            self.blit
                .slot(&handle.queue, 1, Affine::IDENTITY, w, h, (0.0, h));
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("present"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(CANVAS),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some(drawn) = drawn {
                pass.set_pipeline(&self.blit.pipeline);
                let drawn_frame = || self.blit.bind(&handle.device, &self.surface.target_view, 1);
                let moved = pose != drawn;
                if moved {
                    // The page area where the page is now...
                    let top = page_top as u32;
                    let bottom = page_bottom as u32;
                    if bottom > top {
                        let moved_frame =
                            self.blit.bind(&handle.device, &self.surface.target_view, 0);
                        pass.set_bind_group(0, &moved_frame, &[]);
                        pass.set_scissor_rect(0, top, width, bottom - top);
                        pass.draw(0..3, 0..1);
                    }
                    // ...and the chrome where it always is.
                    let group = drawn_frame();
                    pass.set_bind_group(0, &group, &[]);
                    if top > 0 {
                        pass.set_scissor_rect(0, 0, width, top);
                        pass.draw(0..3, 0..1);
                    }
                    let below = height - bottom;
                    if below > 0 {
                        pass.set_scissor_rect(0, bottom, width, below);
                        pass.draw(0..3, 0..1);
                    }
                } else {
                    let group = drawn_frame();
                    pass.set_bind_group(0, &group, &[]);
                    pass.draw(0..3, 0..1);
                }
            }
        }
        handle.queue.submit(Some(encoder.finish()));
        handle.queue.present(frame);
        true
    }
}

// ---------------------------------------------------------------------------
// the app

/// A page turn in progress: the finger drags the page, then it settles.
#[derive(Clone, Copy, Debug)]
enum Swipe {
    /// A finger is down and moving sideways. `dx` is where the current page
    /// sits relative to its resting place.
    Drag { turn: i8, dx: f64 },
    /// The finger is up: run `from` -> `to` and then turn the page (or not).
    Settle {
        from: f64,
        to: f64,
        turn: i8,
        started: Instant,
        commit: bool,
    },
}

#[derive(Default)]
struct App {
    window: Option<Arc<Window>>,
    gfx: Option<Gfx>,
    doc: Option<Doc>,
    /// Where the current touch started, for telling a swipe from a tap.
    touch: Option<(f64, f64)>,
    /// The swipe being dragged or settled, if any.
    swipe: Option<Swipe>,
    /// Window size in physical pixels and winit's scale factor, so the
    /// navigation bar can be hit-tested without reaching into the GPU side.
    size: (f64, f64),
    scale: f64,
    /// Height of the system navigation bar / gesture pill, in pixels: our own
    /// bar sits above it.
    bottom_inset: f64,
    /// Height of the display cutout (camera notch) / status bar at the top, in
    /// pixels: the address bar starts below it.
    top_inset: f64,
    /// Whether the chrome — the strip in the cutout and the bar at the bottom —
    /// is out of the way, and when the screen was last touched so it can time
    /// out. Hidden, the page gets the whole window.
    bars_off: bool,
    bars_at: Option<Instant>,
    /// A touch that woke the chrome: it is not a tap on anything. A drag
    /// still counts as a drag, so a swipe begun on a sleeping screen turns
    /// the page.
    woke: bool,
    /// `internalDataPath` on Android: the app's private files directory.
    data_dir: Option<PathBuf>,
    /// The document that was opened, so the search index can reopen it.
    path: Option<PathBuf>,
    /// Frames presented since the surface appeared. The first few are redrawn
    /// once more, so a surface that Android only settles after the first frame
    /// still ends up with the page on it.
    frames: u32,
    /// A failed surface creation is logged once, not once per frame.
    gfx_failed: bool,
    /// Frames that did not reach the screen since the last one that did.
    retries: u32,
    /// The outline drawer: whether it is up, and how far it is scrolled.
    outline_open: bool,
    outline_scroll: f64,
    /// The log page: the app's own last few hundred lines, opened by holding
    /// the bar's menu button down.
    log_open: bool,
    log_scroll: f64,
    log_drag: Option<f64>,
    /// When the finger went down on the bar, so a hold can be told from a tap.
    pressed_at: Option<Instant>,
    /// Where the finger was on the drawer last, while it drags it.
    outline_drag: Option<f64>,
    /// The search panel: whether it is up, what has been typed, the hits for
    /// that text, and how the panel is laid out.
    search_open: bool,
    query: String,
    search_hits: Vec<search::Hit>,
    search_total: usize,
    /// The query `search_hits` belongs to, so hits are recomputed exactly when
    /// the query (or the index behind it) changes.
    search_for: String,
    search_gen: u64,
    search_built: usize,
    search_pages: usize,
    search_scroll: f64,
    search_keyboard: bool,
    /// Where the finger was on the results last, while it scrolls them.
    search_drag: Option<f64>,
    /// Bumped whenever the panel's contents change, for the GPU side.
    search_rev: u64,
    /// When the hits were last recomputed, to throttle them while the index is
    /// still filling in.
    search_at: Option<Instant>,
    /// Whether the system keyboard is up for the query field. It replaces the
    /// hand-drawn key rows when it works, which on a phone it does.
    ime: bool,
    /// The address bar: whether it is up, what has been typed, and what it has
    /// to say about a document on its way.
    url_open: bool,
    url_text: String,
    url_status: Option<String>,
    url_busy: bool,
    /// Bumped whenever anything the bar shows changes, for the GPU side.
    url_rev: u64,
    /// The framework's own text field, laid over whichever of the bar and the
    /// query pill is open. An input method needs a real `InputConnection`, and
    /// a `NativeActivity` window has none without an editor view in it.
    native: Option<field::Field>,
    /// The one-line note above the bar, when a document is still arriving.
    note: Option<String>,
    /// How the page sits in the page area, and how it got there: the fingers
    /// on the screen right now, the pinch they are making, and where the pan
    /// started when the first of them went down.
    view: View,
    touches: Vec<(u64, (f64, f64))>,
    pinch: Option<Pinch>,
    pan_start: (f64, f64),
    /// The last tap, for telling a double tap from two taps.
    last_tap: Option<(Instant, (f64, f64))>,
    /// The URI the last "Open with" named, so that re-reading the same intent
    /// when the activity comes back to the front does not open it twice.
    opened_uri: Option<String>,
    /// What to open once the first frame is on screen. Until a frame reaches
    /// the surface the window is black, and parsing a document — let alone
    /// copying one out of a document provider — can take a second or more on a
    /// phone. So the chrome is drawn first and the document follows.
    pending: Option<Pending>,
}

/// The document to open once there is a frame on screen.
enum Pending {
    /// An "Open with" intent: a link, a file path, or a provider URI.
    #[cfg(target_os = "android")]
    Uri(String),
    /// Whatever was last opened: the app's own copy of a document.
    Recent,
}

impl App {
    fn open(&mut self) {
        let Some(path) = find_document(self.data_dir.clone()) else {
            log::error!(
                "no PDF found: adb push one to /sdcard/Android/data/org.pdf_rs.viewer/files"
            );
            return;
        };
        self.open_path(path);
    }

    /// Open a document from the device's own storage.
    fn open_path(&mut self, path: PathBuf) {
        match Doc::open(&path) {
            Ok(doc) => {
                self.search_pages = doc.pages();
                self.view = View::default();
                self.doc = Some(doc);
                self.path = Some(path);
            }
            Err(e) => log::error!("{e}"),
        }
    }

    /// Open what an "Open with" intent named: a link, a file path, or a
    /// document provider's URI (which is copied into the app's own files first,
    /// because the provider's stream is not a file we may seek in later).
    #[cfg(target_os = "android")]
    fn open_uri(&mut self, uri: &str) {
        let uri = uri.trim();
        log::info!("opening intent {uri}");
        if uri.starts_with("http://") || uri.starts_with("https://") {
            // The same path the address bar takes: a byte range at a time, so
            // the document opens without being downloaded first.
            self.url_text = uri.to_string();
            self.submit_address();
            return;
        }
        if uri.starts_with("content://") {
            let dir = self
                .data_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from("/data/local/tmp"));
            let dest = dir.join(match intent::display_name(uri) {
                Some(name) => intent::component(&name),
                None => intent::safe_name(uri),
            });
            match intent::read_content(uri, &dest) {
                Ok(n) => {
                    log::info!("copied {n} bytes from {uri} to {}", dest.display());
                    self.open_path(dest);
                }
                Err(e) => log::error!("{e}"),
            }
            return;
        }
        self.open_path(intent::file_path(uri));
    }

    /// The search panel's geometry for the current window.
    fn log_panel(&self) -> logview::Panel {
        let bar_top = nav::layout(self.size.0, self.size.1, self.scale, self.bottom_inset)
            .bar
            .y0;
        logview::Panel::new(self.size.0, bar_top, self.scale)
    }

    fn search_panel(&self) -> search::Panel {
        let bar_top = nav::layout(self.size.0, self.size.1, self.scale, self.bottom_inset)
            .bar
            .y0;
        search::Panel::new(
            self.size.0,
            bar_top,
            self.top_inset,
            self.scale,
            self.search_keyboard,
        )
    }

    /// The framework's field, made on first use. False when there is none: off
    /// Android, or if the framework refuses to make one.
    fn ensure_native(&mut self) -> bool {
        if self.native.is_none() {
            self.native = field::Field::new();
            log::info!(
                "native text field: {}",
                if self.native.is_some() {
                    "an EditText in the content view"
                } else {
                    "unavailable, falling back to key events"
                }
            );
        }
        self.native.is_some()
    }

    /// Where the open field goes: the bar's pill or the query pill, in window
    /// pixels, and the text size the pill is drawn with.
    fn native_place(&self) -> Option<(field::Purpose, Rect, f64)> {
        if self.url_open {
            let panel = url::Panel::new(self.size.0, self.size.1, self.scale, self.top_inset);
            Some((field::Purpose::Url, panel.field, url::URL_PX))
        } else if self.search_open {
            Some((
                field::Purpose::Query,
                self.search_panel().field,
                search::QUERY_PX,
            ))
        } else {
            None
        }
    }

    /// Lay the native field over the open bar or panel, and take back what has
    /// been typed into it. Called once a frame while either is up.
    fn sync_native_field(&mut self) {
        let Some((purpose, rect, px)) = self.native_place() else {
            if let Some(field) = self.native.as_mut() {
                field.hide();
            }
            return;
        };
        let px = px * self.scale;
        // Read the field before telling it anything: its text is the truth
        // while it is up, and a value one frame behind would be written back
        // over what the owner has just typed.
        let (typed, action) = match self.native.as_mut() {
            Some(field) => (field.poll(), field.took_action()),
            None => return,
        };
        if let Some(typed) = typed {
            match purpose {
                field::Purpose::Url => {
                    if typed != self.url_text {
                        self.url_text = typed;
                        self.url_rev += 1;
                    }
                }
                field::Purpose::Query => {
                    if typed != self.query {
                        self.query = typed;
                        self.search_scroll = 0.0;
                        self.refresh_search();
                    }
                }
            }
        }
        let text = match purpose {
            field::Purpose::Url => self.url_text.clone(),
            field::Purpose::Query => self.query.clone(),
        };
        if let Some(field) = self.native.as_mut() {
            field.show(purpose, rect, &text, px);
        }
        // The keyboard's Go / Search key is the bar's own open button.
        if action && purpose == field::Purpose::Url {
            self.submit_address();
        }
    }

    /// Whether a native field is up: every frame then reads it back, because
    /// typing in it does not ask for a redraw of its own.
    fn field_open(&self) -> bool {
        self.native.is_some() && (self.url_open || self.search_open)
    }

    /// Put the cursor in the query field. The framework's own field and the
    /// system keyboard do the typing; the panel's hand-drawn key rows are only
    /// the fallback for a build (or a device) without one.
    fn focus_query(&mut self) {
        self.search_keyboard = !self.ensure_native();
        self.ime = self.search_keyboard;
        if self.search_keyboard {
            if let Some(window) = self.window.as_ref() {
                window.set_ime_allowed(true);
            }
        }
        self.search_rev += 1;
        self.redraw();
    }

    /// Take the system keyboard down again.
    fn blur_query(&mut self) {
        if self.ime {
            self.ime = false;
            if let Some(window) = self.window.as_ref() {
                window.set_ime_allowed(false);
            }
        }
    }

    /// Open the panel. Nothing is indexed yet: the first search pays for the
    /// index, and this is where that starts.
    fn open_search(&mut self) {
        self.search_open = true;
        self.outline_open = false;
        self.search_scroll = 0.0;
        self.search_keyboard = true;
        self.search_drag = None;
        if let Some(doc) = self.doc.as_ref() {
            self.search_pages = doc.pages();
            let url = doc.url().map(str::to_string);
            match (self.path.clone(), url) {
                (Some(path), _) => search::start(path, doc.pages(), doc.page),
                (None, Some(url)) => search::start_url(url, doc.pages(), doc.page),
                (None, None) => {}
            }
        }
        self.refresh_search();
        self.focus_query();
    }

    /// Close the panel and the keyboard with it.
    fn close_search(&mut self) {
        self.search_open = false;
        self.blur_query();
        if let Some(field) = self.native.as_mut() {
            field.hide();
        }
        self.search_rev += 1;
        self.redraw();
    }

    /// Bring up the address bar and the keyboard, ready for a URL.
    fn open_address(&mut self) {
        self.url_open = true;
        self.outline_open = false;
        self.search_open = false;
        self.blur_query();
        // No scheme is filled in for the owner: `https://` in the field would
        // be pasted over, or typed after, and `https://https://…` is a URL
        // nobody meant. The placeholder shows the shape; the parser defaults to
        // https for a bare host.
        self.url_status = None;
        self.url_busy = false;
        self.url_rev += 1;
        // The framework's field brings its own keyboard and input connection.
        // Only without one do the key events winit forwards have to do.
        if !self.ensure_native() {
            if let Some(window) = self.window.as_ref() {
                window.set_ime_allowed(true);
            }
        }
        self.sync_native_field();
        self.redraw();
    }

    /// Put the bar away, keyboard and all.
    fn close_address(&mut self) {
        self.url_open = false;
        self.url_status = None;
        self.url_busy = false;
        self.url_rev += 1;
        if let Some(field) = self.native.as_mut() {
            field.hide();
        }
        if let Some(window) = self.window.as_ref() {
            window.set_ime_allowed(false);
        }
        self.redraw();
    }

    /// Open the document the bar names. Returns at once: the worker fetches the
    /// head and tail of the file, and the page appears when its bytes do.
    fn submit_address(&mut self) {
        // A tap can land between two polls: read the field one last time so the
        // button opens what is on the screen, not what was there a frame ago.
        if let Some(field) = self.native.as_mut() {
            if let Some(typed) = field.poll() {
                self.url_text = typed;
            }
        }
        let url = self.url_text.trim().to_string();
        if url.is_empty() {
            return;
        }
        self.url_text = url.clone();
        self.url_busy = true;
        self.url_status = Some("opening…".into());
        self.url_rev += 1;
        self.note = Some(format!("opening {} …", short(&url)));
        self.view = View::default();
        self.doc = Some(Doc::open_url(&url));
        self.path = None;
        self.search_pages = 0;
        self.search_for.clear();
        self.outline_open = false;
        self.search_open = false;
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.stale = true;
            gfx.page = None;
            gfx.peek = None;
        }
        self.redraw();
    }

    /// Collect what a remote document's worker has brought, and fold in what
    /// the new bytes let it say about itself.
    fn pump_remote(&mut self) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let changed = doc.poll();
        // Where the document actually came from: a redirect moved it, and the
        // bar should name the place the bytes are read from, not the place the
        // owner typed.
        let landed = doc.url().map(str::to_string);
        if doc.pages() > 0 && self.url_busy {
            // The document is readable: the bar has done its job.
            self.url_busy = false;
            if let Some(landed) = landed {
                if landed != self.url_text {
                    self.url_text = landed;
                }
            }
            self.url_open = false;
            self.url_rev += 1;
            self.search_pages = doc.pages();
            if let Some(field) = self.native.as_mut() {
                field.hide();
            }
            if let Some(window) = self.window.as_ref() {
                window.set_ime_allowed(false);
            }
        }
        if let Some(e) = doc.error.clone() {
            log::error!("{e}");
            if self.url_open {
                self.url_status = Some(e);
                self.url_rev += 1;
            }
        }
        // With no page drawn, the note is what tells the owner something is
        // happening rather than nothing.
        self.note = doc.progress();
        if changed {
            if let Some(gfx) = self.gfx.as_mut() {
                gfx.stale = true;
            }
        }
    }

    /// Recompute the hits when the query or the index has changed since the
    /// last frame. The index arrives a page at a time, and rescanning every
    /// indexed page on every arrival would be quadratic, so while it is still
    /// building the hits are refreshed at most four times a second.
    fn refresh_search(&mut self) {
        let gen = search::generation();
        let (built, pages) = search::progress();
        self.search_built = built;
        self.search_pages = self.search_pages.max(pages);
        let query_changed = self.search_for != self.query;
        if gen == self.search_gen && !query_changed {
            return;
        }
        let indexing = built < self.search_pages;
        let recent = self
            .search_at
            .is_some_and(|at| at.elapsed() < Duration::from_millis(250));
        if indexing && !query_changed && recent {
            return;
        }
        self.search_at = Some(Instant::now());
        self.search_gen = gen;
        let (hits, total) = search::hits(&self.query, 80);
        self.search_hits = hits;
        self.search_total = total;
        self.search_for = self.query.clone();
        let panel = self.search_panel();
        self.search_scroll = self
            .search_scroll
            .clamp(0.0, panel.max_scroll(self.search_hits.len(), self.scale));
        self.search_rev += 1;
    }

    fn search_key(&mut self, key: search::Key) {
        match key {
            search::Key::Ch(ch) => {
                if self.query.chars().count() < 120 {
                    self.query.push(ch);
                }
            }
            search::Key::Back => {
                self.query.pop();
            }
            search::Key::Space => {
                if !self.query.is_empty() && !self.query.ends_with(' ') {
                    self.query.push(' ');
                }
            }
            search::Key::Done => self.search_keyboard = false,
        }
        self.search_scroll = 0.0;
        self.refresh_search();
        self.redraw();
    }

    /// Is the page, as it stands, taller than the area it sits in? Fitted to
    /// the width, most pages on a phone are.
    fn tall_page(&self) -> bool {
        let (_, h) = self.page_area();
        self.gfx
            .as_ref()
            .map_or(false, |gfx| gfx.fit * self.view.zoom > h + 0.5)
    }

    /// The page area: the whole window, in physical pixels. The bar and the
    /// strip are overlays over it, so they do not take anything away from it:
    /// the page keeps its place whether they are up or not.
    fn page_area(&self) -> (f64, f64) {
        (self.size.0.max(1.0), self.size.1.max(1.0))
    }

    /// How tall the page under the view is at fit size, as the GPU side last
    /// laid it out. The page is fitted to the width of the area, so on a phone
    /// this is usually taller than the area, and the page scrolls.
    fn fit_height(&self) -> f64 {
        match self.gfx.as_ref() {
            Some(gfx) if gfx.fit > 0.0 => gfx.fit,
            _ => self.page_area().1,
        }
    }

    /// Put the page under the fingers, within the limits the area sets.
    fn set_view(&mut self, view: View) {
        let (w, h) = self.page_area();
        let view = view.clamp(w, h, self.fit_height());
        if view != self.view {
            self.view = view;
            self.redraw();
        }
    }

    /// Zoom to `zoom` with the point under `at` staying put: a pinch keeps the
    /// paper it started on, a double tap opens on what was tapped.
    fn zoom_about(&mut self, zoom: f64, at: (f64, f64)) {
        let (w, h) = self.page_area();
        let fit = self.fit_height();
        let view = self.view.clamp(w, h, fit).about(zoom, at).clamp(w, h, fit);
        if view != self.view {
            self.view = view;
            self.redraw();
        }
    }

    fn turn_page(&mut self, forward: bool) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let pages = doc.pages();
        if pages == 0 {
            return;
        }
        let next = if forward {
            (doc.page + 1).min(pages - 1)
        } else {
            doc.page.saturating_sub(1)
        };
        if next != doc.page {
            doc.page = next;
            // A new page starts fitted, whatever the last one was left at.
            self.view = View::default();
            log::info!("page {}/{}", next + 1, pages);
            if let Some(gfx) = self.gfx.as_mut() {
                gfx.stale = true;
            }
        }
    }

    /// Current page index and page count.
    fn pages(&self) -> Option<(usize, usize)> {
        self.doc.as_ref().map(|doc| (doc.page, doc.pages()))
    }

    /// Is there a page for `turn` (+1 = next, -1 = previous) to slide in?
    fn neighbour(&self, turn: i8) -> bool {
        match self.pages() {
            Some((page, pages)) => {
                if turn > 0 {
                    page + 1 < pages
                } else {
                    page > 0
                }
            }
            None => false,
        }
    }

    /// Where the page sits this frame, if a swipe is under way. Called once per
    /// redraw; a finished settle turns the page here.
    fn advance(&mut self, now: Instant) -> Option<(f64, i8)> {
        match self.swipe? {
            Swipe::Drag { turn, dx } => Some((dx, turn)),
            Swipe::Settle {
                from,
                to,
                turn,
                started,
                commit,
            } => {
                let t = now.duration_since(started).as_secs_f64() * 1000.0 / SETTLE_MS;
                if t >= 1.0 {
                    self.swipe = None;
                    if commit {
                        self.turn_page(turn > 0);
                    }
                    None
                } else {
                    // Ease out: leaves the finger quickly, lands softly.
                    let e = 1.0 - (1.0 - t).powi(3);
                    Some((from + (to - from) * e, turn))
                }
            }
        }
    }

    fn animating(&self) -> bool {
        matches!(self.swipe, Some(Swipe::Settle { .. }))
    }

    fn redraw(&self) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// Ask for a frame `after` from now, without blocking the loop: the wake
    /// comes from a thread of its own, which is how a deadline nobody sleeps on
    /// gets a frame.
    fn redraw_later(&self, after: Duration) {
        let Some(window) = self.window.as_ref().map(Arc::clone) else {
            return;
        };
        std::thread::spawn(move || {
            std::thread::sleep(after);
            window.request_redraw();
        });
    }

    /// Build the GPU side the first time a frame is actually asked for.
    ///
    /// Not in `resumed`: on Android `Resumed` arrives with `InitWindow`, when
    /// the native window may still have no buffer geometry, and winit drops a
    /// `WindowResized` that comes in the same breath (it only forwards one once
    /// the app is running). A swapchain made at that moment is never shown:
    /// the screen stays black until the app is minimized and reopened, which
    /// builds everything a second time, this time with a real window.
    fn ensure_gfx(&mut self) {
        if self.gfx.is_some() || self.gfx_failed {
            return;
        }
        let Some(window) = self.window.clone() else {
            return;
        };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            if self.retries < 2 {
                log::info!("no surface size yet at {:.0} ms, waiting", boot_ms());
            }
            return;
        }
        self.size = (size.width as f64, size.height as f64);
        self.scale = window.scale_factor();
        self.bottom_inset = bottom_inset(&window);
        self.top_inset = top_inset(&window);
        match Gfx::new(window) {
            Ok(gfx) => {
                self.gfx = Some(gfx);
                self.frames = 0;
                self.retries = 0;
            }
            Err(e) => {
                log::error!("{e}");
                self.gfx_failed = true;
            }
        }
    }

    /// If the window has a size the swapchain does not know about (a `Resized`
    /// that never reached us), resize and rebuild the page.
    fn sync_size(&mut self) {
        let (Some(window), Some(gfx)) = (self.window.as_ref(), self.gfx.as_ref()) else {
            return;
        };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        if size.width == gfx.surface.config.width && size.height == gfx.surface.config.height {
            return;
        }
        log::info!(
            "size {}x{} -> {}x{}",
            gfx.surface.config.width,
            gfx.surface.config.height,
            size.width,
            size.height
        );
        self.size = (size.width as f64, size.height as f64);
        self.scale = window.scale_factor();
        self.bottom_inset = bottom_inset(window);
        self.top_inset = top_inset(window);
        let (scale, size) = (self.scale, size);
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.scale = scale;
            gfx.resize(size.width, size.height);
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes().with_title("pdf");
        let window = match event_loop.create_window(attributes) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("window: {e:?}");
                return;
            }
        };
        // No surface here: `ensure_gfx` builds it on the first frame, when
        // Android has given the window its real size (see its comment).
        self.size = (
            window.inner_size().width as f64,
            window.inner_size().height as f64,
        );
        self.scale = window.scale_factor();
        self.bottom_inset = bottom_inset(&window);
        self.top_inset = top_inset(&window);
        log::info!(
            "window {:?}, scale {}, inset {}",
            window.inner_size(),
            self.scale,
            self.bottom_inset
        );
        self.window = Some(window);
        self.gfx = None;
        self.gfx_failed = false;
        self.frames = 0;
        self.retries = 0;
        // The UI font parses on its own thread and asks for this frame when it
        // is ready, rather than holding up the first page.
        if let Some(window) = self.window.as_ref().map(Arc::clone) {
            text::set_wake(move || window.request_redraw());
        }
        // Same for the search index: every page it finishes is one more page
        // of answers, and the panel should show them without a touch.
        if let Some(window) = self.window.as_ref().map(Arc::clone) {
            search::set_wake(move || window.request_redraw());
        }
        // And for a document at a URL: every range that lands lets the next
        // parse pass get further, and that pass wants a frame.
        if let Some(window) = self.window.as_ref().map(Arc::clone) {
            remote::set_wake(move || window.request_redraw());
        }
        // A second "Open with" arrives as `onNewIntent` on the UI thread; this
        // brings that news to the loop, which is otherwise idle.
        #[cfg(target_os = "android")]
        if let Some(window) = self.window.as_ref().map(Arc::clone) {
            intent::set_wake(Box::new(move || window.request_redraw()));
        }
        // The chrome times out on its own, and a deadline nobody sleeps on
        // never arrives: while the bars are up, a slow tick asks for a frame so
        // that `draw` can notice the screen has been left alone. The tick stops
        // as soon as they are gone.
        if let Some(window) = self.window.as_ref().map(Arc::clone) {
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_millis(250));
                if BARS_UP.load(std::sync::atomic::Ordering::Relaxed) {
                    window.request_redraw();
                }
            });
        }
        // The document is opened after the first frame, not here: parsing a
        // 500-page file takes a second or more on a phone, and the window is
        // black for as long as this frame is not presented. `resumed` only
        // records what to open.
        if self.doc.is_none() {
            #[cfg(target_os = "android")]
            if let Some(uri) = intent::take() {
                self.opened_uri = Some(uri.clone());
                self.pending = Some(Pending::Uri(uri));
            }
        }
        if self.doc.is_none() && self.pending.is_none() {
            self.pending = Some(Pending::Recent);
        }
        self.redraw();
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        text,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                // The address bar takes the keyboard first: whatever is typed
                // there is a URL, not a search.
                if self.url_open {
                    match logical_key {
                        Key::Named(NamedKey::Backspace) => {
                            self.url_text.pop();
                            self.url_rev += 1;
                            self.redraw();
                        }
                        Key::Named(NamedKey::Enter) => self.submit_address(),
                        Key::Named(NamedKey::Escape) => self.close_address(),
                        Key::Named(NamedKey::Space) => {
                            self.url_text.push(' ');
                            self.url_rev += 1;
                            self.redraw();
                        }
                        _ => {
                            if let Some(text) = text {
                                let mut typed = false;
                                for ch in text.chars() {
                                    if !ch.is_control() {
                                        self.url_text.push(ch);
                                        typed = true;
                                    }
                                }
                                if typed {
                                    self.url_rev += 1;
                                    self.redraw();
                                }
                            }
                        }
                    }
                    return;
                }
                // The system keyboard types into the query field; without it
                // (a desktop, or an IME that never comes up) the panel keeps
                // its own key rows and nothing arrives here.
                if self.search_open && self.ime {
                    match logical_key {
                        Key::Named(NamedKey::Backspace) => self.search_key(search::Key::Back),
                        Key::Named(NamedKey::Space) => self.search_key(search::Key::Space),
                        Key::Named(NamedKey::Enter) => self.search_key(search::Key::Done),
                        Key::Named(NamedKey::Escape) => self.close_search(),
                        _ => {
                            let mut typed = false;
                            if let Some(text) = text {
                                for ch in text.chars() {
                                    if !ch.is_control() {
                                        self.query.push(ch);
                                        typed = true;
                                    }
                                }
                            }
                            if typed {
                                self.search_scroll = 0.0;
                                self.refresh_search();
                                self.redraw();
                            }
                        }
                    }
                }
            }
            // A second "Open with" while the app is running reaches Java as
            // `onNewIntent`, which a `NativeActivity` never forwards to native
            // code; the activity coming back to the front is the only signal
            // we get, so the intent is re-read there.
            WindowEvent::Focused(true) => {
                #[cfg(target_os = "android")]
                if let Some(uri) = intent::changed(self.opened_uri.as_deref()) {
                    self.opened_uri = Some(uri.clone());
                    self.open_uri(&uri);
                    self.redraw();
                }
            }
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                self.size = (size.width as f64, size.height as f64);
                // The system bars can come and go with a rotation; the scale
                // factor and the inset decide where our own bar ends up.
                if let Some(window) = self.window.as_ref() {
                    self.bottom_inset = bottom_inset(window);
                    self.top_inset = top_inset(window);
                }
                self.ensure_gfx();
                if let Some(gfx) = self.gfx.as_mut() {
                    gfx.resize(size.width, size.height);
                }
                self.redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.scale = scale_factor;
                if let Some(window) = self.window.as_ref() {
                    self.size = (
                        window.inner_size().width as f64,
                        window.inner_size().height as f64,
                    );
                }
                if let Some(gfx) = self.gfx.as_mut() {
                    gfx.scale = scale_factor;
                    gfx.stale = true;
                }
                self.redraw();
            }
            WindowEvent::RedrawRequested => {
                self.ensure_gfx();
                self.sync_size();
                // A document handed to us while we were already in front.
                #[cfg(target_os = "android")]
                if intent::arrived() {
                    if let Some(uri) = intent::changed(self.opened_uri.as_deref()) {
                        self.opened_uri = Some(uri.clone());
                        self.open_uri(&uri);
                    }
                }
                self.pump_remote();
                // The framework's field is where the typing happens: lay it
                // over the pill and read back what it holds before the scene is
                // built, so the bar draws the same text the field shows.
                self.sync_native_field();
                if self.search_open {
                    self.refresh_search();
                }
                let slide = self.advance(Instant::now());
                let drawer = self.outline_open.then_some(self.outline_scroll);
                let log = self.log_open.then_some(self.log_scroll);
                let native = self.native.is_some();
                let find = if self.search_open {
                    Some((
                        search::View {
                            query: &self.query,
                            hits: &self.search_hits,
                            total: self.search_total,
                            built: self.search_built,
                            pages: self.search_pages,
                            scroll: self.search_scroll,
                            keyboard: self.search_keyboard,
                            native,
                        },
                        self.search_rev,
                    ))
                } else {
                    None
                };
                let mut shown = false;
                let address = self.url_open.then(|| Address {
                    text: &self.url_text,
                    status: self.url_status.as_deref(),
                    busy: self.url_busy,
                    native,
                    rev: self.url_rev,
                });
                let note = self.note.clone();
                // The chrome times out: with nothing open and the screen
                // untouched for a few seconds, the strip and the bar get out of
                // the way and the page takes the window. Anything modal, and
                // any finger, keeps them up.
                let busy = self.url_open
                    || self.search_open
                    || self.outline_open
                    || self.swipe.is_some()
                    || self.pinch.is_some()
                    || !self.touches.is_empty();
                if busy {
                    self.bars_off = false;
                }
                if busy || self.bars_at.is_none() {
                    self.bars_at = Some(Instant::now());
                } else if self.bars_at.is_some_and(|at| at.elapsed() >= BARS_IDLE) {
                    self.bars_off = true;
                }
                BARS_UP.store(!self.bars_off, std::sync::atomic::Ordering::Relaxed);
                let (title, stats) = match self.doc.as_ref() {
                    Some(doc) => (doc.title(), doc.stats()),
                    None => (self.note.clone().unwrap_or_default(), String::new()),
                };
                // For the first half minute the strip also says how long the
                // app has been up: a cold start that takes seconds says so on
                // the screen it is spent on, without a cable and logcat.
                let stats = if boot_ms() < 30_000.0 {
                    let up = format!("{stats} · up {:.1}s", boot_ms() / 1000.0);
                    up.trim_start_matches(" · ").to_string()
                } else {
                    stats
                };
                let chrome = Chrome {
                    bars: !self.bars_off,
                    title: &title,
                    stats: &stats,
                };
                // A frame is drawn with or without a document: the canvas and
                // the bar still have to reach the screen, or the window keeps
                // the black it was created with until Android takes it away.
                if let Some(gfx) = self.gfx.as_mut() {
                    shown = gfx.draw(
                        self.doc.as_mut(),
                        self.bottom_inset,
                        self.top_inset,
                        self.view,
                        slide,
                        drawer,
                        log,
                        find,
                        address,
                        note.as_deref(),
                        chrome,
                    );
                }
                if shown {
                    self.retries = 0;
                    // The chrome has reached the screen, so the window is no
                    // longer black: open what `resumed` left waiting, and ask
                    // for the frame that shows it. Doing this before the first
                    // present is what kept a cold start black for seconds.
                    if self.doc.is_none() {
                        if let Some(open) = self.pending.take() {
                            match open {
                                #[cfg(target_os = "android")]
                                Pending::Uri(uri) => self.open_uri(&uri),
                                Pending::Recent => self.open(),
                            }
                            self.redraw();
                        }
                    }
                }
                // The first frames are drawn twice (Android sometimes only
                // hands over the real window after the first present), and a
                // frame that never reached the screen is worth another try.
                // The retries are spaced out: a surface Android has not sized
                // yet does not become ready within microseconds, and running
                // through them in that time left the window black until some
                // unrelated event happened to wake the loop.
                if !shown && self.retries < RETRIES {
                    self.retries += 1;
                    if self.retries < 4 || self.retries % 8 == 0 {
                        log::info!(
                            "retry {}/{} at {:.0} ms, no frame on screen yet",
                            self.retries,
                            RETRIES,
                            boot_ms()
                        );
                    }
                    self.redraw_later(Duration::from_millis(25));
                } else if self.frames < 2 {
                    self.frames += 1;
                    self.redraw();
                } else if self.animating() || self.field_open() {
                    self.redraw();
                }
            }
            WindowEvent::Touch(touch) => match touch.phase {
                TouchPhase::Started => {
                    // A touch brings the chrome back, and restarts its clock.
                    // The touch that woke it is not a tap on anything: it was
                    // aimed at a page that had the bottom of the screen to
                    // itself. A drag is not a tap, though, and the page has
                    // been moving under the finger all the way, so a gesture
                    // that has started finishes as itself.
                    if self.bars_off {
                        self.bars_off = false;
                        self.woke = true;
                        self.redraw();
                    }
                    self.bars_at = Some(Instant::now());
                    self.touches.retain(|(id, _)| *id != touch.id);
                    self.touches.push((touch.id, (touch.location.x, touch.location.y)));
                    self.touch = Some((touch.location.x, touch.location.y));
                    self.pan_start = self.view.pan;
                    self.outline_drag = None;
                    self.search_drag = None;
                    self.log_drag = None;
                    self.pressed_at = Some(Instant::now());
                    self.swipe = None;
                    // Two fingers are a pinch, not a swipe: the page stops
                    // under them where it is, and the drag that was starting is
                    // forgotten rather than committed.
                    if self.touches.len() >= 2 && !self.url_open {
                        let (dist, mid) = pinch_measure(&self.touches);
                        let at = self.view.under(mid);
                        self.pinch = Some(Pinch {
                            dist,
                            at,
                            zoom: self.view.zoom,
                        });
                        self.touch = None;
                        self.swipe = None;
                    }
                }
                TouchPhase::Moved => {
                    if let Some(slot) = self.touches.iter_mut().find(|(id, _)| *id == touch.id) {
                        slot.1 = (touch.location.x, touch.location.y);
                    }
                    // A pinch: the distance sets the zoom and the midpoint
                    // carries the page along with the fingers.
                    if let Some(pinch) = self.pinch {
                        if self.touches.len() >= 2 && !self.url_open {
                            let (dist, mid) = pinch_measure(&self.touches);
                            let zoom = pinch.zoom * (dist / pinch.dist.max(1.0));
                            let zoom = if zoom.is_finite() {
                                zoom.clamp(1.0, MAX_ZOOM)
                            } else {
                                pinch.zoom
                            };
                            let pan = (mid.0 - pinch.at.0 * zoom, mid.1 - pinch.at.1 * zoom);
                            self.set_view(View { zoom, pan });
                            return;
                        }
                    }
                    // The address bar is up: the page does not move under it.
                    if self.url_open {
                        return;
                    }
                    // The search panel takes vertical drags as a scroll of its
                    // results, and leaves the page alone while it is up.
                    if self.search_open {
                        if let Some(last) = self.search_drag {
                            let panel = self.search_panel();
                            self.search_scroll = (self.search_scroll - (touch.location.y - last))
                                .clamp(0.0, panel.max_scroll(self.search_hits.len(), self.scale));
                            self.redraw();
                        }
                        self.search_drag = Some(touch.location.y);
                        return;
                    }
                    // The log page scrolls like the drawer, and keeps the page
                    // underneath where it is.
                    if self.log_open {
                        if let Some(last) = self.log_drag {
                            let panel = self.log_panel();
                            self.log_scroll = (self.log_scroll - (touch.location.y - last))
                                .clamp(0.0, panel.max_scroll(logview::count(), self.scale));
                            self.redraw();
                        }
                        self.log_drag = Some(touch.location.y);
                        return;
                    }
                    // With the drawer up a vertical drag scrolls it; the page
                    // underneath does not move.
                    if self.outline_open {
                        if let Some(last) = self.outline_drag {
                            self.outline_scroll =
                                (self.outline_scroll - (touch.location.y - last)).max(0.0);
                            self.redraw();
                        }
                        self.outline_drag = Some(touch.location.y);
                        return;
                    }
                    // Enlarged, one finger drags the paper around; so does a
                    // page taller than the area at fit size, as long as the
                    // drag goes up or down. Sideways, the page still turns.
                    let drag = self.touch.map(|(x0, y0)| {
                        (touch.location.x - x0, touch.location.y - y0)
                    });
                    let scrolls = self.view.zoom > 1.0001
                        || match drag {
                            Some((dx, dy)) => dy.abs() > dx.abs() && self.tall_page(),
                            None => false,
                        };
                    if scrolls {
                        if let Some((dx, dy)) = drag {
                            let pan = (self.pan_start.0 + dx, self.pan_start.1 + dy);
                            self.set_view(View {
                                zoom: self.view.zoom,
                                pan,
                            });
                        }
                        return;
                    }
                    if let Some((x0, y0)) = self.touch {
                        let (dx, dy) = (touch.location.x - x0, touch.location.y - y0);
                        // Once a drag has started its direction is fixed; before
                        // that, the side the finger went to picks the page.
                        let turn = match self.swipe {
                            Some(Swipe::Drag { turn, .. }) => turn,
                            _ => {
                                if dx < 0.0 {
                                    1
                                } else {
                                    -1
                                }
                            }
                        };
                        let started = matches!(self.swipe, Some(Swipe::Drag { .. }));
                        if started || (dx.abs() > DRAG_START && dx.abs() > dy.abs()) {
                            let width = self.size.0.max(1.0);
                            // Only slide the way the page can go; at the first
                            // and last page the sheet just stretches a little.
                            let mut dx = dx.clamp(-width * 0.95, width * 0.95);
                            dx = if turn > 0 { dx.min(0.0) } else { dx.max(0.0) };
                            let dx = if self.neighbour(turn) { dx } else { dx * 0.25 };
                            self.swipe = Some(Swipe::Drag { turn, dx });
                            self.redraw();
                        }
                    }
                }
                TouchPhase::Ended => {
                    self.touches.retain(|(id, _)| *id != touch.id);
                    if std::mem::take(&mut self.woke)
                        && self.swipe.is_none()
                        && self.pinch.is_none()
                    {
                        // The finger only woke the chrome: it never became a
                        // gesture, so it is not allowed to press anything
                        // underneath. (A gesture — a swipe being dragged, a
                        // pinch — falls through and finishes as it began.)
                        self.touch = None;
                        self.last_tap = None;
                        self.redraw();
                        return;
                    }
                    // A pinch ends when the first of its two fingers lifts; a
                    // finger still down does not then count as a tap.
                    if self.pinch.take().is_some() {
                        if let Some((_, p)) = self.touches.first().copied() {
                            self.touch = Some(p);
                            self.pan_start = self.view.pan;
                        } else {
                            self.touch = None;
                        }
                        // Whatever these fingers do next, it is not half of a
                        // double tap.
                        self.last_tap = None;
                        self.redraw();
                        return;
                    }
                    if self.url_open {
                        let (x, y) = (touch.location.x, touch.location.y);
                        let panel = url::Panel::new(
                            self.size.0,
                            self.size.1,
                            self.scale,
                            self.top_inset,
                        );
                        if panel.close.contains((x, y)) {
                            self.close_address();
                        } else if panel.go.contains((x, y)) {
                            self.submit_address();
                        } else if panel.field.contains((x, y)) {
                            if let Some(window) = self.window.as_ref() {
                                window.set_ime_allowed(true);
                            }
                        }
                        // A tap anywhere else leaves the bar alone: the page
                        // underneath is not what the finger was aiming at.
                        return;
                    }
                    if self.search_open {
                        let (x, y) = (touch.location.x, touch.location.y);
                        let dragged = self.search_drag.take();
                        let nav =
                            nav::layout(self.size.0, self.size.1, self.scale, self.bottom_inset);
                        if let Some(hit) = nav.hit(x, y) {
                            match hit {
                                nav::Hit::Search => self.close_search(),
                                nav::Hit::Url => {
                                    self.search_open = false;
                                    self.blur_query();
                                    self.open_address();
                                }
                                nav::Hit::Menu => {
                                    self.search_open = false;
                                    self.blur_query();
                                    self.outline_open = true;
                                    self.outline_scroll = 0.0;
                                    self.search_rev += 1;
                                }
                                nav::Hit::Prev => {
                                    self.search_open = false;
                                    self.blur_query();
                                    self.search_rev += 1;
                                    self.turn_page(false);
                                }
                                nav::Hit::Next => {
                                    self.search_open = false;
                                    self.blur_query();
                                    self.search_rev += 1;
                                    self.turn_page(true);
                                }
                            }
                            self.redraw();
                            return;
                        }
                        let panel = self.search_panel();
                        if panel.close.contains((x, y)) {
                            self.close_search();
                            return;
                        }
                        if panel.field.contains((x, y)) {
                            self.focus_query();
                            return;
                        }
                        if self.search_keyboard {
                            if let Some(key) = panel.key_at(x, y) {
                                self.search_key(key);
                                return;
                            }
                        }
                        let moved = dragged.map(|y0| (y - y0).abs()).unwrap_or(0.0);
                        if moved < 12.0 * self.scale {
                            if let Some(row) =
                                panel.row_at(self.search_hits.len(), self.search_scroll, x, y)
                            {
                                let page = self.search_hits[row].page;
                                self.close_search();
                                if let Some(doc) = self.doc.as_mut() {
                                    doc.jump(page);
                                }
                                self.view = View::default();
                                if let Some(gfx) = self.gfx.as_mut() {
                                    gfx.stale = true;
                                }
                                self.redraw();
                            }
                        }
                        return;
                    }
                    let start = self.touch.take();
                    let dragged = self.outline_drag.take();
                    if let Some(Swipe::Drag { turn, dx }) = self.swipe {
                        let width = self.size.0.max(1.0);
                        let commit = self.neighbour(turn) && dx.abs() > width * COMMIT;
                        let to = if commit {
                            if turn > 0 {
                                -width
                            } else {
                                width
                            }
                        } else {
                            0.0
                        };
                        self.swipe = Some(Swipe::Settle {
                            from: dx,
                            to,
                            turn,
                            started: Instant::now(),
                            commit,
                        });
                        self.redraw();
                    } else if let Some((x0, y0)) = start {
                        // A plain tap, or too short a flick to have animated:
                        // the bar's buttons answer first, then the drawer, then
                        // a long swipe.
                        let nav =
                            nav::layout(self.size.0, self.size.1, self.scale, self.bottom_inset);
                        let (dx, dy) = (touch.location.x - x0, touch.location.y - y0);
                        match nav.hit(x0, y0) {
                            Some(nav::Hit::Menu) => {
                                // A hold on the menu button is the log page: a
                                // tap still opens the outline.
                                let held = self.pressed_at.is_some_and(|at| {
                                    at.elapsed() >= Duration::from_millis(600)
                                });
                                if held {
                                    self.log_open = !self.log_open;
                                    self.log_scroll = 0.0;
                                } else {
                                    self.log_open = false;
                                    self.outline_open = !self.outline_open;
                                    self.outline_scroll = 0.0;
                                }
                                self.redraw();
                            }
                            Some(nav::Hit::Search) => {
                                self.open_search();
                            }
                            Some(nav::Hit::Url) => {
                                self.outline_open = false;
                                self.open_address();
                            }
                            Some(nav::Hit::Prev) => {
                                self.outline_open = false;
                                self.turn_page(false);
                                self.redraw();
                            }
                            Some(nav::Hit::Next) => {
                                self.outline_open = false;
                                self.turn_page(true);
                                self.redraw();
                            }
                            None if self.outline_open => {
                                // A tap on a row jumps there; a drag was a scroll.
                                let moved = dragged.map(|y| (y - y0).abs()).unwrap_or(0.0);
                                if moved < 12.0 * self.scale {
                                    let panel =
                                        outline::Panel::new(self.size.0, nav.bar.y0, self.scale);
                                    let page = self.doc.as_ref().and_then(|doc| {
                                        let scroll = panel.scroll(
                                            self.outline_scroll,
                                            doc.outline.len(),
                                            self.scale,
                                        );
                                        let row = panel.row_at(
                                            &doc.outline,
                                            scroll,
                                            touch.location.x,
                                            touch.location.y,
                                        )?;
                                        doc.outline[row].page
                                    });
                                    if let Some(page) = page {
                                        self.outline_open = false;
                                        if let Some(doc) = self.doc.as_mut() {
                                            doc.jump(page);
                                        }
                                        self.view = View::default();
                                        if let Some(gfx) = self.gfx.as_mut() {
                                            gfx.stale = true;
                                        }
                                    }
                                    self.redraw();
                                }
                            }
                            None if dx.abs() < SWIPE * 0.5 && dy.abs() < SWIPE * 0.5 => {
                                // A tap. Two in the same place zoom in on what
                                // was tapped, and two more go back to the whole
                                // page.
                                let at = (touch.location.x, touch.location.y);
                                let slop = DOUBLE_TAP_SLOP * self.scale;
                                let double = self.last_tap.is_some_and(|(when, p)| {
                                    when.elapsed().as_millis() <= DOUBLE_TAP_MS
                                        && (p.0 - at.0).abs() <= slop
                                        && (p.1 - at.1).abs() <= slop
                                });
                                if double {
                                    self.last_tap = None;
                                    if self.view.zoom > 1.0001 {
                                        self.set_view(View::default());
                                    } else {
                                        self.zoom_about(2.5, at);
                                    }
                                } else {
                                    self.last_tap = Some((Instant::now(), at));
                                }
                            }
                            None => {
                                if dx.abs() > SWIPE && dx.abs() > dy.abs() {
                                    self.turn_page(dx < 0.0);
                                    self.redraw();
                                }
                            }
                        }
                    }
                }
                TouchPhase::Cancelled => {
                    self.touches.clear();
                    self.pinch = None;
                    self.touch = None;
                    // Let the page fall back rather than jump.
                    if let Some(Swipe::Drag { turn, dx }) = self.swipe {
                        self.swipe = Some(Swipe::Settle {
                            from: dx,
                            to: 0.0,
                            turn,
                            started: Instant::now(),
                            commit: false,
                        });
                        self.redraw();
                    }
                }
            },
            _ => {}
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        // Android takes the surface away with the window; start over on resume.
        self.gfx = None;
        self.window = None;
        self.touch = None;
        self.swipe = None;
        self.touches.clear();
        self.pinch = None;
    }
}

// ---------------------------------------------------------------------------
// entry points

#[cfg(target_os = "android")]
#[no_mangle]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    use winit::platform::android::EventLoopBuilderExtAndroid;

    // First thing, so every later line can say how far into the start it is.
    BOOT.get_or_init(Instant::now);

    // Everything to logcat as before, and a copy to the screen's log page.
    let logger = android_logger::AndroidLogger::new(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("pdf_android"),
    );
    let tee = logview::Tee::new(Box::new(logger));
    if log::set_boxed_logger(Box::new(tee)).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
    log::info!(
        "starting at {:.0} ms, internal data at {:?}",
        boot_ms(),
        app.internal_data_path()
    );

    // What the launcher asked us to open, read out of the activity before winit
    // takes it over.
    intent::capture(&app);

    let data_dir = app.internal_data_path();
    let event_loop = match EventLoop::builder().with_android_app(app).build() {
        Ok(el) => el,
        Err(e) => {
            log::error!("event loop: {e:?}");
            return;
        }
    };
    let mut state = App {
        data_dir,
        ..Default::default()
    };
    if let Err(e) = event_loop.run_app(&mut state) {
        log::error!("run: {e:?}");
    }
}

/// wgpu hands out futures, and a viewer has nothing to do while it waits.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    use std::pin::pin;
    use std::task::{Context, Poll};

    let mut future = pin!(future);
    let mut context = Context::from_waker(std::task::Waker::noop());
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
        std::thread::sleep(Duration::from_micros(200));
    }
}
