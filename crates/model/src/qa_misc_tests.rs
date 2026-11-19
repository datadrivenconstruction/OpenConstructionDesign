//! QA: assorted queries on the shared building.

use crate::qa_methods_tests::{fx, slab};
use crate::WallSide;

#[test]
fn type_layer_queries() {
    let mut f = fx();
    let all = f.m.all_wall_layer_materials().unwrap();
    assert_eq!(all, vec![(f.wt, "Concrete".to_string())]);
    let s = slab(&mut f);
    assert_eq!(f.m.slab_materials(s).unwrap(), vec!["Concrete".to_string()]);
    assert!(f.m.slab_materials(9999).unwrap().is_empty());
    let (n, e) = f.m.underlay_totals().unwrap();
    assert_eq!((n, e), (0, 0), "no underlays");
}

#[test]
fn wall_attachments_and_stacks() {
    let mut f = fx();
    let w = f.walls[0];
    assert!(f.m.wall_stack_tiers(w, 3.0).unwrap().is_none(), "plain wall has no tiers");
    // detaching a wall that is not attached is a no-op that undoes cleanly
    f.m.detach_wall_from_slab(w, WallSide::Top).unwrap();
    f.m.detach_wall_from_slab(w, WallSide::Base).unwrap();
    f.m.undo().unwrap();
    f.m.undo().unwrap();
}

#[test]
fn slab_shape_mesh_of_flat_slab() {
    let mut f = fx();
    let s = slab(&mut f);
    assert!(f.m.slab_shape_mesh(s).unwrap().is_none(), "no shape sketched");
    assert!(f.m.slab_shape_mesh(9999).is_err() || f.m.slab_shape_mesh(9999).unwrap().is_none());
}

#[test]
fn ai_option_revision_is_stable() {
    let f = fx();
    let a = f.m.ai_option_rev().unwrap();
    let b = f.m.ai_option_rev().unwrap();
    assert_eq!(a, b);
}
