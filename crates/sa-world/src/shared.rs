//! A process-wide world for server plugins: loaded on a background thread so a server never blocks on startup,
//! read-locked by queries and write-locked by overlays.

use crate::World;
use std::path::Path;
use std::sync::{OnceLock, RwLock};

static WORLD: OnceLock<RwLock<Option<World>>> = OnceLock::new();
static ERROR: OnceLock<String> = OnceLock::new();

fn cell() -> &'static RwLock<Option<World>> {
    WORLD.get_or_init(|| RwLock::new(None))
}

/// Start loading the embedded world (or `file`, when given) in the background. Later calls do nothing.
pub fn start_loading(file: Option<std::path::PathBuf>, on_done: impl FnOnce(Result<String, String>) + Send + 'static) {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
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
