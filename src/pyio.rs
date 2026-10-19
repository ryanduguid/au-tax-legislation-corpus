//! `open(path, encoding="utf-8")` as CPython 3.15 reads it.
//!
//! Text mode decodes strictly and translates `\r\n` and `\r` to `\n`. Reading
//! line by line decodes 8,192-byte chunks, carrying an incomplete trailing
//! sequence into the next chunk, so a decoding error is raised only when the
//! line reader reaches its chunk, and its position counts from that chunk's
//! input. Reading the whole file decodes it at once.
use std::fs::File;
use std::io::{self, Read};

use crate::py;
use crate::pyos::{OsError, Style};

const CHUNK: usize = 8192;

/// Why a text read failed: Python's `OSError` or `UnicodeDecodeError`.
#[derive(Debug)]
pub enum ReadError {
    Os(OsError),
    Decode(String),
}

/// `open(path)` for reading, refusing a directory as `FileIO` does.
pub fn open(path: &str) -> Result<File, OsError> {
    let file = File::open(path).map_err(|e| OsError::new(&e, Some(path), Style::Errno))?;
    #[cfg(unix)]
    if file.metadata().is_ok_and(|m| m.is_dir()) {
        let error = io::Error::from_raw_os_error(libc::EISDIR);
        return Err(OsError::new(&error, Some(path), Style::Errno));
    }
    Ok(file)
}

fn read_error(error: &io::Error) -> ReadError {
    ReadError::Os(OsError::new(error, None, Style::Errno))
}

/// Text mode's universal newlines.
fn translate(text: &str) -> String {
    py::universal_newlines(text)
}

/// `open(path, encoding="utf-8").read()`.
pub fn read_text(path: &str) -> Result<String, ReadError> {
    let mut bytes = Vec::new();
    open(path)
        .map_err(ReadError::Os)?
        .read_to_end(&mut bytes)
        .map_err(|e| read_error(&e))?;
    let text = py::decode_utf8(&bytes).map_err(|e| ReadError::Decode(e.to_string()))?;
    Ok(translate(&text))
}

/// `for line in open(path, encoding="utf-8")`.
pub struct Lines {
    file: File,
    pending: Vec<u8>,
    pending_cr: bool,
    text: String,
    eof: bool,
}

impl Lines {
    pub fn open(path: &str) -> Result<Lines, OsError> {
        Ok(Lines {
            file: open(path)?,
            pending: Vec::new(),
            pending_cr: false,
            text: String::new(),
            eof: false,
        })
    }

    /// Decode the next chunk into the text buffer.
    fn fill(&mut self) -> Result<(), ReadError> {
        let mut chunk = vec![0; CHUNK];
        let mut read = 0;
        while read < CHUNK {
            match self.file.read(&mut chunk[read..]) {
                Ok(0) => break,
                Ok(n) => read += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(read_error(&e)),
            }
        }
        chunk.truncate(read);
        let last = read == 0;
        let mut input = std::mem::take(&mut self.pending);
        input.extend_from_slice(&chunk);
        let decoded = match std::str::from_utf8(&input) {
            Ok(text) => text.to_string(),
            Err(e) if e.error_len().is_none() && !last => {
                let valid = e.valid_up_to();
                self.pending = input[valid..].to_vec();
                String::from_utf8_lossy(&input[..valid]).into_owned()
            }
            Err(_) => {
                let error = py::decode_utf8(&input).expect_err("invalid input");
                return Err(ReadError::Decode(error.to_string()));
            }
        };
        let mut text = String::with_capacity(decoded.len() + 1);
        if std::mem::take(&mut self.pending_cr) {
            text.push('\r');
        }
        text.push_str(&decoded);
        if !last && text.ends_with('\r') {
            text.pop();
            self.pending_cr = true;
        }
        self.text.push_str(&translate(&text));
        self.eof = last;
        Ok(())
    }

    /// The next line with its `\n`, or None at the end of the file.
    pub fn next_line(&mut self) -> Option<Result<String, ReadError>> {
        loop {
            if let Some(end) = self.text.find('\n') {
                let rest = self.text.split_off(end + 1);
                return Some(Ok(std::mem::replace(&mut self.text, rest)));
            }
            if self.eof {
                return (!self.text.is_empty()).then(|| Ok(std::mem::take(&mut self.text)));
            }
            if let Err(error) = self.fill() {
                self.eof = true;
                self.text.clear();
                return Some(Err(error));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(bytes: &[u8]) -> Vec<Result<String, String>> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, bytes).unwrap();
        let mut reader = Lines::open(&path.to_string_lossy()).unwrap();
        let mut out = Vec::new();
        while let Some(line) = reader.next_line() {
            out.push(line.map_err(|e| match e {
                ReadError::Decode(m) => m,
                ReadError::Os(e) => e.text,
            }));
        }
        out
    }

    #[test]
    fn decode_errors_count_from_their_chunk() {
        let mut data = vec![b'a'; 10_000];
        data.push(0xff);
        assert_eq!(
            lines(&data).last().unwrap().as_ref().unwrap_err(),
            "'utf-8' codec can't decode byte 0xff in position 1808: invalid start byte"
        );
        let mut data = b"x\n".repeat(5000);
        data.extend_from_slice(b"\xe2\x82");
        assert_eq!(
            lines(&data).last().unwrap().as_ref().unwrap_err(),
            "'utf-8' codec can't decode bytes in position 0-1: unexpected end of data"
        );
        let mut data = vec![b'y'; 8191];
        data.extend_from_slice(b"\xe2\x82\xac\xff");
        assert_eq!(
            lines(&data).last().unwrap().as_ref().unwrap_err(),
            "'utf-8' codec can't decode byte 0xff in position 3: invalid start byte"
        );
    }

    #[test]
    fn newlines_are_universal_across_chunks() {
        let mut data = vec![b'a'; 8191];
        data.extend_from_slice(b"\r\nb\rc");
        let got: Vec<String> = lines(&data).into_iter().map(Result::unwrap).collect();
        assert_eq!(got.len(), 3);
        assert!(got[0].ends_with("a\n"));
        assert_eq!(got[1..], ["b\n".to_string(), "c".to_string()]);
    }
}
