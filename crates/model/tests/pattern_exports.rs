//! Both placement types are reachable from the crate root under distinct names.

use ddc_model::{PatternPlacement, Placement};

#[test]
fn pattern_placement_does_not_shadow_component_placement() {
    let p = PatternPlacement { pos: [1.0, 2.0, 0.0], yaw: 0.5, normal: [0.0, 0.0, 1.0] };
    assert_eq!(p.pos[1], 2.0);
    assert_ne!(std::any::type_name::<Placement>(), std::any::type_name::<PatternPlacement>());
}
