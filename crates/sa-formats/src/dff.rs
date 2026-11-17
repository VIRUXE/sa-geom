//! RenderWare clumps (`.dff`): the frame hierarchy, each atomic's geometry vertices, and the collision a vehicle
//! model carries inside its clump. Only what is needed to know a model's shape: no materials, UVs or skinning.

use crate::col::{self, ColModel};
use crate::{err, Rd, Result, V3};

const STRUCT: u32 = 0x01;
const EXTENSION: u32 = 0x03;
const FRAME_LIST: u32 = 0x0E;
const GEOMETRY: u32 = 0x0F;
const CLUMP: u32 = 0x10;
const ATOMIC: u32 = 0x14;
const GEOMETRY_LIST: u32 = 0x1A;
const FRAME_NAME: u32 = 0x0253_F2FE;
const COLLISION: u32 = 0x0253_F2FA;

#[derive(Debug, Clone)]
pub struct Frame {
    /// Rows of the rotation (right, forward, up) relative to the parent.
    pub rot: [V3; 3],
    pub pos: V3,
    /// Index of the parent frame, or -1.
    pub parent: i32,
    /// Lowercased frame name ("" if the clump has none).
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Atomic {
    pub frame: usize,
    /// Vertices in the frame's space.
    pub verts: Vec<V3>,
}

#[derive(Debug, Clone, Default)]
pub struct Clump {
    pub frames: Vec<Frame>,
    pub atomics: Vec<Atomic>,
    /// Collision embedded in the clump (vehicles).
    pub collision: Option<ColModel>,
}

impl Clump {
    /// A point in frame `f`'s space, in the clump's root space.
    pub fn to_root(&self, mut f: usize, mut p: V3) -> V3 {
        let mut guard = 0;
        loop {
            let fr = &self.frames[f];
            let r = fr.rot;
            p = [
                r[0][0] * p[0] + r[1][0] * p[1] + r[2][0] * p[2] + fr.pos[0],
                r[0][1] * p[0] + r[1][1] * p[1] + r[2][1] * p[2] + fr.pos[1],
                r[0][2] * p[0] + r[1][2] * p[1] + r[2][2] * p[2] + fr.pos[2],
            ];
            // the root frame (no parent) is the model's own origin: its transform is identity in practice, but a
            // few exports put an offset on it that the game ignores for objects
            if fr.parent < 0 || fr.parent as usize >= self.frames.len() || guard > 64 {
                return p;
            }
            f = fr.parent as usize;
            guard += 1;
        }
    }

    /// Index of the frame named `name` (lowercase), if any.
    pub fn frame(&self, name: &str) -> Option<usize> {
        self.frames.iter().position(|f| f.name == name)
    }

    /// Every vertex of every atomic, in root space. `keep` filters atomics by their frame name.
    pub fn points(&self, keep: impl Fn(&str) -> bool) -> Vec<V3> {
        let mut out = Vec::new();
        for a in &self.atomics {
            if a.frame >= self.frames.len() || !keep(&self.frames[a.frame].name) {
                continue;
            }
            for v in &a.verts {
                out.push(self.to_root(a.frame, *v));
            }
        }
        out
    }
}

struct Chunk<'a> {
    id: u32,
    body: &'a [u8],
}

fn chunks(b: &[u8]) -> Vec<Chunk<'_>> {
    let mut out = Vec::new();
    let mut p = 0;
    while p + 12 <= b.len() {
        let id = u32::from_le_bytes(b[p..p + 4].try_into().unwrap());
        let size = u32::from_le_bytes(b[p + 4..p + 8].try_into().unwrap()) as usize;
        let end = (p + 12).saturating_add(size).min(b.len());
        out.push(Chunk { id, body: &b[p + 12..end] });
        p = end;
    }
    out
}

/// Parse the first clump of a `.dff`.
pub fn parse(b: &[u8]) -> Result<Clump> {
    let top = chunks(b);
    let Some(clump) = top.iter().find(|c| c.id == CLUMP) else { return err("no clump") };
    let mut out = Clump::default();
    let mut geoms: Vec<Vec<V3>> = Vec::new();
    for c in chunks(clump.body) {
        match c.id {
            FRAME_LIST => out.frames = frame_list(c.body)?,
            GEOMETRY_LIST => {
                for g in chunks(c.body).iter().filter(|g| g.id == GEOMETRY) {
                    geoms.push(geometry(g.body).unwrap_or_default());
                }
            }
            ATOMIC => {
                if let Some(s) = chunks(c.body).iter().find(|s| s.id == STRUCT) {
                    let mut r = Rd::new(s.body, 0);
                    let frame = r.u32()? as usize;
                    let geom = r.u32()? as usize;
                    if let Some(v) = geoms.get(geom) {
                        out.atomics.push(Atomic { frame, verts: v.clone() });
                    }
                }
            }
            EXTENSION => {
                for e in chunks(c.body) {
                    if e.id == COLLISION {
                        out.collision = col::parse_archive(e.body).ok().and_then(|v| v.into_iter().next());
                    }
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

fn frame_list(b: &[u8]) -> Result<Vec<Frame>> {
    let parts = chunks(b);
    let Some(s) = parts.iter().find(|c| c.id == STRUCT) else { return err("frame list without struct") };
    let mut r = Rd::new(s.body, 0);
    let n = r.u32()? as usize;
    let mut frames = Vec::with_capacity(n);
    for _ in 0..n {
        let rot = [r.v3()?, r.v3()?, r.v3()?];
        let pos = r.v3()?;
        let parent = r.i32()?;
        let _flags = r.u32()?;
        frames.push(Frame { rot, pos, parent, name: String::new() });
    }
    // one extension per frame, in order, may hold the frame's name
    for (i, e) in parts.iter().filter(|c| c.id == EXTENSION).enumerate() {
        if let Some(f) = frames.get_mut(i) {
            if let Some(nm) = chunks(e.body).iter().find(|c| c.id == FRAME_NAME) {
                f.name = String::from_utf8_lossy(nm.body).trim_end_matches('\0').to_ascii_lowercase();
            }
        }
    }
    Ok(frames)
}

fn geometry(b: &[u8]) -> Result<Vec<V3>> {
    let Some(s) = chunks(b).into_iter().find(|c| c.id == STRUCT) else { return err("geometry without struct") };
    let mut r = Rd::new(s.body, 0);
    let flags = r.u32()?;
    let ntri = r.u32()? as usize;
    let nvert = r.u32()? as usize;
    let _nmorph = r.u32()?;
    const NATIVE: u32 = 0x0100_0000;
    if flags & NATIVE != 0 {
        return Ok(Vec::new()); // platform-native data (PS2/Xbox): not in PC files
    }
    if flags & 0x08 != 0 {
        r.take(nvert * 4)?; // prelit colours
    }
    let mut sets = (flags >> 16) & 0xFF;
    if sets == 0 {
        sets = if flags & 0x80 != 0 { 2 } else if flags & 0x04 != 0 { 1 } else { 0 };
    }
    r.take(sets as usize * nvert * 8)?;
    r.take(ntri * 8)?;
    // first morph target
    r.take(16)?; // bounding sphere
    let has_verts = r.u32()?;
    let _has_normals = r.u32()?;
    if has_verts == 0 {
        return Ok(Vec::new());
    }
    let mut v = Vec::with_capacity(nvert);
    for _ in 0..nvert {
        v.push(r.v3()?);
    }
    Ok(v)
}
