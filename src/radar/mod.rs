//! The synthetic, provenance-first tax-change impact queue.
//!
//! `compare` turns a baseline title index, a synthetic Register observation and
//! an exact source-to-skill map into an impact queue; `validate_review` checks a
//! human technical-review decision against an intact queue; `exposure` lists
//! pseudonymous client profiles whose workflows a queue names.
pub mod cli;
pub mod exposure;
pub mod monitor;
pub mod persist;
pub mod util;

use crate::pyos::OsError;

/// Why a command stopped.
#[derive(Debug)]
pub enum Error {
    /// `MonitorError`: an input is outside the contract. `too_large` marks
    /// its `SourceTooLargeError` subclass.
    Monitor { message: String, too_large: bool },
    /// An `OSError` that escaped while writing an output.
    Os(OsError),
}

impl Error {
    pub fn monitor(message: impl Into<String>) -> Self {
        Error::Monitor {
            message: message.into(),
            too_large: false,
        }
    }

    pub fn too_large(message: impl Into<String>) -> Self {
        Error::Monitor {
            message: message.into(),
            too_large: true,
        }
    }
}

impl From<OsError> for Error {
    fn from(error: OsError) -> Self {
        Error::Os(error)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Monitor { message, .. } => f.write_str(message),
            Error::Os(error) => f.write_str(&error.text),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
