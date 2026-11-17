//! MTA `.map` files (XML): `<object>` placements and `<removeWorldObject>` removals.
//!
//! A tiny attribute scanner, not a full XML parser: `.map` files are flat lists of self-closing elements.

#[derive(Debug, Clone)]
pub struct MapObject {
    pub model: i32,
    pub pos: [f32; 3],
    /// Degrees, MTA object rotation (applied Z, then X, then Y; see sa-world `euler`).
    pub rot: [f32; 3],
    pub interior: i32,
}

#[derive(Debug, Clone)]
pub struct MapRemoval {
    /// -1 = every model.
    pub model: i32,
    pub pos: [f32; 3],
    pub radius: f32,
    /// -1 = every interior.
    pub interior: i32,
}

#[derive(Debug, Default, Clone)]
pub struct MtaMap {
    pub objects: Vec<MapObject>,
    pub removals: Vec<MapRemoval>,
}

fn attr<'a>(tag: &'a str, key: &str) -> Option<&'a str> {
    let mut rest = tag;
    while let Some(i) = rest.find(key) {
        let before_ok = i == 0 || rest.as_bytes()[i - 1].is_ascii_whitespace();
        let after = rest[i + key.len()..].trim_start();
        if before_ok && after.starts_with('=') {
            let v = after[1..].trim_start();
            let q = v.chars().next()?;
            if q == '"' || q == '\'' {
                let end = v[1..].find(q)?;
                return Some(&v[1..1 + end]);
            }
        }
        rest = &rest[i + key.len()..];
    }
    None
}

fn num(tag: &str, key: &str, default: f32) -> f32 {
    attr(tag, key).and_then(|s| s.trim().parse().ok()).unwrap_or(default)
}

pub fn parse(text: &str) -> MtaMap {
    let mut out = MtaMap::default();
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        let Some(len) = rest[start..].find('>') else { break };
        let tag = &rest[start + 1..start + len];
        rest = &rest[start + len + 1..];
        let name: String = tag.chars().take_while(|c| !c.is_whitespace() && *c != '/').collect();
        match name.as_str() {
            "object" => {
                let Some(model) = attr(tag, "model").and_then(|s| s.trim().parse().ok()) else { continue };
                out.objects.push(MapObject {
                    model,
                    pos: [num(tag, "posX", 0.0), num(tag, "posY", 0.0), num(tag, "posZ", 0.0)],
                    rot: [num(tag, "rotX", 0.0), num(tag, "rotY", 0.0), num(tag, "rotZ", 0.0)],
                    interior: num(tag, "interior", 0.0) as i32,
                });
            }
            "removeWorldObject" => {
                let model = attr(tag, "model").and_then(|s| s.trim().parse().ok()).unwrap_or(-1);
                out.removals.push(MapRemoval {
                    model,
                    pos: [num(tag, "posX", 0.0), num(tag, "posY", 0.0), num(tag, "posZ", 0.0)],
                    radius: num(tag, "radius", 0.0),
                    interior: num(tag, "interior", -1.0) as i32,
                });
                // the LOD model goes too, as MTA's removeWorldModel with lodModel does
                if let Some(lod) = attr(tag, "lodModel").and_then(|s| s.trim().parse::<i32>().ok()).filter(|&m| m > 0) {
                    let r = out.removals.last().unwrap().clone();
                    out.removals.push(MapRemoval { model: lod, ..r });
                }
            }
            _ => {}
        }
    }
    out
}
