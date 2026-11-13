//! Byte ranges over HTTP(S), without a browser in the way.
//!
//! A PDF that lives at a URL is not downloaded to be read: the xref at the tail
//! says where every object is, and only the objects a page needs are fetched.
//! That is one `Range:` request per round, with `pdf_render::rangecache`
//! deciding what to ask for — so this module is the smallest thing that can
//! carry one: parse the URL, open a TCP socket, do TLS if the scheme says so,
//! send a GET with a `Range` header, and hand back the body.
//!
//! TLS is `rustls` with the Mozilla root set compiled in (`webpki-roots`): the
//! NDK has no TLS stack and there is no Java in the APK, so the alternative was
//! a `classes.dex` and JNI for one function. This also stays on the caller's
//! thread, which is what the range loop wants.
//!
//! Nothing here is asynchronous. The caller runs on a worker thread and every
//! socket has a deadline, so a server that says nothing for ten seconds fails a
//! page instead of hanging the viewer.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

/// How long a connect, a read or a write may take before the page gives up.
const TIMEOUT: Duration = Duration::from_secs(15);
/// A single range response bigger than this is a server ignoring `Range` and
/// sending the file: supported, up to a point, but not for a 400-page spec.
const MAX_BODY: usize = 256 * 1024 * 1024;
/// How many `Location:` headers one request may be bounced through. Five is
/// what browsers stop trusting; eight is generous and still bounded.
const MAX_REDIRECTS: usize = 8;

/// A URL, split into the parts a request needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Url {
    pub tls: bool,
    /// `Host:` header value, without the port.
    pub host: String,
    pub port: u16,
    /// Path *and* query, starting with `/`.
    pub path: String,
}

impl Url {
    pub fn parse(s: &str) -> Result<Url, String> {
        let s = s.trim();
        let (tls, rest) = if let Some(r) = s.strip_prefix("https://") {
            (true, r)
        } else if let Some(r) = s.strip_prefix("http://") {
            (false, r)
        } else if s.contains("://") {
            return Err(format!("only http and https: {s}"));
        } else {
            // A bare host is what people paste; https is the only sane guess.
            (true, s)
        };
        let rest = rest.trim_start_matches("//");
        // The authority ends at the first `/`, `?` or `#`.
        let end = rest
            .find(|c| c == '/' || c == '?' || c == '#')
            .unwrap_or(rest.len());
        let (authority, path) = rest.split_at(end);
        let mut path = path.to_string();
        if path.is_empty() || path.starts_with('?') {
            path = format!("/{path}");
        }
        // A fragment never reaches the server.
        if let Some(at) = path.find('#') {
            path.truncate(at);
        }
        // Credentials in the URL are refused: they would end up in the Host
        // header or nowhere, and silently dropping them is worse.
        if authority.contains('@') {
            return Err("credentials in the URL are not supported".into());
        }
        let (host, port) = match authority.rsplit_once(':') {
            // `[::1]:443` — a bracketed IPv6 literal keeps its brackets.
            Some((h, p)) => (
                h.to_string(),
                p.parse::<u16>()
                    .map_err(|_| format!("bad port in {authority}"))?,
            ),
            None => (authority.to_string(), if tls { 443 } else { 80 }),
        };
        if host.is_empty() {
            return Err("no host in the URL".into());
        }
        Ok(Url {
            tls,
            host,
            port,
            path: path.to_string(),
        })
    }

    fn authority(&self) -> String {
        let default = if self.tls { 443 } else { 80 };
        if self.port == default {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    fn host_name(&self) -> String {
        self.host.trim_start_matches('[').trim_end_matches(']').to_string()
    }
}

/// The statuses that mean "the thing you asked for is somewhere else".
fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// Where a redirect points, as a URL that [`Url::parse`] accepts.
///
/// `Location:` may be absolute, scheme-relative (`//host/x`), rooted (`/x`), a
/// query (`?page=2`) or a bare relative path — servers use all five, and a
/// shortener that only ever sends absolute URLs is not the case worth writing
/// for.
fn resolve(base: &Url, location: &str) -> String {
    let loc = location.trim();
    let scheme = if base.tls { "https" } else { "http" };
    if loc.starts_with("http://") || loc.starts_with("https://") {
        return loc.to_string();
    }
    if let Some(rest) = loc.strip_prefix("//") {
        return format!("{scheme}://{rest}");
    }
    let authority = base.authority();
    if loc.starts_with('/') {
        return format!("{scheme}://{authority}{loc}");
    }
    let path = base.path.split('?').next().unwrap_or("/");
    if let Some(query) = loc.strip_prefix('?') {
        return format!("{scheme}://{authority}{path}?{query}");
    }
    // Relative to the directory of the current path, not to the path itself.
    let dir = match path.rfind('/') {
        Some(at) => &path[..=at],
        None => "/",
    };
    format!("{scheme}://{authority}{dir}{loc}")
}

/// The URL in a redirect response's `Location:`, resolved against where it came
/// from.
fn follow(from: &Url, response: &Response) -> Result<String, String> {
    let location = response
        .location()
        .ok_or_else(|| format!("{}: HTTP {} with no Location", from.host, response.status))?;
    let to = resolve(from, location);
    log::info!("{}: HTTP {} -> {to}", from.host, response.status);
    Ok(to)
}

/// The body of a response, and what the status line said about it.
struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn location(&self) -> Option<&str> {
        self.header("location")
    }

    /// `bytes 100-199/12345` -> (100, 200, 12345).
    fn content_range(&self) -> Option<(u64, u64, Option<u64>)> {
        let v = self.header("content-range")?.trim();
        let v = v.strip_prefix("bytes")?.trim();
        let (span, total) = v.split_once('/')?;
        let (a, b) = span.split_once('-')?;
        let total = total.trim();
        let total = if total == "*" {
            None
        } else {
            Some(total.parse().ok()?)
        };
        Some((
            a.trim().parse().ok()?,
            // A 206 carries an inclusive end; the caller wants exclusive.
            b.trim().parse::<u64>().ok()? + 1,
            total,
        ))
    }
}

/// A plain socket or a TLS one, as far as the request code cares.
enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

fn roots() -> rustls::RootCertStore {
    let mut store = rustls::RootCertStore::empty();
    store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    store
}

/// One HTTP/1.1 exchange. `request` is everything after the request line's
/// method and target, which is all a range GET ever varies.
fn request(url: &Url, method: &str, extra: &[(&str, &str)]) -> Result<Response, String> {
    let addrs = (url.host_name().as_str(), url.port)
        .to_socket_addrs()
        .map_err(|e| format!("{}: {e}", url.host))?;
    let mut last = String::new();
    let mut tcp = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, TIMEOUT) {
            Ok(s) => {
                tcp = Some(s);
                break;
            }
            Err(e) => last = format!("{addr}: {e}"),
        }
    }
    let tcp = tcp.ok_or_else(|| format!("connect {}: {last}", url.host))?;
    let _ = tcp.set_read_timeout(Some(TIMEOUT));
    let _ = tcp.set_write_timeout(Some(TIMEOUT));
    let _ = tcp.set_nodelay(true);

    let mut stream = if url.tls {
        let provider = rustls::crypto::ring::default_provider();
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
            .with_safe_default_protocol_versions()
            .map_err(|e| format!("tls: {e}"))?
            .with_root_certificates(roots())
            .with_no_client_auth();
        let name = rustls::pki_types::ServerName::try_from(url.host_name())
            .map_err(|e| format!("{}: {e}", url.host))?;
        let conn = rustls::ClientConnection::new(Arc::new(config), name)
            .map_err(|e| format!("tls {}: {e}", url.host))?;
        Stream::Tls(Box::new(rustls::StreamOwned::new(conn, tcp)))
    } else {
        Stream::Plain(tcp)
    };

    let mut head = format!(
        "{method} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: pdf-android\r\nAccept: */*\r\nConnection: close\r\n",
        url.path,
        url.authority()
    );
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    log::debug!("-> {}", head.lines().next().unwrap_or_default());
    stream
        .write_all(head.as_bytes())
        .and_then(|_| stream.flush())
        .map_err(|e| format!("send {}: {e}", url.host))?;

    read_response(stream, method != "HEAD")
}

fn read_response(stream: Stream, want_body: bool) -> Result<Response, String> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("read status: {e}"))?;
    // `HTTP/1.1 206 Partial Content`
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| format!("bad status line: {:?}", line.trim()))?;
    log::debug!("<- {}", line.trim());
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        let n = reader
            .read_line(&mut h)
            .map_err(|e| format!("read header: {e}"))?;
        if n == 0 || h == "\r\n" || h == "\n" {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }

    if !want_body || status == 204 || status == 304 || status < 200 {
        // `HEAD`, and the statuses that carry no body by definition: the
        // `Content-Length` in the headers describes what a GET would return,
        // not what follows these headers.
        return Ok(Response {
            status,
            headers,
            body: Vec::new(),
        });
    }

    let mut body = Vec::new();
    let chunked = headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("transfer-encoding") && v.contains("chunked"));
    let length = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok());

    if chunked {
        loop {
            let mut size = String::new();
            reader
                .read_line(&mut size)
                .map_err(|e| format!("read chunk: {e}"))?;
            let size = size.trim();
            let size = size.split(';').next().unwrap_or(size);
            let n = usize::from_str_radix(size, 16).map_err(|_| format!("bad chunk size {size:?}"))?;
            if n == 0 {
                break;
            }
            let at = body.len();
            body.resize(at + n, 0);
            reader
                .read_exact(&mut body[at..])
                .map_err(|e| format!("read chunk body: {e}"))?;
            let mut crlf = [0u8; 2];
            let _ = reader.read_exact(&mut crlf);
            if body.len() > MAX_BODY {
                return Err("response too large".into());
            }
        }
    } else if let Some(n) = length {
        if n > MAX_BODY {
            return Err(format!("response of {n} bytes is too large"));
        }
        // Read what the server is willing to send, not what it promised: a
        // short body has to be reported as such, not as a read error.
        let mut buf = Vec::with_capacity(n.min(64 * 1024));
        let got = reader
            .by_ref()
            .take(n as u64)
            .read_to_end(&mut buf)
            .map_err(|e| format!("read body ({n} bytes): {e}"))?;
        if got < n {
            log::warn!(
                "body: {got} of {n} bytes, status {status}, headers {headers:?}"
            );
            return Err(format!("the server sent {got} of {n} bytes"));
        }
        body = buf;
    } else {
        let mut rest = Vec::new();
        reader
            .take(MAX_BODY as u64)
            .read_to_end(&mut rest)
            .map_err(|e| format!("read body: {e}"))?;
        body = rest;
    }

    Ok(Response {
        status,
        headers,
        body,
    })
}

/// What a server says about a file before any of it is read.
pub struct Head {
    /// Length in bytes, if the server said.
    pub len: Option<u64>,
    /// Whether `bytes=` appeared in `Accept-Ranges`.
    pub ranges: bool,
    /// The URL that answered, which is not the one asked for when the server
    /// redirected: every later request belongs on this one.
    pub url: String,
}

/// Ask for the length, and whether ranges are offered.
///
/// `HEAD` is the polite form, but plenty of servers behind a CDN answer it with
/// no `Content-Length` or with `Accept-Ranges: none`; a one-byte range GET is
/// the fallback because its `Content-Range` carries both answers.
///
/// Redirects are followed here instead of being handed to the caller, and
/// [`Head::url`] is where they led: a short link then costs one extra round
/// trip for the whole document, not one per range.
pub fn head(url: &str) -> Result<Head, String> {
    let mut url = url.trim().to_string();
    for _ in 0..=MAX_REDIRECTS {
        let parsed = Url::parse(&url)?;
        match head_once(&parsed)? {
            Answer::Moved(to) => url = to,
            Answer::Here { len, ranges } => return Ok(Head { len, ranges, url }),
        }
    }
    Err(format!("more than {MAX_REDIRECTS} redirects from {url}"))
}

/// What one attempt found: where to go, or what was there.
enum Answer {
    Moved(String),
    Here { len: Option<u64>, ranges: bool },
}

fn head_once(url: &Url) -> Result<Answer, String> {
    let head = request(url, "HEAD", &[])?;
    if is_redirect(head.status) {
        return Ok(Answer::Moved(follow(url, &head)?));
    }
    if head.status == 200 {
        let len = head
            .header("content-length")
            .and_then(|v| v.trim().parse::<u64>().ok());
        let ranges = head
            .header("accept-ranges")
            .is_some_and(|v| v.to_ascii_lowercase().contains("bytes"));
        if len.is_some() && ranges {
            return Ok(Answer::Here { len, ranges: true });
        }
    }
    // Ask for one byte: `Content-Range: bytes 0-0/123456` answers both.
    let probe = request(url, "GET", &[("Range", "bytes=0-0")])?;
    if is_redirect(probe.status) {
        return Ok(Answer::Moved(follow(url, &probe)?));
    }
    if probe.status == 206 {
        if let Some((_, _, total)) = probe.content_range() {
            return Ok(Answer::Here { len: total, ranges: true });
        }
        return Ok(Answer::Here { len: None, ranges: true });
    }
    if probe.status == 200 {
        // No range support: the whole file came back. Its length is all we can
        // say, and the caller will fall back to reading it in one piece.
        return Ok(Answer::Here {
            len: Some(probe.body.len() as u64),
            ranges: false,
        });
    }
    Err(format!("{}: HTTP {}", url.host, probe.status))
}

/// Bytes `[start, end)` of the file. `end` may exceed the file length; a server
/// clamps it.
pub fn range(url: &str, start: u64, end: u64) -> Result<Vec<u8>, String> {
    let last = end.saturating_sub(1);
    let header = format!("bytes={start}-{last}");
    let mut url = url.trim().to_string();
    for _ in 0..=MAX_REDIRECTS {
        let parsed = Url::parse(&url)?;
        let resp = request(&parsed, "GET", &[("Range", &header)])?;
        if is_redirect(resp.status) {
            // Every range is resolved again, because a signed link may bounce
            // somewhere new on each request.
            url = follow(&parsed, &resp)?;
            continue;
        }
        return match resp.status {
            206 => {
                // A server may answer with a narrower range than asked for (the
                // end of the file, usually). Trust `Content-Range` over the ask.
                if let Some((a, b, _)) = resp.content_range() {
                    if a == start {
                        let want = (end.min(b) - start) as usize;
                        let mut body = resp.body;
                        body.truncate(want.min(body.len()));
                        return Ok(body);
                    }
                }
                Ok(resp.body)
            }
            200 => {
                // The server ignored `Range` and sent everything.
                let body = resp.body;
                if start as usize >= body.len() {
                    return Ok(Vec::new());
                }
                Ok(body[start as usize..(end as usize).min(body.len())].to_vec())
            }
            other => Err(format!("HTTP {other} for {header}")),
        };
    }
    Err(format!("more than {MAX_REDIRECTS} redirects from {url}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_schemes_ports_and_paths() {
        let u = Url::parse("https://example.com/a/b.pdf?x=1#p2").unwrap();
        assert_eq!(
            u,
            Url {
                tls: true,
                host: "example.com".into(),
                port: 443,
                path: "/a/b.pdf?x=1".into()
            }
        );
        let u = Url::parse("http://example.com:8080").unwrap();
        assert_eq!(u.port, 8080);
        assert_eq!(u.path, "/");
        // A bare host is https, and a bare authority gets a path.
        let u = Url::parse("mu.li/d#key").unwrap();
        assert_eq!(u.tls, true);
        assert_eq!(u.host, "mu.li");
        assert_eq!(u.path, "/d");
        let u = Url::parse("https://[::1]:8443/x").unwrap();
        assert_eq!(u.host_name(), "::1");
        assert_eq!(u.authority(), "[::1]:8443");
        assert!(Url::parse("ftp://example.com/x").is_err());
        assert!(Url::parse("https://user:pw@example.com/x").is_err());
    }

    #[test]
    fn content_range_is_half_open() {
        let r = Response {
            status: 206,
            headers: vec![(
                "Content-Range".into(),
                "bytes 100-199/12345".into(),
            )],
            body: Vec::new(),
        };
        assert_eq!(r.content_range(), Some((100, 200, Some(12345))));
        let r = Response {
            status: 206,
            headers: vec![("Content-Range".into(), "bytes 0-0/*".into())],
            body: Vec::new(),
        };
        assert_eq!(r.content_range(), Some((0, 1, None)));
    }
}
