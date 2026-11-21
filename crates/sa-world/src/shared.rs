//! A process-wide world for server plugins: loaded on a background thread so a server never blocks on startup,
//! read-locked by queries and write-locked by overlays. `reload` builds a fresh world (plus an overlay) on a background
//! thread and swaps it in, so a changed overlay replaces the old one without restarting the server.

use crate::World;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};

static WORLD: OnceLock<RwLock<Option<World>>> = OnceLock::new();
static ERROR: OnceLock<String> = OnceLock::new();
/// Where the world was loaded from (None: the embedded one), for `reload`.
static SOURCE: OnceLock<Option<PathBuf>> = OnceLock::new();
/// How many worlds have been swapped in: 1 after the first load, +1 per finished reload.
static GENERATION: AtomicU64 = AtomicU64::new(0);
static RELOADING: AtomicBool = AtomicBool::new(false);
/// The last reload's outcome, once one has finished.
static LAST_RELOAD: Mutex<Option<Result<String, String>>> = Mutex::new(None);

fn cell() -> &'static RwLock<Option<World>> {
    WORLD.get_or_init(|| RwLock::new(None))
}

/// Start loading the embedded world (or `file`, when given) in the background. Later calls do nothing.
pub fn start_loading(file: Option<std::path::PathBuf>, on_done: impl FnOnce(Result<String, String>) + Send + 'static) {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    let _ = SOURCE.set(file.clone());
    std::thread::spawn(move || {
        let t = std::time::Instant::now();
        let r = match &file {
            Some(f) => World::load_file(f),
            None => World::embedded(),
        };
        match r {
            Ok(w) => {
                let msg = format!("world loaded in {} ms ({} objects, {} triangles)", t.elapsed().as_millis(), w.live_instances(), w.tris.len());
                *cell().write().unwrap_or_else(|e| e.into_inner()) = Some(w);
                GENERATION.fetch_add(1, Ordering::AcqRel);
                on_done(Ok(msg));
            }
            Err(e) => {
                let _ = ERROR.set(e.to_string());
                on_done(Err(e.to_string()));
            }
        }
    });
}

pub fn is_ready() -> bool {
    cell().read().map(|g| g.is_some()).unwrap_or(false)
}

pub fn load_error() -> Option<&'static str> {
    ERROR.get().map(|s| s.as_str())
}

/// Run `f` with the world, or return None while it is still loading.
pub fn with<R>(f: impl FnOnce(&World) -> R) -> Option<R> {
    let g = cell().read().unwrap_or_else(|e| e.into_inner());
    g.as_ref().map(f)
}

/// Apply an overlay to the loaded world.
pub fn add_overlay(path: &Path) -> Result<String, String> {
    let mut g = cell().write().unwrap_or_else(|e| e.into_inner());
    let w = g.as_mut().ok_or("world is still loading")?;
    w.apply_overlay(path).map(|r| r.to_string())
}

/// Build a fresh world from the source it first loaded from, apply `overlay` (when given) to it and swap it in for
/// the current one, all on a background thread: queries keep answering from the old world until the swap, which only
/// holds the write lock for the assignment. Overlays applied before are dropped with the old world. Returns false
/// (nothing started) while the first load or another reload is still running. `on_done` runs on that thread.
pub fn reload(overlay: Option<PathBuf>, on_done: impl FnOnce(&Result<String, String>) + Send + 'static) -> bool {
    let Some(source) = SOURCE.get().cloned() else { return false };
    if !is_ready() || RELOADING.swap(true, Ordering::AcqRel) {
        return false;
    }
    std::thread::spawn(move || {
        let t = std::time::Instant::now();
        let built = match &source {
            Some(f) => World::load_file(f),
            None => World::embedded(),
        }
        .map_err(|e| e.to_string())
        .and_then(|mut w| match &overlay {
            Some(p) => w.apply_overlay(p).map(|r| (w, format!(", overlay {}: {r}", p.display()))),
            None => Ok((w, String::new())),
        });
        let r = built.map(|(w, rep)| {
            let msg = format!("world reloaded in {} ms ({} objects){rep}", t.elapsed().as_millis(), w.live_instances());
            let old = std::mem::replace(&mut *cell().write().unwrap_or_else(|e| e.into_inner()), Some(w));
            drop(old); // freed outside the lock
            GENERATION.fetch_add(1, Ordering::AcqRel);
            msg
        });
        on_done(&r);
        *LAST_RELOAD.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
        RELOADING.store(false, Ordering::Release);
    });
    true
}

/// generation (see GENERATION), whether a reload is running, and the last finished reload's outcome.
pub fn reload_status() -> (u64, bool, Option<Result<String, String>>) {
    let last = LAST_RELOAD.lock().unwrap_or_else(|e| e.into_inner()).clone();
    (GENERATION.load(Ordering::Acquire), RELOADING.load(Ordering::Acquire), last)
}
