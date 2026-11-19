//! Shared prototypes: one built [`Detail`] per distinct opening type, size and level of detail;
//! a building's hundred identical windows build once and are placed a hundred times.

use super::opening::{proto_key, OpeningType, Placement};
use super::rules::DetailRules;
use super::{door, window, Detail, Lod};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Prototypes kept at most; past it the cache starts over (a model rarely has this many types).
const CAP: usize = 4096;

static BUILT: AtomicUsize = AtomicUsize::new(0);
static SHARED: AtomicUsize = AtomicUsize::new(0);

fn map() -> &'static Mutex<HashMap<u64, Arc<Detail>>> {
    static M: OnceLock<Mutex<HashMap<u64, Arc<Detail>>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

fn key(t: &OpeningType, p: &Placement, lod: Lod, r: &DetailRules) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    proto_key(t, p, lod).hash(&mut h);
    // the opening rules that shape a built prototype
    for x in [r.window_pane, r.window_transom_from, r.window_top_light] {
        ((x * 1e4).round() as i64).hash(&mut h);
    }
    h.finish()
}

/// The prototype of an opening, built on first use.
pub fn opening(t: &OpeningType, p: &Placement, lod: Lod, r: &DetailRules) -> Arc<Detail> {
    let k = key(t, p, lod, r);
    if let Some(d) = map().lock().ok().and_then(|m| m.get(&k).cloned()) {
        SHARED.fetch_add(1, Ordering::Relaxed);
        return d;
    }
    let d = Arc::new(match t {
        OpeningType::Door(dt) => door::build(dt, p, lod),
        OpeningType::Window(wt) => window::build(wt, p, lod, r),
    });
    BUILT.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut m) = map().lock() {
        if m.len() >= CAP {
            m.clear();
        }
        m.insert(k, d.clone());
    }
    d
}

/// Prototypes built and placements that reused one, since the start.
pub fn stats() -> (usize, usize) {
    (BUILT.load(Ordering::Relaxed), SHARED.load(Ordering::Relaxed))
}
