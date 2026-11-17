//! Parsers for GTA San Andreas world data: IMG archives, gta.dat, IDE, IPL (text and binary), COL1-4 collision,
//! water.dat, and the two common map export formats (MTA `.map` XML and SA-MP/open.mp Pawn object scripts).
//!
//! Everything here is a plain parser: bytes or text in, owned structs out. No geometry logic.

pub mod col;
pub mod dat;
pub mod ide;
pub mod img;
pub mod ipl;
pub mod mtamap;
pub mod pawn;
pub mod water;

use std::path::{Path, PathBuf};

/// A 3-float vector as stored in the files.
pub type V3 = [f32; 3];

/// Resolve `rel` (with `\` or `/` separators) under `base`, matching each path component case-insensitively.
/// Game files reference paths like `DATA\MAPS\LA\LAn.IDE` while a Linux copy may be `data/maps/LA/LAn.ide`.
pub fn resolve_ci(base: &Path, rel: &str) -> Option<PathBuf> {
    let mut cur = base.to_path_buf();
    for part in rel.split(['\\', '/']).filter(|p| !p.is_empty() && *p != ".") {
        let direct = cur.join(part);
        if direct.exists() {
            cur = direct;
            continue;
        }
        let lower = part.to_ascii_lowercase();
        let found = std::fs::read_dir(&cur)
            .ok()?
            .flatten()
            .find(|e| e.file_name().to_string_lossy().to_ascii_lowercase() == lower)?;
        cur = found.path();
    }
    Some(cur)
}

/// Strip a `#` or `//` comment and surrounding whitespace.
pub(crate) fn strip_comment(line: &str) -> &str {
    let mut l = line;
    if let Some(i) = l.find('#') {
        l = &l[..i];
    }
    if let Some(i) = l.find("//") {
        l = &l[..i];
    }
    l.trim()
}

#[derive(Debug)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ParseError {}

pub type Result<T> = std::result::Result<T, ParseError>;

pub(crate) fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(ParseError(msg.into()))
}

/// Little-endian reader over a byte slice with bounds-checked reads.
pub(crate) struct Rd<'a> {
    pub b: &'a [u8],
    pub p: usize,
}

impl<'a> Rd<'a> {
    pub fn new(b: &'a [u8], p: usize) -> Self {
        Rd { b, p }
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.p + n > self.b.len() {
            return err(format!("read past end ({} + {} > {})", self.p, n, self.b.len()));
        }
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn v3(&mut self) -> Result<V3> {
        Ok([self.f32()?, self.f32()?, self.f32()?])
    }
}

/// Zero-terminated latin-1 name from a fixed-size field, lowercased.
pub(crate) fn fixed_name(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    b[..end].iter().map(|&c| c as char).collect::<String>().to_ascii_lowercase()
}
