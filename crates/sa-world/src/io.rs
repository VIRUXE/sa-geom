//! `.sagw` world files: a versioned flat binary of the world's arrays, optionally zstd-compressed.

use crate::world::World;
use bytemuck::Pod;
use std::io::Read;

const MAGIC: &[u8; 4] = b"SAGW";
pub const FORMAT_VERSION: u32 = 2;

#[derive(Debug)]
pub struct IoError(pub String);
impl std::fmt::Display for IoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for IoError {}

fn put_pod<T: Pod>(out: &mut Vec<u8>, v: &[T]) {
    out.extend_from_slice(&(v.len() as u64).to_le_bytes());
    out.extend_from_slice(bytemuck::cast_slice(v));
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

struct Cur<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], IoError> {
        if self.p + n > self.b.len() {
            return Err(IoError("truncated world file".into()));
        }
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32, IoError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, IoError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn pod<T: Pod>(&mut self) -> Result<Vec<T>, IoError> {
        let n = self.u64()? as usize;
        let bytes = self.take(n.checked_mul(std::mem::size_of::<T>()).ok_or_else(|| IoError("bad length".into()))?)?;
        Ok(bytemuck::pod_collect_to_vec(bytes))
    }
    fn string(&mut self) -> Result<String, IoError> {
        let n = self.u32()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
}

impl World {
    /// Serialise (uncompressed).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.tris.len() * 40 + self.nodes.len() * 32 + 1024);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        put_pod(&mut out, &self.models);
        put_pod(&mut out, &self.tris);
        put_pod(&mut out, &self.nodes);
        put_pod(&mut out, &self.spheres);
        put_pod(&mut out, &self.instances);
        put_pod(&mut out, &self.water);
        out.extend_from_slice(&(self.sources.len() as u32).to_le_bytes());
        for s in &self.sources {
            put_str(&mut out, s);
        }
        out.extend_from_slice(&(self.materials.len() as u32).to_le_bytes());
        for s in &self.materials {
            put_str(&mut out, s);
        }
        let mut names: Vec<(&i32, &String)> = self.names.iter().collect();
        names.sort();
        out.extend_from_slice(&(names.len() as u32).to_le_bytes());
        for (id, n) in names {
            out.extend_from_slice(&id.to_le_bytes());
            put_str(&mut out, n);
        }
        put_pod(&mut out, &self.shapes);
        put_pod(&mut out, &self.shape_pts);
        out
    }

    /// Load from bytes, zstd-compressed or not. The top-level BVH is rebuilt.
    pub fn from_bytes(data: &[u8]) -> Result<World, IoError> {
        let raw;
        let b: &[u8] = if data.len() >= 4 && data[..4] == [0x28, 0xB5, 0x2F, 0xFD] {
            let mut dec = ruzstd::decoding::StreamingDecoder::new(data).map_err(|e| IoError(format!("zstd: {e}")))?;
            let mut v = Vec::new();
            dec.read_to_end(&mut v).map_err(|e| IoError(format!("zstd: {e}")))?;
            raw = v;
            &raw
        } else {
            data
        };
        let mut c = Cur { b, p: 0 };
        if c.take(4)? != MAGIC {
            return Err(IoError("not a sa-geom world file (bad magic)".into()));
        }
        let ver = c.u32()?;
        if ver != FORMAT_VERSION {
            return Err(IoError(format!("world file format {ver}, this build reads {FORMAT_VERSION}")));
        }
        let mut w = World { models: c.pod()?, tris: c.pod()?, nodes: c.pod()?, spheres: c.pod()?, instances: c.pod()?, water: c.pod()?, ..Default::default() };
        for _ in 0..c.u32()? {
            w.sources.push(c.string()?);
        }
        for _ in 0..c.u32()? {
            w.materials.push(c.string()?);
        }
        for _ in 0..c.u32()? {
            let id = c.u32()? as i32;
            let n = c.string()?;
            w.names.insert(id, n);
        }
        w.shapes = c.pod()?;
        w.shape_pts = c.pod()?;
        for (i, s) in w.shapes.iter().enumerate() {
            w.shape_by_id.insert(s.id, i as u32);
        }
        for (i, m) in w.models.iter().enumerate() {
            w.by_id.insert(m.id, i as u32);
        }
        w.finish();
        Ok(w)
    }

    pub fn load_file(path: &std::path::Path) -> Result<World, IoError> {
        let data = std::fs::read(path).map_err(|e| IoError(format!("{}: {e}", path.display())))?;
        World::from_bytes(&data)
    }
}
