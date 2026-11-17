//! IMG archives, version 2 (`VER2`, San Andreas): a directory of 32-byte entries, data in 2048-byte sectors.

use crate::{err, fixed_name, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const SECTOR: u64 = 2048;

#[derive(Debug, Clone)]
pub struct ImgEntry {
    /// Lowercased entry name, e.g. `lae_1.col`.
    pub name: String,
    pub offset: u64,
    pub size: u64,
}

pub struct Img {
    pub path: PathBuf,
    pub entries: Vec<ImgEntry>,
    file: File,
}

impl Img {
    pub fn open(path: &Path) -> Result<Img> {
        let mut file = File::open(path).map_err(|e| crate::ParseError(format!("{}: {e}", path.display())))?;
        let mut head = [0u8; 8];
        file.read_exact(&mut head).map_err(|e| crate::ParseError(e.to_string()))?;
        if &head[..4] != b"VER2" {
            return err(format!("{}: not a VER2 IMG archive", path.display()));
        }
        let count = u32::from_le_bytes(head[4..8].try_into().unwrap()) as usize;
        let mut dir = vec![0u8; count * 32];
        file.read_exact(&mut dir).map_err(|e| crate::ParseError(e.to_string()))?;
        let entries = dir
            .chunks_exact(32)
            .map(|c| {
                let offset = u32::from_le_bytes(c[0..4].try_into().unwrap()) as u64 * SECTOR;
                let streaming = u16::from_le_bytes(c[4..6].try_into().unwrap()) as u64;
                let archive = u16::from_le_bytes(c[6..8].try_into().unwrap()) as u64;
                let size = streaming.max(archive) * SECTOR;
                ImgEntry { name: fixed_name(&c[8..32]), offset, size }
            })
            .collect();
        Ok(Img { path: path.to_path_buf(), entries, file })
    }

    pub fn read(&mut self, e: &ImgEntry) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; e.size as usize];
        self.file.seek(SeekFrom::Start(e.offset)).map_err(|x| crate::ParseError(x.to_string()))?;
        self.file.read_exact(&mut buf).map_err(|x| crate::ParseError(format!("{}: {x}", e.name)))?;
        Ok(buf)
    }

    /// Entries whose name ends with `ext` (lowercase, with the dot), e.g. `.col`.
    pub fn with_ext<'a>(&'a self, ext: &'a str) -> impl Iterator<Item = &'a ImgEntry> + 'a {
        self.entries.iter().filter(move |e| e.name.ends_with(ext))
    }
}
