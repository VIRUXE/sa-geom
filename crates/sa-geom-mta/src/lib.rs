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

mod lua;

use lua::{lua_State, Lua, LUA_TTABLE};
use sa_world::{place, shared, v3, Filter, Hit};
use std::ffi::{c_char, c_float, c_int, c_void, CString};
use std::sync::atomic::{AtomicPtr, Ordering};

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
    let fns: [(&std::ffi::CStr, lua::lua_CFunction); 9] = [
        (c"sageomPlaceModel", l_place_model),
        (c"sageomGetModelBounds", l_model_bounds),
        (c"sageomIsReady", l_is_ready),
        (c"sageomGround", l_ground),
        (c"sageomRaycast", l_raycast),
        (c"sageomPlace", l_place),
        (c"sageomGroundMany", l_ground_many),
        (c"sageomWaterLevel", l_water),
        (c"sageomAddOverlay", l_add_overlay),
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
pub extern "C" fn ResourceStopped(_l: *mut lua_State) -> bool {
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

unsafe extern "C" fn l_place_model(l: *mut lua_State) -> c_int {
    guard(l, || l_place_model_impl(l))
}

unsafe fn l_place_model_impl(l: *mut lua_State) -> c_int {
    let Some(s) = lua_of(l) else { return 0 };
    let (Some(m), Some(x), Some(y), Some(from), Some(yaw)) = (s.num(1), s.num(2), s.num(3), s.num(4), s.num(5)) else { return fail(&s) };
    let lift = s.num(6).unwrap_or(0.0) as f32;
    let f = filter(s.num(7));
    match shared::with(|w| w.place_model(m as i32, x as f32, y as f32, from as f32, SEARCH_DEPTH, yaw as f32, lift, f)).flatten() {
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
