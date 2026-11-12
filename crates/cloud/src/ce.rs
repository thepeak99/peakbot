//! Canonical Encoding (CE) helpers (spec §1.2): hand-written, fixed-width,
//! big-endian. Parsers consume the input exactly (P1); `bytes()` fields
//! enforce their per-field maximum before allocation (P2).

use crate::Error;

/// A cursor over CE input.
#[derive(Debug, Clone)]
pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// True once the input is fully consumed (P1: the parser must end here).
    pub(crate) fn at_end(&self) -> bool {
        self.pos == self.buf.len()
    }

    /// Consume exactly `n` bytes; short input is `Malformed` (P1).
    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|e| *e <= self.buf.len())
            .ok_or(Error::Malformed)?;
        let out = &self.buf[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn u32(&mut self) -> Result<u32, Error> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub(crate) fn u64(&mut self) -> Result<u64, Error> {
        let b = self.take(8)?;
        Ok(u64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// `fix[n](x)`: exactly `n` raw bytes.
    pub(crate) fn fix(&mut self, n: usize) -> Result<&'a [u8], Error> {
        self.take(n)
    }

    /// `fix[n](x)` copied into an array of known size.
    pub(crate) fn fix_array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let b = self.take(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(b);
        Ok(out)
    }

    /// `bytes(x)` with a per-field maximum (P2, checked before allocation).
    pub(crate) fn bytes(&mut self, max: usize) -> Result<&'a [u8], Error> {
        let len = usize::try_from(self.u32()?).map_err(|_| Error::Malformed)?;
        if len > max {
            return Err(Error::Malformed);
        }
        self.take(len)
    }

    /// `bytes(x)` with an exact required length.
    pub(crate) fn bytes_exact(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if usize::try_from(self.u32()?).map_err(|_| Error::Malformed)? != n {
            return Err(Error::Malformed);
        }
        self.take(n)
    }

    /// `str(s)`: `bytes(utf8(s))`, valid UTF-8, per-field maximum.
    pub(crate) fn str_field(&mut self, max: usize) -> Result<String, Error> {
        let b = self.bytes(max)?;
        std::str::from_utf8(b)
            .map(|s| s.to_string())
            .map_err(|_| Error::Malformed)
    }
}

/// Append `u32(len) ‖ x` (the `bytes()` primitive).
pub(crate) fn push_bytes(out: &mut Vec<u8>, x: &[u8]) {
    out.extend_from_slice(&(x.len() as u32).to_be_bytes());
    out.extend_from_slice(x);
}

/// Append `str(s)` (the `str()` primitive).
pub(crate) fn push_str(out: &mut Vec<u8>, s: &str) {
    push_bytes(out, s.as_bytes());
}
