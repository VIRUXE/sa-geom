//! The Lua 5.1 C API, resolved at runtime from the MTA server (it is not linked into the module).
//! Linux: `dlopen("x64/deathmatch.so", RTLD_NOLOAD)` (the server's working directory is its root), as the
//! official ml_sockets module does. Windows: `lua5.1.dll`, already loaded by the server.

#![allow(non_camel_case_types, dead_code)]
use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::OnceLock;

pub type lua_State = c_void;
pub type lua_CFunction = unsafe extern "C" fn(*mut lua_State) -> c_int;

pub const LUA_TNIL: c_int = 0;
pub const LUA_TBOOLEAN: c_int = 1;
pub const LUA_TNUMBER: c_int = 3;
pub const LUA_TSTRING: c_int = 4;
pub const LUA_TTABLE: c_int = 5;

pub struct Api {
    pub gettop: unsafe extern "C" fn(*mut lua_State) -> c_int,
    pub settop: unsafe extern "C" fn(*mut lua_State, c_int),
    pub type_: unsafe extern "C" fn(*mut lua_State, c_int) -> c_int,
    pub tonumber: unsafe extern "C" fn(*mut lua_State, c_int) -> f64,
    pub tolstring: unsafe extern "C" fn(*mut lua_State, c_int, *mut usize) -> *const c_char,
    pub objlen: unsafe extern "C" fn(*mut lua_State, c_int) -> usize,
    pub pushnumber: unsafe extern "C" fn(*mut lua_State, f64),
    pub pushboolean: unsafe extern "C" fn(*mut lua_State, c_int),
    pub pushlstring: unsafe extern "C" fn(*mut lua_State, *const c_char, usize),
    pub pushnil: unsafe extern "C" fn(*mut lua_State),
    pub createtable: unsafe extern "C" fn(*mut lua_State, c_int, c_int),
    pub rawgeti: unsafe extern "C" fn(*mut lua_State, c_int, c_int),
    pub rawseti: unsafe extern "C" fn(*mut lua_State, c_int, c_int),
    pub getfield: unsafe extern "C" fn(*mut lua_State, c_int, *const c_char),
    pub setfield: unsafe extern "C" fn(*mut lua_State, c_int, *const c_char),
}

static API: OnceLock<Option<Api>> = OnceLock::new();

pub fn api() -> Option<&'static Api> {
    API.get_or_init(|| unsafe { resolve() }).as_ref()
}

#[cfg(unix)]
unsafe fn resolve() -> Option<Api> {
    extern "C" {
        fn dlopen(file: *const c_char, mode: c_int) -> *mut c_void;
        fn dlsym(h: *mut c_void, name: *const c_char) -> *mut c_void;
    }
    const RTLD_NOW: c_int = 2;
    const RTLD_NOLOAD: c_int = 4;
    let mut h = std::ptr::null_mut();
    for cand in [c"x64/deathmatch.so", c"mods/deathmatch/deathmatch.so", c"x64/deathmatch_d.so"] {
        h = dlopen(cand.as_ptr(), RTLD_NOW | RTLD_NOLOAD);
        if !h.is_null() {
            break;
        }
    }
    let lookup = |name: &CStr| -> *mut c_void {
        let p = if h.is_null() { std::ptr::null_mut() } else { dlsym(h, name.as_ptr()) };
        // RTLD_DEFAULT (null) as a fallback, for servers that export Lua globally
        if p.is_null() {
            dlsym(std::ptr::null_mut(), name.as_ptr())
        } else {
            p
        }
    };
    build(lookup)
}

#[cfg(windows)]
unsafe fn resolve() -> Option<Api> {
    extern "system" {
        fn GetModuleHandleA(name: *const c_char) -> *mut c_void;
        fn GetProcAddress(h: *mut c_void, name: *const c_char) -> *mut c_void;
    }
    let h = GetModuleHandleA(c"lua5.1.dll".as_ptr());
    if h.is_null() {
        return None;
    }
    build(|name: &CStr| GetProcAddress(h, name.as_ptr()))
}

unsafe fn build(lookup: impl Fn(&CStr) -> *mut c_void) -> Option<Api> {
    macro_rules! f {
        ($name:literal) => {{
            let p = lookup($name);
            if p.is_null() {
                return None;
            }
            std::mem::transmute(p)
        }};
    }
    Some(Api {
        gettop: f!(c"lua_gettop"),
        settop: f!(c"lua_settop"),
        type_: f!(c"lua_type"),
        tonumber: f!(c"lua_tonumber"),
        tolstring: f!(c"lua_tolstring"),
        objlen: f!(c"lua_objlen"),
        pushnumber: f!(c"lua_pushnumber"),
        pushboolean: f!(c"lua_pushboolean"),
        pushlstring: f!(c"lua_pushlstring"),
        pushnil: f!(c"lua_pushnil"),
        createtable: f!(c"lua_createtable"),
        rawgeti: f!(c"lua_rawgeti"),
        rawseti: f!(c"lua_rawseti"),
        getfield: f!(c"lua_getfield"),
        setfield: f!(c"lua_setfield"),
    })
}

/// Thin safe-ish wrapper over one call's stack.
pub struct Lua {
    pub l: *mut lua_State,
    pub a: &'static Api,
}

impl Lua {
    pub fn top(&self) -> c_int {
        unsafe { (self.a.gettop)(self.l) }
    }
    pub fn ty(&self, i: c_int) -> c_int {
        unsafe { (self.a.type_)(self.l, i) }
    }
    /// Number argument `i`, or None if absent/not a number.
    pub fn num(&self, i: c_int) -> Option<f64> {
        if i > self.top() || self.ty(i) != LUA_TNUMBER {
            return None;
        }
        Some(unsafe { (self.a.tonumber)(self.l, i) })
    }
    pub fn str(&self, i: c_int) -> Option<String> {
        if i > self.top() || self.ty(i) != LUA_TSTRING {
            return None;
        }
        let mut len = 0usize;
        let p = unsafe { (self.a.tolstring)(self.l, i, &mut len) };
        if p.is_null() {
            return None;
        }
        let b = unsafe { std::slice::from_raw_parts(p as *const u8, len) };
        Some(String::from_utf8_lossy(b).into_owned())
    }
    pub fn push_num(&self, v: f64) {
        unsafe { (self.a.pushnumber)(self.l, v) }
    }
    pub fn push_bool(&self, v: bool) {
        unsafe { (self.a.pushboolean)(self.l, v as c_int) }
    }
    pub fn push_str(&self, s: &str) {
        unsafe { (self.a.pushlstring)(self.l, s.as_ptr() as *const c_char, s.len()) }
    }
}
