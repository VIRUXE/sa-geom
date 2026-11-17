//! `water.dat`: water planes as quads or triangles of (x, y, z, ...) corners.

use crate::strip_comment;

#[derive(Debug, Clone)]
pub struct WaterPoly {
    /// 3 or 4 corners (x, y, z).
    pub corners: Vec<[f32; 3]>,
}

pub fn parse(text: &str) -> Vec<WaterPoly> {
    let mut out = Vec::new();
    for line in text.lines() {
        let l = strip_comment(line);
        if l.is_empty() || l.starts_with("processed") {
            continue;
        }
        let n: Vec<f32> = l.split_whitespace().filter_map(|s| s.parse().ok()).collect();
        // each corner: x y z speedX speedY unknown waveHeight (7 floats), then a flag
        let corners = match n.len() {
            29 => 4,
            22 => 3,
            _ => continue,
        };
        let c = (0..corners).map(|i| [n[i * 7], n[i * 7 + 1], n[i * 7 + 2]]).collect();
        out.push(WaterPoly { corners: c });
    }
    out
}
