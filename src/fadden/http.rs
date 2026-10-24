//! HTTP for the network stages: `urllib.request.urlopen(Request(url,
//! headers={"User-Agent": UA}), timeout=90)` as `fadden/http_fetch.py` and the
//! rulings stage call it, the shared attempt budget, and the local date.
//!
//! `Net` ports the parts of CPython 3.15's `urllib.request` and `http.client`
//! those calls reach: the default opener's handlers (`ProxyHandler` with the
//! environment's proxies, `HTTPRedirectHandler`, `HTTPErrorProcessor`), one
//! `Request` reused for every attempt with the changes those handlers make to
//! it, the request `http.client` writes, and `HTTPResponse`'s status line,
//! header, framing and read rules. A status outside 200-299 is urllib's
//! `HTTPError`; any other failure carries the name of the exception urllib
//! raises there, and a `ValueError` the callers do not catch stops the stage.
//! TLS is rustls with the operating system's certificate verifier, where
//! Python uses OpenSSL with the system's root certificates. Each connection
//! attempt, socket read and socket write may take 90 seconds and the TLS
//! handshake 90 seconds in all, as Python's socket timeout allows; Python's
//! `sendall` gives the whole request one 90-second budget.
//!
//! The request is checked and encoded as `http.client` does before sending:
//! an ASCII request line, Latin-1 header values without a bare line break,
//! and a tunnel host without control characters.
//!
//! Not ported: urllib's FTP, file and data handlers (a redirect or proxy to one
//! of those schemes fails as `URLError`), the proxy settings Python reads from
//! the Windows registry or macOS System Configuration when no proxy variable
//! is set (set `HTTPS_PROXY` instead), and IDNA encoding of non-ASCII host
//! names.
//!
//! With the `fake-http` feature and `FADDEN_FAKE_HTTP` naming a script, the
//! responses, the waits and the date come from that script instead, and each
//! request and wait is logged to `FADDEN_FAKE_HTTP_LOG`. The differential
//! harness drives the base-revision Python stages through the same script.
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use num_bigint::{BigInt, Sign};

use super::Exc;
use crate::py;
use crate::pyjson::{self, DecodeError, Json};

pub const UA: &str =
    "au-tax-legislation-corpus (+https://github.com/ryanduguid/au-tax-legislation-corpus)";
const TIMEOUT: Duration = Duration::from_secs(90);
/// One attempt budget for every Register caller.
pub const TRIES: u32 = 3;
pub const RETRY_DELAY: f64 = 6.0;
/// `http.client`'s limits: line length, header lines, interim responses.
const MAXLINE: usize = 65536;
const MAXHEADERS: usize = 100;
const MAXINTERIM: usize = 100;

/// A 2xx response: the final URL after redirects, its Content-Type and body.
#[derive(Debug, Clone)]
pub struct Response {
    pub url: String,
    pub content_type: String,
    pub body: Vec<u8>,
}

/// Why `urlopen` raised: an HTTP status, the class name of another failure
/// the callers catch, or a `ValueError` (or subclass) they do not.
#[derive(Debug, Clone, PartialEq)]
pub enum Failure {
    Status(u16),
    Transport(String),
    Uncaught {
        class: &'static str,
        message: String,
    },
}

/// The exception an uncaught failure raises in the stage.
pub fn uncaught(class: &'static str, message: String) -> Exc {
    if class == "UnicodeEncodeError" {
        Exc::UnicodeEncode(message)
    } else {
        Exc::Value { class, message }
    }
}

/// One response as the capture recorder keeps it: any status, every header
/// in the order received, and the body of a 200 response only.
#[derive(Debug, Clone)]
pub struct Exchange {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

/// What one `urllib.request.Request` carries from one attempt to the next.
/// The callers reuse one request for every attempt, so its redirect record
/// (`redirect_dict`, shared with the requests its redirects create) and the
/// changes `ProxyHandler` made to it carry over.
#[derive(Debug, Default)]
pub struct Request {
    redirects: Option<HashMap<String, u32>>,
    sent: Option<Req>,
}

impl Request {
    /// `http_error_302`'s loop check before following a redirect to `url`:
    /// false once `url` has been visited 4 times or 10 URLs have been. The
    /// request's first redirect is never refused.
    pub fn follow(&mut self, url: &str) -> bool {
        visit(&mut self.redirects, url)
    }
}

fn visit(record: &mut Option<HashMap<String, u32>>, url: &str) -> bool {
    let visited = match record {
        None => record.insert(HashMap::new()),
        Some(visited) => {
            if visited.get(url).copied().unwrap_or(0) >= 4 || visited.len() >= 10 {
                return false;
            }
            visited
        }
    };
    *visited.entry(url.to_owned()).or_insert(0) += 1;
    true
}

pub trait Http {
    /// One GET, reading at most `limit` bytes of the body when given;
    /// `request` is the caller's request.
    fn get(
        &mut self,
        url: &str,
        limit: Option<usize>,
        request: &mut Request,
    ) -> Result<Response, Failure>;
    /// One GET that follows no redirect and reports every status, reading at
    /// most `limit` bytes of a 200 body; an `Err` is never `Status`. A fake
    /// that scripts only `get` fails every exchange.
    fn exchange(
        &mut self,
        url: &str,
        limit: usize,
        request: &mut Request,
    ) -> Result<Exchange, Failure> {
        let _ = (url, limit, request);
        Err(Failure::Transport("URLError".into()))
    }
    /// `time.monotonic()`; a fake's clock stands still.
    fn monotonic(&mut self) -> f64 {
        0.0
    }
    /// `datetime.now(timezone.utc).isoformat(timespec="seconds")` with `Z`.
    fn utc_now(&mut self) -> String {
        "1970-01-01T00:00:00Z".into()
    }
    /// `time.sleep(seconds)`.
    fn sleep(&mut self, seconds: f64);
    /// `datetime.date.today().isoformat()`.
    fn today(&mut self) -> String;
}

/// The network, or the harness's script when the build allows one.
pub fn connect() -> Box<dyn Http> {
    #[cfg(feature = "fake-http")]
    if let Ok(path) = std::env::var("FADDEN_FAKE_HTTP") {
        return Box::new(fake::Script::load(&path));
    }
    Box::new(Net::new())
}

/// `http_fetch.attempts()`: attempt numbers 1..=tries, waiting `delay` before
/// each attempt after the first.
pub fn attempt(http: &mut dyn Http, number: u32) {
    if number > 1 {
        http.sleep(RETRY_DELAY);
    }
}

/// `http_fetch.fetch_json(url)`: the decoded JSON object, or None after
/// `TRIES` attempts. A failed request, a body that does not decode or parse,
/// a non-object and an object with an `"error"` member are all retried; a
/// `ValueError` urllib raises and the int digit limit's, which `fetch_json`
/// does not catch, stop the stage.
pub fn fetch_json(http: &mut dyn Http, url: &str) -> Result<Option<Json>, Exc> {
    let mut request = Request::default();
    for number in 1..=TRIES {
        attempt(http, number);
        let response = match http.get(url, None, &mut request) {
            Ok(response) => response,
            Err(Failure::Uncaught { class, message }) => return Err(uncaught(class, message)),
            Err(_) => continue,
        };
        match pyjson::loads_bytes(&response.body) {
            Ok(document @ Json::Object(_)) if document.get("error").is_none() => {
                return Ok(Some(document));
            }
            Err(Some(DecodeError::Invalid(message)))
                if message.starts_with("Exceeds the limit") =>
            {
                return Err(Exc::value(message));
            }
            Err(Some(error @ DecodeError::Recursion(_))) => return Err(super::json_error(error)),
            _ => {}
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------- the opener

/// `urlopen`'s default opener, or the capture stage's, which follows no
/// redirect.
struct Net {
    /// `getproxies()`, read once as the opener reads it.
    proxies: HashMap<String, String>,
    tls: Option<Arc<rustls::ClientConfig>>,
    started: Instant,
}

/// A `urllib.request.Request` as the opener's handlers read and change it.
#[derive(Debug, Clone)]
struct Req {
    /// `full_url` without the fragment, which `fragment` keeps.
    url: String,
    fragment: String,
    kind: String,
    host: String,
    selector: String,
    tunnel_host: String,
    /// `headers` and `unredirected_hdrs`: `str.capitalize()`d names in
    /// insertion order.
    headers: Vec<(String, String)>,
    unredirected: Vec<(String, String)>,
}

/// What `OpenerDirector.open` raises: `HTTPError`, which carries its
/// response, or another failure.
enum Raised {
    Status(Box<Opened>),
    Failure(Failure),
}

impl From<Failure> for Raised {
    fn from(failure: Failure) -> Raised {
        Raised::Failure(failure)
    }
}

/// A response the opener returned: status, headers in the order received,
/// `geturl()` and the unread body.
struct Opened {
    status: u16,
    headers: Vec<(String, String)>,
    url: String,
    body: Body,
}

/// A CONNECT tunnel `set_tunnel` asked for.
struct Tunnel {
    host: String,
    port: i64,
    headers: Vec<(String, String)>,
}

impl Req {
    /// `Request(url, headers=headers)`.
    fn new(url: &str, headers: Vec<(String, String)>) -> Result<Req, Failure> {
        let url = unwrap(url);
        let (url, fragment) = url.rsplit_once('#').unwrap_or((url, ""));
        let mut req = Req {
            url: url.to_owned(),
            fragment: fragment.to_owned(),
            kind: String::new(),
            host: String::new(),
            selector: String::new(),
            tunnel_host: String::new(),
            headers: Vec::new(),
            unredirected: Vec::new(),
        };
        let Some((kind, rest)) = split_type(url) else {
            return Err(value_error(format!(
                "unknown url type: {}",
                py::repr_str(&req.full_url())
            )));
        };
        req.kind = kind;
        (req.host, req.selector) = match split_host(rest) {
            Some((host, selector)) => (unquote(host), selector),
            None => (String::new(), rest.to_owned()),
        };
        for (name, value) in headers {
            set_header(&mut req.headers, &capitalize(&name), &value);
        }
        Ok(req)
    }

    fn full_url(&self) -> String {
        if self.fragment.is_empty() {
            self.url.clone()
        } else {
            format!("{}#{}", self.url, self.fragment)
        }
    }

    fn has_header(&self, name: &str) -> bool {
        self.headers
            .iter()
            .chain(&self.unredirected)
            .any(|(key, _)| key == name)
    }

    fn set_proxy(&mut self, host: String, kind: String) {
        if self.kind == "https" && self.tunnel_host.is_empty() {
            self.tunnel_host = self.host.clone();
        } else {
            self.kind = kind;
            self.selector = self.full_url();
        }
        self.host = host;
    }

    /// `AbstractHTTPHandler.do_request_` for a GET: the `Host` header, from
    /// the selector once a proxy has made it a full URL.
    fn prepare(&mut self) -> Result<(), Failure> {
        if self.host.is_empty() {
            return Err(url_error());
        }
        let mut sel_host = self.host.clone();
        if self.selector == self.full_url() {
            let selector = self.selector.clone();
            let sel = split_type(&selector).map_or(selector.as_str(), |(_, rest)| rest);
            sel_host = split_host(sel).map_or_else(String::new, |(host, _)| host.to_owned());
        }
        if !self.has_header("Host") {
            set_header(&mut self.unredirected, "Host", &sel_host);
        }
        Ok(())
    }
}

impl Net {
    fn new() -> Net {
        Net {
            proxies: environment_proxies(),
            tls: None,
            started: Instant::now(),
        }
    }

    /// The opener's `open(request)` for the caller's request, which the
    /// first attempt builds from `url`.
    fn open_request(
        &mut self,
        url: &str,
        request: &mut Request,
        follow: bool,
    ) -> Result<Opened, Raised> {
        let mut req = match request.sent.take() {
            Some(req) => req,
            None => Req::new(url, vec![("User-Agent".into(), UA.into())])?,
        };
        let opened = self.open(&mut req, &mut request.redirects, follow);
        request.sent = Some(req);
        opened
    }

    /// `OpenerDirector.open(req)`, with `HTTPRedirectHandler` when `follow`
    /// and otherwise the capture stage's subclass, which declines every
    /// redirect.
    fn open(
        &mut self,
        req: &mut Req,
        redirects: &mut Option<HashMap<String, u32>>,
        follow: bool,
    ) -> Result<Opened, Raised> {
        let protocol = req.kind.clone();
        let web = matches!(protocol.as_str(), "http" | "https");
        if web {
            req.prepare()?;
        }
        // _open: ProxyHandler's `<type>_open` first, then the scheme's own.
        let mut opened = None;
        if !protocol.contains('_')
            && let Some(proxy) = self.proxies.get(&protocol).cloned()
        {
            opened = self.proxy_open(req, &proxy, redirects, follow)?;
        }
        let mut response = match opened {
            Some(response) => response,
            None if web => self.do_open(protocol == "https", req)?,
            // shortcut: urllib's FTP, file and data handlers are not ported;
            // their URLs fail as unknown schemes do.
            None => return Err(url_error().into()),
        };
        if !web || (200..300).contains(&response.status) {
            return Ok(response);
        }
        if matches!(response.status, 301 | 302 | 303 | 307 | 308)
            && let Some(next) = self.redirect(req, &mut response, redirects, follow)?
        {
            return Ok(next);
        }
        Err(Raised::Status(Box::new(response)))
    }

    /// `ProxyHandler.proxy_open(req, proxy, type)`. None leaves the changed
    /// request to the scheme's own handler; a proxy of another scheme opens
    /// it again from the start.
    fn proxy_open(
        &mut self,
        req: &mut Req,
        proxy: &str,
        redirects: &mut Option<HashMap<String, u32>>,
        follow: bool,
    ) -> Result<Option<Opened>, Raised> {
        let orig_type = req.kind.clone();
        let (proxy_type, user, password, hostport) = parse_proxy(proxy)?;
        let proxy_type = proxy_type.unwrap_or_else(|| orig_type.clone());
        if !req.host.is_empty() && self.bypass(&req.host) {
            return Ok(None);
        }
        if let (Some(user), Some(password)) = (user, password)
            && !user.is_empty()
            && !password.is_empty()
        {
            let pair = format!("{}:{}", unquote(user), unquote(password));
            let creds = super::live_evidence::base64(pair.as_bytes());
            set_header(
                &mut req.headers,
                "Proxy-authorization",
                &format!("Basic {creds}"),
            );
        }
        req.set_proxy(unquote(hostport), proxy_type.clone());
        if orig_type == proxy_type || orig_type == "https" {
            return Ok(None);
        }
        self.open(req, redirects, follow).map(Some)
    }

    /// `proxy_bypass(host)` against the environment's `no_proxy`.
    fn bypass(&self, host: &str) -> bool {
        let Some(no_proxy) = self.proxies.get("no") else {
            return false;
        };
        if no_proxy == "*" {
            return true;
        }
        let host = host.to_lowercase();
        let (hostonly, _) = split_port(&host);
        for name in no_proxy.split(',') {
            let name = py_strip(name);
            if name.is_empty() {
                continue;
            }
            let name = name.trim_start_matches('.').to_lowercase();
            if hostonly == name || host == name {
                return true;
            }
            let name = format!(".{name}");
            if hostonly.ends_with(&name) || host.ends_with(&name) {
                return true;
            }
        }
        false
    }

    /// `HTTPRedirectHandler.http_error_302` for `response` to `req`: the
    /// redirected request's response, or None where urllib goes on to raise
    /// `HTTPError` for `response` (no target, a refused scheme, the loop
    /// check, or a declined redirect when not following).
    fn redirect(
        &mut self,
        req: &Req,
        response: &mut Opened,
        redirects: &mut Option<HashMap<String, u32>>,
        follow: bool,
    ) -> Result<Option<Opened>, Raised> {
        let Some(location) =
            header(&response.headers, "location").or_else(|| header(&response.headers, "uri"))
        else {
            return Ok(None);
        };
        let Some(newurl) = redirect_target(&req.full_url(), location)? else {
            return Ok(None);
        };
        if !follow {
            return Ok(None);
        }
        // redirect_request: a GET with every header but the content ones.
        let headers = req
            .headers
            .iter()
            .filter(|(name, _)| {
                !matches!(
                    name.to_lowercase().as_str(),
                    "content-length" | "content-type"
                )
            })
            .cloned()
            .collect();
        let mut new = Req::new(&newurl.replace(' ', "%20"), headers)?;
        if !visit(redirects, &newurl) {
            return Ok(None);
        }
        // The redirect's body is read before the next request.
        response.body.read(None)?;
        self.open(&mut new, redirects, follow).map(Some)
    }

    /// `AbstractHTTPHandler.do_open` with `HTTPConnection`, or for `tls`
    /// `HTTPSConnection`, up to `getresponse()`.
    fn do_open(&mut self, tls: bool, req: &Req) -> Result<Opened, Failure> {
        if req.host.is_empty() {
            return Err(url_error());
        }
        let default_port = if tls { 443 } else { 80 };
        let (host, port) = host_port(&req.host, default_port)?;
        if has_control(&host) {
            return Err(transport("InvalidURL"));
        }
        let mut merged = req.unredirected.clone();
        for (name, value) in &req.headers {
            set_header(&mut merged, name, value);
        }
        set_header(&mut merged, "Connection", "close");
        let mut headers = Vec::new();
        for (name, value) in merged {
            set_header(&mut headers, &title(&name), &value);
        }
        let tunnel = if req.tunnel_host.is_empty() {
            None
        } else {
            // Proxy-Authorization goes to the proxy, not the origin server.
            let mut tunnel_headers: Vec<_> = headers
                .iter()
                .position(|(name, _)| name == "Proxy-Authorization")
                .map(|i| headers.remove(i))
                .into_iter()
                .collect();
            let (host, port) = host_port(&req.tunnel_host, default_port)?;
            if !tunnel_headers
                .iter()
                .any(|(name, _)| name.to_lowercase() == "host")
            {
                tunnel_headers.push(("Host".into(), format!("{host}:{port}")));
            }
            Some(Tunnel {
                host,
                port,
                headers: tunnel_headers,
            })
        };
        let path = if req.selector.is_empty() {
            "/"
        } else {
            &req.selector
        };
        if has_control(path) {
            return Err(transport("InvalidURL"));
        }
        // h.request() builds the request before connect() opens the socket.
        let mut message = encode(&format!("GET {path} HTTP/1.1"), "ascii", 128)?;
        message.extend_from_slice(b"\r\n");
        put_header(&mut message, "Accept-Encoding", "identity")?;
        for (name, value) in &headers {
            put_header(&mut message, name, value)?;
        }
        message.extend_from_slice(b"\r\n");
        let mut stream = self.connect(tls, &host, port, tunnel.as_ref())?;
        stream
            .write_all(&message)
            .and_then(|()| stream.flush())
            .map_err(|_| url_error())?;
        let mut wire = BufReader::with_capacity(1 << 16, stream);
        let head = begin(&mut wire)?;
        Ok(Opened {
            status: head.status,
            headers: head.headers,
            url: req.full_url(),
            body: Body {
                wire,
                chunked: head.chunked,
                length: head.length,
            },
        })
    }

    /// `HTTPConnection.connect()`: TCP, the CONNECT tunnel and, for `tls`, the
    /// handshake. Each failure is the `URLError` urllib wraps an `OSError` in,
    /// but for the `http.client` exceptions reading the proxy's reply raises.
    fn connect(
        &mut self,
        tls: bool,
        host: &str,
        port: i64,
        tunnel: Option<&Tunnel>,
    ) -> Result<Stream, Failure> {
        let port = u16::try_from(port).map_err(|_| url_error())?;
        let addresses = (host, port).to_socket_addrs().map_err(|_| url_error())?;
        let mut socket = addresses
            .into_iter()
            .find_map(|address| TcpStream::connect_timeout(&address, TIMEOUT).ok())
            .ok_or_else(url_error)?;
        let _ = socket.set_nodelay(true);
        set_timeouts(&socket, TIMEOUT).map_err(|_| url_error())?;
        if let Some(tunnel) = tunnel {
            open_tunnel(&mut socket, tunnel).map_err(in_request)?;
        }
        if !tls {
            return Ok(Stream::Plain(socket));
        }
        let config = self.tls_config()?;
        let server_name = tunnel.map_or(host, |t| t.host.as_str());
        let name = rustls::pki_types::ServerName::try_from(server_name)
            .map_err(|_| url_error())?
            .to_owned();
        let mut session = rustls::ClientConnection::new(config, name).map_err(|_| url_error())?;
        // wrap_socket's handshake has one 90-second deadline: each blocking
        // read or write may take only what is left of it.
        let deadline = Instant::now() + TIMEOUT;
        while session.is_handshaking() || session.wants_write() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || set_timeouts(&socket, left).is_err() {
                return Err(url_error());
            }
            if session.wants_write() {
                session.write_tls(&mut socket).map_err(|_| url_error())?;
            } else if !session.wants_read()
                || session.read_tls(&mut socket).map_err(|_| url_error())? == 0
            {
                return Err(url_error());
            } else if session.process_new_packets().is_err() {
                // Send the alert rustls queued, as the handshake's failure does.
                let _ = session.write_tls(&mut socket);
                return Err(url_error());
            }
        }
        set_timeouts(&socket, TIMEOUT).map_err(|_| url_error())?;
        Ok(Stream::Tls(Box::new(rustls::StreamOwned::new(
            session, socket,
        ))))
    }

    fn tls_config(&mut self) -> Result<Arc<rustls::ClientConfig>, Failure> {
        use rustls_platform_verifier::BuilderVerifierExt;
        if let Some(config) = &self.tls {
            return Ok(config.clone());
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .and_then(BuilderVerifierExt::with_platform_verifier)
            .map_err(|_| url_error())?
            .with_no_client_auth();
        // _create_https_context offers HTTP/1.1 by ALPN.
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let config = Arc::new(config);
        self.tls = Some(config.clone());
        Ok(config)
    }
}

/// `getproxies_environment()`: `<scheme>_proxy` variables, any case, with a
/// lowercase name winning; `HTTP_PROXY` is ignored under CGI. Windows
/// environment names are upper case, as Python's `os.environ` gives them.
fn environment_proxies() -> HashMap<String, String> {
    let environ: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .map(|(name, value)| {
            if cfg!(windows) {
                (name.to_uppercase(), value)
            } else {
                (name, value)
            }
        })
        .collect();
    let mut proxies = HashMap::new();
    let mut matched = Vec::new();
    for (name, value) in &environ {
        let chars: Vec<char> = name.chars().collect();
        let n = chars.len();
        if n > 5
            && chars[n - 6] == '_'
            && chars[n - 5..].iter().collect::<String>().to_lowercase() == "proxy"
        {
            let proxy_name = chars[..n - 6].iter().collect::<String>().to_lowercase();
            if !value.is_empty() {
                proxies.insert(proxy_name.clone(), value.clone());
            }
            matched.push((name, value, proxy_name));
        }
    }
    if environ.iter().any(|(name, _)| name == "REQUEST_METHOD") {
        proxies.remove("http");
    }
    for (name, value, proxy_name) in matched {
        if name.ends_with("_proxy") {
            if value.is_empty() {
                proxies.remove(&proxy_name);
            } else {
                proxies.insert(proxy_name, value.clone());
            }
        }
    }
    proxies
}

type ProxyParts<'a> = (Option<String>, Option<&'a str>, Option<&'a str>, &'a str);

/// `_parse_proxy(proxy)`: scheme, user, password and host:port.
fn parse_proxy(proxy: &str) -> Result<ProxyParts<'_>, Failure> {
    let (scheme, r_scheme) = match split_type(proxy) {
        Some((scheme, rest)) => (Some(scheme), rest),
        None => (None, proxy),
    };
    let (scheme, authority) = if !r_scheme.starts_with('/') {
        (None, proxy)
    } else {
        if !r_scheme.starts_with("//") {
            return Err(value_error(format!(
                "proxy URL with no authority: {}",
                py::repr_str(proxy)
            )));
        }
        let end = match r_scheme.find('@') {
            Some(at) => r_scheme[at..].find('/').map(|i| at + i),
            None => r_scheme[2..].find('/').map(|i| i + 2),
        };
        (scheme, &r_scheme[2..end.unwrap_or(r_scheme.len())])
    };
    let (user, password, hostport) = match authority.rsplit_once('@') {
        Some((userinfo, hostport)) => match userinfo.split_once(':') {
            Some((user, password)) => (Some(user), Some(password), hostport),
            None => (Some(userinfo), None, hostport),
        },
        None => (None, None, authority),
    };
    Ok((scheme, user, password, hostport))
}

/// `HTTPConnection._tunnel()`: CONNECT, then the proxy's status line and
/// headers. Bytes after them in the read buffer are dropped with it, as
/// Python's closed reply drops them.
/// Python encodes the host as IDNA; this sends it as UTF-8, which is the same
/// for the ASCII hosts the stages request.
fn open_tunnel(socket: &mut TcpStream, tunnel: &Tunnel) -> Result<(), Failure> {
    if has_control(&tunnel.host) {
        return Err(value_error(format!(
            "Tunnel host can't contain control characters {}",
            py::repr_str(&tunnel.host)
        )));
    }
    let host = if tunnel.host.contains(':') && !tunnel.host.starts_with('[') {
        format!("[{}]", tunnel.host)
    } else {
        tunnel.host.clone()
    };
    let mut message = format!("CONNECT {host}:{} HTTP/1.1\r\n", tunnel.port).into_bytes();
    for (name, value) in &tunnel.headers {
        put_header(&mut message, name, value)?;
    }
    message.extend_from_slice(b"\r\n");
    socket.write_all(&message).map_err(|e| os_error(&e))?;
    let mut reply = BufReader::new(&mut *socket);
    let (_, code) = read_status(&mut reply)?;
    read_headers(&mut reply)?;
    if code != 200 {
        // "Tunnel connection failed"
        return Err(transport("OSError"));
    }
    Ok(())
}

/// An exception `h.request()` raised: urllib wraps each `OSError` in
/// `URLError`; the `http.client` exceptions pass through.
fn in_request(failure: Failure) -> Failure {
    match failure {
        Failure::Transport(name)
            if !matches!(
                name.as_str(),
                "BadStatusLine"
                    | "LineTooLong"
                    | "HTTPException"
                    | "UnknownProtocol"
                    | "IncompleteRead"
                    | "InvalidURL"
            ) =>
        {
            url_error()
        }
        other => other,
    }
}

fn set_timeouts(socket: &TcpStream, timeout: Duration) -> io::Result<()> {
    socket.set_read_timeout(Some(timeout))?;
    socket.set_write_timeout(Some(timeout))
}

// ---------------------------------------------------------------- the wire

/// The connection: TCP, or TLS over it. A TCP close without TLS's
/// close_notify ends the stream, as Python's `suppress_ragged_eofs` does.
enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(socket) => socket.read(buf),
            Stream::Tls(stream) => match stream.read(buf) {
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(0),
                read => read,
            },
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(socket) => socket.write(buf),
            Stream::Tls(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(socket) => socket.flush(),
            Stream::Tls(stream) => stream.flush(),
        }
    }
}

/// The exception a failed socket read raises, by class name.
fn os_error(error: &io::Error) -> Failure {
    use io::ErrorKind::*;
    transport(match error.kind() {
        ConnectionReset => "ConnectionResetError",
        ConnectionAborted => "ConnectionAbortedError",
        BrokenPipe => "BrokenPipeError",
        TimedOut | WouldBlock => "TimeoutError",
        // rustls reports a TLS failure after the handshake this way.
        InvalidData => "SSLError",
        _ => "OSError",
    })
}

/// `fp.readline(_MAXLINE + 1)`.
fn read_line<R: BufRead>(reader: &mut R) -> Result<Vec<u8>, Failure> {
    let mut line = Vec::new();
    reader
        .by_ref()
        .take(MAXLINE as u64 + 1)
        .read_until(b'\n', &mut line)
        .map_err(|e| os_error(&e))?;
    Ok(line)
}

/// `fp.read(n)`: `n` bytes or fewer at the end of the stream.
fn read_upto<R: BufRead>(reader: &mut R, n: u64) -> Result<Vec<u8>, Failure> {
    let mut data = Vec::new();
    reader
        .by_ref()
        .take(n)
        .read_to_end(&mut data)
        .map_err(|e| os_error(&e))?;
    Ok(data)
}

/// `_safe_read(amt)`: exactly `amt` bytes or `IncompleteRead`; a negative
/// `amt` reads to the end, as `fp.read(-n)` does.
fn safe_read<R: BufRead>(reader: &mut R, amt: i128) -> Result<Vec<u8>, Failure> {
    let data = read_upto(reader, u64::try_from(amt).unwrap_or(u64::MAX))?;
    if amt >= 0 && (data.len() as i128) < amt {
        return Err(transport("IncompleteRead"));
    }
    Ok(data)
}

/// `HTTPResponse._read_status()`: the version and status code.
fn read_status<R: BufRead>(reader: &mut R) -> Result<(String, u16), Failure> {
    let raw = read_line(reader)?;
    if raw.len() > MAXLINE {
        return Err(transport("LineTooLong"));
    }
    let line = latin1(&raw);
    if line.is_empty() {
        return Err(transport("RemoteDisconnected"));
    }
    let (version, status) = match split_ws(&line, 2).as_slice() {
        [version, status, _] => (version.clone(), status.clone()),
        _ => match split_ws(&line, 1).as_slice() {
            [version, status] => (version.clone(), status.clone()),
            _ => (String::new(), String::new()),
        },
    };
    if !version.starts_with("HTTP/") {
        return Err(transport("BadStatusLine"));
    }
    let status = py_int(&status)
        .and_then(|n| u16::try_from(&n).ok())
        .filter(|n| (100..=999).contains(n))
        .ok_or_else(|| transport("BadStatusLine"))?;
    Ok((version, status))
}

/// `_read_headers(fp)`: the raw lines up to the blank line or the end.
fn read_headers<R: BufRead>(reader: &mut R) -> Result<Vec<Vec<u8>>, Failure> {
    let mut lines = Vec::new();
    loop {
        let line = read_line(reader)?;
        if line.len() > MAXLINE {
            return Err(transport("LineTooLong"));
        }
        if matches!(line.as_slice(), b"\r\n" | b"\n" | b"") {
            return Ok(lines);
        }
        lines.push(line);
        if lines.len() > MAXHEADERS {
            return Err(transport("HTTPException"));
        }
    }
}

struct Head {
    status: u16,
    headers: Vec<(String, String)>,
    chunked: bool,
    length: Option<u64>,
}

/// `HTTPResponse.begin()`: skip `100 Continue`s, check the version, read the
/// headers and settle the body's framing.
fn begin<R: BufRead>(reader: &mut R) -> Result<Head, Failure> {
    let mut found = None;
    for _ in 0..MAXINTERIM {
        let (version, status) = read_status(reader)?;
        if status != 100 {
            found = Some((version, status));
            break;
        }
        read_headers(reader)?;
    }
    let (version, status) = found.ok_or_else(|| transport("HTTPException"))?;
    if !(version == "HTTP/1.0" || version == "HTTP/0.9" || version.starts_with("HTTP/1.")) {
        return Err(transport("UnknownProtocol"));
    }
    let headers = parse_header_lines(&read_headers(reader)?);
    let chunked =
        header(&headers, "transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked"));
    let mut length = match header(&headers, "content-length") {
        // int(length); an invalid or negative length is ignored.
        Some(text) if !text.is_empty() && !chunked => py_int(text)
            .filter(|n| n.sign() != Sign::Minus)
            .map(|n| u64::try_from(&n).unwrap_or(u64::MAX)),
        _ => None,
    };
    if matches!(status, 204 | 304) || (100..200).contains(&status) {
        length = Some(0);
    }
    Ok(Head {
        status,
        headers,
        chunked,
        length,
    })
}

/// `http.client.parse_headers`: the email package's compat32 parse of the
/// header lines. Lines break at `\r\n`, `\r` or `\n`; the first that is not a
/// field (`name:` with name characters 0x21-0x7E but `:`), a continuation or
/// a `From ` line ends the headers. `From ` lines and fields without a name
/// are dropped, as is a continuation with no field before it.
fn parse_header_lines(raw: &[Vec<u8>]) -> Vec<(String, String)> {
    let text = latin1(&raw.concat());
    let mut lines = Vec::new();
    let mut rest = text.as_str();
    while !rest.is_empty() {
        let end = match rest.find(['\r', '\n']) {
            Some(i) if rest[i..].starts_with("\r\n") => i + 2,
            Some(i) => i + 1,
            None => rest.len(),
        };
        lines.push(&rest[..end]);
        rest = &rest[end..];
    }
    let mut headers = Vec::new();
    let mut pending: Vec<&str> = Vec::new();
    for line in lines.into_iter().take_while(|line| is_field_line(line)) {
        if line.starts_with([' ', '\t']) {
            if !pending.is_empty() {
                pending.push(line);
            }
            continue;
        }
        if !pending.is_empty() {
            headers.push(source_parse(&pending));
            pending.clear();
        }
        if line.starts_with("From ") || line.starts_with(':') {
            continue;
        }
        pending.push(line);
    }
    if !pending.is_empty() {
        headers.push(source_parse(&pending));
    }
    headers
}

/// feedparser's `headerRE`.
fn is_field_line(line: &str) -> bool {
    if line.starts_with("From ") || line.starts_with([' ', '\t']) {
        return true;
    }
    let end = line
        .find(|c: char| !matches!(c, '\x21'..='\x39' | '\x3b'..='\x7e'))
        .unwrap_or(line.len());
    line[end..].starts_with(':')
}

/// compat32's `header_source_parse`: the name before the first `:` and the
/// rest, continuations included, less leading whitespace and line ends and
/// trailing line ends.
fn source_parse(lines: &[&str]) -> (String, String) {
    let (name, first) = lines[0].split_once(':').unwrap_or((lines[0], ""));
    let value = [first]
        .iter()
        .chain(&lines[1..])
        .copied()
        .collect::<String>();
    let value = value
        .trim_start_matches([' ', '\t', '\r', '\n'])
        .trim_end_matches(['\r', '\n']);
    (name.to_owned(), value.to_owned())
}

/// `Message.get(name)`: the first field of that name, in any case.
fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// An `HTTPResponse`'s unread body and its framing.
struct Body {
    wire: BufReader<Stream>,
    chunked: bool,
    length: Option<u64>,
}

impl Body {
    /// `HTTPResponse.read(amt)`. A bounded read of a body shorter than its
    /// Content-Length returns what arrived; an unbounded one is
    /// `IncompleteRead`.
    fn read(&mut self, amt: Option<usize>) -> Result<Vec<u8>, Failure> {
        if self.chunked {
            return self.read_chunked(amt);
        }
        match (amt, self.length) {
            (Some(amt), length) => {
                read_upto(&mut self.wire, length.unwrap_or(u64::MAX).min(amt as u64))
            }
            (None, None) => read_upto(&mut self.wire, u64::MAX),
            (None, Some(length)) => safe_read(&mut self.wire, i128::from(length)),
        }
    }

    /// `_read_chunked(amt)`: a size line that is not hex, a short chunk or a
    /// missing chunk end is `IncompleteRead`; the line and trailer limits
    /// raise as `http.client` raises them.
    fn read_chunked(&mut self, amt: Option<usize>) -> Result<Vec<u8>, Failure> {
        let mut amt = amt.map(|n| n as i128);
        let mut value = Vec::new();
        let mut first = true;
        loop {
            if !first {
                // The CRLF that ends a chunk.
                safe_read(&mut self.wire, 2)?;
            }
            first = false;
            let line = read_line(&mut self.wire)?;
            if line.len() > MAXLINE {
                return Err(transport("LineTooLong"));
            }
            let size = chunk_size(&line).ok_or_else(|| transport("IncompleteRead"))?;
            if size == 0 {
                self.discard_trailer()?;
                return Ok(value);
            }
            if let Some(n) = amt
                && n <= size
            {
                value.extend(safe_read(&mut self.wire, n)?);
                return Ok(value);
            }
            value.extend(safe_read(&mut self.wire, size)?);
            if let Some(n) = &mut amt {
                *n -= size;
            }
        }
    }

    /// `_read_and_discard_trailer()`.
    fn discard_trailer(&mut self) -> Result<(), Failure> {
        let mut count = 0;
        loop {
            let line = read_line(&mut self.wire)?;
            if line.len() > MAXLINE {
                return Err(transport("LineTooLong"));
            }
            if matches!(line.as_slice(), b"" | b"\r\n" | b"\n") {
                return Ok(());
            }
            count += 1;
            if count > MAXHEADERS {
                return Err(transport("HTTPException"));
            }
        }
    }
}

/// `int(line, 16)` of a chunk-size line without its extensions: `int()` of
/// bytes strips only ASCII whitespace and refuses other bytes.
fn chunk_size(line: &[u8]) -> Option<i128> {
    let digits = line.split(|&b| b == b';').next().unwrap_or_default();
    let text = std::str::from_utf8(digits).ok()?;
    if !text.is_ascii() || text.contains(['\x1c', '\x1d', '\x1e', '\x1f']) {
        return None;
    }
    let n = py::parse_int(text, 16).ok()?;
    let saturated = if n.sign() == Sign::Minus {
        i128::MIN
    } else {
        i128::MAX
    };
    Some(i128::try_from(&n).unwrap_or(saturated))
}

// ---------------------------------------------------------------- URL text

/// `str.isspace()` for the characters header text holds.
fn py_space(c: char) -> bool {
    matches!(c, '\t'..='\r' | '\x1c'..='\x1f' | ' ' | '\u{85}' | '\u{a0}')
        || (c > '\u{ff}' && c.is_whitespace())
}

fn py_strip(s: &str) -> &str {
    s.trim_matches(py_space)
}

/// `int(text)` of a str.
fn py_int(text: &str) -> Option<BigInt> {
    py::parse_int(py_strip(text), 10).ok()
}

/// `str.split(None, maxsplit)`.
fn split_ws(s: &str, maxsplit: usize) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut parts = Vec::new();
    let mut i = 0;
    while parts.len() < maxsplit {
        while i < chars.len() && py_space(chars[i]) {
            i += 1;
        }
        if i == chars.len() {
            break;
        }
        let start = i;
        while i < chars.len() && !py_space(chars[i]) {
            i += 1;
        }
        parts.push(chars[start..i].iter().collect());
    }
    while i < chars.len() && py_space(chars[i]) {
        i += 1;
    }
    if i < chars.len() {
        parts.push(chars[i..].iter().collect());
    }
    parts
}

/// `urllib.request.unwrap(url)`: `<URL:...>` and surrounding whitespace off.
fn unwrap(url: &str) -> &str {
    let mut url = py_strip(url);
    if url.len() >= 2 && url.starts_with('<') && url.ends_with('>') {
        url = py_strip(&url[1..url.len() - 1]);
    }
    if let Some(rest) = url.strip_prefix("URL:") {
        url = py_strip(rest);
    }
    url
}

/// `_splittype(url)`: the lowercased scheme and the rest.
fn split_type(url: &str) -> Option<(String, &str)> {
    let end = url.find([':', '/'])?;
    (end > 0 && url.as_bytes()[end] == b':').then(|| (url[..end].to_lowercase(), &url[end + 1..]))
}

/// `_splithost(url)` of `//host/path`: the host and the path, given a `/`.
fn split_host(url: &str) -> Option<(&str, String)> {
    let rest = url.strip_prefix("//")?;
    let end = rest.find(['/', '#', '?']).unwrap_or(rest.len());
    let (host, path) = rest.split_at(end);
    let path = if !path.is_empty() && !path.starts_with('/') {
        format!("/{path}")
    } else {
        path.to_owned()
    };
    Some((host, path))
}

/// `_splitport(host)`.
fn split_port(host: &str) -> (&str, Option<&str>) {
    match host.rfind(':') {
        Some(i) if host[i + 1..].bytes().all(|b| b.is_ascii_digit()) => {
            let port = &host[i + 1..];
            (&host[..i], (!port.is_empty()).then_some(port))
        }
        _ => (host, None),
    }
}

/// `HTTPConnection._get_hostport(host, None)`: a non-numeric port is
/// `InvalidURL`, an empty one the default.
fn host_port(host: &str, default_port: i64) -> Result<(String, i64), Failure> {
    let mut host = host;
    let mut port = default_port;
    if let Some(i) = host.rfind(':')
        && host.rfind(']').is_none_or(|j| i > j)
    {
        let text = &host[i + 1..];
        port = match py::parse_int(text, 10) {
            Ok(n) => i64::try_from(&n).unwrap_or(i64::MAX),
            Err(_) if text.is_empty() => default_port,
            Err(_) => return Err(transport("InvalidURL")),
        };
        host = &host[..i];
    }
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    Ok((host.to_owned(), port))
}

/// `http.client`'s check for characters a host or path may not hold.
fn has_control(text: &str) -> bool {
    text.chars().any(|c| c <= ' ' || c == '\x7f')
}

/// `urllib.parse.unquote(s)`: escapes in each ASCII run decode as UTF-8,
/// replacing invalid sequences.
fn unquote(s: &str) -> String {
    if !s.contains('%') {
        return s.to_owned();
    }
    let bytes = s.as_bytes();
    let mut out = String::new();
    let mut run = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if !b.is_ascii() {
            out.push_str(&String::from_utf8_lossy(&run));
            run.clear();
            let c = s[i..].chars().next().unwrap_or_default();
            out.push(c);
            i += c.len_utf8();
            continue;
        }
        let hex = |j: usize| bytes.get(j).copied().filter(u8::is_ascii_hexdigit);
        match (b, hex(i + 1), hex(i + 2)) {
            (b'%', Some(high), Some(low)) => {
                let digit = |d: u8| (d as char).to_digit(16).unwrap_or(0) as u8;
                run.push(digit(high) * 16 + digit(low));
                i += 3;
            }
            _ => {
                run.push(b);
                i += 1;
            }
        }
    }
    out.push_str(&String::from_utf8_lossy(&run));
    out
}

/// `str.capitalize()`.
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map_or_else(String::new, |first| {
        first
            .to_uppercase()
            .chain(chars.flat_map(char::to_lowercase))
            .collect()
    })
}

/// `str.title()`.
fn title(s: &str) -> String {
    let mut out = String::new();
    let mut in_word = false;
    for c in s.chars() {
        if c.is_alphabetic() {
            if in_word {
                out.extend(c.to_lowercase());
            } else {
                out.extend(c.to_uppercase());
            }
            in_word = true;
        } else {
            out.push(c);
            in_word = false;
        }
    }
    out
}

/// A dict's `d[name] = value`: replaced in place, or added at the end.
fn set_header(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    match headers.iter_mut().find(|(key, _)| key == name) {
        Some(entry) => entry.1 = value.to_owned(),
        None => headers.push((name.to_owned(), value.to_owned())),
    }
}

/// `HTTPRedirectHandler.http_error_302`'s target for a `Location` (or `URI`)
/// header on a response to `url`: None for a scheme it refuses to follow. A
/// malformed URL is the `ValueError` urllib raises and nothing catches.
fn redirect_target(url: &str, location: &str) -> Result<Option<String>, Failure> {
    let mut parts = crate::pyurl::urlsplit(location).map_err(value_error)?;
    if !matches!(parts.scheme.as_str(), "http" | "https" | "ftp" | "") {
        return Ok(None);
    }
    // urlparse splits `;params` off the path; urlunparse drops an empty one.
    let (path, params) = if parts.path.contains(';') {
        let at = match parts.path.rfind('/') {
            Some(slash) => parts.path[slash..].find(';').map(|i| slash + i),
            None => parts.path.find(';'),
        };
        at.map_or((parts.path.as_str(), ""), |i| {
            (&parts.path[..i], &parts.path[i + 1..])
        })
    } else {
        (parts.path.as_str(), "")
    };
    let path = if path.is_empty() && !parts.netloc.is_empty() {
        "/"
    } else {
        path
    };
    parts.path = if params.is_empty() {
        path.to_owned()
    } else {
        format!("{path};{params}")
    };
    // quote(newurl, encoding="iso-8859-1", safe=string.punctuation)
    let mut quoted = String::new();
    for c in crate::pyurl::urlunsplit(&parts).chars() {
        if c.is_ascii_graphic() {
            quoted.push(c);
        } else {
            quoted.push_str(&format!("%{:02X}", u32::from(c)));
        }
    }
    crate::pyurl::urljoin(url, &quoted)
        .map(Some)
        .map_err(value_error)
}

/// A header value as `http.client` decodes it: ISO-8859-1.
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

/// `seconds` since the Unix epoch as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn utc_text(seconds: u64) -> String {
    let days = i64::try_from(seconds / 86_400).unwrap_or(0);
    let rest = seconds % 86_400;
    // 719163 is the proleptic Gregorian ordinal of 1970-01-01.
    let date = crate::py::Date::from_ordinal(719_163 + days)
        .map_or_else(|| "9999-12-31".to_string(), |d| d.isoformat());
    format!(
        "{date}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

fn transport(name: &str) -> Failure {
    Failure::Transport(name.to_string())
}

fn url_error() -> Failure {
    transport("URLError")
}

fn value_error(message: String) -> Failure {
    Failure::Uncaught {
        class: "ValueError",
        message,
    }
}

/// `text.encode(codec)` for `ascii` (`limit` 128) or `latin-1` (256): the
/// bytes, or the `UnicodeEncodeError` the strict codec raises for the first
/// run of characters at or above `limit`.
fn encode(text: &str, codec: &str, limit: u32) -> Result<Vec<u8>, Failure> {
    let chars: Vec<char> = text.chars().collect();
    let Some(start) = chars.iter().position(|&c| u32::from(c) >= limit) else {
        return Ok(chars.iter().map(|&c| c as u8).collect());
    };
    let end = chars[start..]
        .iter()
        .position(|&c| u32::from(c) < limit)
        .map_or(chars.len(), |n| start + n);
    let reason = format!("ordinal not in range({limit})");
    let message = if end - start == 1 {
        let c = u32::from(chars[start]);
        let escape = match c {
            0..=0xff => format!("\\x{c:02x}"),
            0x100..=0xffff => format!("\\u{c:04x}"),
            _ => format!("\\U{c:08x}"),
        };
        format!("'{codec}' codec can't encode character '{escape}' in position {start}: {reason}")
    } else {
        format!(
            "'{codec}' codec can't encode characters in position {start}-{}: {reason}",
            end - 1
        )
    };
    Err(Failure::Uncaught {
        class: "UnicodeEncodeError",
        message,
    })
}

/// `http.client._is_illegal_header_value`: a line break not followed by
/// folding whitespace.
fn illegal_header_value(value: &[u8]) -> bool {
    value.iter().enumerate().any(|(i, &b)| {
        let next = value.get(i + 1).copied();
        (b == b'\n' && !matches!(next, Some(b' ' | b'\t')))
            || (b == b'\r' && !matches!(next, Some(b' ' | b'\t' | b'\n')))
    })
}

/// `HTTPConnection.putheader(name, value)`, appended to `out`: the value as
/// Latin-1, refused where it could end the header early.
fn put_header(out: &mut Vec<u8>, name: &str, value: &str) -> Result<(), Failure> {
    let value = encode(value, "latin-1", 256)?;
    if illegal_header_value(&value) {
        return Err(value_error(format!(
            "Invalid header value {}",
            py::repr_bytes(&value)
        )));
    }
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(b": ");
    out.extend_from_slice(&value);
    out.extend_from_slice(b"\r\n");
    Ok(())
}

impl Http for Net {
    fn get(
        &mut self,
        url: &str,
        limit: Option<usize>,
        request: &mut Request,
    ) -> Result<Response, Failure> {
        let mut opened = match self.open_request(url, request, true) {
            Ok(opened) => opened,
            Err(Raised::Status(response)) => return Err(Failure::Status(response.status)),
            Err(Raised::Failure(failure)) => return Err(failure),
        };
        let content_type = header(&opened.headers, "content-type")
            .unwrap_or_default()
            .to_owned();
        let body = opened.body.read(limit)?;
        // geturl(): the last request's URL, fragment included.
        Ok(Response {
            url: opened.url,
            content_type,
            body,
        })
    }

    fn exchange(
        &mut self,
        url: &str,
        limit: usize,
        request: &mut Request,
    ) -> Result<Exchange, Failure> {
        match self.open_request(url, request, false) {
            Ok(mut opened) => {
                let body = if opened.status == 200 {
                    Some(opened.body.read(Some(limit))?)
                } else {
                    None
                };
                Ok(Exchange {
                    status: opened.status,
                    headers: opened.headers,
                    body,
                })
            }
            Err(Raised::Status(response)) => Ok(Exchange {
                status: response.status,
                headers: response.headers,
                body: None,
            }),
            Err(Raised::Failure(failure)) => Err(failure),
        }
    }

    fn sleep(&mut self, seconds: f64) {
        std::thread::sleep(Duration::from_secs_f64(seconds));
    }

    fn today(&mut self) -> String {
        local_today()
    }

    fn monotonic(&mut self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    fn utc_now(&mut self) -> String {
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        utc_text(seconds)
    }
}

/// Today's date in the local time zone, as `YYYY-MM-DD`.
#[cfg(windows)]
pub fn local_today() -> String {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    let mut now = SYSTEMTIME {
        wYear: 0,
        wMonth: 0,
        wDayOfWeek: 0,
        wDay: 0,
        wHour: 0,
        wMinute: 0,
        wSecond: 0,
        wMilliseconds: 0,
    };
    // SAFETY: GetLocalTime writes the whole structure it is given.
    unsafe { GetLocalTime(&mut now) };
    format!("{:04}-{:02}-{:02}", now.wYear, now.wMonth, now.wDay)
}

/// Today's date in the local time zone, as `YYYY-MM-DD`.
#[cfg(unix)]
pub fn local_today() -> String {
    // SAFETY: time(NULL) only returns the clock; localtime_r fills `now`.
    unsafe {
        let clock = libc::time(std::ptr::null_mut());
        let mut now: libc::tm = std::mem::zeroed();
        libc::localtime_r(&clock, &mut now);
        format!(
            "{:04}-{:02}-{:02}",
            now.tm_year + 1900,
            now.tm_mon + 1,
            now.tm_mday
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(raw: &[&[u8]]) -> Vec<(String, String)> {
        parse_header_lines(&raw.iter().map(|l| l.to_vec()).collect::<Vec<_>>())
    }

    #[test]
    fn header_lines_parse_as_the_email_package_does() {
        let folded = lines(&[b"Content-Type: text/plain\r\n", b"  more\r\n", b"X: 1\r\n"]);
        assert_eq!(folded[0].1, "text/plain\r\n  more");
        assert_eq!(folded[1], ("X".into(), "1".into()));
        // A line that is not a field ends the headers.
        assert_eq!(lines(&[b"NoColon\r\n", b"Content-Length: 2\r\n"]), []);
        assert_eq!(lines(&[b"Content-Length : 2\r\n"]), []);
        // A bare CR breaks a line.
        assert_eq!(
            lines(&[b"A: 1\rB: 2\r\n"]),
            [("A".into(), "1".into()), ("B".into(), "2".into())]
        );
        assert_eq!(
            lines(&[b"From x\r\n", b":v\r\n", b" c\r\n", b"Y:  2 \r\n"]),
            [("Y".into(), "2 ".into())]
        );
    }

    #[test]
    fn chunk_sizes_parse_as_int_with_base_16() {
        assert_eq!(chunk_size(b"1a;ext\r\n"), Some(26));
        assert_eq!(chunk_size(b" 0x1_0 \r\n"), Some(16));
        assert_eq!(chunk_size(b"-3\r\n"), Some(-3));
        assert_eq!(chunk_size(b"zz\r\n"), None);
        assert_eq!(chunk_size(b"3\x1c\r\n"), None);
    }

    #[test]
    fn requests_split_and_unquote_as_urllib_does() {
        let req = Req::new(" <URL:HTTP://u%40h:8%30/p#f> ", Vec::new()).unwrap();
        assert_eq!(
            (req.kind.as_str(), req.host.as_str(), req.selector.as_str()),
            ("http", "u@h:80", "/p")
        );
        assert_eq!(req.full_url(), "HTTP://u%40h:8%30/p#f");
        assert!(matches!(
            Req::new("nothing", Vec::new()),
            Err(Failure::Uncaught { .. })
        ));
        assert_eq!(
            parse_proxy("http://us:pw@p:3128/x").unwrap(),
            (Some("http".into()), Some("us"), Some("pw"), "p:3128")
        );
        assert_eq!(parse_proxy("p:3128").unwrap(), (None, None, None, "p:3128"));
        assert!(parse_proxy("http:/p").is_err());
        assert_eq!(host_port("[::1]:8080", 80).unwrap(), ("::1".into(), 8080));
        assert_eq!(host_port("h:", 443).unwrap(), ("h".into(), 443));
        assert!(host_port("h:x", 80).is_err());
        assert_eq!(split_ws("HTTP/1.1 200\r\n", 1), ["HTTP/1.1", "200\r\n"]);
        assert_eq!(split_ws("  a b  c d ", 2), ["a", "b", "c d "]);
        assert_eq!(unquote("a%20%e9%C3%A9é%zz"), "a \u{fffd}éé%zz");
        assert_eq!(title("proxy-authorization"), "Proxy-Authorization");
    }

    fn refusal(result: Result<impl std::fmt::Debug, Failure>) -> (&'static str, String) {
        match result {
            Err(Failure::Uncaught { class, message }) => (class, message),
            other => panic!("not refused: {other:?}"),
        }
    }

    #[test]
    fn requests_encode_and_refuse_as_http_client_does() {
        // The messages are CPython 3.15's.
        assert_eq!(encode("café", "latin-1", 256).unwrap(), b"caf\xe9");
        let unicode = |text, codec, limit| refusal(encode(text, codec, limit)).1;
        assert_eq!(
            unicode("a€b", "latin-1", 256),
            r"'latin-1' codec can't encode character '\u20ac' in position 1: ordinal not in range(256)"
        );
        assert_eq!(
            unicode("a€€b", "latin-1", 256),
            "'latin-1' codec can't encode characters in position 1-2: ordinal not in range(256)"
        );
        assert_eq!(
            unicode("x\u{1f600}", "latin-1", 256),
            r"'latin-1' codec can't encode character '\U0001f600' in position 1: ordinal not in range(256)"
        );
        assert_eq!(
            unicode("GET /café HTTP/1.1", "ascii", 128),
            r"'ascii' codec can't encode character '\xe9' in position 8: ordinal not in range(128)"
        );
        let mut out = Vec::new();
        for value in ["a\r\n b", "a\n\tb", "é"] {
            put_header(&mut out, "X", value).unwrap();
        }
        assert_eq!(out, b"X: a\r\n b\r\nX: a\n\tb\r\nX: \xe9\r\n");
        for (value, repr) in [
            ("a\nb", r"b'a\nb'"),
            ("a\rb", r"b'a\rb'"),
            ("a\r", r"b'a\r'"),
        ] {
            assert_eq!(
                refusal(put_header(&mut out, "X", value)),
                ("ValueError", format!("Invalid header value {repr}"))
            );
        }
        assert_eq!(
            refusal(put_header(&mut out, "X", "€")).0,
            "UnicodeEncodeError"
        );
    }
}

#[cfg(feature = "fake-http")]
mod fake {
    use std::io::Write;

    use super::{Exchange, Failure, Http, Request, Response};
    use crate::pyjson::{self, Json};

    /// A scripted run: `{"today": "...", "now": "...", "responses": [...]}`,
    /// each response `{"status": n, "url": "...", "type": "...", "headers":
    /// [[name, value], ...], "body": "...", "read_error": true}` (body bytes
    /// as code points below 256) or `{"error": "<exception class>"}`. The
    /// clock stands still: `monotonic` is 0 and `utc_now` is `now`.
    pub struct Script {
        today: String,
        now: String,
        responses: std::vec::IntoIter<Json>,
        log: Option<String>,
    }

    impl Script {
        pub fn load(path: &str) -> Script {
            let text = std::fs::read_to_string(path).expect("FADDEN_FAKE_HTTP script");
            let script = pyjson::loads_standard(&text).expect("FADDEN_FAKE_HTTP is JSON");
            let today = script
                .get("today")
                .and_then(Json::as_str)
                .unwrap_or("2099-01-01");
            let responses = match script.get("responses") {
                Some(Json::Array(items)) => items.clone(),
                _ => Vec::new(),
            };
            let log = std::env::var("FADDEN_FAKE_HTTP_LOG").ok();
            let now = script
                .get("now")
                .and_then(Json::as_str)
                .unwrap_or("2099-01-01T00:00:00Z");
            Script {
                today: today.to_string(),
                now: now.to_string(),
                responses: responses.into_iter(),
                log,
            }
        }

        /// Append one line, creating the log at the first, as the harness's
        /// Python side does.
        fn log(&mut self, line: &str) {
            if let Some(path) = &self.log {
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .expect("FADDEN_FAKE_HTTP_LOG");
                let _ = writeln!(file, "{line}");
            }
        }
    }

    fn bytes(value: Option<&Json>) -> Vec<u8> {
        value
            .and_then(|v| match v {
                Json::Str(s) => Some(s.code_points().into_iter().map(|cp| cp as u8).collect()),
                _ => None,
            })
            .unwrap_or_default()
    }

    impl Http for Script {
        fn get(
            &mut self,
            url: &str,
            limit: Option<usize>,
            _request: &mut Request,
        ) -> Result<Response, Failure> {
            self.log(&format!("GET {url}"));
            let Some(next) = self.responses.next() else {
                return Err(Failure::Transport("URLError".into()));
            };
            if let Some(name) = next.get("error").and_then(Json::as_str) {
                return Err(Failure::Transport(name.to_string()));
            }
            let status = next.get("status").and_then(|s| match s {
                Json::Int(i) => u16::try_from(i).ok(),
                _ => None,
            });
            let status = status.unwrap_or(200);
            if !(200..300).contains(&status) {
                return Err(Failure::Status(status));
            }
            let mut body = bytes(next.get("body"));
            if let Some(n) = limit {
                body.truncate(n);
            }
            Ok(Response {
                url: next
                    .get("url")
                    .and_then(Json::as_str)
                    .unwrap_or(url)
                    .to_string(),
                content_type: next
                    .get("type")
                    .and_then(Json::as_str)
                    .unwrap_or("")
                    .to_string(),
                body,
            })
        }

        fn exchange(
            &mut self,
            url: &str,
            limit: usize,
            _request: &mut Request,
        ) -> Result<Exchange, Failure> {
            self.log(&format!("GET {url}"));
            let Some(next) = self.responses.next() else {
                return Err(Failure::Transport("URLError".into()));
            };
            if let Some(name) = next.get("error").and_then(Json::as_str) {
                return Err(Failure::Transport(name.to_string()));
            }
            let status = match next.get("status") {
                Some(Json::Int(i)) => u16::try_from(i).unwrap_or(200),
                _ => 200,
            };
            let text = |value: &Json| {
                let points = match value {
                    Json::Str(s) => s.code_points(),
                    _ => Vec::new(),
                };
                points
                    .into_iter()
                    .filter_map(char::from_u32)
                    .collect::<String>()
            };
            let headers = match next.get("headers") {
                Some(Json::Array(pairs)) => pairs
                    .iter()
                    .filter_map(|pair| match pair {
                        Json::Array(kv) if kv.len() == 2 => Some((text(&kv[0]), text(&kv[1]))),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            if status == 200 && next.get("read_error").is_some_and(Json::truthy) {
                return Err(Failure::Transport("IncompleteRead".into()));
            }
            let body = (status == 200).then(|| {
                let mut body = bytes(next.get("body"));
                body.truncate(limit);
                body
            });
            Ok(Exchange {
                status,
                headers,
                body,
            })
        }

        fn sleep(&mut self, seconds: f64) {
            self.log(&format!("sleep {}", crate::py::float_repr(seconds)));
        }

        fn today(&mut self) -> String {
            self.today.clone()
        }

        fn monotonic(&mut self) -> f64 {
            0.0
        }

        fn utc_now(&mut self) -> String {
            self.now.clone()
        }
    }
}
