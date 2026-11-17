use clap::{Args, Parser, Subcommand};
use sa_world::{place, v3, Filter, Hit, World};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Instant;

#[derive(Parser)]
#[command(name = "sa-geom", version, about = "GTA San Andreas collision queries: ground height, raycasts and slope placement")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
struct WorldOpts {
    /// Use this world file instead of the built-in vanilla world.
    #[arg(long, global = true)]
    world: Option<PathBuf>,
    /// Build the world in memory from a game install instead of using the built-in one.
    #[arg(long, global = true, conflicts_with = "world")]
    gta: Option<PathBuf>,
    /// Overlay to apply (manifest .toml, MTA .map, SA-MP .pwn, .ipl, .col, .img, .ide). Repeatable.
    #[arg(long, global = true)]
    overlay: Vec<PathBuf>,
    /// Interior to query (-1 = every interior). Area 13 objects are seen from every interior.
    #[arg(long, global = true, default_value_t = 0, allow_hyphen_values = true)]
    interior: i32,
}

#[derive(Subcommand)]
enum Cmd {
    /// Ground under (X, Y): height, surface normal, material, and the rotation that lays an object flat on it.
    Ground {
        #[arg(allow_hyphen_values = true)]
        x: f32,
        #[arg(allow_hyphen_values = true)]
        y: f32,
        /// Start the downward ray here (just above the item finds the floor under it, not the roof above it).
        #[arg(long, default_value_t = 1000.0, allow_hyphen_values = true)]
        from: f32,
        /// How far down to search.
        #[arg(long, default_value_t = 1200.0)]
        depth: f32,
        /// Object heading in degrees (GTA convention: 0 faces +Y, 90 faces -X).
        #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
        yaw: f32,
        /// Lift the object this far along the surface normal.
        #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
        lift: f32,
        /// Fit a WIDTH,DEPTH footprint (metres) instead of a single point: highest z of the 4 corners and centre,
        /// normal of the plane through the corners.
        #[arg(long, value_parser = parse_pair, conflicts_with = "model")]
        footprint: Option<(f32, f32)>,
        /// Place this model: its shape (render mesh, else collision bounds) gives the footprint and lifts its
        /// lowest point (not its origin) onto the ground.
        #[arg(long, allow_hyphen_values = true)]
        model: Option<i32>,
        /// The model's rest rotation RX,RY,RZ (GTA Euler degrees) before the yaw, e.g. 90,0,0 for a rifle lying
        /// on its side. The returned rotation includes it.
        #[arg(long, value_parser = parse_triple, requires = "model")]
        rest: Option<[f32; 3]>,
        /// With --model: how far (metres) collision may reach into the model's box before `blocked` reports it.
        /// The box comes from the visual mesh, mirrors and trim included.
        #[arg(long, default_value_t = 0.15)]
        margin: f32,
        #[command(flatten)]
        world: WorldOpts,
    },
    /// What sa-geom knows about models: kind, where their shape comes from, size and how high their origin sits
    /// above their lowest point (in a rest rotation, if given). JSON, one object per model.
    Model {
        #[arg(required = true)]
        ids: Vec<i32>,
        #[arg(long, value_parser = parse_triple)]
        rest: Option<[f32; 3]>,
        #[command(flatten)]
        world: WorldOpts,
    },
    /// Nearest surface on the segment X1 Y1 Z1 -> X2 Y2 Z2.
    Ray {
        #[arg(num_args = 6, allow_hyphen_values = true)]
        coords: Vec<f32>,
        #[command(flatten)]
        world: WorldOpts,
    },
    /// Many ground queries from stdin, one per line: JSON objects {"x","y","z"?,"yaw"?,"lift"?,"w"?,"d"?,"model"?,"rest"?:[rx,ry,rz],"margin"?,"interior"?}
    /// or CSV "x,y[,z[,yaw[,lift]]]" (z = where the downward ray starts, default 1000).
    Batch {
        /// Output format.
        #[arg(long, default_value = "jsonl", value_parser = ["jsonl", "csv"])]
        format: String,
        /// Extra metres above the given z to start each ray (a spawn point stored a little under the floor still hits it).
        #[arg(long, default_value_t = 1.0)]
        above: f32,
        #[command(flatten)]
        world: WorldOpts,
    },
    /// Which headings boxes fit in on the ground, from stdin, one JSON object per line:
    /// {"x","y","z","boxes":[[length,width,height],...],"headings"?:16,"lift"?:0.03,"step"?:0.05,"interior"?}. The ground is found
    /// as `batch` finds it (from z + --above); each box stands on it, centred on the point, tilted to the surface,
    /// its length along the heading. Out: the ground and, per box, a bitmask of the headings k*360/N that are clear.
    Clearance {
        #[arg(long, default_value_t = 1.0)]
        above: f32,
        #[command(flatten)]
        world: WorldOpts,
    },
    /// World statistics, or the objects around a point.
    Info {
        /// List the objects whose bounds come within RADIUS (default 5) of X Y Z.
        #[arg(long, num_args = 3, allow_hyphen_values = true)]
        at: Option<Vec<f32>>,
        #[arg(long, default_value_t = 5.0)]
        radius: f32,
        #[command(flatten)]
        world: WorldOpts,
    },
    /// Write the collision triangles inside a box to a Wavefront OBJ (one group per surface material), for
    /// viewing in Blender or any 3D tool.
    Export {
        /// Box corner X Y Z.
        #[arg(long, num_args = 3, allow_hyphen_values = true)]
        min: Vec<f32>,
        /// Opposite box corner X Y Z.
        #[arg(long, num_args = 3, allow_hyphen_values = true)]
        max: Vec<f32>,
        /// Subtract this point from every vertex (keeps coordinates small for 3D tools).
        #[arg(long, num_args = 3, allow_hyphen_values = true)]
        origin: Option<Vec<f32>>,
        #[arg(short, long)]
        out: PathBuf,
        #[command(flatten)]
        world: WorldOpts,
    },
    /// List every map placement (with or without collision) whose position lies inside a box, from a game
    /// install, as JSON: model id, interior, source IPL and the 4x4 world matrix (column-major).
    Placements {
        #[arg(long)]
        gta: PathBuf,
        #[arg(long, num_args = 3, allow_hyphen_values = true)]
        min: Vec<f32>,
        #[arg(long, num_args = 3, allow_hyphen_values = true)]
        max: Vec<f32>,
        /// Only this interior (-1 = all).
        #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
        interior: i32,
        /// Extend the box by this margin for objects whose position is outside but which may reach inside.
        #[arg(long, default_value_t = 0.0)]
        margin: f32,
    },
    /// Build a world file from a game install (maintainers: regenerates data/vanilla.sagw.zst).
    Bake {
        #[arg(long)]
        gta: PathBuf,
        /// Output; a .zst extension compresses it.
        #[arg(short, long)]
        out: PathBuf,
        #[arg(long, default_value_t = 19)]
        level: i32,
    },
}

fn parse_pair(s: &str) -> Result<(f32, f32), String> {
    let (a, b) = s.split_once(',').ok_or("expected WIDTH,DEPTH")?;
    Ok((a.trim().parse().map_err(|e| format!("{e}"))?, b.trim().parse().map_err(|e| format!("{e}"))?))
}

fn parse_triple(s: &str) -> Result<[f32; 3], String> {
    let v: Vec<f32> = s.split(',').map(|p| p.trim().parse::<f32>().map_err(|e| format!("{e}"))).collect::<Result<_, _>>()?;
    v.try_into().map_err(|_| "expected RX,RY,RZ".to_string())
}

fn kind_name(k: u32) -> &'static str {
    match k {
        sa_world::shape::KIND_VEHICLE => "vehicle",
        sa_world::shape::KIND_WEAPON => "weapon",
        _ => "object",
    }
}

fn model_json(w: &World, id: i32, rest: [f32; 3]) -> Value {
    let mut v = json!({ "model": id, "name": w.names.get(&id).cloned().unwrap_or_default(), "kind": kind_name(sa_world::shape::kind_of(id)) });
    match w.model_rest(id, rest) {
        Some((r, src)) => {
            v["shape"] = json!(src);
            v["base"] = json!(r3(r.base));
            v["size"] = json!([r3(r.size.x), r3(r.size.y), r3(r.size.z)]);
            v["rect"] = json!(r.rect.map(r3));
        }
        None => v["shape"] = json!(null),
    }
    if let Some(b) = w.model_bounds(id) {
        v["collision"] = json!([[r3(b.min.x), r3(b.min.y), r3(b.min.z)], [r3(b.max.x), r3(b.max.y), r3(b.max.z)]]);
    }
    v
}

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("sa-geom: {msg}");
    std::process::exit(2)
}

fn load(o: &WorldOpts) -> World {
    let mut w = if let Some(p) = &o.world {
        World::load_file(p).unwrap_or_else(|e| die(e))
    } else if let Some(g) = &o.gta {
        let (w, _) = sa_world::bake::bake(g, &mut |m| eprintln!("{m}")).unwrap_or_else(|e| die(e));
        w
    } else {
        World::embedded().unwrap_or_else(|e| die(e))
    };
    for ov in &o.overlay {
        match w.apply_overlay(ov) {
            Ok(r) => eprintln!("overlay {}: {r}", ov.display()),
            Err(e) => die(e),
        }
    }
    w
}

fn r3(v: f32) -> f64 {
    ((v as f64) * 10000.0).round() / 10000.0 + 0.0
}

fn hit_json(w: &World, h: &Hit) -> Value {
    json!({
        "hit": true,
        "x": r3(h.pos.x), "y": r3(h.pos.y), "z": r3(h.pos.z),
        "nx": r3(h.normal.x), "ny": r3(h.normal.y), "nz": r3(h.normal.z),
        "material": h.material,
        "material_name": w.material_name(h.material),
        "model": h.model,
        "model_name": w.names.get(&h.model).cloned().unwrap_or_default(),
    })
}

struct GroundQ {
    x: f32,
    y: f32,
    from: f32,
    depth: f32,
    yaw: f32,
    lift: f32,
    footprint: Option<(f32, f32)>,
    model: Option<i32>,
    rest: Option<[f32; 3]>,
    /// How far into the model's box something may reach before it counts as blocking.
    margin: f32,
    interior: i32,
}

fn ground_json(w: &World, q: &GroundQ) -> Value {
    let f = Filter::interior(q.interior);
    let mut gap = None;
    let mut blocked = None;
    let r = match (q.model, q.footprint) {
        (Some(m), _) => w.lay_model(m, q.x, q.y, q.from, q.depth, q.yaw, q.rest.unwrap_or([0.0; 3]), q.lift, f).map(|l| {
            gap = Some(l.gap);
            // body clearance: shrink by `margin` (mirrors and trim stick out of the body), ignore the bottom fifth (at
            // most 0.4 m: wheels, kerbs, the ground)
            let floor = l.rest.map(|r| (r.size.z * 0.2).clamp(0.02, 0.4)).unwrap_or(0.0);
            // a face rising less than this above the body's bottom is a kerb or a step, not an obstacle
            let step = l.rest.map(|r| (r.size.z * 0.2).clamp(0.05, 0.3)).unwrap_or(0.05);
            blocked = Some(w.obstruction(&l, q.margin, floor, step, f));
            (l.hit, l.placement)
        }),
        (None, Some((fw, fd))) => w.footprint(q.x, q.y, q.from, q.depth, q.yaw, fw, fd, f).map(|h| (h, place(h.pos, h.normal, q.yaw, q.lift))),
        (None, None) => w.ground(q.x, q.y, q.from, q.depth, f).map(|h| (h, place(h.pos, h.normal, q.yaw, q.lift))),
    };
    let water = w.water_z(q.x, q.y);
    match r {
        Some((h, p)) => {
            let mut v = hit_json(w, &h);
            v["rot"] = json!([r3(p.rot[0]), r3(p.rot[1]), r3(p.rot[2])]);
            v["pos"] = json!([r3(p.pos.x), r3(p.pos.y), r3(p.pos.z)]);
            if let Some(m) = q.model {
                let mj = model_json(w, m, q.rest.unwrap_or([0.0; 3]));
                v["shape"] = mj["shape"].clone();
                v["gap"] = json!(gap.map(r3));
                v["blocked"] = match blocked.flatten() {
                    Some(b) => json!({ "model": b.model, "name": w.names.get(&b.model).cloned().unwrap_or_default(), "at": [r3(b.at.x), r3(b.at.y), r3(b.at.z)], "part": b.part }),
                    None => Value::Null,
                };
                v["base"] = mj["base"].clone();
                if let Some(b) = w.model_bounds(m) {
                    v["bounds"] = json!([[r3(b.min.x), r3(b.min.y), r3(b.min.z)], [r3(b.max.x), r3(b.max.y), r3(b.max.z)]]);
                }
            }
            v["water_z"] = json!(water.map(r3));
            v["underwater"] = json!(water.is_some_and(|wz| wz > h.pos.z));
            v
        }
        None => json!({ "hit": false, "x": r3(q.x), "y": r3(q.y), "water_z": water.map(r3) }),
    }
}

fn main() {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Ground { x, y, from, depth, yaw, lift, footprint, model, rest, margin, world } => {
            let w = load(&world);
            let v = ground_json(&w, &GroundQ { x, y, from, depth, yaw, lift, footprint, model, rest, margin, interior: world.interior });
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
            if v["hit"] == false {
                std::process::exit(1);
            }
        }
        Cmd::Model { ids, rest, world } => {
            let w = load(&world);
            let list: Vec<Value> = ids.iter().map(|&id| model_json(&w, id, rest.unwrap_or([0.0; 3]))).collect();
            println!("{}", serde_json::to_string_pretty(&list).unwrap());
        }
        Cmd::Ray { coords, world } => {
            let w = load(&world);
            let h = w.raycast(v3(coords[0], coords[1], coords[2]), v3(coords[3], coords[4], coords[5]), Filter::interior(world.interior));
            match h {
                Some(h) => println!("{}", serde_json::to_string_pretty(&hit_json(&w, &h)).unwrap()),
                None => {
                    println!("{}", json!({ "hit": false }));
                    std::process::exit(1);
                }
            }
        }
        Cmd::Batch { format, above, world } => {
            let w = load(&world);
            let stdin = std::io::stdin();
            let mut out = std::io::BufWriter::new(std::io::stdout().lock());
            if format == "csv" {
                writeln!(out, "x,y,z,nx,ny,nz,rx,ry,rz,material,model").unwrap();
            }
            for line in stdin.lock().lines() {
                let line = line.unwrap_or_else(|e| die(e));
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let (q, input) = if line.starts_with('{') {
                    let v: Value = serde_json::from_str(line).unwrap_or_else(|e| die(format!("bad JSON line: {e}")));
                    let g = |k: &str| v.get(k).and_then(|x| x.as_f64()).map(|x| x as f32);
                    let fp = match (g("w"), g("d")) {
                        (Some(a), Some(b)) => Some((a, b)),
                        _ => None,
                    };
                    let q = GroundQ {
                        x: g("x").unwrap_or_else(|| die("missing x")),
                        y: g("y").unwrap_or_else(|| die("missing y")),
                        from: g("z").map(|z| z + above).unwrap_or(1000.0),
                        depth: g("depth").unwrap_or(1200.0),
                        yaw: g("yaw").unwrap_or(0.0),
                        lift: g("lift").unwrap_or(0.0),
                        footprint: fp,
                        model: v.get("model").and_then(|x| x.as_i64()).map(|m| m as i32),
                        margin: g("margin").unwrap_or(0.15),
                        rest: v.get("rest").and_then(|r| r.as_array()).and_then(|a| {
                            let f: Vec<f32> = a.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect();
                            f.try_into().ok()
                        }),
                        interior: g("interior").map(|i| i as i32).unwrap_or(world.interior),
                    };
                    (q, Some(v))
                } else {
                    let n: Vec<f32> = line.split(',').map(|s| s.trim().parse().unwrap_or_else(|_| die(format!("bad CSV line: {line}")))).collect();
                    if n.len() < 2 {
                        die(format!("bad CSV line: {line}"));
                    }
                    let q = GroundQ {
                        x: n[0],
                        y: n[1],
                        from: n.get(2).map(|z| z + above).unwrap_or(1000.0),
                        depth: 1200.0,
                        yaw: n.get(3).copied().unwrap_or(0.0),
                        lift: n.get(4).copied().unwrap_or(0.0),
                        footprint: None,
                        model: None,
                        rest: None,
                        margin: 0.15,
                        interior: world.interior,
                    };
                    (q, None)
                };
                let mut v = ground_json(&w, &q);
                if format == "csv" {
                    if v["hit"] == true {
                        let p = &v["pos"];
                        writeln!(out, "{},{},{},{},{},{},{},{},{},{},{}", p[0], p[1], p[2], v["nx"], v["ny"], v["nz"], v["rot"][0], v["rot"][1], v["rot"][2], v["material"], v["model"]).unwrap();
                    } else {
                        writeln!(out, "{},{},,,,,,,,,", q.x, q.y).unwrap();
                    }
                } else {
                    if let Some(Value::Object(inp)) = input {
                        v["input"] = Value::Object(inp);
                    }
                    writeln!(out, "{}", v).unwrap();
                }
            }
        }
        Cmd::Clearance { above, world } => {
            let w = load(&world);
            let stdin = std::io::stdin();
            let mut out = std::io::BufWriter::new(std::io::stdout().lock());
            for line in stdin.lock().lines() {
                let line = line.unwrap_or_else(|e| die(e));
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let v: Value = serde_json::from_str(line).unwrap_or_else(|e| die(format!("bad JSON line: {e}")));
                let g = |k: &str| v.get(k).and_then(|x| x.as_f64()).map(|x| x as f32);
                let (x, y) = (g("x").unwrap_or_else(|| die("missing x")), g("y").unwrap_or_else(|| die("missing y")));
                let from = g("z").map(|z| z + above).unwrap_or(1000.0);
                let f = Filter::interior(g("interior").map(|i| i as i32).unwrap_or(world.interior));
                let n = g("headings").map(|n| n as u32).unwrap_or(16).clamp(1, 32);
                let lift = g("lift").unwrap_or(0.03);
                let step = g("step").unwrap_or(0.05);
                let boxes: Vec<[f32; 3]> = v.get("boxes").and_then(|b| b.as_array()).map(|a| a.iter().filter_map(|b| {
                    let d: Vec<f32> = b.as_array()?.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect();
                    d.try_into().ok()
                }).collect()).unwrap_or_default();
                let Some(hit) = w.ground(x, y, from, g("depth").unwrap_or(1200.0), f) else {
                    writeln!(out, "{}", json!({ "hit": false, "input": v })).unwrap();
                    continue;
                };
                let mut masks = vec![0u32; boxes.len()];
                for k in 0..n {
                    let yaw = k as f32 * 360.0 / n as f32;
                    let p = place(hit.pos, hit.normal, yaw, 0.0);
                    // biggest first: a heading clear for a box is clear for every box inside it
                    let mut order: Vec<usize> = (0..boxes.len()).collect();
                    order.sort_by(|a, b| (boxes[*b][0] * boxes[*b][1] * boxes[*b][2]).total_cmp(&(boxes[*a][0] * boxes[*a][1] * boxes[*a][2])));
                    let mut clear: Vec<usize> = Vec::new();
                    for &i in &order {
                        let [l, wd, h] = boxes[i];
                        let inside = clear.iter().any(|&j| boxes[j][0] >= l && boxes[j][1] >= wd && boxes[j][2] >= h);
                        let free = inside || {
                            let half = v3(wd * 0.5, l * 0.5, h * 0.5);
                            w.box_clear(hit.pos + p.up * (lift + h * 0.5), p.right, p.forward, p.up, half, Some(step), f).is_none()
                        };
                        if free {
                            masks[i] |= 1 << k;
                            clear.push(i);
                        }
                    }
                }
                writeln!(out, "{}", json!({ "hit": true, "z": r3(hit.pos.z), "nx": r3(hit.normal.x), "ny": r3(hit.normal.y), "nz": r3(hit.normal.z), "masks": masks, "input": v })).unwrap();
            }
        }
        Cmd::Info { at, radius, world } => {
            let t = Instant::now();
            let w = load(&world);
            let load_ms = t.elapsed().as_millis();
            match at {
                Some(p) => {
                    let mut list: Vec<Value> = w
                        .instances_near(v3(p[0], p[1], p[2]), radius)
                        .into_iter()
                        .map(|i| {
                            let inst = &w.instances[i as usize];
                            let id = w.models[inst.model as usize].id;
                            json!({
                                "model": id,
                                "name": w.names.get(&id).cloned().unwrap_or_default(),
                                "pos": [r3(inst.xf.t.x), r3(inst.xf.t.y), r3(inst.xf.t.z)],
                                "interior": inst.interior,
                                "source": w.sources.get(inst.source as usize).cloned().unwrap_or_default(),
                            })
                        })
                        .collect();
                    list.sort_by(|a, b| a["model"].as_i64().cmp(&b["model"].as_i64()));
                    println!("{}", serde_json::to_string_pretty(&list).unwrap());
                }
                None => {
                    let v = json!({
                        "models": w.models.len(),
                        "triangles": w.tris.len(),
                        "spheres": w.spheres.len(),
                        "instances": w.instances.len(),
                        "live_instances": w.live_instances(),
                        "water_polygons": w.water.len(),
                        "shapes": w.shapes.len(),
                        "materials": w.materials.len(),
                        "embedded_bytes": sa_world::embedded_bytes().map(|b| b.len()),
                        "load_ms": load_ms,
                    });
                    println!("{}", serde_json::to_string_pretty(&v).unwrap());
                }
            }
        }
        Cmd::Export { min, max, origin, out, world } => {
            let w = load(&world);
            let q = sa_world::math::Aabb { min: v3(min[0], min[1], min[2]).min(v3(max[0], max[1], max[2])), max: v3(min[0], min[1], min[2]).max(v3(max[0], max[1], max[2])) };
            let o = origin.map(|o| v3(o[0], o[1], o[2])).unwrap_or(sa_world::Vec3::ZERO);
            let mut tris = w.triangles_in(q, Filter::interior(world.interior));
            tris.sort_by_key(|t| t.1);
            let mut f = std::io::BufWriter::new(std::fs::File::create(&out).unwrap_or_else(|e| die(format!("{}: {e}", out.display()))));
            writeln!(f, "# sa-geom collision export: {} triangles", tris.len()).unwrap();
            let mut current = u32::MAX;
            for (i, (t, mat, _)) in tris.iter().enumerate() {
                if *mat != current {
                    current = *mat;
                    let name = w.material_name(*mat);
                    writeln!(f, "g {}\nusemtl {}", if name.is_empty() { format!("mat{mat}") } else { name.to_string() }, if name.is_empty() { format!("mat{mat}") } else { name.to_string() }).unwrap();
                }
                for p in t {
                    let p = *p - o;
                    writeln!(f, "v {:.4} {:.4} {:.4}", p.x, p.y, p.z).unwrap();
                }
                let k = i * 3 + 1;
                writeln!(f, "f {} {} {}", k, k + 1, k + 2).unwrap();
            }
            eprintln!("wrote {} ({} triangles)", out.display(), tris.len());
        }
        Cmd::Placements { gta, min, max, interior, margin } => {
            let all = sa_world::bake::map_placements(&gta, &mut |m| eprintln!("{m}")).unwrap_or_else(|e| die(e));
            let (lo, hi) = (v3(min[0], min[1], min[2]).min(v3(max[0], max[1], max[2])), v3(min[0], min[1], min[2]).max(v3(max[0], max[1], max[2])));
            let m = v3(margin, margin, margin);
            let (lo, hi) = (lo - m, hi + m);
            let list: Vec<Value> = all
                .iter()
                .filter(|p| interior < 0 || p.interior == interior)
                .filter(|p| { let t = p.xf.t; t.x >= lo.x && t.x <= hi.x && t.y >= lo.y && t.y <= hi.y && t.z >= lo.z && t.z <= hi.z })
                .map(|p| {
                    let x = &p.xf;
                    json!({
                        "model": p.model, "interior": p.interior, "source": p.source,
                        "matrix": [[x.x.x, x.x.y, x.x.z, 0.0], [x.y.x, x.y.y, x.y.z, 0.0], [x.z.x, x.z.y, x.z.z, 0.0], [x.t.x, x.t.y, x.t.z, 1.0]],
                    })
                })
                .collect();
            println!("{}", serde_json::to_string(&list).unwrap());
            eprintln!("{} placements", list.len());
        }
        Cmd::Bake { gta, out, level } => {
            let t = Instant::now();
            let (w, rep) = sa_world::bake::bake(&gta, &mut |m| eprintln!("{m}")).unwrap_or_else(|e| die(e));
            eprintln!("{rep:#?}");
            eprintln!("built in {:.1}s: {} models, {} triangles, {} instances", t.elapsed().as_secs_f32(), w.models.len(), w.tris.len(), w.instances.len());
            let raw = w.to_bytes();
            let data = if out.extension().is_some_and(|e| e == "zst") { zstd::encode_all(&raw[..], level).unwrap_or_else(|e| die(e)) } else { raw.clone() };
            std::fs::write(&out, &data).unwrap_or_else(|e| die(format!("{}: {e}", out.display())));
            eprintln!("wrote {} ({} bytes, {} uncompressed)", out.display(), data.len(), raw.len());
        }
    }
}
