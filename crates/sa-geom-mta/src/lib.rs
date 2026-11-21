//! MTA:SA server module `ml_sageom`: server-side ground height, raycasts and slope placement against the
//! built-in GTA San Andreas collision world.
//!
//! Lua API (registered in every resource):
//!   sageomIsReady() -> bool
//!   sageomGround(x, y [, zFrom = 1000, interior = 0, depth = 1200]) -> z, nx, ny, nz, material, model | false
//!   sageomRaycast(x1, y1, z1, x2, y2, z2 [, interior = 0]) -> true, x, y, z, nx, ny, nz, material, model | false
//!   sageomPlace(x, y, zFrom, yaw [, lift = 0, interior = 0, width = 0, depth = 0]) -> x, y, z, rx, ry, rz, nx, ny, nz | false
//!   sageomPlaceModel(model, x, y, zFrom, yaw [, lift = 0, interior = 0]) -> x, y, z, rx, ry, rz, nx, ny, nz | false
//!   sageomGetModelBounds(model) -> minX, minY, minZ, maxX, maxY, maxZ | false
//!   sageomGroundMany({ {x, y [, zFrom]}, ... } [, interior = 0]) -> { {z, nx, ny, nz} | false, ... } | false
//!   sageomWaterLevel(x, y) -> z | false
//!   sageomAddOverlay(path) -> true, summary | false, error   (path relative to the server root)
//!   sageomReload([overlayPath]) -> true | false, reason   (starts rebuilding the world plus that overlay in the
//!     background; the old world and every overlay applied to it are replaced once it is built)
//!   sageomReloadStatus() -> generation, busy, ok | false, message | false   (generation counts swapped-in worlds;
//!     ok and message describe the last finished reload, false before any)
//!
//! Live edits (apply at once, survive sageomReload; owned by the calling resource and dropped when it stops):
//!   sageomAddObject(model, x, y, z [, rx, ry, rz, interior, sx, sy, sz]) -> handle | false   (false: no collision
//!     for model; rotation in degrees as an MTA object's / .map's; scale as setObjectScale, sy and sz default to sx)
//!   sageomAddObjects({ {model, x, y, z [, rx, ry, rz, interior, sx, sy, sz]}, ... }) -> { handle | false, ... } | false
//!   sageomMoveObject(handle, x, y, z [, rx, ry, rz]) -> bool
//!   sageomSetObjectScale(handle, sx [, sy, sz]) -> bool
//!   sageomSetObjectModel(handle, model) -> bool   (false with a valid handle: the model has no collision; the
//!     object keeps its handle and gets collision once one is loaded for that id)
//!   sageomRemoveObject(handle) -> bool
//!   sageomRemoveWorldModel(model, radius, x, y, z [, interior = -1]) -> true, count | false   (base-game
//!     instances, as MTA's removeWorldModel; live objects are never removed by it)
//!   sageomRestoreWorldModel(model, radius, x, y, z [, interior = -1]) -> true, count | false
//!   sageomReplaceCOL(colData, id) -> true | false, error   (raw .col bytes; every instance of id, now and later)
//!   sageomRestoreCOL(id) -> bool
//!   sageomGetStats() -> { objects, objectsWithoutCollision, removals, removedInstances, cols, pending, liveTree,
//!     instances, generation, nextHandle, reloading } | false

mod lua;

use lua::{lua_State, Lua, LUA_TTABLE};
use sa_world::live::{ObjectRow, Op, Owner, Removal};
use sa_world::{place, shared, v3, Filter, Hit};
use std::ffi::{c_char, c_float, c_int, c_void, CString};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;

const NAME: &[u8] = b"sa-geom\0";
const AUTHOR: &[u8] = b"sa-geom contributors\0";
const MAX_INFO_LENGTH: usize = 128;
const SEARCH_DEPTH: f32 = 1200.0;

/// ILuaModuleManager10*: a C++ object; its first word is the vtable.
static MANAGER: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

type PrintfFn = unsafe extern "C" fn(*mut c_void, *const c_char, ...);
// C++ member functions: `this` first. 32-bit MSVC uses thiscall for non-variadic members; variadic ones (Printf)
// and every other platform pass `this` like a normal first argument.
#[cfg(all(windows, target_arch = "x86"))]
type RegisterFn = unsafe extern "thiscall" fn(*mut c_void, *mut lua_State, *const c_char, lua::lua_CFunction) -> bool;
#[cfg(not(all(windows, target_arch = "x86")))]
type RegisterFn = unsafe extern "C" fn(*mut c_void, *mut lua_State, *const c_char, lua::lua_CFunction) -> bool;

unsafe fn vtable_entry(slot: usize) -> Option<*const c_void> {
    let m = MANAGER.load(Ordering::Acquire);
    if m.is_null() {
        return None;
    }
    let vt = *(m as *const *const *const c_void);
    Some(*vt.add(slot))
}

/// ILuaModuleManager::Printf (vtable slot 2).
fn log(msg: &str) {
    let m = MANAGER.load(Ordering::Acquire);
    let Ok(c) = CString::new(format!("[sa-geom] {msg}\n")) else { return };
    unsafe {
        if let Some(f) = vtable_entry(2) {
            let f: PrintfFn = std::mem::transmute(f);
            f(m, c"%s".as_ptr(), c.as_ptr());
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn InitModule(manager: *mut c_void, name: *mut c_char, author: *mut c_char, version: *mut c_float) -> bool {
    MANAGER.store(manager, Ordering::Release);
    std::ptr::copy_nonoverlapping(NAME.as_ptr() as *const c_char, name, NAME.len().min(MAX_INFO_LENGTH));
    std::ptr::copy_nonoverlapping(AUTHOR.as_ptr() as *const c_char, author, AUTHOR.len().min(MAX_INFO_LENGTH));
    *version = 0.1;
    if lua::api().is_none() {
        log("could not resolve the Lua API from the server (deathmatch module / lua5.1.dll)");
        return false;
    }
    let file = std::env::var_os("SAGEOM_WORLD").map(std::path::PathBuf::from);
    shared::start_loading(file, |r| match r {
        Ok(m) => log(&m),
        Err(e) => log(&format!("world failed to load: {e}")),
    });
    true
}

#[no_mangle]
pub unsafe extern "C" fn RegisterFunctions(l: *mut lua_State) {
    let m = MANAGER.load(Ordering::Acquire);
    if m.is_null() || l.is_null() {
        return;
    }
    let Some(f) = vtable_entry(3) else { return };
    let register: RegisterFn = std::mem::transmute(f);
    let fns: [(&std::ffi::CStr, lua::lua_CFunction); 22] = [
        (c"sageomPlaceModel", l_place_model),
        (c"sageomGetModelBounds", l_model_bounds),
        (c"sageomIsReady", l_is_ready),
        (c"sageomGround", l_ground),
        (c"sageomRaycast", l_raycast),
        (c"sageomPlace", l_place),
        (c"sageomGroundMany", l_ground_many),
        (c"sageomWaterLevel", l_water),
        (c"sageomAddOverlay", l_add_overlay),
        (c"sageomReload", l_reload),
        (c"sageomReloadStatus", l_reload_status),
        (c"sageomAddObject", l_add_object),
        (c"sageomAddObjects", l_add_objects),
        (c"sageomMoveObject", l_move_object),
        (c"sageomSetObjectModel", l_set_object_model),
        (c"sageomSetObjectScale", l_set_object_scale),
        (c"sageomRemoveObject", l_remove_object),
        (c"sageomRemoveWorldModel", l_remove_world_model),
        (c"sageomRestoreWorldModel", l_restore_world_model),
        (c"sageomReplaceCOL", l_replace_col),
        (c"sageomRestoreCOL", l_restore_col),
        (c"sageomGetStats", l_stats),
    ];
    for (name, func) in fns {
        register(m, l, name.as_ptr(), func);
    }
}

#[no_mangle]
pub extern "C" fn DoPulse() -> bool {
    true
}

#[no_mangle]
pub extern "C" fn ShutdownModule() -> bool {
    true
}

#[no_mangle]
pub extern "C" fn ResourceStopping(_l: *mut lua_State) -> bool {
    true
}

#[no_mangle]
pub extern "C" fn ResourceStopped(l: *mut lua_State) -> bool {
    // the stopped resource's live edits go with it
    let o = owner(l);
    let r = std::panic::catch_unwind(|| {
        shared::edit(|w| {
            let n = w.drop_owner(o);
            (n, if n > 0 { vec![Op::DropOwner(o)] } else { Vec::new() })
        })
    });
    if let Ok(Some(n)) = r {
        if n > 0 {
            log(&format!("dropped {n} live edits of a stopped resource"));
        }
    }
    true
}

/// Run a Lua callback, turning a panic into `false` instead of unwinding into the server.
fn guard(l: *mut lua_State, f: impl FnOnce() -> c_int) -> c_int {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(n) => n,
        Err(_) => {
            log("internal error in a sageom function (see stderr)");
            match lua_of(l) {
                Some(s) => fail(&s),
                None => 0,
            }
        }
    }
}

fn lua_of(l: *mut lua_State) -> Option<Lua> {
    lua::api().map(|a| Lua { l, a })
}

fn filter(i: Option<f64>) -> Filter {
    Filter::interior(i.map(|v| v as i32).unwrap_or(0))
}

/// Push `false` and return 1.
fn fail(s: &Lua) -> c_int {
    s.push_bool(false);
    1
}

unsafe extern "C" fn l_is_ready(l: *mut lua_State) -> c_int {
    guard(l, || l_is_ready_impl(l))
}

unsafe fn l_is_ready_impl(l: *mut lua_State) -> c_int {
    let Some(s) = lua_of(l) else { return 0 };
    s.push_bool(shared::is_ready());
    1
}

unsafe extern "C" fn l_ground(l: *mut lua_State) -> c_int {
    guard(l, || l_ground_impl(l))
}

unsafe fn l_ground_impl(l: *mut lua_State) -> c_int {
    let Some(s) = lua_of(l) else { return 0 };
    let (Some(x), Some(y)) = (s.num(1), s.num(2)) else { return fail(&s) };
    let from = s.num(3).unwrap_or(1000.0) as f32;
    let f = filter(s.num(4));
    let depth = s.num(5).map(|d| d as f32).unwrap_or(SEARCH_DEPTH);
    match shared::with(|w| w.ground(x as f32, y as f32, from, depth, f)).flatten() {
        Some(h) => {
            for v in [h.pos.z, h.normal.x, h.normal.y, h.normal.z] {
                s.push_num(v as f64);
            }
            s.push_num(h.material as f64);
            s.push_num(h.model as f64);
            6
        }
        None => fail(&s),
    }
}

unsafe extern "C" fn l_raycast(l: *mut lua_State) -> c_int {
    guard(l, || l_raycast_impl(l))
}

unsafe fn l_raycast_impl(l: *mut lua_State) -> c_int {
    let Some(s) = lua_of(l) else { return 0 };
    let mut c = [0f32; 6];
    for (i, v) in c.iter_mut().enumerate() {
        match s.num(i as c_int + 1) {
            Some(n) => *v = n as f32,
            None => return fail(&s),
        }
    }
    let f = filter(s.num(7));
    match shared::with(|w| w.raycast(v3(c[0], c[1], c[2]), v3(c[3], c[4], c[5]), f)).flatten() {
        Some(h) => {
            s.push_bool(true);
            push_hit(&s, &h);
            9
        }
        None => fail(&s),
    }
}

fn push_hit(s: &Lua, h: &Hit) {
    for v in [h.pos.x, h.pos.y, h.pos.z, h.normal.x, h.normal.y, h.normal.z] {
        s.push_num(v as f64);
    }
    s.push_num(h.material as f64);
    s.push_num(h.model as f64);
}

unsafe extern "C" fn l_place(l: *mut lua_State) -> c_int {
    guard(l, || l_place_impl(l))
}

unsafe fn l_place_impl(l: *mut lua_State) -> c_int {
    let Some(s) = lua_of(l) else { return 0 };
    let (Some(x), Some(y), Some(from), Some(yaw)) = (s.num(1), s.num(2), s.num(3), s.num(4)) else { return fail(&s) };
    let lift = s.num(5).unwrap_or(0.0) as f32;
    let f = filter(s.num(6));
    let (w_, d_) = (s.num(7).unwrap_or(0.0) as f32, s.num(8).unwrap_or(0.0) as f32);
    let (x, y, from, yaw) = (x as f32, y as f32, from as f32, yaw as f32);
    let hit = shared::with(|w| if w_ > 0.0 && d_ > 0.0 { w.footprint(x, y, from, SEARCH_DEPTH, yaw, w_, d_, f) } else { w.ground(x, y, from, SEARCH_DEPTH, f) }).flatten();
    match hit {
        Some(h) => {
            let p = place(h.pos, h.normal, yaw, lift);
            for v in [p.pos.x, p.pos.y, p.pos.z, p.rot[0], p.rot[1], p.rot[2], h.normal.x, h.normal.y, h.normal.z] {
                s.push_num(v as f64);
            }
            9
        }
        None => fail(&s),
    }
}

unsafe extern "C" fn l_ground_many(l: *mut lua_State) -> c_int {
    guard(l, || l_ground_many_impl(l))
}

unsafe fn l_ground_many_impl(l: *mut lua_State) -> c_int {
    let Some(s) = lua_of(l) else { return 0 };
    if s.ty(1) != LUA_TTABLE {
        return fail(&s);
    }
    let f = filter(s.num(2));
    let a = s.a;
    let n = (a.objlen)(l, 1);
    // read every point first (no world lock held while touching the Lua stack)
    let mut pts = Vec::with_capacity(n);
    for i in 1..=n as c_int {
        (a.rawgeti)(l, 1, i);
        let mut p = [f64::NAN, f64::NAN, 1000.0];
        if s.ty(-1) == LUA_TTABLE {
            for (k, v) in p.iter_mut().enumerate() {
                (a.rawgeti)(l, -1 - k as c_int, k as c_int + 1);
                if let Some(x) = s.num(s.top()) {
                    *v = x;
                }
            }
            (a.settop)(l, -5); // the 3 fields and the point table
        } else {
            (a.settop)(l, -2);
        }
        pts.push(p);
    }
    let Some(hits) = shared::with(|w| pts.iter().map(|p| if p[0].is_nan() || p[1].is_nan() { None } else { w.ground(p[0] as f32, p[1] as f32, p[2] as f32, SEARCH_DEPTH, f) }).collect::<Vec<_>>()) else {
        return fail(&s);
    };
    (a.createtable)(l, hits.len() as c_int, 0);
    for (i, h) in hits.iter().enumerate() {
        match h {
            Some(h) => {
                (a.createtable)(l, 4, 0);
                for (k, v) in [h.pos.z, h.normal.x, h.normal.y, h.normal.z].iter().enumerate() {
                    s.push_num(*v as f64);
                    (a.rawseti)(l, -2, k as c_int + 1);
                }
            }
            None => s.push_bool(false),
        }
        (a.rawseti)(l, -2, i as c_int + 1);
    }
    1
}

unsafe extern "C" fn l_water(l: *mut lua_State) -> c_int {
    guard(l, || l_water_impl(l))
}

unsafe fn l_water_impl(l: *mut lua_State) -> c_int {
    let Some(s) = lua_of(l) else { return 0 };
    let (Some(x), Some(y)) = (s.num(1), s.num(2)) else { return fail(&s) };
    match shared::with(|w| w.water_z(x as f32, y as f32)).flatten() {
        Some(z) => {
            s.push_num(z as f64);
            1
        }
        None => fail(&s),
    }
}

unsafe extern "C" fn l_add_overlay(l: *mut lua_State) -> c_int {
    guard(l, || l_add_overlay_impl(l))
}

unsafe fn l_add_overlay_impl(l: *mut lua_State) -> c_int {
    let Some(s) = lua_of(l) else { return 0 };
    let Some(path) = s.str(1) else { return fail(&s) };
    match shared::add_overlay(std::path::Path::new(&path)) {
        Ok(r) => {
            log(&format!("overlay {path}: {r}"));
            s.push_bool(true);
            s.push_str(&r);
            2
        }
        Err(e) => {
            s.push_bool(false);
            s.push_str(&e);
            2
        }
    }
}

unsafe extern "C" fn l_reload(l: *mut lua_State) -> c_int {
    guard(l, || l_reload_impl(l))
}

unsafe fn l_reload_impl(l: *mut lua_State) -> c_int {
    let Some(s) = lua_of(l) else { return 0 };
    let overlay = s.str(1).map(std::path::PathBuf::from);
    // no log from the worker thread (the module manager is not ours to call off the main thread): Lua polls
    // sageomReloadStatus and reports the outcome
    if shared::reload(overlay, |_| {}) {
        s.push_bool(true);
        1
    } else {
        s.push_bool(false);
        s.push_str(if shared::is_ready() { "a reload is already running" } else { "world is still loading" });
        2
    }
}

unsafe extern "C" fn l_reload_status(l: *mut lua_State) -> c_int {
    guard(l, || l_reload_status_impl(l))
}

unsafe fn l_reload_status_impl(l: *mut lua_State) -> c_int {
    let Some(s) = lua_of(l) else { return 0 };
    let (generation, busy, last) = shared::reload_status();
    s.push_num(generation as f64);
    s.push_bool(busy);
    match last {
        Some(Ok(m)) => {
            s.push_bool(true);
            s.push_str(&m);
        }
        Some(Err(e)) => {
            s.push_bool(false);
            s.push_str(&e);
        }
        None => {
            s.push_bool(false);
            s.push_bool(false);
        }
    }
    4
}

unsafe extern "C" fn l_place_model(l: *mut lua_State) -> c_int {
    guard(l, || l_place_model_impl(l))
}

unsafe fn l_place_model_impl(l: *mut lua_State) -> c_int {
    let Some(s) = lua_of(l) else { return 0 };
    let (Some(m), Some(x), Some(y), Some(from), Some(yaw)) = (s.num(1), s.num(2), s.num(3), s.num(4), s.num(5)) else { return fail(&s) };
    let lift = s.num(6).unwrap_or(0.0) as f32;
    let f = filter(s.num(7));
    // optional rest rotation (GTA Euler degrees) the model lies in before the yaw, e.g. 90, 0, 0 for a rifle on its side
    let rest = [s.num(8).unwrap_or(0.0) as f32, s.num(9).unwrap_or(0.0) as f32, s.num(10).unwrap_or(0.0) as f32];
    match shared::with(|w| w.lay_model(m as i32, x as f32, y as f32, from as f32, SEARCH_DEPTH, yaw as f32, rest, lift, f).map(|l| (l.placement, l.hit))).flatten() {
        Some((p, h)) => {
            for v in [p.pos.x, p.pos.y, p.pos.z, p.rot[0], p.rot[1], p.rot[2], h.normal.x, h.normal.y, h.normal.z] {
                s.push_num(v as f64);
            }
            9
        }
        None => fail(&s),
    }
}

unsafe extern "C" fn l_model_bounds(l: *mut lua_State) -> c_int {
    guard(l, || l_model_bounds_impl(l))
}

unsafe fn l_model_bounds_impl(l: *mut lua_State) -> c_int {
    let Some(s) = lua_of(l) else { return 0 };
    let Some(m) = s.num(1) else { return fail(&s) };
    match shared::with(|w| w.model_bounds(m as i32)).flatten() {
        Some(b) => {
            for v in [b.min.x, b.min.y, b.min.z, b.max.x, b.max.y, b.max.z] {
                s.push_num(v as f64);
            }
            6
        }
        None => fail(&s),
    }
}

// ---- live edits -------------------------------------------------------------------------------------------------

/// Edits are owned by the script VM that made them, and dropped when its resource stops.
fn owner(l: *mut lua_State) -> Owner {
    l as usize
}

fn handle(s: &Lua, i: c_int) -> Option<u32> {
    s.num(i).filter(|v| *v >= 1.0 && *v <= u32::MAX as f64).map(|v| v as u32)
}

fn f3(s: &Lua, i: c_int) -> Option<[f32; 3]> {
    Some([s.num(i)? as f32, s.num(i + 1)? as f32, s.num(i + 2)? as f32])
}

fn opt3(s: &Lua, i: c_int) -> [f32; 3] {
    [0, 1, 2].map(|k| s.num(i + k).unwrap_or(0.0) as f32)
}

/// sx [, sy = sx, sz = sx], all optional (1).
fn scale_args(s: &Lua, i: c_int) -> [f32; 3] {
    let sx = s.num(i).unwrap_or(1.0);
    [sx, s.num(i + 1).unwrap_or(sx), s.num(i + 2).unwrap_or(sx)].map(|v| v as f32)
}

unsafe extern "C" fn l_set_object_scale(l: *mut lua_State) -> c_int {
    guard(l, || l_set_object_scale_impl(l))
}

unsafe fn l_set_object_scale_impl(l: *mut lua_State) -> c_int {
    let Some(s) = lua_of(l) else { return 0 };
    let (Some(handle), Some(_)) = (handle(&s, 1), s.num(2)) else { return fail(&s) };
    let scale = scale_args(&s, 2);
    let ok = shared::edit(|w| {
        let ok = w.scale_object(handle, scale);
        (ok, if ok { vec![Op::Scale { handle, scale }] } else { Vec::new() })
    });
    s.push_bool(ok == Some(true));
    1
}

unsafe extern "C" fn l_add_object(l: *mut lua_State) -> c_int {
    guard(l, || l_add_object_impl(l))
}

unsafe fn l_add_object_impl(l: *mut lua_State) -> c_int {
    let Some(s) = lua_of(l) else { return 0 };
    let (Some(model), Some(pos)) = (s.num(1), f3(&s, 2)) else { return fail(&s) };
    let row = ObjectRow { model: model as i32, pos, rot: opt3(&s, 5), interior: s.num(8).unwrap_or(0.0) as i32, scale: scale_args(&s, 9) };
    let o = owner(l);
    match shared::edit(|w| {
        let (h, op) = w.add_object(row, o);
        (h, if h.is_some() { vec![op] } else { Vec::new() })
    })
    .flatten()
    {
        Some(h) => {
            s.push_num(h as f64);
            1
        }
        None => fail(&s),
    }
}

unsafe extern "C" fn l_add_objects(l: *mut lua_State) -> c_int {
    guard(l, || l_add_objects_impl(l))
}

unsafe fn l_add_objects_impl(l: *mut lua_State) -> c_int {
    let Some(s) = lua_of(l) else { return 0 };
    if s.ty(1) != LUA_TTABLE {
        return fail(&s);
    }
    let a = s.a;
    let n = (a.objlen)(l, 1);
    // read every row first (no world lock held while touching the Lua stack); None for a malformed row
    let mut rows: Vec<Option<ObjectRow>> = Vec::with_capacity(n);
    for i in 1..=n as c_int {
        (a.rawgeti)(l, 1, i);
        let mut row = None;
        if s.ty(-1) == LUA_TTABLE {
            let t = s.top();
            let mut v = [f64::NAN; 11];
            for (k, x) in v.iter_mut().enumerate() {
                (a.rawgeti)(l, t, k as c_int + 1);
                if let Some(num) = s.num(t + 1) {
                    *x = num;
                }
                (a.settop)(l, t);
            }
            if !v[..4].iter().any(|x| x.is_nan()) {
                let z = |x: f64| if x.is_nan() { 0.0 } else { x };
                let sx = if v[8].is_nan() { 1.0 } else { v[8] };
                let (sy, sz) = (if v[9].is_nan() { sx } else { v[9] }, if v[10].is_nan() { sx } else { v[10] });
                row = Some(ObjectRow {
                    model: v[0] as i32,
                    pos: [v[1] as f32, v[2] as f32, v[3] as f32],
                    rot: [z(v[4]) as f32, z(v[5]) as f32, z(v[6]) as f32],
                    interior: z(v[7]) as i32,
                    scale: [sx as f32, sy as f32, sz as f32],
                });
            }
        }
        (a.settop)(l, -2);
        rows.push(row);
    }
    let valid: Vec<ObjectRow> = rows.iter().flatten().copied().collect();
    let o = owner(l);
    let Some(handles) = shared::edit(|w| {
        let (hs, op) = w.add_objects(&valid, o);
        let changed = hs.iter().any(|h| h.is_some());
        (hs, if changed { vec![op] } else { Vec::new() })
    }) else {
        return fail(&s);
    };
    let mut it = handles.into_iter();
    (a.createtable)(l, rows.len() as c_int, 0);
    for (i, r) in rows.iter().enumerate() {
        match r.and_then(|_| it.next().flatten()) {
            Some(h) => s.push_num(h as f64),
            None => s.push_bool(false),
        }
        (a.rawseti)(l, -2, i as c_int + 1);
    }
    1
}

unsafe extern "C" fn l_move_object(l: *mut lua_State) -> c_int {
    guard(l, || l_move_object_impl(l))
}

unsafe fn l_move_object_impl(l: *mut lua_State) -> c_int {
    let Some(s) = lua_of(l) else { return 0 };
    let (Some(handle), Some(pos)) = (handle(&s, 1), f3(&s, 2)) else { return fail(&s) };
    let rot = opt3(&s, 5);
    let ok = shared::edit(|w| {
        let ok = w.move_object(handle, pos, rot);
        (ok, if ok { vec![Op::Move { handle, pos, rot }] } else { Vec::new() })
    });
    s.push_bool(ok == Some(true));
    1
}

unsafe extern "C" fn l_set_object_model(l: *mut lua_State) -> c_int {
    guard(l, || l_set_object_model_impl(l))
}

unsafe fn l_set_object_model_impl(l: *mut lua_State) -> c_int {
    let Some(s) = lua_of(l) else { return 0 };
    let (Some(handle), Some(model)) = (handle(&s, 1), s.num(2)) else { return fail(&s) };
    let model = model as i32;
    let r = shared::edit(|w| {
        let r = w.set_object_model(handle, model);
        (r, if r.is_some() { vec![Op::SetModel { handle, model }] } else { Vec::new() })
    });
    s.push_bool(r == Some(Some(true)));
    1
}

unsafe extern "C" fn l_remove_object(l: *mut lua_State) -> c_int {
    guard(l, || l_remove_object_impl(l))
}

unsafe fn l_remove_object_impl(l: *mut lua_State) -> c_int {
    let Some(s) = lua_of(l) else { return 0 };
    let Some(handle) = handle(&s, 1) else { return fail(&s) };
    let ok = shared::edit(|w| {
        let ok = w.remove_object(handle);
        (ok, if ok { vec![Op::Remove { handle }] } else { Vec::new() })
    });
    s.push_bool(ok == Some(true));
    1
}

/// model, radius, x, y, z [, interior = -1]
fn removal_args(s: &Lua) -> Option<(i32, f32, [f32; 3], i32)> {
    Some((s.num(1)? as i32, s.num(2)? as f32, f3(s, 3)?, s.num(6).unwrap_or(-1.0) as i32))
}

unsafe extern "C" fn l_remove_world_model(l: *mut lua_State) -> c_int {
    guard(l, || l_remove_world_model_impl(l))
}

unsafe fn l_remove_world_model_impl(l: *mut lua_State) -> c_int {
    let Some(s) = lua_of(l) else { return 0 };
    let Some((model, radius, pos, interior)) = removal_args(&s) else { return fail(&s) };
    let r = Removal { model, pos, radius, interior, owner: owner(l) };
    match shared::edit(|w| (w.remove_world_model(r), vec![Op::RemoveWorld(r)])) {
        Some(n) => {
            s.push_bool(true);
            s.push_num(n as f64);
            2
        }
        None => fail(&s),
    }
}

unsafe extern "C" fn l_restore_world_model(l: *mut lua_State) -> c_int {
    guard(l, || l_restore_world_model_impl(l))
}

unsafe fn l_restore_world_model_impl(l: *mut lua_State) -> c_int {
    let Some(s) = lua_of(l) else { return 0 };
    let Some((model, radius, pos, interior)) = removal_args(&s) else { return fail(&s) };
    match shared::edit(|w| (w.restore_world_model(model, radius, pos, interior), vec![Op::RestoreWorld { model, radius, pos, interior }])) {
        Some(n) => {
            s.push_bool(true);
            s.push_num(n as f64);
            2
        }
        None => fail(&s),
    }
}

unsafe extern "C" fn l_replace_col(l: *mut lua_State) -> c_int {
    guard(l, || l_replace_col_impl(l))
}

unsafe fn l_replace_col_impl(l: *mut lua_State) -> c_int {
    let Some(s) = lua_of(l) else { return 0 };
    let (Some(data), Some(id)) = (s.bytes(1), s.num(2)) else { return fail(&s) };
    let (id, data, o) = (id as i32, Arc::new(data), owner(l));
    let r = shared::edit(|w| match w.replace_col(id, data.clone(), o) {
        Ok(()) => (Ok(()), vec![Op::ReplaceCol { id, data: data.clone(), owner: o }]),
        Err(e) => (Err(e), Vec::new()),
    });
    match r {
        Some(Ok(())) => {
            s.push_bool(true);
            1
        }
        Some(Err(e)) => {
            s.push_bool(false);
            s.push_str(&e);
            2
        }
        None => {
            s.push_bool(false);
            s.push_str("world is still loading");
            2
        }
    }
}

unsafe extern "C" fn l_restore_col(l: *mut lua_State) -> c_int {
    guard(l, || l_restore_col_impl(l))
}

unsafe fn l_restore_col_impl(l: *mut lua_State) -> c_int {
    let Some(s) = lua_of(l) else { return 0 };
    let Some(id) = s.num(1) else { return fail(&s) };
    let id = id as i32;
    let ok = shared::edit(|w| {
        let ok = w.restore_col(id);
        (ok, if ok { vec![Op::RestoreCol { id }] } else { Vec::new() })
    });
    s.push_bool(ok == Some(true));
    1
}

unsafe extern "C" fn l_stats(l: *mut lua_State) -> c_int {
    guard(l, || l_stats_impl(l))
}

unsafe fn l_stats_impl(l: *mut lua_State) -> c_int {
    let Some(s) = lua_of(l) else { return 0 };
    let Some((st, instances, handle)) = shared::with(|w| (w.live_stats(), w.live_instances(), w.live.next_handle)) else { return fail(&s) };
    let (generation, busy, _) = shared::reload_status();
    let a = s.a;
    (a.createtable)(l, 0, 10);
    let fields: [(&std::ffi::CStr, f64); 10] = [
        (c"objects", st.objects as f64),
        (c"objectsWithoutCollision", st.objects_without_collision as f64),
        (c"removals", st.removals as f64),
        (c"removedInstances", st.removed_instances as f64),
        (c"cols", st.cols as f64),
        (c"pending", st.pending as f64),
        (c"liveTree", st.dyn_tree as f64),
        (c"instances", instances as f64),
        (c"generation", generation as f64),
        (c"nextHandle", handle as f64),
    ];
    for (k, v) in fields {
        s.push_num(v);
        (a.setfield)(l, -2, k.as_ptr());
    }
    s.push_bool(busy);
    (a.setfield)(l, -2, c"reloading".as_ptr());
    1
}
