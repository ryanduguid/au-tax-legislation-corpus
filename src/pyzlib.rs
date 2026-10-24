//! `zlib.decompressobj(-zlib.MAX_WBITS)` and `zlib.crc32`, as CPython 3.15
//! drives zlib 1.3.1 (and zlib-ng 2.2.4, which keeps the same state machine).
//!
//! The inflater is zlib's `inflate()` slow path for raw deflate, state for
//! state. The reader in `fadden::epub` stops on whichever check fails first,
//! so what matters is not only whether a stream is valid but when zlib notices:
//! after filling its output buffer zlib keeps decoding steps that write nothing
//! (the next code, a block header, a code table) until it needs output space
//! or input, and it reports a bad code only once that code's bits are in hand.
//! Huffman tables follow `inflate_table`'s acceptance rules: over-subscribed
//! sets are refused, and an incomplete set is accepted only for literal/length
//! and distance codes whose longest code is 1 bit, the missing code then
//! decoding as invalid after 1 bit.
//!
//! Scope: the subset `fadden/download.py`'s EPUB reader uses. `decompress`
//! always gets a positive `max_length` (0, unbounded in CPython, is refused),
//! there is no `flush()`, and `ZlibError` drops zlib's message, because the
//! reader turns every `zlib.error` into one `DownloadError`.

const ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];
const LBASE: [u32; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEXT: [u32; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DBASE: [u32; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DEXT: [u32; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
const WSIZE: usize = 1 << 15;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Codes,
    Lens,
    Dists,
}

/// A canonical Huffman code as `inflate_table` accepts it.
#[derive(Clone)]
struct Huffman {
    count: [u16; 16],
    symbol: Vec<u16>,
    /// No symbol has a code: every lookup reads 1 bit and finds no code.
    empty: bool,
    /// One 1-bit code: the other 1-bit pattern is an invalid code.
    half: bool,
    /// Indexed by the next `FAST_BITS` bits: `len << 16 | symbol` for a code
    /// of at most that many bits, else 0 (decoded bit by bit).
    fast: Vec<u32>,
}

/// Bits a [`Huffman::fast`] lookup reads; longer codes are rarer.
const FAST_BITS: u32 = 9;

#[derive(Debug, PartialEq)]
enum Decoded {
    Symbol(u16, u32),
    /// An invalid-code table entry, reached after this many bits.
    Invalid(u32),
    NeedMore,
}

impl Huffman {
    fn build(lengths: &[u16], kind: Kind) -> Option<Huffman> {
        let mut count = [0u16; 16];
        for &len in lengths {
            count[len as usize] += 1;
        }
        let max = (1..16).rev().find(|&len| count[len] != 0).unwrap_or(0);
        if max == 0 {
            return Some(Huffman {
                count,
                symbol: Vec::new(),
                empty: true,
                half: false,
                fast: Vec::new(),
            });
        }
        let mut left: i32 = 1;
        for &n in &count[1..] {
            left <<= 1;
            left -= i32::from(n);
            if left < 0 {
                return None;
            }
        }
        if left > 0 && (kind == Kind::Codes || max != 1) {
            return None;
        }
        let mut offsets = [0u16; 16];
        for len in 1..15 {
            offsets[len + 1] = offsets[len] + count[len];
        }
        let mut symbol = vec![0u16; lengths.len()];
        for (sym, &len) in lengths.iter().enumerate() {
            if len != 0 {
                symbol[offsets[len as usize] as usize] = sym as u16;
                offsets[len as usize] += 1;
            }
        }
        // Canonical codes, as `decode_bits` walks them, stored bit-reversed
        // because the stream holds a code's first bit lowest.
        let mut fast = vec![0u32; 1 << FAST_BITS];
        let (mut first, mut index) = (0u32, 0usize);
        for len in 1..=FAST_BITS {
            let n = usize::from(count[len as usize]);
            for k in 0..n {
                let code = first + k as u32;
                let reversed = code.reverse_bits() >> (32 - len);
                let entry = len << 16 | u32::from(symbol[index + k]);
                for slot in (reversed as usize..fast.len()).step_by(1 << len) {
                    fast[slot] = entry;
                }
            }
            index += n;
            first = (first + n as u32) << 1;
        }
        Some(Huffman {
            count,
            symbol,
            empty: false,
            half: left > 0,
            fast,
        })
    }

    /// Decode from the low `bits` bits of `hold`, as zlib's table lookups do.
    fn decode(&self, hold: u64, bits: u32) -> Decoded {
        // Bits of `hold` past `bits` are 0, so an entry no longer than `bits` is
        // the code the walk below would find.
        if let Some(&entry) = self.fast.get((hold & ((1 << FAST_BITS) - 1)) as usize)
            && entry != 0
            && entry >> 16 <= bits
        {
            return Decoded::Symbol(entry as u16, entry >> 16);
        }
        self.decode_bits(hold, bits)
    }

    /// `decode` one bit at a time: the canonical code walk.
    fn decode_bits(&self, hold: u64, bits: u32) -> Decoded {
        if self.empty {
            return if bits >= 1 {
                Decoded::Invalid(1)
            } else {
                Decoded::NeedMore
            };
        }
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16u32 {
            if len > bits {
                return Decoded::NeedMore;
            }
            code |= ((hold >> (len - 1)) & 1) as i32;
            let count = i32::from(self.count[len as usize]);
            if code - count < first {
                return Decoded::Symbol(self.symbol[(index + code - first) as usize], len);
            }
            if self.half {
                return Decoded::Invalid(1);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        unreachable!("a complete code resolves within 15 bits")
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    TypeDo,
    Stored,
    Copy,
    Table,
    LenLens,
    CodeLens,
    Len,
    LenExt,
    Dist,
    DistExt,
    Match,
    Lit,
    Done,
    Bad,
}

/// How one `inflate(strm, Z_SYNC_FLUSH)` call ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// Z_OK or Z_BUF_ERROR: more input or output space is needed.
    Ok,
    StreamEnd,
    DataError,
}

/// A raw-deflate `z_stream` after `inflateInit2(strm, -15)`.
#[derive(Clone)]
pub struct Inflate {
    mode: Mode,
    last: bool,
    hold: u64,
    bits: u32,
    length: u32,
    offset: u32,
    extra: u32,
    nlen: usize,
    ndist: usize,
    ncode: usize,
    have: usize,
    lens: [u16; 320],
    lencode: Option<Huffman>,
    distcode: Option<Huffman>,
    window: Vec<u8>,
    total_out: u64,
}

impl Default for Inflate {
    fn default() -> Self {
        Inflate {
            mode: Mode::TypeDo,
            last: false,
            hold: 0,
            bits: 0,
            length: 0,
            offset: 0,
            extra: 0,
            nlen: 0,
            ndist: 0,
            ncode: 0,
            have: 0,
            lens: [0; 320],
            lencode: None,
            distcode: None,
            window: vec![0; WSIZE],
            total_out: 0,
        }
    }
}

/// The input cursor of one call.
struct Input<'a> {
    data: &'a [u8],
    next: usize,
}

impl Inflate {
    /// NEEDBITS(n): false when the input ran out first.
    fn need(&mut self, n: u32, input: &mut Input) -> bool {
        while self.bits < n {
            if !self.pull(input) {
                return false;
            }
        }
        true
    }

    /// PULLBYTE().
    fn pull(&mut self, input: &mut Input) -> bool {
        let Some(&byte) = input.data.get(input.next) else {
            return false;
        };
        input.next += 1;
        self.hold |= u64::from(byte) << self.bits;
        self.bits += 8;
        true
    }

    fn take(&mut self, n: u32) -> u32 {
        let value = (self.hold & ((1u64 << n) - 1)) as u32;
        self.hold >>= n;
        self.bits -= n;
        value
    }

    /// Decode one code, pulling bytes until it resolves; None leaves the call.
    fn decode(&mut self, dist: bool, input: &mut Input) -> Option<Decoded> {
        loop {
            let table = if dist { &self.distcode } else { &self.lencode };
            let decoded = table
                .as_ref()
                .expect("tables are built before decoding")
                .decode(self.hold, self.bits);
            match decoded {
                Decoded::NeedMore => {
                    if !self.pull(input) {
                        return None;
                    }
                }
                other => return Some(other),
            }
        }
    }

    fn put(&mut self, byte: u8, out: &mut Vec<u8>) {
        self.window[(self.total_out % WSIZE as u64) as usize] = byte;
        self.total_out += 1;
        out.push(byte);
    }

    fn fixed_tables(&mut self) {
        let mut lens = [0u16; 288];
        lens[..144].fill(8);
        lens[144..256].fill(9);
        lens[256..280].fill(7);
        lens[280..].fill(8);
        self.lencode = Huffman::build(&lens, Kind::Lens);
        self.distcode = Huffman::build(&[5; 32], Kind::Dists);
    }

    /// One `inflate(strm, Z_SYNC_FLUSH)` call with `space` bytes of output
    /// room: the input bytes it consumed and how it ended.
    pub fn inflate(&mut self, data: &[u8], space: usize, out: &mut Vec<u8>) -> (usize, Status) {
        let mut input = Input { data, next: 0 };
        let mut left = space;
        loop {
            match self.mode {
                Mode::TypeDo => {
                    if self.last {
                        let drop = self.bits & 7;
                        self.take(drop);
                        self.mode = Mode::Done;
                        continue;
                    }
                    if !self.need(3, &mut input) {
                        break;
                    }
                    self.last = self.take(1) == 1;
                    self.mode = match self.take(2) {
                        0 => Mode::Stored,
                        1 => {
                            self.fixed_tables();
                            Mode::Len
                        }
                        2 => Mode::Table,
                        _ => Mode::Bad,
                    };
                }
                Mode::Stored => {
                    let drop = self.bits & 7;
                    self.take(drop);
                    if !self.need(32, &mut input) {
                        break;
                    }
                    let hold = self.hold as u32;
                    if hold & 0xffff != (hold >> 16) ^ 0xffff {
                        self.mode = Mode::Bad;
                        continue;
                    }
                    self.length = hold & 0xffff;
                    self.hold = 0;
                    self.bits = 0;
                    self.mode = Mode::Copy;
                }
                Mode::Copy => {
                    if self.length == 0 {
                        self.mode = Mode::TypeDo;
                        continue;
                    }
                    let available = input.data.len() - input.next;
                    let copy = (self.length as usize).min(available).min(left);
                    if copy == 0 {
                        break;
                    }
                    for i in 0..copy {
                        let byte = input.data[input.next + i];
                        self.put(byte, out);
                    }
                    input.next += copy;
                    left -= copy;
                    self.length -= copy as u32;
                }
                Mode::Table => {
                    if !self.need(14, &mut input) {
                        break;
                    }
                    self.nlen = self.take(5) as usize + 257;
                    self.ndist = self.take(5) as usize + 1;
                    self.ncode = self.take(4) as usize + 4;
                    if self.nlen > 286 || self.ndist > 30 {
                        self.mode = Mode::Bad;
                        continue;
                    }
                    self.have = 0;
                    self.mode = Mode::LenLens;
                }
                Mode::LenLens => {
                    while self.have < self.ncode {
                        if !self.need(3, &mut input) {
                            return self.leave(input.next);
                        }
                        self.lens[ORDER[self.have]] = self.take(3) as u16;
                        self.have += 1;
                    }
                    while self.have < 19 {
                        self.lens[ORDER[self.have]] = 0;
                        self.have += 1;
                    }
                    match Huffman::build(&self.lens[..19], Kind::Codes) {
                        Some(table) => {
                            self.lencode = Some(table);
                            self.have = 0;
                            self.mode = Mode::CodeLens;
                        }
                        None => self.mode = Mode::Bad,
                    }
                }
                Mode::CodeLens => {
                    let total = self.nlen + self.ndist;
                    while self.have < total {
                        // An empty code-length code still reads 1 bit as length 0.
                        let (val, used) = match self.decode(false, &mut input) {
                            None => return self.leave(input.next),
                            Some(Decoded::Symbol(val, used)) => (val, used),
                            Some(Decoded::Invalid(used)) => (0, used),
                            Some(Decoded::NeedMore) => unreachable!(),
                        };
                        if val < 16 {
                            self.take(used);
                            self.lens[self.have] = val;
                            self.have += 1;
                            continue;
                        }
                        let (extra, base) = match val {
                            16 => (2, 3),
                            17 => (3, 3),
                            _ => (7, 11),
                        };
                        if !self.need(used + extra, &mut input) {
                            return self.leave(input.next);
                        }
                        self.take(used);
                        let len = if val == 16 {
                            if self.have == 0 {
                                self.mode = Mode::Bad;
                                break;
                            }
                            self.lens[self.have - 1]
                        } else {
                            0
                        };
                        let copy = base + self.take(extra) as usize;
                        if self.have + copy > total {
                            self.mode = Mode::Bad;
                            break;
                        }
                        self.lens[self.have..self.have + copy].fill(len);
                        self.have += copy;
                    }
                    if self.mode == Mode::Bad {
                        continue;
                    }
                    if self.lens[256] == 0 {
                        self.mode = Mode::Bad;
                        continue;
                    }
                    let lencode = Huffman::build(&self.lens[..self.nlen], Kind::Lens);
                    let distcode =
                        Huffman::build(&self.lens[self.nlen..self.nlen + self.ndist], Kind::Dists);
                    match (lencode, distcode) {
                        (Some(lencode), Some(distcode)) => {
                            self.lencode = Some(lencode);
                            self.distcode = Some(distcode);
                            self.mode = Mode::Len;
                        }
                        _ => self.mode = Mode::Bad,
                    }
                }
                Mode::Len => {
                    let (sym, used) = match self.decode(false, &mut input) {
                        None => break,
                        Some(Decoded::Symbol(sym, used)) => (sym, used),
                        Some(Decoded::Invalid(used)) => {
                            self.take(used);
                            self.mode = Mode::Bad;
                            continue;
                        }
                        Some(Decoded::NeedMore) => unreachable!(),
                    };
                    self.take(used);
                    match sym {
                        0..=255 => {
                            self.length = u32::from(sym);
                            self.mode = Mode::Lit;
                        }
                        256 => self.mode = Mode::TypeDo,
                        257..=285 => {
                            let i = usize::from(sym - 257);
                            self.length = LBASE[i];
                            self.extra = LEXT[i];
                            self.mode = Mode::LenExt;
                        }
                        _ => self.mode = Mode::Bad,
                    }
                }
                Mode::LenExt => {
                    if self.extra != 0 {
                        if !self.need(self.extra, &mut input) {
                            break;
                        }
                        let extra = self.extra;
                        self.length += self.take(extra);
                    }
                    self.mode = Mode::Dist;
                }
                Mode::Dist => {
                    let (sym, used) = match self.decode(true, &mut input) {
                        None => break,
                        Some(Decoded::Symbol(sym, used)) => (sym, used),
                        Some(Decoded::Invalid(used)) => {
                            self.take(used);
                            self.mode = Mode::Bad;
                            continue;
                        }
                        Some(Decoded::NeedMore) => unreachable!(),
                    };
                    self.take(used);
                    if sym >= 30 {
                        self.mode = Mode::Bad;
                        continue;
                    }
                    self.offset = DBASE[usize::from(sym)];
                    self.extra = DEXT[usize::from(sym)];
                    self.mode = Mode::DistExt;
                }
                Mode::DistExt => {
                    if self.extra != 0 {
                        if !self.need(self.extra, &mut input) {
                            break;
                        }
                        let extra = self.extra;
                        self.offset += self.take(extra);
                    }
                    self.mode = Mode::Match;
                }
                Mode::Match => {
                    if left == 0 {
                        break;
                    }
                    if u64::from(self.offset) > self.total_out {
                        self.mode = Mode::Bad;
                        continue;
                    }
                    let copy = (self.length as usize).min(left);
                    for _ in 0..copy {
                        let at = (self.total_out - u64::from(self.offset)) % WSIZE as u64;
                        let byte = self.window[at as usize];
                        self.put(byte, out);
                    }
                    left -= copy;
                    self.length -= copy as u32;
                    if self.length == 0 {
                        self.mode = Mode::Len;
                    }
                }
                Mode::Lit => {
                    if left == 0 {
                        break;
                    }
                    let byte = self.length as u8;
                    self.put(byte, out);
                    left -= 1;
                    self.mode = Mode::Len;
                }
                Mode::Done | Mode::Bad => break,
            }
        }
        self.leave(input.next)
    }

    fn leave(&self, consumed: usize) -> (usize, Status) {
        let status = match self.mode {
            Mode::Done => Status::StreamEnd,
            Mode::Bad => Status::DataError,
            _ => Status::Ok,
        };
        (consumed, status)
    }
}

/// `zlib.decompressobj(-zlib.MAX_WBITS)`.
#[derive(Clone, Default)]
pub struct Decompress {
    inflate: Inflate,
    pub unused_data: Vec<u8>,
    pub unconsumed_tail: Vec<u8>,
    pub eof: bool,
}

/// The `zlib.error` a decompress call raised, without zlib's message.
#[derive(Debug, PartialEq, Eq)]
pub struct ZlibError;

impl Decompress {
    /// `decompress(data, max_length)` with `max_length` at least 1.
    pub fn decompress(&mut self, data: &[u8], max_length: usize) -> Result<Vec<u8>, ZlibError> {
        assert!(
            max_length > 0,
            "max_length 0 is unbounded in CPython; not supported"
        );
        let mut out = Vec::new();
        // CPython grows its output buffer in steps and calls inflate until it
        // fills max_length or inflate stops short; zlib's state carries across
        // those calls, so one call with the whole room decodes the same.
        let (consumed, status) = self.inflate.inflate(data, max_length, &mut out);
        let rest = &data[consumed..];
        // CPython 3.15's save_unconsumed_input: at the stream end the leftover
        // input moves to unused_data and avail_in drops to 0 without advancing
        // next_in, so a non-empty previous tail is replaced by that leftover.
        if status == Status::StreamEnd {
            self.unused_data.extend_from_slice(rest);
            if !self.unconsumed_tail.is_empty() {
                self.unconsumed_tail = rest.to_vec();
            }
        } else if !rest.is_empty() || !self.unconsumed_tail.is_empty() {
            self.unconsumed_tail = rest.to_vec();
        }
        match status {
            Status::StreamEnd => self.eof = true,
            Status::DataError => return Err(ZlibError),
            Status::Ok => {}
        }
        Ok(out)
    }
}

/// `zlib.crc32(data, value)`, 8 bytes a step (slicing-by-8).
pub fn crc32(data: &[u8], value: u32) -> u32 {
    static TABLES: std::sync::OnceLock<[[u32; 256]; 8]> = std::sync::OnceLock::new();
    let t = TABLES.get_or_init(|| {
        let mut t = [[0u32; 256]; 8];
        for (n, entry) in t[0].iter_mut().enumerate() {
            let mut c = n as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xedb8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *entry = c;
        }
        // Table k advances a byte's CRC through k more zero bytes.
        for k in 1..8 {
            let (done, rest) = t.split_at_mut(k);
            for (entry, &prev) in rest[0].iter_mut().zip(&done[k - 1]) {
                *entry = (prev >> 8) ^ done[0][(prev & 0xff) as usize];
            }
        }
        t
    });
    let mut crc = !value;
    let (chunks, rest) = data.as_chunks::<8>();
    for chunk in chunks {
        let lo = crc ^ u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        let hi = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
        crc = t[7][(lo & 0xff) as usize]
            ^ t[6][((lo >> 8) & 0xff) as usize]
            ^ t[5][((lo >> 16) & 0xff) as usize]
            ^ t[4][(lo >> 24) as usize]
            ^ t[3][(hi & 0xff) as usize]
            ^ t[2][((hi >> 8) & 0xff) as usize]
            ^ t[1][((hi >> 16) & 0xff) as usize]
            ^ t[0][(hi >> 24) as usize];
    }
    for &byte in rest {
        crc = t[0][((crc ^ u32::from(byte)) & 0xff) as usize] ^ (crc >> 8);
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Random complete codes (a leaf split until the tree is full), the
    /// fixed tables and a lone 1-bit code: the table lookup decodes every
    /// bit pattern as the bit-by-bit walk does, however few bits are held.
    #[test]
    fn the_fast_table_decodes_as_the_walk_does() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut tables = vec![Huffman::build(&[1, 0], Kind::Dists).unwrap()];
        let mut fixed = Inflate::default();
        fixed.fixed_tables();
        tables.extend([fixed.lencode.unwrap(), fixed.distcode.unwrap()]);
        for _ in 0..200 {
            let symbols = 2 + (next() % 287) as usize;
            let mut depths = vec![0u16];
            while depths.len() < symbols {
                let i = (next() % depths.len() as u64) as usize;
                if depths[i] < 15 {
                    depths[i] += 1;
                    depths.push(depths[i]);
                }
            }
            for i in (1..depths.len()).rev() {
                depths.swap(i, (next() % (i as u64 + 1)) as usize);
            }
            tables.push(Huffman::build(&depths, Kind::Lens).expect("a complete code"));
        }
        for table in &tables {
            for _ in 0..2000 {
                let bits = (next() % 16) as u32;
                let hold = next() & ((1u64 << bits) - 1);
                assert_eq!(table.decode(hold, bits), table.decode_bits(hold, bits));
            }
        }
    }

    #[test]
    fn crc32_matches_zlib() {
        assert_eq!(crc32(b"", 0), 0);
        assert_eq!(crc32(b"123456789", 0), 0xcbf4_3926);
        assert_eq!(crc32(b"6789", crc32(b"12345", 0)), 0xcbf4_3926);
        // Every split of a longer input, against the bit-at-a-time definition.
        let data: Vec<u8> = (0..100u32).map(|i| (i * 37 + 11) as u8).collect();
        let bitwise = |data: &[u8], value: u32| {
            let mut crc = !value;
            for &byte in data {
                crc ^= u32::from(byte);
                for _ in 0..8 {
                    crc = if crc & 1 != 0 {
                        0xedb8_8320 ^ (crc >> 1)
                    } else {
                        crc >> 1
                    };
                }
            }
            !crc
        };
        let whole = bitwise(&data, 0);
        for cut in 0..=data.len() {
            assert_eq!(crc32(&data[cut..], crc32(&data[..cut], 0)), whole, "{cut}");
        }
    }

    #[test]
    fn stored_and_fixed_blocks_inflate() {
        // A final stored block holding "abc", then a fixed block cannot follow.
        let stored = [0x01, 0x03, 0x00, 0xfc, 0xff, b'a', b'b', b'c'];
        let mut d = Decompress::default();
        assert_eq!(d.decompress(&stored, 100).unwrap(), b"abc");
        assert!(d.eof && d.unused_data.is_empty());
        // zlib.compress(b"hello hello hello", wbits=-15)
        let fixed = [203, 72, 205, 201, 201, 87, 200, 64, 144, 0];
        let mut d = Decompress::default();
        assert_eq!(d.decompress(&fixed, 100).unwrap(), b"hello hello hello");
        assert!(d.eof);
    }

    #[test]
    fn trailing_bytes_become_unused_data() {
        let mut d = Decompress::default();
        let data = [0x01, 0x00, 0x00, 0xff, 0xff, 7, 8];
        assert_eq!(d.decompress(&data, 10).unwrap(), b"");
        assert_eq!(d.unused_data, [7, 8]);
        assert!(d.unconsumed_tail.is_empty());
        assert_eq!(d.decompress(&[9], 10).unwrap(), b"");
        assert_eq!(d.unused_data, [7, 8, 9]);
    }

    #[test]
    fn max_length_leaves_an_unconsumed_tail() {
        let stored = [0x01, 0x03, 0x00, 0xfc, 0xff, b'a', b'b', b'c'];
        let mut d = Decompress::default();
        assert_eq!(d.decompress(&stored, 2).unwrap(), b"ab");
        assert_eq!(d.unconsumed_tail, b"c");
        assert!(!d.eof);
        let tail = d.unconsumed_tail.clone();
        assert_eq!(d.decompress(&tail, 2).unwrap(), b"c");
        assert!(d.eof && d.unconsumed_tail.is_empty());
    }

    #[test]
    fn invalid_block_type_and_stored_lengths_are_errors() {
        assert_eq!(
            Decompress::default().decompress(&[0x07], 10),
            Err(ZlibError)
        );
        let bad = [0x01, 0x03, 0x00, 0xfc, 0xfe];
        assert_eq!(Decompress::default().decompress(&bad, 10), Err(ZlibError));
    }

    #[test]
    fn a_distance_before_the_start_is_an_error() {
        // Fixed block: length 3 at distance 1 with nothing written yet.
        // Symbol 257 (7 bits 0000001), distance code 0 (5 bits 00000).
        let mut bits: Vec<u8> = vec![1, 1, 0];
        bits.extend([0, 0, 0, 0, 0, 0, 1]);
        bits.extend([0, 0, 0, 0, 0]);
        let mut bytes = vec![0u8; bits.len().div_ceil(8)];
        for (i, b) in bits.iter().enumerate() {
            bytes[i / 8] |= b << (i % 8);
        }
        assert_eq!(Decompress::default().decompress(&bytes, 10), Err(ZlibError));
    }
}
