//! User overlays: extra collision models and map placements/removals on top of a world, from a TOML manifest or
//! directly from `.map` (MTA), `.pwn`/`.inc`/`.txt` (SA-MP/open.mp Pawn), `.ipl` and `.col` files.

use crate::bake::ipl_affine;
use crate::euler;
use crate::math::Vec3;
use crate::world::World;
use sa_formats::{ide, img::Img, ipl, mtamap, pawn};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    #[serde(default)]
    pub ide: Vec<PathEntry>,
    #[serde(default)]
    pub img: Vec<PathEntry>,
    #[serde(default)]
    pub col: Vec<ColEntry>,
    /// Loose `.dff` files: shapes for placing models by their mesh. `id` as for `col`.
    #[serde(default)]
    pub dff: Vec<ColEntry>,
    #[serde(default)]
    pub ipl: Vec<PathEntry>,
    #[serde(default)]
    pub map: Vec<PathEntry>,
    #[serde(default)]
    pub pawn: Vec<PathEntry>,
    #[serde(default)]
    pub remove: Vec<RemoveEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathEntry {
    pub path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColEntry {
    pub path: String,
    /// Model id for every model in the file (a single-model .col whose name is not in any IDE).
    pub id: Option<i32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoveEntry {
    #[serde(default = "minus_one")]
    pub model: i32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub radius: f32,
    #[serde(default = "minus_one")]
    pub interior: i32,
}

fn minus_one() -> i32 {
    -1
}

#[derive(Debug, Default, Clone)]
pub struct OverlayReport {
    pub models: usize,
    pub objects: usize,
    pub objects_without_collision: usize,
    pub removed: usize,
    pub files: usize,
    /// Models given a mesh shape (from .dff files).
    pub shapes: usize,
}

impl std::fmt::Display for OverlayReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} files, {} models, {} shapes, {} objects ({} without collision), {} removed", self.files, self.models, self.shapes, self.objects, self.objects_without_collision, self.removed)
    }
}

/// Expand `pattern` relative to `base`: a file, a directory (every file in it, recursively) or a path whose last
/// component holds `*` wildcards (optionally preceded by `**/` for recursion).
pub fn expand(base: &Path, pattern: &str) -> Vec<PathBuf> {
    let p = base.join(pattern);
    let s = p.to_string_lossy().replace('\\', "/");
    if !s.contains('*') {
        if p.is_dir() {
            let mut out = Vec::new();
            walk(&p, true, &mut |f| out.push(f.to_path_buf()));
            out.sort();
            return out;
        }
        return vec![p];
    }
    let (dir, file_pat) = s.rsplit_once('/').unwrap_or((".", &s));
    let (dir, recursive) = match dir.strip_suffix("/**") {
        Some(d) => (d.to_string(), true),
        None if dir.ends_with("**") => (dir.trim_end_matches("**").trim_end_matches('/').to_string(), true),
        None => (dir.to_string(), false),
    };
    let mut out = Vec::new();
    walk(Path::new(&dir), recursive, &mut |f| {
        if let Some(n) = f.file_name() {
            if wildcard(file_pat, &n.to_string_lossy()) {
                out.push(f.to_path_buf());
            }
        }
    });
    out.sort();
    out
}

fn walk(dir: &Path, recursive: bool, f: &mut dyn FnMut(&Path)) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if recursive {
                walk(&p, true, f);
            }
        } else {
            f(&p);
        }
    }
}

fn wildcard(pat: &str, s: &str) -> bool {
    let (p, s) = (pat.to_ascii_lowercase(), s.to_ascii_lowercase());
    let parts: Vec<&str> = p.split('*').collect();
    if parts.len() == 1 {
        return p == s;
    }
    let mut rest = s.as_str();
    if !rest.starts_with(parts[0]) {
        return false;
    }
    rest = &rest[parts[0].len()..];
    for (i, part) in parts[1..].iter().enumerate() {
        if i == parts.len() - 2 {
            return rest.ends_with(part);
        }
        match rest.find(part) {
            Some(k) => rest = &rest[k + part.len()..],
            None => return false,
        }
    }
    true
}

fn read(p: &Path) -> Result<String, String> {
    std::fs::read(p).map(|b| String::from_utf8_lossy(&b).into_owned()).map_err(|e| format!("{}: {e}", p.display()))
}

impl World {
    /// Apply an overlay file. The kind comes from the extension: `.toml` manifest, `.map`, `.pwn`/`.inc`/`.txt`,
    /// `.ipl`, `.ide`, `.col`, `.img`. Rebuilds the top-level BVH.
    pub fn apply_overlay(&mut self, path: &Path) -> Result<OverlayReport, String> {
        let mut rep = OverlayReport::default();
        let mut names = self.name_index();
        self.apply_one(path, &mut names, &mut rep, None)?;
        self.finish();
        Ok(rep)
    }

    fn name_index(&self) -> HashMap<String, i32> {
        self.names.iter().map(|(id, n)| (n.clone(), *id)).collect()
    }

    fn apply_one(&mut self, path: &Path, names: &mut HashMap<String, i32>, rep: &mut OverlayReport, col_id: Option<i32>) -> Result<(), String> {
        let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        rep.files += 1;
        let source = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match ext.as_str() {
            "toml" => {
                let text = read(path)?;
                let m: Manifest = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
                let base = path.parent().unwrap_or(Path::new("."));
                rep.files -= 1;
                // definitions first, then models, then placements, then removals
                for e in &m.ide {
                    for f in expand(base, &e.path) {
                        self.apply_one(&f, names, rep, None)?;
                    }
                }
                for e in &m.img {
                    for f in expand(base, &e.path) {
                        self.apply_one(&f, names, rep, None)?;
                    }
                }
                for e in &m.col {
                    for f in expand(base, &e.path) {
                        if f.extension().is_some_and(|x| x.eq_ignore_ascii_case("col")) {
                            self.apply_one(&f, names, rep, e.id)?;
                        }
                    }
                }
                for e in &m.dff {
                    for f in expand(base, &e.path) {
                        if f.extension().is_some_and(|x| x.eq_ignore_ascii_case("dff")) {
                            self.apply_one(&f, names, rep, e.id)?;
                        }
                    }
                }
                for e in m.ipl.iter().chain(&m.map).chain(&m.pawn) {
                    for f in expand(base, &e.path) {
                        self.apply_one(&f, names, rep, None)?;
                    }
                }
                for r in &m.remove {
                    rep.removed += self.remove(r.model, Vec3 { x: r.x, y: r.y, z: r.z }, r.radius, r.interior);
                }
            }
            "ide" => {
                for o in ide::parse(&read(path)?) {
                    names.insert(o.name, o.id);
                }
            }
            "img" => {
                let mut img = Img::open(path).map_err(|e| e.to_string())?;
                let entries = img.entries.clone();
                for e in entries.iter().filter(|e| e.name.ends_with(".col")) {
                    let data = img.read(e).map_err(|x| x.to_string())?;
                    self.add_cols(&data, names, rep, None, &e.name)?;
                }
                for e in entries.iter().filter(|e| e.name.ends_with(".dff")) {
                    if let Some(&id) = names.get(e.name.trim_end_matches(".dff")) {
                        let data = img.read(e).map_err(|x| x.to_string())?;
                        if self.add_dff(id, &data) {
                            rep.shapes += 1;
                        }
                    }
                }
                for e in entries.iter().filter(|e| e.name.ends_with(".ipl")) {
                    let data = img.read(e).map_err(|x| x.to_string())?;
                    if ipl::is_binary(&data) {
                        let list = ipl::parse_binary(&data).map_err(|x| x.to_string())?;
                        self.place_ipl(&list, &e.name, rep);
                    }
                }
            }
            "dff" => {
                let stem = path.file_stem().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
                if let Some(id) = col_id.or_else(|| names.get(&stem).copied()) {
                    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
                    if self.add_dff(id, &data) {
                        rep.shapes += 1;
                    }
                }
            }
            "col" => {
                let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
                self.add_cols(&data, names, rep, col_id, &source)?;
            }
            "ipl" => {
                let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
                let list = if ipl::is_binary(&data) { ipl::parse_binary(&data).map_err(|x| x.to_string())? } else { ipl::parse_text(&String::from_utf8_lossy(&data)) };
                self.place_ipl(&list, &source, rep);
            }
            "map" => self.place_map(&mtamap::parse(&read(path)?), &source, rep),
            "pwn" | "inc" | "txt" | "p" => self.place_map(&pawn::parse(&read(path)?), &source, rep),
            _ => return Err(format!("{}: unknown overlay file type", path.display())),
        }
        Ok(())
    }

    fn add_cols(&mut self, data: &[u8], names: &HashMap<String, i32>, rep: &mut OverlayReport, id: Option<i32>, src: &str) -> Result<(), String> {
        let models = sa_formats::col::parse_archive(data).map_err(|e| format!("{src}: {e}"))?;
        for m in models {
            let Some(id) = id.or_else(|| names.get(&m.name).copied()) else { continue };
            if self.add_model(id, &m.name, &m).is_some() {
                rep.models += 1;
            }
        }
        Ok(())
    }

    fn place_ipl(&mut self, list: &[ipl::IplInst], src: &str, rep: &mut OverlayReport) {
        let source = self.source_index(src);
        for i in list {
            if self.add_instance(i.model, ipl_affine(i.pos, i.rot), i.interior, source, 0) {
                rep.objects += 1;
            } else {
                rep.objects_without_collision += 1;
            }
        }
    }

    fn place_map(&mut self, m: &mtamap::MtaMap, src: &str, rep: &mut OverlayReport) {
        let source = self.source_index(src);
        for o in &m.objects {
            let xf = euler::object_affine(Vec3::from_arr(o.pos), o.rot);
            if self.add_instance(o.model, xf, o.interior, source, 0) {
                rep.objects += 1;
            } else {
                rep.objects_without_collision += 1;
            }
        }
        for r in &m.removals {
            rep.removed += self.remove(r.model, Vec3::from_arr(r.pos), r.radius, r.interior);
        }
    }
}
