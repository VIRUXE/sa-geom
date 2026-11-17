//! SA-MP / open.mp plugin `sageom`: Pawn natives for ground height, raycasts and slope placement against the
//! built-in GTA San Andreas collision world. Uses the classic plugin ABI, which open.mp loads from `plugins/`
//! (or `legacy_plugins`) as well. Natives are declared in `include/sageom.inc`.

use sa_world::{place, shared, v3, Filter};
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::atomic::{AtomicPtr, Ordering};

type Cell = i32;
type Amx = c_void;
type Native = unsafe extern "C" fn(*mut Amx, *const Cell) -> Cell;

#[repr(C)]
struct NativeInfo {
    name: *const c_char,
    func: Option<Native>,
}

const SUPPORTS_VERSION: u32 = 0x0200;
const SUPPORTS_AMX_NATIVES: u32 = 0x10000;
const PLUGIN_DATA_LOGPRINTF: usize = 0x00;
const PLUGIN_DATA_AMX_EXPORTS: usize = 0x10;
const EXPORT_GET_ADDR: usize = 13;
const EXPORT_GET_STRING: usize = 17;
const EXPORT_REGISTER: usize = 33;
const EXPORT_STR_LEN: usize = 39;
const SEARCH_DEPTH: f32 = 1200.0;

static AMX_EXPORTS: AtomicPtr<*const c_void> = AtomicPtr::new(std::ptr::null_mut());
static LOGPRINTF: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

fn log(msg: &str) {
    let p = LOGPRINTF.load(Ordering::Acquire);
    let Ok(c) = CString::new(format!("[sa-geom] {msg}")) else { return };
    if !p.is_null() {
        let f: unsafe extern "C" fn(*const c_char, ...) = unsafe { std::mem::transmute(p) };
        unsafe { f(c"%s".as_ptr(), c.as_ptr()) };
    }
}

unsafe fn export(i: usize) -> *const c_void {
    *AMX_EXPORTS.load(Ordering::Acquire).add(i)
}

#[no_mangle]
pub extern "system" fn Supports() -> u32 {
    SUPPORTS_VERSION | SUPPORTS_AMX_NATIVES
}

#[no_mangle]
pub unsafe extern "system" fn Load(data: *mut *mut c_void) -> bool {
    AMX_EXPORTS.store(*data.add(PLUGIN_DATA_AMX_EXPORTS) as *mut *const c_void, Ordering::Release);
    LOGPRINTF.store(*data.add(PLUGIN_DATA_LOGPRINTF), Ordering::Release);
    log(&format!("v{} loading the San Andreas collision world in the background", env!("CARGO_PKG_VERSION")));
    let file = std::env::var_os("SAGEOM_WORLD").map(std::path::PathBuf::from);
    shared::start_loading(file, |r| match r {
        Ok(m) => log(&m),
        Err(e) => log(&format!("world failed to load: {e}")),
    });
    true
}

#[no_mangle]
pub extern "system" fn Unload() {}

#[no_mangle]
pub unsafe extern "system" fn AmxLoad(amx: *mut Amx) -> c_int {
    let natives: &[(&CStr, Native)] = &[
        (c"SAGeom_IsReady", n_is_ready),
        (c"SAGeom_GetGround", n_get_ground),
        (c"SAGeom_RayCast", n_raycast),
        (c"SAGeom_Place", n_place),
        (c"SAGeom_WaterLevel", n_water),
        (c"SAGeom_AddOverlay", n_add_overlay),
    ];
    let mut list: Vec<NativeInfo> = natives.iter().map(|(n, f)| NativeInfo { name: n.as_ptr(), func: Some(*f) }).collect();
    list.push(NativeInfo { name: std::ptr::null(), func: None });
    // the AMX keeps a pointer to the list's strings only (static), and copies the entries
    let register: unsafe extern "C" fn(*mut Amx, *const NativeInfo, c_int) -> c_int = std::mem::transmute(export(EXPORT_REGISTER));
    let r = register(amx, list.as_ptr(), -1);
    std::mem::forget(list);
    r
}

#[no_mangle]
pub extern "system" fn AmxUnload(_amx: *mut Amx) -> c_int {
    0
}

// ---- argument helpers -------------------------------------------------------------------------------------

struct Args {
    amx: *mut Amx,
    p: *const Cell,
}

impl Args {
    fn count(&self) -> usize {
        unsafe { (*self.p / 4) as usize }
    }
    fn cell(&self, i: usize) -> Option<Cell> {
        (i <= self.count()).then(|| unsafe { *self.p.add(i) })
    }
    fn float(&self, i: usize) -> Option<f32> {
        self.cell(i).map(|c| f32::from_bits(c as u32))
    }
    fn int_or(&self, i: usize, d: i32) -> i32 {
        self.cell(i).unwrap_or(d)
    }
    fn float_or(&self, i: usize, d: f32) -> f32 {
        self.float(i).unwrap_or(d)
    }
    /// Physical address of by-reference argument `i`.
    fn addr(&self, i: usize) -> Option<*mut Cell> {
        let a = self.cell(i)?;
        let mut phys: *mut Cell = std::ptr::null_mut();
        let get: unsafe extern "C" fn(*mut Amx, Cell, *mut *mut Cell) -> c_int = unsafe { std::mem::transmute(export(EXPORT_GET_ADDR)) };
        let r = unsafe { get(self.amx, a, &mut phys) };
        (r == 0 && !phys.is_null()).then_some(phys)
    }
    fn set_float(&self, i: usize, v: f32) {
        if let Some(p) = self.addr(i) {
            unsafe { *p = v.to_bits() as Cell };
        }
    }
    fn set_int(&self, i: usize, v: i32) {
        if let Some(p) = self.addr(i) {
            unsafe { *p = v };
        }
    }
    fn string(&self, i: usize) -> Option<String> {
        let p = self.addr(i)?;
        unsafe {
            let strlen: unsafe extern "C" fn(*const Cell, *mut c_int) -> c_int = std::mem::transmute(export(EXPORT_STR_LEN));
            let getstr: unsafe extern "C" fn(*mut c_char, *const Cell, c_int, usize) -> c_int = std::mem::transmute(export(EXPORT_GET_STRING));
            let mut len: c_int = 0;
            strlen(p, &mut len);
            let mut buf = vec![0u8; len as usize + 1];
            getstr(buf.as_mut_ptr() as *mut c_char, p, 0, buf.len());
            buf.pop();
            Some(String::from_utf8_lossy(&buf).into_owned())
        }
    }
}

/// Run a native, turning a panic into 0 (false) instead of unwinding into the server.
fn guard(amx: *mut Amx, p: *const Cell, f: impl FnOnce(&Args) -> Cell) -> Cell {
    let a = Args { amx, p };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&a))).unwrap_or_else(|_| {
        log("internal error in a SAGeom native");
        0
    })
}

fn filter(i: i32) -> Filter {
    Filter::interior(i)
}

// ---- natives ----------------------------------------------------------------------------------------------

/// native bool:SAGeom_IsReady();
unsafe extern "C" fn n_is_ready(amx: *mut Amx, p: *const Cell) -> Cell {
    guard(amx, p, |_| shared::is_ready() as Cell)
}

/// native bool:SAGeom_GetGround(Float:x, Float:y, &Float:z, Float:zFrom = 1000.0, interior = 0,
///     &Float:nx = 0.0, &Float:ny = 0.0, &Float:nz = 0.0, &material = 0, &model = 0);
unsafe extern "C" fn n_get_ground(amx: *mut Amx, p: *const Cell) -> Cell {
    guard(amx, p, |a| {
        let (Some(x), Some(y)) = (a.float(1), a.float(2)) else { return 0 };
        let from = a.float_or(4, 1000.0);
        let f = filter(a.int_or(5, 0));
        match shared::with(|w| w.ground(x, y, from, SEARCH_DEPTH, f)).flatten() {
            Some(h) => {
                a.set_float(3, h.pos.z);
                a.set_float(6, h.normal.x);
                a.set_float(7, h.normal.y);
                a.set_float(8, h.normal.z);
                a.set_int(9, h.material as i32);
                a.set_int(10, h.model);
                1
            }
            None => 0,
        }
    })
}

/// native bool:SAGeom_RayCast(Float:x1, Float:y1, Float:z1, Float:x2, Float:y2, Float:z2,
///     &Float:hx, &Float:hy, &Float:hz, interior = 0, &Float:nx = 0.0, &Float:ny = 0.0, &Float:nz = 0.0,
///     &model = 0, &material = 0);
unsafe extern "C" fn n_raycast(amx: *mut Amx, p: *const Cell) -> Cell {
    guard(amx, p, |a| {
        let mut c = [0f32; 6];
        for (i, v) in c.iter_mut().enumerate() {
            match a.float(i + 1) {
                Some(f) => *v = f,
                None => return 0,
            }
        }
        let f = filter(a.int_or(10, 0));
        match shared::with(|w| w.raycast(v3(c[0], c[1], c[2]), v3(c[3], c[4], c[5]), f)).flatten() {
            Some(h) => {
                a.set_float(7, h.pos.x);
                a.set_float(8, h.pos.y);
                a.set_float(9, h.pos.z);
                a.set_float(11, h.normal.x);
                a.set_float(12, h.normal.y);
                a.set_float(13, h.normal.z);
                a.set_int(14, h.model);
                a.set_int(15, h.material as i32);
                1
            }
            None => 0,
        }
    })
}

/// native bool:SAGeom_Place(Float:x, Float:y, Float:zFrom, Float:yaw, &Float:z, &Float:rx, &Float:ry, &Float:rz,
///     Float:lift = 0.0, interior = 0, Float:width = 0.0, Float:depth = 0.0);
unsafe extern "C" fn n_place(amx: *mut Amx, p: *const Cell) -> Cell {
    guard(amx, p, |a| {
        let (Some(x), Some(y), Some(from), Some(yaw)) = (a.float(1), a.float(2), a.float(3), a.float(4)) else { return 0 };
        let lift = a.float_or(9, 0.0);
        let f = filter(a.int_or(10, 0));
        let (fw, fd) = (a.float_or(11, 0.0), a.float_or(12, 0.0));
        let hit = shared::with(|w| if fw > 0.0 && fd > 0.0 { w.footprint(x, y, from, SEARCH_DEPTH, yaw, fw, fd, f) } else { w.ground(x, y, from, SEARCH_DEPTH, f) }).flatten();
        match hit {
            Some(h) => {
                let pl = place(h.pos, h.normal, yaw, lift);
                a.set_float(5, pl.pos.z);
                a.set_float(6, pl.rot[0]);
                a.set_float(7, pl.rot[1]);
                a.set_float(8, pl.rot[2]);
                1
            }
            None => 0,
        }
    })
}

/// native bool:SAGeom_WaterLevel(Float:x, Float:y, &Float:z);
unsafe extern "C" fn n_water(amx: *mut Amx, p: *const Cell) -> Cell {
    guard(amx, p, |a| {
        let (Some(x), Some(y)) = (a.float(1), a.float(2)) else { return 0 };
        match shared::with(|w| w.water_z(x, y)).flatten() {
            Some(z) => {
                a.set_float(3, z);
                1
            }
            None => 0,
        }
    })
}

/// native bool:SAGeom_AddOverlay(const path[]);  // relative to scriptfiles/
unsafe extern "C" fn n_add_overlay(amx: *mut Amx, p: *const Cell) -> Cell {
    guard(amx, p, |a| {
        let Some(path) = a.string(1) else { return 0 };
        let full = std::path::Path::new("scriptfiles").join(&path);
        match shared::add_overlay(&full) {
            Ok(r) => {
                log(&format!("overlay {path}: {r}"));
                1
            }
            Err(e) => {
                log(&format!("overlay {path} failed: {e}"));
                0
            }
        }
    })
}
