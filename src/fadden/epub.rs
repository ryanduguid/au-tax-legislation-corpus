//! `fadden/download.py`'s bounded EPUB reader: `open_epub` and
//! `read_epub_member` over CPython 3.15's `zipfile.ZipFile`.
//!
//! The archive is read into memory once `_check_zip_directory`'s size budget
//! admits it. Every error is the exception Python raises, with its `str()`:
//! the `DownloadError` checks, then what `ZipFile` itself still refuses once
//! those checks pass (a filename that is not UTF-8, an extract version past
//! 6.3 and a corrupt extra field).
use std::collections::HashMap;
use std::io::Read;

use super::Exc;
use crate::py;
use crate::pyos;
use crate::pyzlib::{Decompress, crc32};

pub const MAX_EPUB_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_EPUB_MEMBERS: usize = 4096;
pub const MAX_EPUB_MEMBER_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_EPUB_UNCOMPRESSED_BYTES: u128 = 256 * 1024 * 1024;
pub const MAX_EPUB_DIRECTORY_BYTES: u64 = 8 * 1024 * 1024;

const STORED: u16 = 0;
const DEFLATED: u16 = 8;
/// `zipfile.MAX_EXTRACT_VERSION`.
const MAX_EXTRACT_VERSION: u8 = 63;
/// The bytes `_EndRecData` searches: a maximum comment and the record.
const TAIL: usize = 65_557;

/// Python's `cp437` codec for bytes 0x80 to 0xFF; lower bytes are ASCII.
const CP437_HIGH: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐\
└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";

/// One `ZipInfo` as the reader uses it.
#[derive(Debug, Clone)]
pub struct Member {
    /// The sanitised name `namelist()` returns.
    pub filename: String,
    pub orig_filename: String,
    pub flag_bits: u16,
    pub compress_type: u16,
    pub crc: u32,
    pub compress_size: u64,
    pub file_size: u64,
    pub header_offset: u64,
}

/// An admitted archive, as `open_epub` yields it.
pub struct Epub {
    data: Vec<u8>,
    pub members: Vec<Member>,
    /// `_epub_member_ends`: where each member's local data must stop.
    ends: HashMap<u64, u64>,
}

fn download(message: &str) -> Exc {
    Exc::Other {
        class: "DownloadError",
        message: message.to_string(),
    }
}

fn bad_zip(message: String) -> Exc {
    Exc::Other {
        class: "zipfile.BadZipFile",
        message,
    }
}

fn u16_at(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([data[at], data[at + 1]])
}

fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}

fn cp437(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| match b {
            0..=0x7f => char::from(b),
            _ => CP437_HIGH.chars().nth(usize::from(b - 0x80)).unwrap(),
        })
        .collect()
}

/// `_sanitize_filename`: cut at the first NUL; Windows maps `\` to `/`.
fn sanitize(name: &str) -> String {
    let name = name.split('\0').next().unwrap_or_default();
    if cfg!(windows) {
        name.replace('\\', "/")
    } else {
        name.to_string()
    }
}

/// `bytes[start:end]` with Python's clamping.
fn slice(data: &[u8], start: u64, len: u64) -> &[u8] {
    let n = data.len() as u64;
    let start = start.min(n);
    &data[start as usize..start.saturating_add(len).min(n) as usize]
}

/// `open_epub(path)`: read and admit the archive.
pub fn open(path: &str) -> Result<Epub, Exc> {
    let mut file = pyos::open_read(path)?;
    let size = file
        .metadata()
        .map_err(|e| pyos::OsError::new(&e, None, pyos::Style::Errno))?
        .len();
    if size > MAX_EPUB_BYTES {
        return Err(download("EPUB exceeds the compressed input budget"));
    }
    let mut data = Vec::with_capacity(size as usize);
    file.read_to_end(&mut data)
        .map_err(|e| pyos::OsError::new(&e, None, pyos::Style::Errno))?;
    from_bytes(data)
}

/// `open_epub` over bytes already read.
pub fn from_bytes(data: Vec<u8>) -> Result<Epub, Exc> {
    if data.len() as u64 > MAX_EPUB_BYTES {
        return Err(download("EPUB exceeds the compressed input budget"));
    }
    let (start_dir, directory_end) = check_directory(&data)?;
    let members = read_directory(&data, start_dir, directory_end)?;
    let ends = admit(&members, start_dir)?;
    Ok(Epub {
        data,
        members,
        ends,
    })
}

/// `_check_zip_directory`; returns the central directory's start and end.
fn check_directory(data: &[u8]) -> Result<(u64, u64), Exc> {
    let tail_start = data.len().saturating_sub(TAIL);
    let tail = &data[tail_start..];
    let marker = tail
        .windows(4)
        .rposition(|w| w == b"PK\x05\x06")
        .filter(|&m| tail.len() - m >= 22)
        .ok_or_else(|| download("EPUB has no complete ZIP directory record"))?;
    let record = &tail[marker..marker + 22];
    let (disk, directory_disk) = (u16_at(record, 4), u16_at(record, 6));
    let (disk_members, members) = (u16_at(record, 8), u16_at(record, 10));
    let directory_bytes = u64::from(u32_at(record, 12));
    let offset = u64::from(u32_at(record, 16));
    let comment_bytes = usize::from(u16_at(record, 20));
    let directory_end = (tail_start + marker) as u64;
    let end = directory_end as usize;
    if end >= 20 && &data[end - 20..end - 16] == b"PK\x06\x07" {
        return Err(download("EPUB uses an unsupported ZIP64 directory"));
    }
    if disk != 0
        || directory_disk != 0
        || disk_members != members
        || marker + 22 + comment_bytes != tail.len()
        || usize::from(members) > MAX_EPUB_MEMBERS
        || directory_bytes > MAX_EPUB_DIRECTORY_BYTES
        || offset == 0xffff_ffff
        || offset + directory_bytes != directory_end
    {
        return Err(download("EPUB exceeds the supported ZIP directory budget"));
    }
    let mut position = offset;
    let mut count = 0;
    while position < directory_end {
        let header = slice(data, position, 46);
        if header.len() != 46 || &header[..4] != b"PK\x01\x02" {
            return Err(download("EPUB has an invalid ZIP directory member"));
        }
        count += 1;
        if count > MAX_EPUB_MEMBERS {
            return Err(download("EPUB exceeds the member count budget"));
        }
        let lengths: u64 = (0..3).map(|i| u64::from(u16_at(header, 28 + 2 * i))).sum();
        let next = position + 46 + lengths;
        if next > directory_end {
            return Err(download("EPUB has an invalid ZIP directory extent"));
        }
        position = next;
    }
    if count != usize::from(members) {
        return Err(download("EPUB ZIP directory member count is inconsistent"));
    }
    Ok((offset, directory_end))
}

/// `ZipFile._RealGetContents` over a directory `check_directory` admitted,
/// so only the per-member errors remain reachable.
fn read_directory(data: &[u8], start_dir: u64, directory_end: u64) -> Result<Vec<Member>, Exc> {
    let mut members = Vec::new();
    let mut at = start_dir as usize;
    while at < directory_end as usize {
        let header = &data[at..at + 46];
        let name_len = usize::from(u16_at(header, 28));
        let extra_len = usize::from(u16_at(header, 30));
        let comment_len = usize::from(u16_at(header, 32));
        let raw_name = &data[at + 46..at + 46 + name_len];
        let extra = &data[at + 46 + name_len..at + 46 + name_len + extra_len];
        let flag_bits = u16_at(header, 8);
        let name = if flag_bits & 0x800 != 0 {
            py::decode_utf8(raw_name).map_err(|e| Exc::Value {
                class: "UnicodeDecodeError",
                message: e.to_string(),
            })?
        } else {
            cp437(raw_name)
        };
        let extract_version = header[6];
        if extract_version > MAX_EXTRACT_VERSION {
            return Err(Exc::Other {
                class: "NotImplementedError",
                message: format!(
                    "zip file version {}.{}",
                    extract_version / 10,
                    extract_version % 10
                ),
            });
        }
        let mut member = Member {
            filename: sanitize(&name),
            orig_filename: name,
            flag_bits,
            compress_type: u16_at(header, 10),
            crc: u32_at(header, 16),
            compress_size: u64::from(u32_at(header, 20)),
            file_size: u64::from(u32_at(header, 24)),
            header_offset: u64::from(u32_at(header, 42)),
        };
        decode_extra(&mut member, extra, crc32(raw_name, 0))?;
        members.push(member);
        at += 46 + name_len + extra_len + comment_len;
    }
    Ok(members)
}

/// `ZipInfo._decodeExtra`.
fn decode_extra(member: &mut Member, mut extra: &[u8], name_crc: u32) -> Result<(), Exc> {
    while extra.len() >= 4 {
        let kind = u16_at(extra, 0);
        let len = usize::from(u16_at(extra, 2));
        if len + 4 > extra.len() {
            return Err(bad_zip(format!(
                "Corrupt extra field {kind:04x} (size={len})"
            )));
        }
        let mut field = &extra[4..len + 4];
        if kind == 0x0001 {
            let mut take = |name: &str| -> Result<u64, Exc> {
                let value = field.get(..8).ok_or_else(|| {
                    bad_zip(format!("Corrupt zip64 extra field. {name} not found."))
                })?;
                field = &field[8..];
                Ok(u64::from_le_bytes(value.try_into().unwrap()))
            };
            if member.file_size == u64::MAX || member.file_size == 0xffff_ffff {
                member.file_size = take("File size")?;
            }
            if member.compress_size == 0xffff_ffff {
                member.compress_size = take("Compress size")?;
            }
            if member.header_offset == 0xffff_ffff {
                member.header_offset = take("Header offset")?;
            }
        } else if kind == 0x7075 {
            if field.len() < 5 {
                return Err(bad_zip(
                    "Corrupt unicode path extra field (0x7075)".to_string(),
                ));
            }
            if field[0] == 1 && u32_at(field, 1) == name_crc {
                let name = py::decode_utf8(&field[5..]).map_err(|_| {
                    bad_zip(
                        "Corrupt unicode path extra field (0x7075): invalid utf-8 bytes"
                            .to_string(),
                    )
                })?;
                // shortcut: Python warns on stderr for an empty name; the port
                // stays silent (a documented divergence).
                if !name.is_empty() {
                    member.filename = sanitize(&name);
                }
            }
        }
        extra = &extra[len + 4..];
    }
    Ok(())
}

/// `_admit_epub_members`; returns `_epub_member_ends`.
fn admit(members: &[Member], start_dir: u64) -> Result<HashMap<u64, u64>, Exc> {
    let mut names: Vec<&str> = members.iter().map(|m| m.filename.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    if names.len() != members.len() {
        return Err(download("EPUB contains duplicate ZIP member names"));
    }
    if members.len() > MAX_EPUB_MEMBERS {
        return Err(download("EPUB exceeds the member count budget"));
    }
    if members.iter().any(|m| m.file_size > MAX_EPUB_MEMBER_BYTES) {
        return Err(download("EPUB exceeds the uncompressed member budget"));
    }
    let total: u128 = members.iter().map(|m| u128::from(m.file_size)).sum();
    if total > MAX_EPUB_UNCOMPRESSED_BYTES {
        return Err(download("EPUB exceeds the total uncompressed budget"));
    }
    if members
        .iter()
        .any(|m| !matches!(m.compress_type, STORED | DEFLATED) || m.flag_bits & 1 != 0)
    {
        return Err(download("EPUB uses unsupported compression or encryption"));
    }
    let mut offsets: Vec<u64> = members.iter().map(|m| m.header_offset).collect();
    offsets.sort_unstable();
    if offsets.windows(2).any(|w| w[0] == w[1]) {
        return Err(download("EPUB members share a local header"));
    }
    let nexts = offsets.iter().skip(1).copied().chain([start_dir]);
    Ok(offsets.iter().copied().zip(nexts).collect())
}

impl Epub {
    /// `namelist()`.
    pub fn namelist(&self) -> impl Iterator<Item = &str> {
        self.members.iter().map(|m| m.filename.as_str())
    }

    /// `read_epub_member(archive, name)`.
    pub fn read(&self, name: &str) -> Result<Vec<u8>, Exc> {
        let info = self
            .members
            .iter()
            .find(|m| m.filename == name)
            .ok_or_else(|| {
                Exc::Key(py::repr_str(&format!(
                    "There is no item named {} in the archive",
                    py::repr_str(name)
                )))
            })?;
        let (mut position, method) = self.position(info)?;
        let mut inflater = (method == DEFLATED).then(Decompress::default);
        let mut remaining = info.compress_size;
        let (mut total, mut crc) = (0u64, 0u32);
        let mut content = Vec::new();
        let invalid = || download("EPUB member has invalid deflate data");
        while remaining > 0 {
            let mut block = slice(&self.data, position, remaining.min(65_536)).to_vec();
            if block.is_empty() {
                return Err(download("EPUB member data is truncated"));
            }
            remaining -= block.len() as u64;
            position += block.len() as u64;
            loop {
                let limit = 65_536.min(info.file_size + 1 - total) as usize;
                let piece = match inflater.as_mut() {
                    Some(d) => d.decompress(&block, limit).map_err(|_| invalid())?,
                    None => std::mem::take(&mut block),
                };
                total += piece.len() as u64;
                if total > info.file_size {
                    return Err(download(
                        "EPUB member exceeds its declared uncompressed length",
                    ));
                }
                crc = crc32(&piece, crc);
                content.extend_from_slice(&piece);
                let Some(d) = inflater.as_ref() else { break };
                if !d.unused_data.is_empty() {
                    return Err(download("EPUB member has trailing compressed data"));
                }
                block = d.unconsumed_tail.clone();
                // A full output buffer can leave a match pending after input ends.
                if block.is_empty() && (piece.len() < limit || d.eof) {
                    break;
                }
            }
        }
        if total != info.file_size || crc != info.crc || inflater.is_some_and(|d| !d.eof) {
            return Err(download(
                "EPUB member length, checksum or deflate endpoint is invalid",
            ));
        }
        Ok(content)
    }

    /// `_position_epub_member`: the offset of the member's data and its method.
    fn position(&self, info: &Member) -> Result<(u64, u16), Exc> {
        if info.file_size > MAX_EPUB_MEMBER_BYTES
            || info.compress_size > MAX_EPUB_BYTES
            || !matches!(info.compress_type, STORED | DEFLATED)
            || info.flag_bits & 1 != 0
        {
            return Err(download("EPUB member is outside the supported byte budget"));
        }
        if info.header_offset > i64::MAX as u64 {
            return Err(Exc::value("cannot fit 'int' into an offset-sized integer"));
        }
        let header = slice(&self.data, info.header_offset, 30);
        if header.len() != 30 || &header[..4] != b"PK\x03\x04" {
            return Err(download("EPUB member has an invalid local header"));
        }
        let (flags, method) = (u16_at(header, 6), u16_at(header, 8));
        let (name_bytes, extra_bytes) = (u16_at(header, 26), u16_at(header, 28));
        let raw = slice(&self.data, info.header_offset + 30, u64::from(name_bytes));
        let name = if flags & 0x800 != 0 {
            py::decode_utf8(raw).map_err(|_| download("EPUB member has an invalid filename"))?
        } else {
            cp437(raw)
        };
        let position = info.header_offset + 30 + raw.len() as u64 + u64::from(extra_bytes);
        if name != info.orig_filename
            || flags != info.flag_bits
            || method != info.compress_type
            || position + info.compress_size > self.ends[&info.header_offset]
        {
            return Err(download(
                "EPUB member metadata does not match its local data",
            ));
        }
        if method == STORED && info.compress_size != info.file_size {
            return Err(download("EPUB stored member has inconsistent lengths"));
        }
        Ok((position, method))
    }
}
