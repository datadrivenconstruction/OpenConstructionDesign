// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Roofs and slabs on surfaces: an exact advanced body for an open patch, triangles per layer
//! for a dome with an opening, the layer set, quantities, IFC2X3 down conversion.

use super::super::{export_ifc, export_ifc_with, IfcOptions, IfcSchema};
use crate::ifc::validate_ifc;
use ddc_geom::nurbs::{self, Trim};
use ddc_geom::v3;
use ddc_model::{FreeformSpec, Model, ShellKind, ShellLayer, ShellSide};

fn count(t: &str, e: &str) -> usize {
    t.matches(&format!("={e}(")).count()
}

fn clean(t: &str) {
    let rep = validate_ifc(t.as_bytes());
    assert!(rep.is_clean(), "{}", rep.brief(12));
}

fn host(level: i64, surface: nurbs::Surface) -> FreeformSpec {
    FreeformSpec {
        level,
        name: "Host".into(),
        material: String::new(),
        surface,
        thickness: 0.0,
        side: ShellSide::Normal,
        trim: Trim::default(),
        origin: "preset".into(),
    }
}

fn layers() -> Vec<ShellLayer> {
    vec![
        ShellLayer { name: "Structure".into(), material: "Concrete".into(), thickness: 0.15 },
        ShellLayer { name: "Insulation".into(), material: "Mineral wool".into(), thickness: 0.1 },
    ]
}

#[test]
fn vault_slab_is_an_exact_brep_with_a_layer_set() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 3.0).unwrap();
    let v = nurbs::vault(v3(0.0, 0.0, 0.0), v3(6.0, 0.0, 0.0), 4.0, 1.0).unwrap();
    let h = m.add_freeform(&host(l, v)).unwrap();
    let s = m.shell_on_host(h, ShellKind::Slab, layers(), ShellSide::Reverse, vec![], "Vault").unwrap();
    let id = m.add_shell_elem(&s).unwrap();
    let vol = m.shell_elem_quantities(id).unwrap().volume;
    let t = export_ifc(&m).unwrap();
    clean(&t);
    assert_eq!(count(&t, "IFCSLAB"), 1);
    assert_eq!(count(&t, "IFCADVANCEDBREP"), 1);
    assert!(t.contains("'Body','AdvancedBrep'") && t.contains("'Body-FallBack','Tessellation'"));
    assert!(t.contains("'Structure'") && t.contains("'Insulation'") && t.contains("'Mineral wool'"));
    assert!(t.contains("'DDC_SurfaceShell'") && t.contains(".FLOOR."));
    assert!(t.contains("'GrossVolume'") && t.contains("'GrossArea'") && t.contains("'Width'"));
    assert!(vol > 0.0);
    // one closed triangle set per layer (the host's own fallback has one item)
    let fb = |n: usize| t.lines().filter(|x| x.contains("'Body-FallBack','Tessellation'") && x.matches('#').count() == n).count();
    assert_eq!((fb(4), fb(3)), (1, 1));
}

#[test]
fn dome_roof_with_an_opening_is_triangles_and_goes_to_ifc2x3() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("Roof", 6.0).unwrap();
    let h = m.add_freeform(&host(l, nurbs::dome(v3(0.0, 0.0, 0.0), 5.0, 2.5).unwrap())).unwrap();
    let hole = vec![[1.0, 1.0], [2.0, 1.0], [2.0, 2.0], [1.0, 2.0]];
    let s = m.shell_on_host(h, ShellKind::Roof, layers(), ShellSide::Normal, vec![hole], "Dome").unwrap();
    m.add_shell_elem(&s).unwrap();
    let t = export_ifc(&m).unwrap();
    clean(&t);
    assert_eq!(count(&t, "IFCROOF"), 1);
    assert!(t.contains(".FREEFORM."));
    // the host surface is closed with a pole: no exact body for the host nor the roof
    assert_eq!(count(&t, "IFCADVANCEDBREP"), 0);
    let line = t.lines().find(|x| x.contains("'Body','Tessellation'") && x.matches('#').count() == 4);
    assert!(line.is_some(), "a Body of two layer sets");

    let opts = IfcOptions { schema: IfcSchema::Ifc2x3, timestamp: Some("2026-01-01T00:00:00".into()), ..Default::default() };
    let (t2, _) = export_ifc_with(&m, &opts).unwrap();
    clean(&t2);
    assert_eq!(count(&t2, "IFCROOF"), 1);
    assert!(!t2.contains("Advanced"));
}
