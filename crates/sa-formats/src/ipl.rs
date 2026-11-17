//! IPL placements: text `inst` sections and binary (`bnry`) stream IPLs.

use crate::{err, strip_comment, Rd, Result, V3};

#[derive(Debug, Clone)]
pub struct IplInst {
    pub model: i32,
    /// Lowercased model name (text IPLs only; empty for binary).
    pub name: String,
    /// Interior/area code, low byte of the stored field (13 = visible in every interior).
    pub interior: i32,
    /// Raw interior field including flag bits.
    pub interior_raw: u32,
    pub pos: V3,
    /// Quaternion (x, y, z, w) exactly as stored. SA stores the conjugate of the object's rotation.
    pub rot: [f32; 4],
    /// Index of this instance's LOD within the base text IPL's inst list, or -1.
    pub lod: i32,
}

/// Text IPL: every row of every `inst` section.
pub fn parse_text(text: &str) -> Vec<IplInst> {
    let mut out = Vec::new();
    let mut in_inst = false;
    for line in text.lines() {
        let l = strip_comment(line);
        if l.is_empty() {
            continue;
        }
        if !l.contains(',') {
            let low = l.to_ascii_lowercase();
            in_inst = low == "inst";
            continue;
        }
        if !in_inst {
            continue;
        }
        let f: Vec<&str> = l.split(',').map(|s| s.trim()).collect();
        let num = |i: usize| f.get(i).and_then(|s| s.parse::<f32>().ok());
        // SA: id, name, interior, x, y, z, rx, ry, rz, rw, lod
        if f.len() >= 10 {
            let (Ok(model), Some(x), Some(y), Some(z), Some(qx), Some(qy), Some(qz), Some(qw)) =
                (f[0].parse::<i32>(), num(3), num(4), num(5), num(6), num(7), num(8), num(9))
            else {
                continue;
            };
            let raw = f[2].parse::<i64>().unwrap_or(0) as u32;
            out.push(IplInst {
                model,
                name: f[1].to_ascii_lowercase(),
                interior: (raw & 0xFF) as i32,
                interior_raw: raw,
                pos: [x, y, z],
                rot: [qx, qy, qz, qw],
                lod: f.get(10).and_then(|s| s.parse().ok()).unwrap_or(-1),
            });
        }
    }
    out
}

pub fn is_binary(b: &[u8]) -> bool {
    b.len() >= 4 && &b[..4] == b"bnry"
}

/// Binary stream IPL (`bnry`): 76-byte header, then 40-byte instances.
pub fn parse_binary(b: &[u8]) -> Result<Vec<IplInst>> {
    if !is_binary(b) {
        return err("not a binary IPL");
    }
    let mut h = Rd::new(b, 4);
    let count = h.u32()? as usize;
    let mut r = Rd::new(b, 0x1C);
    let at = r.u32()? as usize;
    let mut r = Rd::new(b, at);
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let pos = r.v3()?;
        let rot = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
        let model = r.i32()?;
        let raw = r.u32()?;
        let lod = r.i32()?;
        out.push(IplInst { model, name: String::new(), interior: (raw & 0xFF) as i32, interior_raw: raw, pos, rot, lod });
    }
    Ok(out)
}
