//! Build a world from a GTA San Andreas install: gta.dat/default.dat -> IDEs, IPLs (text and binary streams in the
//! IMGs) and COL archives in the IMGs.

use crate::math::{Affine, Quat, Vec3};
use crate::world::{World, WaterRec, INST_BASE};
use sa_formats::{dat::DatFile, ide, img::Img, ipl, resolve_ci, water};
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Debug, Default)]
pub struct BakeReport {
    pub ide_objects: usize,
    pub col_models: usize,
    pub col_unmatched: usize,
    pub ipl_instances: usize,
    pub lod_skipped: usize,
    pub no_collision: usize,
    pub placed: usize,
    pub water_polys: usize,
    pub script_ipls_skipped: usize,
    /// Model ids placed without collision, with their placement count (most frequent first, top 20).
    pub no_collision_models: Vec<(i32, usize)>,
}

fn read_text(gta: &Path, rel: &str) -> Option<String> {
    let p = resolve_ci(gta, rel)?;
    std::fs::read(&p).ok().map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// IPL quaternions store the inverse rotation.
pub fn ipl_affine(pos: [f32; 3], q: [f32; 4]) -> Affine {
    let q = Quat { x: q[0], y: q[1], z: q[2], w: q[3] }.conj().normalize();
    Affine::from_quat(q, Vec3::from_arr(pos))
}

pub fn bake(gta: &Path, log: &mut dyn FnMut(&str)) -> Result<(World, BakeReport), String> {
    let mut rep = BakeReport::default();
    let mut dat = DatFile::default();
    for f in ["data/default.dat", "data/gta.dat"] {
        let t = read_text(gta, f).ok_or_else(|| format!("{f} not found under {}", gta.display()))?;
        DatFile::parse(&t, &mut dat);
    }

    // model names
    let mut name_to_id: HashMap<String, i32> = HashMap::new();
    for f in &dat.ide {
        match read_text(gta, f) {
            Some(t) => {
                for o in ide::parse(&t) {
                    name_to_id.insert(o.name, o.id);
                    rep.ide_objects += 1;
                }
            }
            None => log(&format!("warning: IDE {f} not found")),
        }
    }

    let mut w = World::default();

    // materials
    if let Some(t) = read_text(gta, "data/surfinfo.dat") {
        for l in t.lines() {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            if let Some(name) = l.split_whitespace().next() {
                w.materials.push(name.to_string());
            }
        }
    }

    // IMGs: collision and binary stream IPLs
    let mut imgs = vec!["models/gta3.img".to_string(), "models/gta_int.img".to_string()];
    imgs.extend(dat.img.iter().cloned());
    let mut streams: Vec<(String, Vec<ipl::IplInst>)> = Vec::new();
    let mut seen_img = HashSet::new();
    for rel in &imgs {
        let Some(path) = resolve_ci(gta, rel) else { continue };
        if !seen_img.insert(path.clone()) {
            continue;
        }
        let mut img = match Img::open(&path) {
            Ok(i) => i,
            Err(e) => {
                log(&format!("warning: {e}"));
                continue;
            }
        };
        let entries = img.entries.clone();
        for e in &entries {
            if e.name.ends_with(".col") {
                let data = img.read(e).map_err(|x| x.to_string())?;
                match sa_formats::col::parse_archive(&data) {
                    Ok(models) => {
                        for m in models {
                            match name_to_id.get(&m.name) {
                                Some(&id) => {
                                    if w.add_model(id, &m.name, &m).is_some() {
                                        rep.col_models += 1;
                                    }
                                }
                                None => rep.col_unmatched += 1,
                            }
                        }
                    }
                    Err(x) => log(&format!("warning: {}: {x}", e.name)),
                }
            } else if e.name.ends_with(".ipl") {
                let data = img.read(e).map_err(|x| x.to_string())?;
                if ipl::is_binary(&data) {
                    match ipl::parse_binary(&data) {
                        Ok(v) => streams.push((e.name.clone(), v)),
                        Err(x) => log(&format!("warning: {}: {x}", e.name)),
                    }
                }
            }
        }
    }

    // text IPLs, keyed by lowercased stem for stream LOD links
    let mut texts: Vec<(String, Vec<ipl::IplInst>)> = Vec::new();
    for f in &dat.ipl {
        let Some(t) = read_text(gta, f) else {
            log(&format!("warning: IPL {f} not found"));
            continue;
        };
        let stem = Path::new(&f.replace('\\', "/")).file_stem().map(|s| s.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        texts.push((stem, ipl::parse_text(&t)));
    }
    // Binary IPLs in the IMGs that are not streams of a gta.dat IPL (barriers1/2, crack, carter, truthsfarm) are
    // groups the single-player script switches on and off; multiplayer worlds never load them.
    let stems: HashSet<String> = texts.iter().map(|(s, _)| s.clone()).collect();
    let stream_base = |name: &str| name.trim_end_matches(".ipl").split("_stream").next().unwrap_or("").to_string();
    streams.retain(|(name, _)| {
        let keep = name.contains("_stream") && stems.contains(&stream_base(name));
        if !keep {
            rep.script_ipls_skipped += 1;
        }
        keep
    });
    let mut lods: HashMap<String, HashSet<i32>> = HashMap::new();
    for (stem, v) in &texts {
        let set = lods.entry(stem.clone()).or_default();
        set.extend(v.iter().filter(|i| i.lod >= 0).map(|i| i.lod));
    }
    for (name, v) in &streams {
        let set = lods.entry(stream_base(name)).or_default();
        set.extend(v.iter().filter(|i| i.lod >= 0).map(|i| i.lod));
    }

    let mut missing: HashMap<i32, usize> = HashMap::new();
    let mut place = |w: &mut World, src: &str, list: &[ipl::IplInst], lod_set: Option<&HashSet<i32>>, rep: &mut BakeReport| {
        let source = w.source_index(src);
        for (k, i) in list.iter().enumerate() {
            rep.ipl_instances += 1;
            if lod_set.is_some_and(|s| s.contains(&(k as i32))) {
                rep.lod_skipped += 1;
                continue;
            }
            if w.add_instance(i.model, ipl_affine(i.pos, i.rot), i.interior, source, INST_BASE) {
                rep.placed += 1;
            } else {
                rep.no_collision += 1;
                *missing.entry(i.model).or_default() += 1;
            }
        }
    };
    for (stem, v) in &texts {
        place(&mut w, &format!("{stem}.ipl"), v, lods.get(stem), &mut rep);
    }
    for (name, v) in &streams {
        place(&mut w, name, v, None, &mut rep);
    }

    let mut missing: Vec<(i32, usize)> = missing.into_iter().collect();
    missing.sort_by(|a, b| b.1.cmp(&a.1));
    missing.truncate(20);
    rep.no_collision_models = missing;

    if let Some(t) = read_text(gta, "data/water.dat") {
        for p in water::parse(&t) {
            let mut pts = [Vec3::ZERO; 4];
            for (i, c) in p.corners.iter().enumerate() {
                pts[i] = Vec3::from_arr(*c);
            }
            w.water.push(WaterRec { pts, n: p.corners.len() as u32 });
            rep.water_polys += 1;
        }
    }

    w.finish();
    Ok((w, rep))
}
