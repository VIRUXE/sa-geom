//! sa-geom core: a GTA San Andreas collision world with raycasts, ground height/normal queries and slope placement.
//!
//! ```no_run
//! let world = sa_world::World::embedded().expect("built without embedded data");
//! if let Some(hit) = world.ground(2495.0, -1687.0, 100.0, 200.0, Default::default()) {
//!     println!("ground z = {}", hit.pos.z);
//! }
//! ```

pub mod bake;
pub mod bvh;
pub mod euler;
pub mod io;
pub mod math;
pub mod overlay;
pub mod query;
pub mod shape;
pub mod shared;
pub mod world;

pub use math::{v3, Vec3};
pub use query::{place, Filter, Hit, Placement};
pub use world::World;

#[cfg(sagw_embedded)]
static EMBEDDED: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/vanilla.sagw.zst"));

/// The compressed vanilla world built into this binary, if any.
pub fn embedded_bytes() -> Option<&'static [u8]> {
    #[cfg(sagw_embedded)]
    {
        Some(EMBEDDED)
    }
    #[cfg(not(sagw_embedded))]
    {
        None
    }
}

impl World {
    /// Decompress and load the built-in vanilla world.
    pub fn embedded() -> Result<World, io::IoError> {
        let b = embedded_bytes().ok_or_else(|| io::IoError("this build has no embedded world (run `sa-geom bake` and rebuild)".into()))?;
        World::from_bytes(b)
    }
}
