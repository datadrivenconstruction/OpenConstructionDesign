//! IFC round trip of a generated braced steel frame: columns, beams and lattice bars out as
//! IfcColumn / IfcBeam / IfcMember, and back in with the same member count.

use super::{export_ifc, import_ifc};
use ddc_geom::steel::tower;
use ddc_model::Model;

#[test]
fn braced_steel_frame_round_trips() {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L0", 0.0).unwrap();
    let f = tower::braced_frame(2, 1, 6.0, 6.0, 2, 3.5).unwrap();
    let ids = m.add_steel_frame(lv, &f).unwrap();
    let ifc = export_ifc(&m).unwrap();
    let n = |e: &str| ifc.matches(&format!("={e}(")).count();
    assert_eq!(n("IFCCOLUMN"), ids.columns.len());
    assert_eq!(n("IFCBEAM"), ids.beams.len());
    assert_eq!(n("IFCMEMBER"), ids.braces.len());
    let mut back = Model::open(":memory:").unwrap();
    let r = import_ifc(&mut back, ifc.as_bytes()).unwrap();
    assert_eq!(r.columns, ids.columns.len());
    assert_eq!(r.columns + r.beams + r.meshes, f.members.len(), "{r:?}");
}
