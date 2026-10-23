//! The corpus builder stages ported from the Python `fadden` package.
//!
//! Each stage keeps its Python command line: the program name, arguments,
//! messages and exit statuses of `python -m fadden <stage>`, which forwards a
//! ported stage to this binary. A Python exception a stage did not catch
//! printed a traceback and exited 1; here it prints the traceback's last line,
//! `<Exception>: <message>`, and exits 1.
pub mod as_at;
pub mod capture_register;
pub mod cli;
pub mod compare_builds;
pub mod corpus_paths;
pub mod discover;
pub mod dist;
pub mod dist_verify;
pub mod epub;
pub mod http;
pub mod iso;
pub mod live_evidence;
pub mod monitor_contract;
pub mod pii;
pub mod pii_scan;
pub mod probe13;
pub mod publication_bundles;
pub mod rates;
pub mod rulings;
pub mod search;
pub mod versions;

use crate::pyjson::Json;
use crate::pyos::OsError;
use crate::pyvalue::type_name;

/// A Python exception a stage raises.
#[derive(Debug)]
pub enum Exc {
    /// `ValueError` or a subclass, named by `class`.
    Value {
        class: &'static str,
        message: String,
    },
    Os(OsError),
    Type(String),
    Attribute(String),
    Key(String),
    Runtime(String),
    UnicodeEncode(String),
    Overflow(String),
    /// `sqlite3.Error`, which the stages catch.
    Sqlite(String),
    /// `RecursionError`, a `RuntimeError` no stage catches.
    Recursion(String),
    /// `shutil.Error` or `shutil.SpecialFileError`, named by `class`.
    Shutil {
        class: &'static str,
        message: String,
    },
    /// Any other exception, named by `class`, that callers catch only as
    /// `Exception` (`DownloadError`, `zipfile.BadZipFile`).
    Other {
        class: &'static str,
        message: String,
    },
}

impl Exc {
    pub fn value(message: impl Into<String>) -> Exc {
        Exc::Value {
            class: "ValueError",
            message: message.into(),
        }
    }

    /// The exception's class as a traceback's last line names it.
    pub fn class(&self) -> &str {
        match self {
            Exc::Value { class, .. } => class,
            Exc::Os(error) => error.class(),
            Exc::Type(_) => "TypeError",
            Exc::Attribute(_) => "AttributeError",
            Exc::Key(_) => "KeyError",
            Exc::Runtime(_) => "RuntimeError",
            Exc::UnicodeEncode(_) => "UnicodeEncodeError",
            Exc::Overflow(_) => "OverflowError",
            Exc::Sqlite(_) => "sqlite3.Error",
            Exc::Recursion(_) => "RecursionError",
            Exc::Shutil { class, .. } | Exc::Other { class, .. } => class,
        }
    }

    pub fn is_value(&self) -> bool {
        matches!(self, Exc::Value { .. })
    }
}

impl std::fmt::Display for Exc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Exc::Value { message, .. }
            | Exc::Type(message)
            | Exc::Attribute(message)
            | Exc::Runtime(message)
            | Exc::UnicodeEncode(message)
            | Exc::Overflow(message)
            | Exc::Sqlite(message)
            | Exc::Recursion(message)
            | Exc::Shutil { message, .. }
            | Exc::Other { message, .. } => f.write_str(message),
            Exc::Key(message) => f.write_str(message),
            Exc::Os(error) => f.write_str(&error.text),
        }
    }
}

impl From<OsError> for Exc {
    fn from(error: OsError) -> Self {
        Exc::Os(error)
    }
}

impl From<crate::pyos::NotUnicode> for Exc {
    fn from(error: crate::pyos::NotUnicode) -> Self {
        Exc::value(format!(
            "file name is not valid Unicode: {}",
            crate::py::repr_str(&error.0)
        ))
    }
}

impl From<crate::pyio::ReadError> for Exc {
    fn from(error: crate::pyio::ReadError) -> Self {
        match error {
            crate::pyio::ReadError::Os(error) => Exc::Os(error),
            crate::pyio::ReadError::Decode(message) => Exc::Value {
                class: "UnicodeDecodeError",
                message,
            },
        }
    }
}

/// The deepest container nesting `json.dump` writes. Its encoder is pure
/// Python, one frame per nested container, and stops at the recursion limit
/// of 1,000: about 995 containers in a bare script, fewer under a deeper call
/// stack. Refusing past 900 refuses only more than Python does.
pub const ENCODE_DEPTH: usize = 900;

/// The `RecursionError` `json.dump` raises for nesting past `ENCODE_DEPTH`.
pub fn too_deep() -> Exc {
    Exc::Recursion("maximum recursion depth exceeded".into())
}

/// `json.dump(value, ...)`'s recursion check: containers nested deeper than
/// `ENCODE_DEPTH` stop the encoder.
pub fn refuse_deep(value: &Json) -> Result<(), Exc> {
    fn depth(value: &Json) -> usize {
        match value {
            Json::Array(items) => 1 + items.iter().map(depth).max().unwrap_or(0),
            Json::Object(pairs) => 1 + pairs.iter().map(|(_, v)| depth(v)).max().unwrap_or(0),
            _ => 0,
        }
    }
    if depth(value) > ENCODE_DEPTH {
        return Err(too_deep());
    }
    Ok(())
}

/// A JSON text `json.loads` refused, as its exception.
pub fn json_error(error: crate::pyjson::DecodeError) -> Exc {
    match error {
        crate::pyjson::DecodeError::Invalid(message)
            if message.starts_with("Exceeds the limit") =>
        {
            Exc::value(message)
        }
        crate::pyjson::DecodeError::Invalid(message) => Exc::Value {
            class: "json.decoder.JSONDecodeError",
            message,
        },
        crate::pyjson::DecodeError::Duplicate => Exc::value("duplicate JSON member"),
        crate::pyjson::DecodeError::Recursion(kind) => Exc::Recursion(format!(
            "maximum recursion depth exceeded while decoding a JSON {kind} from a unicode string"
        )),
    }
}

/// What a stage printed and the status it exited with.
#[derive(Default)]
pub struct Run {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub code: i32,
}

impl Run {
    /// `print(text)` to stdout; a surrogate stdout cannot encode stops the run.
    pub fn print_points(&mut self, points: &[u32]) -> Result<(), Exc> {
        let bytes = crate::pyvalue::encode_print(points).map_err(Exc::UnicodeEncode)?;
        self.stdout.extend_from_slice(&bytes);
        self.stdout.push(b'\n');
        Ok(())
    }

    pub fn print(&mut self, text: &str) {
        self.stdout.extend_from_slice(text.as_bytes());
        self.stdout.push(b'\n');
    }

    pub fn eprint(&mut self, text: &str) {
        self.stderr.extend_from_slice(text.as_bytes());
        self.stderr.push(b'\n');
    }

    /// An exception the stage did not catch: the traceback's last line, exit 1.
    pub fn uncaught(&mut self, error: &Exc) -> i32 {
        self.eprint(&format!("{}: {error}", error.class()));
        1
    }
}

/// `value.get(field)` on a dict, or the AttributeError `.get` raises otherwise.
pub fn get<'a>(title: &'a Json, field: &str) -> Result<Option<&'a Json>, Exc> {
    match title {
        Json::Object(_) => Ok(title.get(field)),
        other => Err(Exc::Attribute(format!(
            "'{}' object has no attribute 'get'",
            type_name(other)
        ))),
    }
}

/// The items `for item in value` sees.
pub fn iterate(titles: &Json) -> Result<Vec<Json>, Exc> {
    match titles {
        Json::Array(items) => Ok(items.clone()),
        // A dict iterates its keys and a str its characters, all strs.
        Json::Object(pairs) => Ok(pairs.iter().map(|(k, _)| Json::Str(k.clone())).collect()),
        Json::Str(s) => Ok(s
            .code_points()
            .into_iter()
            .map(|cp| Json::Str(crate::pyjson::PyStr::from_code_points(vec![cp])))
            .collect()),
        other => Err(Exc::Type(format!(
            "'{}' object is not iterable",
            type_name(other)
        ))),
    }
}

/// `value[key]` for a str key, with the exception Python raises otherwise.
pub fn getitem<'a>(value: &'a Json, key: &str) -> Result<&'a Json, Exc> {
    match value {
        Json::Object(_) => value
            .get(key)
            .ok_or_else(|| Exc::Key(crate::py::repr_str(key))),
        Json::Array(_) => Err(Exc::Type(
            "list indices must be integers or slices, not str".into(),
        )),
        Json::Str(_) => Err(Exc::Type(
            "string indices must be integers, not 'str'".into(),
        )),
        other => Err(Exc::Type(format!(
            "'{}' object is not subscriptable",
            type_name(other)
        ))),
    }
}

/// `value[:n]`, or the exception slicing raises.
pub fn head(value: &Json, n: usize) -> Result<Json, Exc> {
    match value {
        Json::Str(s) => Ok(Json::Str(crate::pyjson::PyStr::from_code_points(
            s.code_points().into_iter().take(n).collect(),
        ))),
        Json::Array(items) => Ok(Json::Array(items.iter().take(n).cloned().collect())),
        Json::Object(_) => Err(Exc::Key(format!("slice(None, {n}, None)"))),
        other => Err(Exc::Type(format!(
            "'{}' object is not subscriptable",
            type_name(other)
        ))),
    }
}
