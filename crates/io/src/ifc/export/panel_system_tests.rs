//! Curtain system on a freeform host: class counts, aggregation, quantities, IFC2X3.

use std::f64::consts::PI;

use super::super::{export_ifc, export_ifc_with, IfcOptions, IfcSchema};
use crate::ifc::validate_ifc;
use ddc_geom::nurbs::{self, Curve, Trim};
use ddc_geom::panelize::{PanelLimits, Pattern, Rows, Spacing};
use ddc_geom::v3;
use ddc_model::{FreeformSpec, Model, PanelLayout, PanelSystemSpec, ShellSide};

fn count(t: &str, e: &str) -> usize {
    t.matches(&format!("={e}(")).count()
}

fn clean(t: &str) {
    let rep = validate_ifc(t.as_bytes());
    assert!(rep.is_clean(), "{}", rep.brief(12));
}

fn facade(m: &mut Model, pattern: Pattern) -> i64 {
    let l = m.add_level("L1", 3.0).unwrap();
    let line = Curve::polyline(&[v3(2.0, 0.0, 0.0), v3(2.0, 0.0, 3.0)]).unwrap();
    let surface = nurbs::revolve(&line, v3(0.0, 0.0, 0.0), v3(0.0, 0.0, 1.0), PI).unwrap();
    let h = m
        .add_freeform(&FreeformSpec {
            level: l,
            name: "Drum".into(),
            material: String::new(),
            surface,
            thickness: 0.0,
            side: ShellSide::Normal,
            trim: Trim::default(),
            origin: "revolve".into(),
        })
        .unwrap();
    m.add_panel_system(&PanelSystemSpec {
        host: h,
        name: "Drum glazing".into(),
        material: "Glass".into(),
        layout: PanelLayout {
            cols: Spacing::Equal(6),
            rows: Rows::Spacing(Spacing::Equal(3)),
            pattern,
            limits: PanelLimits::default(),
            planarize: 0,
        },
        mullion: [0.06, 0.15],
        glass: 0.024,
    })
    .unwrap()
}

#[test]
fn curtain_wall_aggregates_plates_and_mullions() {
    let mut m = Model::open(":memory:").unwrap();
    facade(&mut m, Pattern::Quad);
    let t = export_ifc(&m).unwrap();
    clean(&t);
    // 6 x 3 panels; mullions on 7 x 3 + 4 x 6 grid edges
    assert_eq!(count(&t, "IFCCURTAINWALL"), 1);
    assert_eq!(count(&t, "IFCPLATE"), 18);
    assert_eq!(count(&t, "IFCMEMBER"), 45);
    assert_eq!(t.matches(".CURTAIN_PANEL.").count(), 18);
    assert_eq!(t.matches(".MULLION.").count(), 45);
    // one aggregation lists all 63 parts
    let agg = t.lines().filter(|l| l.contains("=IFCRELAGGREGATES(")).map(|l| l.matches('#').count()).max().unwrap();
    assert!(agg >= 63 + 2, "{agg}");
    // the drum facets are planar rectangles of one type
    assert_eq!(t.matches("'Type 1'").count(), 18);
    assert_eq!(t.matches("'flat'").count(), 18);
    assert!(t.contains("'DDC_PanelSystem'") && t.contains("'MullionLength'"));
    assert!(t.contains("'Qto_PlateBaseQuantities'") && t.contains("'Qto_MemberBaseQuantities'"));
    assert!(count(&t, "IFCRELASSOCIATESMATERIAL") >= 1);

    let opts = IfcOptions { schema: IfcSchema::Ifc2x3, timestamp: Some("2026-01-01T00:00:00".into()), ..Default::default() };
    let (t2, _) = export_ifc_with(&m, &opts).unwrap();
    clean(&t2);
    assert_eq!((count(&t2, "IFCPLATE"), count(&t2, "IFCMEMBER")), (18, 45));
}

#[test]
fn diamond_pattern_counts() {
    let mut m = Model::open(":memory:").unwrap();
    facade(&mut m, Pattern::Diamond);
    let t = export_ifc(&m).unwrap();
    clean(&t);
    // a 7 x 4 node grid has 14 nodes with i + j odd, one panel around each
    assert_eq!(count(&t, "IFCPLATE"), 14);
    assert_eq!(count(&t, "IFCCURTAINWALL"), 1);
}
