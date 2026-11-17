//! Collision files: concatenated COLL (COL1), COL2, COL3 and COL4 entries, one per model, keyed by model name.
//!
//! Layout references: COL1 is the GTA3/VC format (float vertices, u32 indices); COL2+ (San Andreas) uses
//! fixed-point int16 vertices scaled by 1/128, u16 face indices with a material byte, and offset tables.

use crate::{err, fixed_name, Rd, Result, V3};

#[derive(Debug, Clone, Copy)]
pub struct Sphere {
    pub center: V3,
    pub radius: f32,
    pub material: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct ColBox {
    pub min: V3,
    pub max: V3,
    pub material: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct Face {
    pub a: u32,
    pub b: u32,
    pub c: u32,
    pub material: u8,
}

#[derive(Debug, Clone, Default)]
pub struct ColModel {
    /// Lowercased model name from the entry header.
    pub name: String,
    /// Model id from the entry header (often unreliable; resolve through IDE names).
    pub header_id: u16,
    /// 1..=4
    pub version: u8,
    pub bounds_min: V3,
    pub bounds_max: V3,
    pub spheres: Vec<Sphere>,
    pub boxes: Vec<ColBox>,
    pub verts: Vec<V3>,
    pub faces: Vec<Face>,
}

impl ColModel {
    pub fn is_empty(&self) -> bool {
        self.spheres.is_empty() && self.boxes.is_empty() && self.faces.is_empty()
    }
}

/// Parse every entry of a COL file/archive. Unknown trailing bytes (sector padding) are ignored.
pub fn parse_archive(b: &[u8]) -> Result<Vec<ColModel>> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p + 8 <= b.len() {
        let fourcc = &b[p..p + 4];
        let version = match fourcc {
            b"COLL" => 1,
            b"COL2" => 2,
            b"COL3" => 3,
            b"COL4" => 4,
            _ => break, // padding or end
        };
        let size = u32::from_le_bytes(b[p + 4..p + 8].try_into().unwrap()) as usize;
        let end = p + 8 + size;
        if end > b.len() {
            return err(format!("COL entry at {p} overruns buffer ({end} > {})", b.len()));
        }
        // Offsets inside COL2+ entries are relative to the entry start + 4.
        let entry = &b[p..end];
        let m = if version == 1 { parse_col1(entry) } else { parse_col2(entry, version) };
        match m {
            Ok(m) => out.push(m),
            Err(e) => return err(format!("COL entry at {p} ({}): {e}", fixed_name(&entry[8..30.min(entry.len())]))),
        }
        p = end;
    }
    Ok(out)
}

fn surface(r: &mut Rd) -> Result<u8> {
    let m = r.u8()?;
    r.take(3)?; // flag, brightness, light
    Ok(m)
}

fn parse_col1(e: &[u8]) -> Result<ColModel> {
    let mut r = Rd::new(e, 8);
    let name = fixed_name(r.take(22)?);
    let header_id = r.u16()?;
    // bounds: radius, center, min, max
    let _radius = r.f32()?;
    let _center = r.v3()?;
    let bounds_min = r.v3()?;
    let bounds_max = r.v3()?;
    let mut m = ColModel { name, header_id, version: 1, bounds_min, bounds_max, ..Default::default() };
    let ns = r.u32()? as usize;
    for _ in 0..ns {
        let radius = r.f32()?;
        let center = r.v3()?;
        let material = surface(&mut r)?;
        m.spheres.push(Sphere { center, radius, material });
    }
    let nl = r.u32()? as usize; // unused lines
    r.take(nl * 24)?;
    let nb = r.u32()? as usize;
    for _ in 0..nb {
        let min = r.v3()?;
        let max = r.v3()?;
        let material = surface(&mut r)?;
        m.boxes.push(ColBox { min, max, material });
    }
    let nv = r.u32()? as usize;
    for _ in 0..nv {
        m.verts.push(r.v3()?);
    }
    let nf = r.u32()? as usize;
    for _ in 0..nf {
        let (a, b, c) = (r.u32()?, r.u32()?, r.u32()?);
        let material = surface(&mut r)?;
        m.faces.push(Face { a, b, c, material });
    }
    validate(&mut m);
    Ok(m)
}

fn parse_col2(e: &[u8], version: u8) -> Result<ColModel> {
    let mut r = Rd::new(e, 8);
    let name = fixed_name(r.take(22)?);
    let header_id = r.u16()?;
    // bounds: min, max, center, radius
    let bounds_min = r.v3()?;
    let bounds_max = r.v3()?;
    let _center = r.v3()?;
    let _radius = r.f32()?;
    let ns = r.u16()? as usize;
    let nb = r.u16()? as usize;
    let nf = r.u16()? as usize;
    let _nl = r.u8()?;
    let _pad = r.u8()?;
    let flags = r.u32()?;
    let off_spheres = r.u32()? as usize;
    let off_boxes = r.u32()? as usize;
    let _off_lines = r.u32()?;
    let off_verts = r.u32()? as usize;
    let off_faces = r.u32()? as usize;
    let _off_planes = r.u32()?;
    let mut m = ColModel { name, header_id, version, bounds_min, bounds_max, ..Default::default() };
    if flags & 2 == 0 && ns == 0 && nb == 0 && nf == 0 {
        return Ok(m); // empty model
    }
    let at = |off: usize| off + 4;
    if ns > 0 {
        let mut r = Rd::new(e, at(off_spheres));
        for _ in 0..ns {
            let center = r.v3()?;
            let radius = r.f32()?;
            let material = surface(&mut r)?;
            m.spheres.push(Sphere { center, radius, material });
        }
    }
    if nb > 0 {
        let mut r = Rd::new(e, at(off_boxes));
        for _ in 0..nb {
            let min = r.v3()?;
            let max = r.v3()?;
            let material = surface(&mut r)?;
            m.boxes.push(ColBox { min, max, material });
        }
    }
    if nf > 0 {
        let mut r = Rd::new(e, at(off_faces));
        let mut max_index = 0u32;
        for _ in 0..nf {
            let (a, b, c) = (r.u16()? as u32, r.u16()? as u32, r.u16()? as u32);
            let material = r.u8()?;
            let _light = r.u8()?;
            max_index = max_index.max(a).max(b).max(c);
            m.faces.push(Face { a, b, c, material });
        }
        let nv = max_index as usize + 1;
        let mut r = Rd::new(e, at(off_verts));
        for _ in 0..nv {
            let (x, y, z) = (r.i16()?, r.i16()?, r.i16()?);
            m.verts.push([x as f32 / 128.0, y as f32 / 128.0, z as f32 / 128.0]);
        }
    }
    validate(&mut m);
    Ok(m)
}

/// Drop faces with out-of-range or repeated indices.
fn validate(m: &mut ColModel) {
    let n = m.verts.len() as u32;
    m.faces.retain(|f| f.a < n && f.b < n && f.c < n && f.a != f.b && f.b != f.c && f.a != f.c);
}
