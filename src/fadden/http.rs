//! HTTP for the network stages: `urllib.request.urlopen(Request(url,
//! headers={"User-Agent": UA}), timeout=90)` as `fadden/http_fetch.py` and the
//! rulings stage call it, the shared attempt budget, and the local date.
//!
//! A GET sends urllib's headers (`Accept-Encoding: identity`, `User-Agent`,
//! `Connection: close`, no `Accept`), follows redirects by
//! `HTTPRedirectHandler`'s rules, uses any proxy the environment names and
//! verifies TLS against the operating system's certificate store, as urllib
//! does. A status outside 200-299 is urllib's `HTTPError`; any other failure
//! carries the name of the exception urllib would most likely raise there.
//! The 90-second limit applies to each socket read and write, as urllib's
//! socket timeout does; connecting (TCP and TLS) has one 90-second budget,
//! where urllib allows 90 seconds per address and per handshake operation.
//!
//! With the `fake-http` feature and `FADDEN_FAKE_HTTP` naming a script, the
//! responses, the waits and the date come from that script instead, and each
//! request and wait is logged to `FADDEN_FAKE_HTTP_LOG`. The differential
//! harness drives the base-revision Python stages through the same script.
use std::collections::HashMap;
use std::io::Read;
use std::time::Duration;

use super::Exc;
use crate::pyjson::{self, DecodeError, Json};

pub const UA: &str =
    "au-tax-legislation-corpus (+https://github.com/ryanduguid/au-tax-legislation-corpus)";
const TIMEOUT: Duration = Duration::from_secs(90);
/// One attempt budget for every Register caller.
pub const TRIES: u32 = 3;
pub const RETRY_DELAY: f64 = 6.0;

/// A 2xx response: the final URL after redirects, its Content-Type and body.
#[derive(Debug, Clone)]
pub struct Response {
    pub url: String,
    pub content_type: String,
    pub body: Vec<u8>,
}

/// Why `urlopen` raised: an HTTP status, or the class name of another failure.
#[derive(Debug, Clone, PartialEq)]
pub enum Failure {
    Status(u16),
    Transport(String),
}

/// One response as the capture recorder keeps it: any status, every header
/// in the order received, and the body of a 200 response only.
#[derive(Debug, Clone)]
pub struct Exchange {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

/// The redirects one `urllib.request.Request` has followed, its
/// `redirect_dict`. The callers reuse one request for every attempt, so the
/// counts carry from one attempt to the next.
#[derive(Debug, Default)]
pub struct Redirects(Option<HashMap<String, u32>>);

impl Redirects {
    /// `http_error_302`'s loop check before following a redirect to `url`:
    /// false once `url` has been visited 4 times or 10 URLs have been. The
    /// request's first redirect is never refused.
    pub fn follow(&mut self, url: &str) -> bool {
        let visited = match &mut self.0 {
            None => self.0.insert(HashMap::new()),
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
}

pub trait Http {
    /// One GET, reading at most `limit` bytes of the body when given;
    /// `redirects` is the request's redirect record.
    fn get(
        &mut self,
        url: &str,
        limit: Option<usize>,
        redirects: &mut Redirects,
    ) -> Result<Response, Failure>;
    /// One GET that follows no redirect and reports every status, reading at
    /// most `limit` bytes of a 200 body; an `Err` is always `Transport`. A
    /// fake that scripts only `get` fails every exchange.
    fn exchange(&mut self, url: &str, limit: usize) -> Result<Exchange, Failure> {
        let _ = (url, limit);
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
/// a non-object and an object with an `"error"` member are all retried; only
/// the int digit limit's `ValueError`, which urllib's callers do not catch,
/// stops the stage.
pub fn fetch_json(http: &mut dyn Http, url: &str) -> Result<Option<Json>, Exc> {
    let mut redirects = Redirects::default();
    for number in 1..=TRIES {
        attempt(http, number);
        let Ok(response) = http.get(url, None, &mut redirects) else {
            continue;
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

struct Net {
    /// Follows no redirect itself: `get` follows them as urllib does.
    agent: ureq::Agent,
    started: std::time::Instant,
}

impl Net {
    fn new() -> Net {
        let tls = ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        let agent = ureq::Agent::config_builder()
            .tls_config(tls)
            .user_agent(UA)
            .accept(ureq::config::AutoHeaderValue::None)
            .accept_encoding("identity")
            .http_status_as_error(false)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .timeout_connect(Some(TIMEOUT))
            .timeout_per_read(Some(TIMEOUT))
            .timeout_per_write(Some(TIMEOUT))
            .build()
            .new_agent();
        Net {
            agent,
            started: std::time::Instant::now(),
        }
    }

    /// `opener.open(Request(url))` up to the response head. A failure while
    /// connecting or sending is urllib's `URLError`; one reading the response
    /// head is the exception `http.client` raises there. A Content-Length
    /// that is not a number fails here as `BadStatusLine`, where `http.client`
    /// reads the body to the close.
    fn call(&self, url: &str) -> Result<ureq::http::Response<ureq::Body>, Failure> {
        // Request sends the URL without its fragment, which follows the last `#`.
        let sent = url.rsplit_once('#').map_or(url, |(sent, _)| sent);
        self.agent
            .get(sent)
            .header("Connection", "close")
            .call()
            .map_err(|error| match error {
                ureq::Error::Timeout(ureq::Timeout::RecvResponse | ureq::Timeout::PerRead) => {
                    transport("TimeoutError")
                }
                ureq::Error::Io(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    transport("RemoteDisconnected")
                }
                ureq::Error::Protocol(_) => transport("BadStatusLine"),
                ureq::Error::LargeResponseHeader(..) => transport("LineTooLong"),
                _ => transport("URLError"),
            })
    }
}

/// Up to `limit` bytes of a response body; a failed read is the exception
/// `http.client` raises.
fn read_body(
    response: &mut ureq::http::Response<ureq::Body>,
    limit: u64,
) -> Result<Vec<u8>, Failure> {
    let mut body = Vec::new();
    response
        .body_mut()
        .with_config()
        .limit(u64::MAX)
        .reader()
        .take(limit)
        .read_to_end(&mut body)
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::TimedOut => transport("TimeoutError"),
            std::io::ErrorKind::ConnectionReset => transport("ConnectionResetError"),
            _ => transport("IncompleteRead"),
        })?;
    Ok(body)
}

/// `HTTPRedirectHandler.http_error_302`'s target for a `Location` (or `URI`)
/// header on a response to `url`: None for a scheme it refuses to follow.
fn redirect_target(url: &str, location: &str) -> Result<Option<String>, Failure> {
    // shortcut: urllib's ValueError for a malformed URL is uncaught and stops
    // the stage; here it fails the attempt like a transport error.
    let malformed = |_| transport("ValueError");
    let mut parts = crate::pyurl::urlsplit(location).map_err(malformed)?;
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
        .map_err(malformed)
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

impl Http for Net {
    fn get(
        &mut self,
        url: &str,
        limit: Option<usize>,
        redirects: &mut Redirects,
    ) -> Result<Response, Failure> {
        let mut url = url.to_owned();
        loop {
            let mut response = self.call(&url)?;
            let status = response.status().as_u16();
            if (200..300).contains(&status) {
                let content_type = response
                    .headers()
                    .get("content-type")
                    .map(|v| latin1(v.as_bytes()))
                    .unwrap_or_default();
                let body = read_body(&mut response, limit.map_or(u64::MAX, |n| n as u64))?;
                // geturl(): the last request's URL, fragment included.
                return Ok(Response {
                    url,
                    content_type,
                    body,
                });
            }
            if !matches!(status, 301 | 302 | 303 | 307 | 308) {
                return Err(Failure::Status(status));
            }
            let header = |name| response.headers().get(name).map(|v| latin1(v.as_bytes()));
            let Some(location) = header("location").or_else(|| header("uri")) else {
                return Err(Failure::Status(status));
            };
            match redirect_target(&url, &location)? {
                Some(next) if redirects.follow(&next) => {
                    // The redirect's body is read before the next request.
                    read_body(&mut response, u64::MAX)?;
                    url = next;
                }
                _ => return Err(Failure::Status(status)),
            }
        }
    }

    fn exchange(&mut self, url: &str, limit: usize) -> Result<Exchange, Failure> {
        let mut response = self.call(url)?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| (name.as_str().to_string(), latin1(value.as_bytes())))
            .collect();
        let body = if status == 200 {
            Some(read_body(&mut response, limit as u64)?)
        } else {
            None
        };
        Ok(Exchange {
            status,
            headers,
            body,
        })
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

#[cfg(feature = "fake-http")]
mod fake {
    use std::io::Write;

    use super::{Exchange, Failure, Http, Redirects, Response};
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
            _redirects: &mut Redirects,
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

        fn exchange(&mut self, url: &str, limit: usize) -> Result<Exchange, Failure> {
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
