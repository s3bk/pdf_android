//! Reading a PDF that is not on the phone.
//!
//! The point of this module is what it does *not* do: it never downloads the
//! document. A PDF carries its cross-reference table at the end and a page's
//! objects wherever the writer felt like putting them, so a reader with a byte
//! range request can open a 700-page file by asking for a few dozen kilobytes
//! of it — and, on a phone, that is the difference between "opens" and "2 GB of
//! data allowance".
//!
//! Two rules shape the code:
//!
//! * **The UI thread never waits for the network.** Rendering a page is a
//!   sequential parse: it asks for bytes, and when one is missing the whole
//!   parse has to be run again from the top with that byte range present. So a
//!   render here runs one pass with what is cached, notices the misses, asks
//!   the worker for them, and draws an empty page; the bytes arrive, the frame
//!   is rebuilt, and the pass gets further. A page that is already cached takes
//!   one pass and no network at all.
//! * **The cache is the document.** `RangeCache` holds the bytes in sorted,
//!   coalesced chunks, so the render passes (and the search index) share one
//!   copy of what has been paid for.
//!
//! Everything network-facing runs on one worker thread per open document, which
//! also means a page turn never queues behind a DNS lookup on another
//! connection.

use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

use pdf::file::{File, NoCache, NoLog};
use pdf_render::rangecache::{coalesce, render_with_ranges, Borrowed, LazyReader, RangeCache};

use crate::net;
use crate::outline;

/// How much of the end of the file the very first request carries. The cross
/// reference table and the trailer live there, and most writers keep them well
/// inside the last kilobyte.
const TAIL: usize = 48 * 1024;
/// ... and of the front, where a linearised file puts its first page.
const FRONT: usize = 4 * 1024;
/// Two wanted ranges closer together than this travel in one request: a round
/// trip costs more than 32 KiB of a mobile link.
const SLACK: usize = 32 * 1024;
/// How many fetch rounds one render may take before the page is declared lost.
const ROUNDS: usize = 8;

/// A request to the worker thread.
enum Job {
    /// Bytes `[start, end)` of each range, coalesced and filtered by the
    /// caller, to be handed back as `Msg::Bytes`.
    Fetch(Vec<std::ops::Range<usize>>),
}

/// What the worker sends back.
enum Msg {
    /// The document's structure: page count, bookmarks, and the bytes that
    /// took, as a cache the UI thread takes over.
    Ready(Box<Opened>),
    /// Ranges that were asked for.
    Bytes(Vec<(usize, Vec<u8>)>),
    /// Something went wrong; the string is for the owner to read.
    Failed(String),
}

/// The first answer: everything that is needed to show a document at all.
struct Opened {
    /// The address the document was actually found at: the one asked for,
    /// unless the server redirected.
    url: String,
    len: usize,
    pages: usize,
    /// The document's `/Title`, if its `/Info` dictionary has one: what the top
    /// strip calls it.
    title: Option<String>,
    outline: Vec<outline::Row>,
    cache: RangeCache,
    got: u64,
}

static WAKE: OnceLock<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>> = OnceLock::new();

/// Give the worker a way to ask for a redraw when bytes land.
pub fn set_wake(f: impl Fn() + Send + Sync + 'static) {
    let slot = WAKE.get_or_init(|| Mutex::new(None));
    if let Ok(mut slot) = slot.lock() {
        *slot = Some(Arc::new(f));
    }
}

fn wake() {
    let waker = WAKE.get().and_then(|slot| slot.lock().ok().and_then(|s| s.clone()));
    if let Some(f) = waker {
        f();
    }
}

/// A document being read over HTTP.
pub struct Remote {
    pub url: String,
    /// The document's size in bytes. `0` until the server has said.
    pub len: usize,
    /// Pages, once the cross-reference table has been read. `0` before that.
    pub pages: usize,
    /// The document's own name, once its structure has been read.
    pub title: Option<String>,
    /// The bytes of the document that are here.
    pub cache: RangeCache,
    /// How many bytes have crossed the wire, for the status line.
    pub got: u64,
    /// Requests sent and not answered yet. While this is above zero, a render
    /// pass would only repeat a request, so it is not run.
    pub pending: usize,
    /// The last failure, shown until something better happens.
    pub error: Option<String>,
    /// Answers that carried nothing, in a row: a server that says it has the
    /// range but sends an empty body would otherwise be asked forever.
    empty: u32,
    /// The bookmarks, handed over once and then owned by the document.
    outline: Option<Vec<outline::Row>>,
    tx: Sender<Job>,
    rx: Receiver<Msg>,
}

impl Remote {
    /// Start reading `url`. Returns at once: the head of the file is fetched on
    /// a worker thread, and [`Remote::poll`] collects the answer.
    pub fn connect(url: &str) -> Remote {
        let (tx, jobs) = channel::<Job>();
        let (sender, mine) = channel::<Msg>();
        let target = url.to_string();
        let spawned = thread::Builder::new()
            .name("pdf-net".into())
            .spawn(move || {
                match open(&target) {
                    Ok(opened) => {
                        // Past this point the document has an address: the one
                        // the redirects led to, which is where its ranges are.
                        let effective = opened.url.clone();
                        let _ = sender.send(Msg::Ready(Box::new(opened)));
                        wake();
                        work(&effective, jobs, sender);
                    }
                    Err(e) => {
                        let _ = sender.send(Msg::Failed(e));
                        wake();
                    }
                }
            });
        if let Err(e) = spawned {
            log::error!("pdf-net thread: {e}");
        }
        Remote {
            url: url.to_string(),
            len: 0,
            pages: 0,
            title: None,
            cache: RangeCache::new(0),
            got: 0,
            // The open request is outstanding from the start.
            pending: 1,
            error: None,
            empty: 0,
            outline: None,
            tx,
            rx: mine,
        }
    }

    /// Take whatever the worker has sent. Returns whether the document changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.rx.try_recv() {
                Ok(Msg::Ready(opened)) => {
                    self.url = opened.url.clone();
                    self.len = opened.len;
                    self.pages = opened.pages;
                    self.title = opened.title.clone();
                    self.cache = opened.cache;
                    self.got = opened.got;
                    self.outline = Some(opened.outline);
                    self.pending = self.pending.saturating_sub(1);
                    changed = true;
                }
                Ok(Msg::Bytes(items)) => {
                    self.pending = self.pending.saturating_sub(1);
                    let mut took = 0usize;
                    for (start, data) in items {
                        took += data.len();
                        self.cache.insert(start, data);
                    }
                    self.got += took as u64;
                    self.empty = if took == 0 { self.empty + 1 } else { 0 };
                    changed = true;
                }
                Ok(Msg::Failed(e)) => {
                    self.pending = self.pending.saturating_sub(1);
                    log::error!("{}: {e}", self.url);
                    self.error = Some(e);
                    changed = true;
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        changed
    }

    /// The bookmarks, if the worker has finished reading the structure.
    pub fn take_outline(&mut self) -> Option<Vec<outline::Row>> {
        self.outline.take()
    }

    /// A server that answered a range request with nothing, twice running, is
    /// not going to answer it.
    pub fn stalled(&self) -> bool {
        self.empty >= 2
    }

    /// How much of the file is here, 0..=1, for the status line.
    pub fn fraction(&self) -> f64 {
        if self.len == 0 {
            0.0
        } else {
            (self.cache.held() as f64 / self.len as f64).clamp(0.0, 1.0)
        }
    }

    /// Ask the worker for these ranges. Nothing is asked twice: ranges already
    /// here are dropped, and neighbours travel together.
    pub fn request(&mut self, wanted: &[std::ops::Range<usize>]) {
        let wanted = coalesce(self.cache.missing(wanted), SLACK);
        if wanted.is_empty() {
            return;
        }
        log::info!("{}: asking for {} range(s)", self.url, wanted.len());
        if self.tx.send(Job::Fetch(wanted)).is_ok() {
            self.pending += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// the worker

/// Read the document's structure: how many pages, the bookmarks, what bytes it
/// took.
fn open(url: &str) -> Result<Opened, String> {
    let head = net::head(url)?;
    // A redirect moves the document: `head` followed it, and this is where it
    // ended up. Every range below, and every one the worker asks for later,
    // goes there — the short link is paid for once.
    let url = head.url.as_str();
    let len = head
        .len
        .ok_or_else(|| "the server did not say how big the file is".to_string())? as usize;
    if len < 8 {
        return Err(format!("that is {} bytes, not a PDF", len));
    }
    let mut cache = RangeCache::new(len);
    let mut got = 0u64;

    // The first response: the end of the file, where the cross-reference table
    // and the trailer are, plus the start, where a linearised file puts the
    // objects of page one.
    let mut boot = vec![len.saturating_sub(TAIL)..len, 0..FRONT.min(len)];
    if !head.ranges {
        // No `Range:` support: the whole file has to come down in one piece.
        // That is still better than refusing to open it.
        log::warn!("{url}: no range support, downloading all {len} bytes");
        boot = vec![0..len];
    }
    got += fetch(url, &mut cache, &boot)?;

    let mut pages = 0usize;
    let mut rows = Vec::new();
    let mut title = None;
    let outcome = render_with_ranges(
        &mut cache,
        ROUNDS * 4,
        |reader| {
            let file: File<Borrowed<'_, LazyReader<'_>>, NoCache, NoCache, NoLog> = File::load_data(
                Borrowed(reader),
                b"",
                // Lenient, not strict: real files put `Null` where a
                // dictionary belongs (arXiv's `/Info`, for one) and a viewer
                // that refuses to open those is useless. And generous about
                // image size, as the local open is: see `crate::page_options`.
                crate::page_options(),
                NoCache,
                NoCache,
                NoLog,
            )?;
            pages = file.num_pages() as usize;
            rows = outline::read(&file);
            title = crate::info_title(&file.trailer);
            Ok(())
        },
        |cache, missing| {
            got += fetch(url, cache, missing)?;
            Ok(())
        },
    );
    outcome.map_err(|e| format!("reading the structure: {e}"))?;
    log::info!(
        "{url}: {pages} pages, {} outline rows, {got} of {len} bytes",
        rows.len()
    );
    Ok(Opened {
        url: url.to_string(),
        len,
        pages,
        title,
        outline: rows,
        cache,
        got,
    })
}

/// One round trip: fetch `wanted` into `cache`, and say how many bytes came.
fn fetch(
    url: &str,
    cache: &mut RangeCache,
    wanted: &[std::ops::Range<usize>],
) -> Result<u64, String> {
    let wanted = coalesce(cache.missing(wanted), SLACK);
    let mut got = 0u64;
    for range in wanted {
        if range.is_empty() {
            continue;
        }
        let data = net::range(url, range.start as u64, range.end as u64)?;
        got += data.len() as u64;
        cache.insert(range.start, data);
    }
    Ok(got)
}

/// Serve fetch requests until the document is dropped.
fn work(url: &str, jobs: Receiver<Job>, out: Sender<Msg>) {
    for job in jobs {
        match job {
            Job::Fetch(ranges) => {
                let mut items = Vec::with_capacity(ranges.len());
                let mut failure = None;
                for range in ranges {
                    if range.is_empty() {
                        continue;
                    }
                    match net::range(url, range.start as u64, range.end as u64) {
                        Ok(data) => items.push((range.start, data)),
                        Err(e) => {
                            failure = Some(e);
                            break;
                        }
                    }
                }
                let failed = failure.is_some();
                if failed {
                    let _ = out.send(Msg::Failed(failure.unwrap_or_default()));
                } else {
                    let _ = out.send(Msg::Bytes(items));
                }
                wake();
            }
        }
    }
}
