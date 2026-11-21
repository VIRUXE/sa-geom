//! Timings for live edits on the embedded world: `cargo run --release -p sa-world --example live_bench`.

use sa_world::live::ObjectRow;
use sa_world::{v3, Filter, World};
use std::time::Instant;

fn main() {
    let t = Instant::now();
    let mut w = World::embedded().expect("embedded world");
    println!("loaded in {} ms, {} instances", t.elapsed().as_millis(), w.live_instances());
    let mut rng = 0x9E37_79B9u32;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 17;
        rng ^= rng << 5;
        rng
    };
    let mut pts = Vec::new();
    for _ in 0..200_000 {
        pts.push(((next() % 6000) as f32 - 3000.0, (next() % 6000) as f32 - 3000.0));
    }
    let bench = |w: &World, label: &str| {
        let t = Instant::now();
        let mut hits = 0;
        for &(x, y) in &pts {
            if w.ground(x, y, 1000.0, 1200.0, Filter::default()).is_some() {
                hits += 1;
            }
        }
        let g = t.elapsed().as_secs_f64() * 1e9 / pts.len() as f64;
        let t = Instant::now();
        for &(x, y) in pts.iter().take(50_000) {
            let _ = w.raycast(v3(x, y, 20.0), v3(x + 80.0, y + 30.0, 15.0), Filter::default());
        }
        let r = t.elapsed().as_secs_f64() * 1e9 / 50_000.0;
        println!("{label}: ground {g:.0} ns ({hits} hits / {}), horizontal 85 m raycast {r:.0} ns", pts.len());
    };
    bench(&w, "vanilla");
    let mut st = World::embedded().unwrap();
    // ~26k rows of map models placed at random
    let ids: Vec<i32> = {
        let mut v: Vec<i32> = w.by_id.keys().copied().filter(|id| (600..18630).contains(id)).collect();
        v.sort();
        v
    };
    let rows: Vec<ObjectRow> = (0..26_000)
        .map(|_| {
            let (x, y) = ((next() % 6000) as f32 - 3000.0, (next() % 6000) as f32 - 3000.0);
            ObjectRow::new(ids[next() as usize % ids.len()], [x, y, (next() % 40) as f32], [0.0, 0.0, (next() % 360) as f32], 0)
        })
        .collect();
    for r in &rows {
        st.add_instance(r.model, sa_world::euler::object_affine(sa_world::Vec3::from_arr(r.pos), r.rot), 0, 0, 0);
    }
    st.finish();
    bench(&st, "+26k as static map objects (one BVH)");
    drop(st);
    let t = Instant::now();
    let (hs, _) = w.add_objects(&rows, 0);
    println!("add_objects 26000 rows: {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
    let hs: Vec<u32> = hs.into_iter().flatten().collect();
    bench(&w, "+26k live objects");
    let t = Instant::now();
    w.rebuild_dyn();
    println!("rebuild of the live BVH: {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
    let n = 10_000;
    let t = Instant::now();
    let mut worst = 0f64;
    for k in 0..n {
        let h = hs[next() as usize % hs.len()];
        let s = Instant::now();
        let p = w.object(h).unwrap().row.pos;
        w.move_object(h, [p[0] + 0.5, p[1], p[2]], [0.0, 0.0, (k % 360) as f32]);
        let e = s.elapsed().as_secs_f64();
        if e > 0.005 {
            println!("  slow move {k}: {:.1} ms", e * 1e3);
        }
        worst = worst.max(e);
    }
    println!("move_object: {:.1} us mean (rebuilds included), worst {:.1} ms", t.elapsed().as_secs_f64() * 1e6 / n as f64, worst * 1e3);
    let t = Instant::now();
    for _ in 0..n {
        let h = hs[next() as usize % hs.len()];
        w.move_object(h, [0.0, 0.0, 0.0], [0.0; 3]);
    }
    let _ = t;
    w.rebuild_dyn();
    let p = w.pending.len();
    // pending at its fullest
    for _ in 0..sa_world::world::PENDING_MAX {
        let h = hs[next() as usize % hs.len()];
        let pos = w.object(h).unwrap().row.pos;
        w.move_object(h, pos, [0.0; 3]);
    }
    println!("pending {} -> {}, recent {}", p, w.pending.len(), w.recent_order.len());
    bench(&w, "+26k live, pending full");
    let t = Instant::now();
    for k in 0..1000 {
        w.remove_world_model(sa_world::live::Removal { model: ids[k % ids.len()], pos: [0.0, 0.0, 0.0], radius: 3000.0, interior: -1, owner: 0 });
    }
    println!("remove_world_model: {:.1} us each", t.elapsed().as_secs_f64() * 1e6 / 1000.0);
}
