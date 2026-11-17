//! SA-MP / open.mp map exports in Pawn: `CreateObject`, `CreateDynamicObject` (streamer) and
//! `RemoveBuildingForPlayer` calls, read with a best-effort scanner (numeric literal arguments only).

use crate::mtamap::{MapObject, MapRemoval, MtaMap};

/// Arguments of every `name(...)` call in `text` (top-level commas split, nested parens kept).
fn calls<'a>(text: &'a str, name: &str) -> Vec<Vec<&'a str>> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = text[from..].find(name) {
        let s = from + i;
        from = s + name.len();
        let prev_ok = s == 0 || !(text.as_bytes()[s - 1].is_ascii_alphanumeric() || text.as_bytes()[s - 1] == b'_');
        let after = &text[from..];
        let trimmed = after.trim_start();
        if !prev_ok || !trimmed.starts_with('(') {
            continue;
        }
        let open = from + (after.len() - trimmed.len()) + 1;
        let mut depth = 1;
        let mut args = Vec::new();
        let mut arg_start = open;
        let bytes = text.as_bytes();
        let mut j = open;
        while j < bytes.len() && depth > 0 {
            match bytes[j] {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        args.push(text[arg_start..j].trim());
                    }
                }
                b',' if depth == 1 => {
                    args.push(text[arg_start..j].trim());
                    arg_start = j + 1;
                }
                _ => {}
            }
            j += 1;
        }
        out.push(args);
        from = j;
    }
    out
}

fn f(a: &[&str], i: usize) -> Option<f32> {
    let s = a.get(i)?.trim().trim_end_matches(['f', 'F']);
    s.parse().ok()
}

/// Objects and removals in a Pawn file. Interior comes from `CreateDynamicObject`'s interiorid (-1 = any, mapped to
/// 0 for placement); plain `CreateObject` objects are in every interior, placed as interior 0.
pub fn parse(text: &str) -> MtaMap {
    let mut out = MtaMap::default();
    for name in ["CreateObject", "CreateDynamicObject", "CreateDynamicObjectEx"] {
        for a in calls(text, name) {
            let (Some(model), Some(x), Some(y), Some(z)) = (f(&a, 0), f(&a, 1), f(&a, 2), f(&a, 3)) else { continue };
            let rot = [f(&a, 4).unwrap_or(0.0), f(&a, 5).unwrap_or(0.0), f(&a, 6).unwrap_or(0.0)];
            let interior = if name == "CreateDynamicObject" { f(&a, 8).map(|v| v as i32).unwrap_or(-1) } else { -1 };
            out.objects.push(MapObject { model: model as i32, pos: [x, y, z], rot, interior: interior.max(0) });
        }
    }
    for a in calls(text, "RemoveBuildingForPlayer") {
        // (playerid, modelid, x, y, z, radius)
        let (Some(model), Some(x), Some(y), Some(z), Some(r)) = (f(&a, 1), f(&a, 2), f(&a, 3), f(&a, 4), f(&a, 5)) else {
            continue;
        };
        out.removals.push(MapRemoval { model: model as i32, pos: [x, y, z], radius: r, interior: -1 });
    }
    out
}
