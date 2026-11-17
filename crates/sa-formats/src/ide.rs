//! IDE item definitions: model id -> model name for objects that can carry collision.

use crate::strip_comment;

#[derive(Debug, Clone)]
pub struct IdeObject {
    pub id: i32,
    /// Lowercased model (DFF/COL) name.
    pub name: String,
    pub draw_distance: f32,
    pub flags: u32,
}

/// Parse `objs`, `tobj` and `anim` sections. Other sections (peds, cars, weap, hier, txdp, 2dfx, path) are skipped.
pub fn parse(text: &str) -> Vec<IdeObject> {
    let mut out = Vec::new();
    let mut section = String::new();
    for line in text.lines() {
        let l = strip_comment(line);
        if l.is_empty() {
            continue;
        }
        let low = l.to_ascii_lowercase();
        if !l.contains(',') {
            section = if low == "end" { String::new() } else { low };
            continue;
        }
        if !matches!(section.as_str(), "objs" | "tobj" | "anim") {
            continue;
        }
        let f: Vec<&str> = l.split(',').map(|s| s.trim()).collect();
        if f.len() < 4 {
            continue;
        }
        let Ok(id) = f[0].parse::<i32>() else { continue };
        let name = f[1].to_ascii_lowercase();
        // objs: id, model, txd, dist, flags  (SA). Older variants: id, model, txd, meshcount, dist..., flags
        // anim: id, model, txd, anim, dist, flags. tobj: objs + timeOn, timeOff.
        let (dist, flags) = match section.as_str() {
            "anim" => (f.get(4).and_then(|s| s.parse().ok()).unwrap_or(300.0), f.get(5).and_then(|s| s.parse().ok()).unwrap_or(0)),
            _ => {
                if f.len() >= 6 && section == "objs" && f[3].parse::<u32>().is_ok() && f.len() > 5 && !f[3].contains('.') && f[3].len() == 1 {
                    // id, model, txd, meshcount, dist(s), flags
                    let n = f[3].parse::<usize>().unwrap_or(1);
                    (f.get(4).and_then(|s| s.parse().ok()).unwrap_or(300.0), f.get(4 + n).and_then(|s| s.parse().ok()).unwrap_or(0))
                } else {
                    (f[3].parse().unwrap_or(300.0), f.get(4).and_then(|s| s.parse().ok()).unwrap_or(0))
                }
            }
        };
        out.push(IdeObject { id, name, draw_distance: dist, flags });
    }
    out
}
