//! HTTP for the network stages: `urllib.request.urlopen(Request(url,
//! headers={"User-Agent": UA}), timeout=90)` as `fadden/http_fetch.py` and the
//! rulings stage call it, the shared attempt budget, and the local date.
//!
//! A GET follows up to 10 redirects, uses any proxy the environment names and
//! verifies TLS against the operating system's certificate store, as urllib
//! does. A status outside 200-299 is urllib's `HTTPError`; any other failure
//! carries the name of the exception urllib would most likely raise there.
//! The 90-second limit applies to each phase of the request (connecting, the
//! response head, the body) rather than to each socket read.
//!
//! With the `fake-http` feature and `FADDEN_FAKE_HTTP` naming a script, the
//! responses, the waits and the date come from that script instead, and each
//! request and wait is logged to `FADDEN_FAKE_HTTP_LOG`. The differential
//! harness drives the base-revision Python stages through the same script.
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

pub trait Http {
    /// One GET, reading at most `limit` bytes of the body when given.
    fn get(&mut self, url: &str, limit: Option<usize>) -> Result<Response, Failure>;
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
    for number in 1..=TRIES {
        attempt(http, number);
        let Ok(response) = http.get(url, None) else {
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
    agent: ureq::Agent,
    /// The capture recorder's agent: no redirect is followed.
    direct: ureq::Agent,
    started: std::time::Instant,
}

impl Net {
    fn new() -> Net {
        let agent = |redirects: u32| {
            let tls = ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build();
            ureq::Agent::config_builder()
                .tls_config(tls)
                .user_agent(UA)
                .http_status_as_error(false)
                .max_redirects(redirects)
                .max_redirects_will_error(false)
                .timeout_connect(Some(TIMEOUT))
                .timeout_recv_response(Some(TIMEOUT))
                .timeout_recv_body(Some(TIMEOUT))
                .build()
                .new_agent()
        };
        Net {
            agent: agent(10),
            direct: agent(0),
            started: std::time::Instant::now(),
        }
    }
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
    fn get(&mut self, url: &str, limit: Option<usize>) -> Result<Response, Failure> {
        use ureq::ResponseExt;
        let mut response = self.agent.get(url).call().map_err(|error| match error {
            ureq::Error::Timeout(ureq::Timeout::RecvResponse) => transport("TimeoutError"),
            _ => transport("URLError"),
        })?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(Failure::Status(status));
        }
        let final_url = response.get_uri().to_string();
        // http.client decodes header values as ISO-8859-1.
        let content_type = response
            .headers()
            .get("content-type")
            .map(|v| v.as_bytes().iter().map(|&b| char::from(b)).collect())
            .unwrap_or_default();
        let mut body = Vec::new();
        let reader = response.body_mut().with_config().limit(u64::MAX).reader();
        let read = match limit {
            Some(n) => reader.take(n as u64).read_to_end(&mut body),
            None => reader.take(u64::MAX).read_to_end(&mut body),
        };
        read.map_err(|error| match error.kind() {
            std::io::ErrorKind::TimedOut => transport("TimeoutError"),
            _ => transport("IncompleteRead"),
        })?;
        Ok(Response {
            url: final_url,
            content_type,
            body,
        })
    }

    fn exchange(&mut self, url: &str, limit: usize) -> Result<Exchange, Failure> {
        let mut response = self
            .direct
            .get(url)
            .call()
            .map_err(|_| transport("URLError"))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| (name.as_str().to_string(), latin1(value.as_bytes())))
            .collect();
        let body = if status == 200 {
            let mut body = Vec::new();
            response
                .body_mut()
                .with_config()
                .limit(u64::MAX)
                .reader()
                .take(limit as u64)
                .read_to_end(&mut body)
                .map_err(|_| transport("IncompleteRead"))?;
            Some(body)
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

    use super::{Exchange, Failure, Http, Response};
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
        fn get(&mut self, url: &str, limit: Option<usize>) -> Result<Response, Failure> {
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
