// Copyright (C) 2026 Synaelle
// SPDX-License-Identifier: AGPL-3.0-or-later
// A small HTTP/1.1 client for the update check and download: GET only,
// redirects followed, the body streamed (never held whole). HTTPS (TLS 1.3)
// goes through rustls (ring's crypto, Mozilla's roots from webpki-roots), with the
// certificates always verified: what it downloads gets installed. Plain http
// only exists in the PC test build (sys::PLAIN_HTTP).

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use rustls::crypto::ring as provider;
use rustls::pki_types::{ServerName, UnixTime};

use crate::sys;

const TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REDIRECTS: usize = 5;
const MAX_HEADERS: usize = 16 * 1024; // bytes of status line and headers

/// why a request failed (update.rs words it for the screen)
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Fail {
    Connect, // no DNS answer or no connection
    Cert,    // the server's certificate didn't verify
    Status(u16),
    Bad, // not HTTP as expected
    Io,  // the connection broke
}

pub struct Url {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    pub path: String,
}

/// "https://host[:port]/path"; plain http only where sys::PLAIN_HTTP allows it
pub fn parse_url(s: &str) -> Option<Url> {
    let (tls, rest) = match s.split_once("://")? {
        ("https", r) => (true, r),
        ("http", r) if sys::PLAIN_HTTP => (false, r),
        _ => return None,
    };
    let (auth, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match auth.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (auth, if tls { 443 } else { 80 }),
    };
    let ok = |c: char| c.is_ascii_alphanumeric() || c == '.' || c == '-';
    if host.is_empty() || !host.chars().all(ok) || path.bytes().any(|b| b <= b' ' || b >= 0x7F) {
        return None;
    }
    Some(Url { tls, host: host.to_ascii_lowercase(), port, path: path.to_string() })
}

/// a Location header against the URL that sent it
fn redirect(from: &Url, loc: &str) -> Option<Url> {
    if loc.contains("://") {
        return parse_url(loc);
    }
    let path = if loc.starts_with('/') { loc.to_string() } else { format!("{}/{loc}", from.path.rsplit_once('/')?.0) };
    let scheme = if from.tls { "https" } else { "http" };
    parse_url(&format!("{scheme}://{}:{}{path}", from.host, from.port))
}

/// the certificates' clock: the console's (local time, close enough for
/// certificates), but never before this build's time of writing, so a 3DS
/// whose clock was reset to 2000 still checks the dates sensibly
#[derive(Debug)]
struct Clock;

/// 2026-10-01
const EARLIEST: u64 = 1_790_812_800;

impl rustls::time_provider::TimeProvider for Clock {
    fn current_time(&self) -> Option<UnixTime> {
        let now = sys::unix_time().unwrap_or(0).max(EARLIEST);
        Some(UnixTime::since_unix_epoch(Duration::from_secs(now)))
    }
}

fn tls_config() -> Option<Arc<rustls::ClientConfig>> {
    static CONFIG: OnceLock<Option<Arc<rustls::ClientConfig>>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            // ChaCha20 first: the ARM11 has no AES instructions, ring's portable AES is slower
            let p = provider::default_provider();
            let suites = [
                provider::cipher_suite::TLS13_CHACHA20_POLY1305_SHA256,
                provider::cipher_suite::TLS13_AES_128_GCM_SHA256,
                provider::cipher_suite::TLS13_AES_256_GCM_SHA384,
            ];
            let p = rustls::crypto::CryptoProvider { cipher_suites: suites.to_vec(), ..p };
            let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
            let config = rustls::ClientConfig::builder_with_details(Arc::new(p), Arc::new(Clock))
                .with_safe_default_protocol_versions()
                .ok()?
                .with_root_certificates(roots)
                .with_no_client_auth();
            Some(Arc::new(config))
        })
        .clone()
}

enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

/// a TLS failure is a certificate problem when rustls says so
fn fail_of(e: &io::Error) -> Fail {
    match e.get_ref().and_then(|e| e.downcast_ref::<rustls::Error>()) {
        Some(rustls::Error::InvalidCertificate(_)) => Fail::Cert,
        Some(_) => Fail::Bad,
        None => Fail::Io,
    }
}

enum Framing {
    Length(u64),        // bytes left
    Chunked(u64, bool), // bytes left in this chunk, the last one was read
    Close,              // until the server closes
}

/// a response's body, read as it arrives
pub struct Body {
    r: BufReader<Stream>,
    framing: Framing,
    pub length: Option<u64>, // Content-Length
}

impl Read for Body {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let bad = || io::Error::new(io::ErrorKind::InvalidData, "bad chunk");
        match &mut self.framing {
            Framing::Length(0) | Framing::Chunked(_, true) => Ok(0),
            Framing::Length(left) => {
                let want = buf.len().min(*left as usize);
                let n = self.r.read(&mut buf[..want])?;
                if n == 0 {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                *left -= n as u64;
                Ok(n)
            }
            Framing::Chunked(left, end) => {
                if *left == 0 {
                    let line = read_line(&mut self.r)?;
                    let size = line.split(';').next().unwrap_or("").trim();
                    *left = u64::from_str_radix(size, 16).map_err(|_| bad())?;
                    if *left == 0 {
                        // trailers, up to the empty line
                        while !read_line(&mut self.r)?.is_empty() {}
                        *end = true;
                        return Ok(0);
                    }
                }
                let want = buf.len().min(*left as usize);
                let n = self.r.read(&mut buf[..want])?;
                if n == 0 {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                *left -= n as u64;
                if *left == 0 && !read_line(&mut self.r)?.is_empty() {
                    return Err(bad());
                }
                Ok(n)
            }
            Framing::Close => match self.r.read(buf) {
                // servers often close without a TLS close_notify
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(0),
                r => r,
            },
        }
    }
}

/// one CRLF line, without it (at most 4 KB)
fn read_line(r: &mut impl BufRead) -> io::Result<String> {
    let mut line = Vec::new();
    r.take(4096).read_until(b'\n', &mut line)?;
    if line.last() != Some(&b'\n') {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    line.pop();
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    String::from_utf8(line).map_err(|_| io::ErrorKind::InvalidData.into())
}

/// GET `url`, following redirects; `accept` goes in the Accept header. `sock`
/// gets a handle on each connection, so a stalled one can be shut down from
/// another thread.
pub fn get(url: &str, accept: &str, sock: &Mutex<Option<TcpStream>>) -> Result<Body, Fail> {
    let mut url = parse_url(url).ok_or(Fail::Bad)?;
    for _ in 0..=MAX_REDIRECTS {
        let (status, location, body) = request(&url, accept, sock)?;
        match status {
            200 => return Ok(body),
            301 | 302 | 303 | 307 | 308 => {
                url = location.and_then(|l| redirect(&url, &l)).ok_or(Fail::Bad)?;
            }
            s => return Err(Fail::Status(s)),
        }
    }
    Err(Fail::Bad)
}

fn request(url: &Url, accept: &str, sock: &Mutex<Option<TcpStream>>) -> Result<(u16, Option<String>, Body), Fail> {
    let addr = (url.host.as_str(), url.port).to_socket_addrs().map_err(|_| Fail::Connect)?.find(|a| a.is_ipv4());
    let addr = addr.ok_or(Fail::Connect)?;
    // a socket layer without non-blocking connects gets a plain one
    let tcp = TcpStream::connect_timeout(&addr, TIMEOUT)
        .or_else(|e| if e.kind() == io::ErrorKind::TimedOut { Err(e) } else { TcpStream::connect(addr) })
        .map_err(|_| Fail::Connect)?;
    // not every socket layer has timeouts: the caller's watchdog covers that
    let _ = tcp.set_read_timeout(Some(TIMEOUT));
    let _ = tcp.set_write_timeout(Some(TIMEOUT));
    let _ = tcp.set_nodelay(true);
    *sock.lock().unwrap() = tcp.try_clone().ok();
    let mut stream = if url.tls {
        let name = ServerName::try_from(url.host.clone()).map_err(|_| Fail::Bad)?;
        let conn = rustls::ClientConnection::new(tls_config().ok_or(Fail::Bad)?, name).map_err(|_| Fail::Bad)?;
        Stream::Tls(Box::new(rustls::StreamOwned::new(conn, tcp)))
    } else {
        Stream::Plain(tcp)
    };
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: Cement/{}\r\nAccept: {accept}\r\nConnection: close\r\n\r\n",
        url.path,
        url.host,
        crate::VERSION
    );
    stream.write_all(req.as_bytes()).and_then(|_| stream.flush()).map_err(|e| fail_of(&e))?;

    let mut r = BufReader::with_capacity(16 * 1024, stream);
    let line = read_line(&mut r).map_err(|e| fail_of(&e))?;
    let mut parts = line.split(' ');
    let status = match (parts.next(), parts.next()) {
        (Some(v), Some(code)) if v.starts_with("HTTP/1.") => code.parse::<u16>().map_err(|_| Fail::Bad)?,
        _ => return Err(Fail::Bad),
    };
    let (mut length, mut chunked, mut location, mut seen) = (None, false, None, line.len());
    loop {
        let h = read_line(&mut r).map_err(|e| fail_of(&e))?;
        seen += h.len() + 2;
        if seen > MAX_HEADERS {
            return Err(Fail::Bad);
        }
        if h.is_empty() {
            break;
        }
        let Some((k, v)) = h.split_once(':') else { continue };
        let v = v.trim();
        match k.trim().to_ascii_lowercase().as_str() {
            "content-length" => length = Some(v.parse::<u64>().map_err(|_| Fail::Bad)?),
            "transfer-encoding" => chunked = v.to_ascii_lowercase().contains("chunked"),
            "location" => location = Some(v.to_string()),
            _ => {}
        }
    }
    let framing = match (chunked, length) {
        (true, _) => Framing::Chunked(0, false),
        (false, Some(n)) => Framing::Length(n),
        (false, None) => Framing::Close,
    };
    Ok((status, location, Body { r, framing, length: if chunked { None } else { length } }))
}

/// ends a connection another thread is blocked on
pub fn cut(sock: &Mutex<Option<TcpStream>>) {
    if let Some(s) = sock.lock().unwrap().as_ref() {
        let _ = s.shutdown(Shutdown::Both);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        let u = parse_url("https://GitHub.com/Synaelle/cement/releases/download/v1.2.0/Cement.cia").unwrap();
        assert!(
            u.tls
                && u.host == "github.com"
                && u.port == 443
                && u.path == "/Synaelle/cement/releases/download/v1.2.0/Cement.cia"
        );
        let u = parse_url("https://example.org:8443").unwrap();
        assert!(u.port == 8443 && u.path == "/");
        assert!(parse_url("ftp://example.org/").is_none());
        assert!(parse_url("https://bad host/").is_none());
        assert!(parse_url("https://example.org/a b").is_none());
        let from = parse_url("https://a.example/x/y").unwrap();
        let to = redirect(&from, "/z?q=1").unwrap();
        assert!(to.host == "a.example" && to.path == "/z?q=1");
        assert_eq!(redirect(&from, "w").unwrap().path, "/x/w");
        assert_eq!(redirect(&from, "https://b.example/v").unwrap().host, "b.example");
    }

    #[test]
    fn chunks() {
        // a body read from a plain stream: a local socket pair
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.write_all(b"5\r\nhello\r\n7;x=1\r\n, world\r\n0\r\nTrailer: y\r\n\r\n").unwrap();
        });
        let tcp = TcpStream::connect(addr).unwrap();
        let mut body =
            Body { r: BufReader::new(Stream::Plain(tcp)), framing: Framing::Chunked(0, false), length: None };
        let mut out = String::new();
        body.read_to_string(&mut out).unwrap();
        assert_eq!(out, "hello, world");
        t.join().unwrap();
    }
}
