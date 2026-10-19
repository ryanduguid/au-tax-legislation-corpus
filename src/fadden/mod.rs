//! The corpus builder stages ported from the Python `fadden` package.
//!
//! Each stage keeps its Python command line: the program name, arguments,
//! messages and exit statuses of `python -m fadden <stage>`, which forwards a
//! ported stage to this binary. A Python exception a stage did not catch
//! printed a traceback and exited 1; here it prints the traceback's last line,
//! `<Exception>: <message>`, and exits 1.
pub mod as_at;
pub mod cli;
pub mod compare_builds;
pub mod corpus_paths;
pub mod search;

use crate::pyos::OsError;

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
            | Exc::Sqlite(message) => f.write_str(message),
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
