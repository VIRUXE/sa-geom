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
        /// Place this model: its collision bounds give the footprint and lift its base (not its origin) onto
        /// the ground.
        #[arg(long, allow_hyphen_values = true)]
        model: Option<i32>,
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
    /// Many ground queries from stdin, one per line: JSON objects {"x","y","z"?,"yaw"?,"lift"?,"w"?,"d"?,"model"?,"interior"?}
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
    interior: i32,
}

fn ground_json(w: &World, q: &GroundQ) -> Value {
    let f = Filter::interior(q.interior);
    let r = match (q.model, q.footprint) {
        (Some(m), _) => w.place_model(m, q.x, q.y, q.from, q.depth, q.yaw, q.lift, f).map(|(p, h)| (h, p)),
        (None, Some((fw, fd))) => w.footprint(q.x, q.y, q.from, q.depth, q.yaw, fw, fd, f).map(|h| (h, place(h.pos, h.normal, q.yaw, q.lift))),
        (None, None) => w.ground(q.x, q.y, q.from, q.depth, f).map(|h| (h, place(h.pos, h.normal, q.yaw, q.lift))),
    };
    let water = w.water_z(q.x, q.y);
    match r {
        Some((h, p)) => {
            let mut v = hit_json(w, &h);
            v["rot"] = json!([r3(p.rot[0]), r3(p.rot[1]), r3(p.rot[2])]);
            v["pos"] = json!([r3(p.pos.x), r3(p.pos.y), r3(p.pos.z)]);
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
        Cmd::Ground { x, y, from, depth, yaw, lift, footprint, model, world } => {
            let w = load(&world);
            let v = ground_json(&w, &GroundQ { x, y, from, depth, yaw, lift, footprint, model, interior: world.interior });
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
            if v["hit"] == false {
                std::process::exit(1);
            }
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
                        "materials": w.materials.len(),
                        "embedded_bytes": sa_world::embedded_bytes().map(|b| b.len()),
                        "load_ms": load_ms,
                    });
                    println!("{}", serde_json::to_string_pretty(&v).unwrap());
                }
            }
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
