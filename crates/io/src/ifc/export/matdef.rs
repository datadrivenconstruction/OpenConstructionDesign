// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! IfcMaterial with its look and physics: one entity set per used material (deduplicated by
//! name): colour as IfcSurfaceStyleRendering through IfcMaterialDefinitionRepresentation, and
//! Pset_MaterialCommon / Pset_MaterialThermal (SI) for the properties that are set.

use std::collections::BTreeMap;

use ddc_model::{MaterialSpec, Model};

use super::{list, r, real, s, text, Ctx, NONE};

/// Specs of every material in use (rows and texts without a row), by name.
pub(super) fn load(model: &Model) -> Result<BTreeMap<String, MaterialSpec>, String> {
    let rows = model.materials().map_err(|e| format!("db: {e}"))?;
    Ok(rows.into_iter().map(|m| (m.spec.name.clone(), m.spec)).collect())
}

/// Glass-like materials are written see-through.
fn transparency(name: &str) -> f64 {
    let n = name.to_lowercase();
    if n.contains("glass") || n.contains("\u{441}\u{442}\u{435}\u{43a}\u{43b}") {
        0.7
    } else {
        0.0
    }
}

/// A physical material value is written only when finite and not negative.
fn valid(v: f64) -> bool {
    v.is_finite() && v >= 0.0
}

fn prop(c: &mut Ctx, name: &str, measure: &str, v: f64) -> u32 {
    c.s.add(
        "IFCPROPERTYSINGLEVALUE",
        &[
            text(name),
            s(NONE),
            format!("{measure}({})", real(v)),
            s(NONE),
        ],
    )
}

impl Ctx {
    /// Surface colour of material `id` through IfcMaterialDefinitionRepresentation.
    fn style(&mut self, id: u32, name: &str, rgb: u32) {
        let ch = |sh: u32| (f64::from((rgb >> sh) & 0xFF) / 255.0).clamp(0.0, 1.0);
        let col = self.s.add(
            "IFCCOLOURRGB",
            &[s(NONE), real(ch(16)), real(ch(8)), real(ch(0))],
        );
        let render = self.s.add(
            "IFCSURFACESTYLERENDERING",
            &[
                r(col),
                real(transparency(name)),
                s(NONE),
                s(NONE),
                s(NONE),
                s(NONE),
                s(NONE),
                s(NONE),
                ".NOTDEFINED.".into(),
            ],
        );
        let style = self.s.add(
            "IFCSURFACESTYLE",
            &[text(name), ".BOTH.".into(), list(&[render])],
        );
        let item = self
            .s
            .add("IFCSTYLEDITEM", &[s(NONE), list(&[style]), s(NONE)]);
        let rep = self.s.add(
            "IFCSTYLEDREPRESENTATION",
            &[
                r(self.mat_ctx),
                text("Style"),
                text("Material"),
                list(&[item]),
            ],
        );
        self.s.add(
            "IFCMATERIALDEFINITIONREPRESENTATION",
            &[s(NONE), s(NONE), list(&[rep]), r(id)],
        );
    }

    /// The IfcMaterial named `name`, created with its style and property sets on first use.
    pub(super) fn material(&mut self, name: &str) -> u32 {
        if let Some(i) = self.mat_ent.get(name) {
            return *i;
        }
        let id = self.s.add("IFCMATERIAL", &[text(name), s(NONE), s(NONE)]);
        self.mat_ent.insert(name.to_string(), id);
        // a name without a row colour still exports the colour its library look shows in 3D
        let look = ddc_model::material_look::resolve(name, ddc_model::material_defaults(name).color).map(|l| l.rgb);
        let Some(spec) = self.mat_spec.get(name).cloned() else {
            if let Some(rgb) = look {
                self.style(id, name, rgb);
            }
            return id;
        };
        let mut skipped = 0usize;
        let rgb = match spec.color {
            Some(c) if c <= 0xFF_FFFF => Some(c),
            Some(_) => {
                skipped += 1;
                None
            }
            None => look,
        };
        if let Some(rgb) = rgb {
            self.style(id, name, rgb);
        }
        let d = spec.density.filter(|v| valid(*v) || { skipped += 1; false });
        let k = spec.conductivity.filter(|v| valid(*v) || { skipped += 1; false });
        let cp = spec.specific_heat.filter(|v| valid(*v) || { skipped += 1; false });
        if skipped > 0 {
            self.g.warnings.push(format!(
                "material '{name}': {skipped} invalid value(s) not written (non-finite or negative)"
            ));
        }
        if let Some(d) = d {
            let p = prop(self, "MassDensity", "IFCMASSDENSITYMEASURE", d);
            self.s.add(
                "IFCMATERIALPROPERTIES",
                &[text("Pset_MaterialCommon"), s(NONE), list(&[p]), r(id)],
            );
        }
        let mut th = vec![];
        if let Some(k) = k {
            th.push(prop(
                self,
                "ThermalConductivity",
                "IFCTHERMALCONDUCTIVITYMEASURE",
                k,
            ));
        }
        if let Some(cp) = cp {
            th.push(prop(
                self,
                "SpecificHeatCapacity",
                "IFCSPECIFICHEATCAPACITYMEASURE",
                cp,
            ));
        }
        if !th.is_empty() {
            self.s.add(
                "IFCMATERIALPROPERTIES",
                &[text("Pset_MaterialThermal"), s(NONE), list(&th), r(id)],
            );
        }
        id
    }
}

#[cfg(test)]
mod tests {
    use crate::ifc::check_step;
    use crate::ifc::export_ifc;
    use ddc_model::{LayerSpec, MaterialSpec, Model, WallSpec};

    const NAME: &str = "Zz Test Frit";

    fn spec(color: Option<u32>, d: Option<f64>, k: Option<f64>, c: Option<f64>) -> MaterialSpec {
        MaterialSpec {
            name: NAME.into(),
            color,
            hatch: "none".into(),
            density: d,
            conductivity: k,
            specific_heat: c,
            note: String::new(),
        }
    }

    fn model(s: &MaterialSpec) -> Model {
        let mut m = Model::open(":memory:").unwrap();
        m.add_material(s).unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let layer = |t: f64| LayerSpec {
            material: NAME.into(),
            thickness: t,
        };
        let wt = m.add_wall_type("zz", &[layer(0.1), layer(0.2)]).unwrap();
        m.add_wall(&WallSpec {
            a: (0.0, 0.0),
            b: (4.0, 0.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .unwrap();
        m
    }

    fn count(t: &str, name: &str) -> usize {
        t.matches(&format!("={name}(")).count()
    }

    #[test]
    fn export_writes_material_color_and_psets() {
        let m = model(&spec(Some(0x33_66_99), Some(1234.5), Some(0.5), Some(900.0)));
        let t = export_ifc(&m).unwrap();
        check_step(&t).unwrap();
        let named = format!("=IFCMATERIAL('{NAME}'");
        assert_eq!(t.matches(&named).count(), 1, "one material per name");
        assert_eq!(count(&t, "IFCSTYLEDITEM"), 1);
        assert_eq!(count(&t, "IFCSURFACESTYLE"), 1);
        assert_eq!(count(&t, "IFCSURFACESTYLERENDERING"), 1);
        assert_eq!(count(&t, "IFCMATERIALDEFINITIONREPRESENTATION"), 1);
        assert!(t.contains("IFCCOLOURRGB($,0.2,0.4,0.6)"));
        assert!(t.contains("'MassDensity',$,IFCMASSDENSITYMEASURE(1234.5)"));
        assert!(t.contains("IFCTHERMALCONDUCTIVITYMEASURE(0.5)"));
        assert!(t.contains("IFCSPECIFICHEATCAPACITYMEASURE(900.)"));
        assert_eq!(count(&t, "IFCMATERIALPROPERTIES"), 2);
    }

    #[test]
    fn a_material_without_a_row_colour_exports_its_library_look_and_reimports_without_one() {
        let name = "steel S355";
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let layer = LayerSpec { material: name.into(), thickness: 0.2 };
        let wt = m.add_wall_type("zz", &[layer]).unwrap();
        m.add_wall(&WallSpec { a: (0.0, 0.0), b: (4.0, 0.0), level: lv, base_offset: 0.0, height: 3.0, wall_type: wt, offset0: None })
            .unwrap();
        let row = m.material(name).unwrap().spec.color;
        let want = m.material_look(name).unwrap().expect("steel has a look").rgb;
        let t = export_ifc(&m).unwrap();
        check_step(&t).unwrap();
        assert_eq!(count(&t, "IFCSTYLEDITEM"), 1, "{t}");
        assert_eq!(count(&t, "IFCCOLOURRGB"), 1);
        let ch = |sh: u32| f64::from((want >> sh) & 0xFF) / 255.0;
        let got = t.lines().find(|l| l.contains("=IFCCOLOURRGB(")).unwrap();
        let nums: Vec<f64> = got
            .split(|c: char| !(c.is_ascii_digit() || c == '.'))
            .filter(|s| s.contains('.'))
            .map(|s| s.parse().unwrap())
            .collect();
        assert_eq!(nums.len(), 3, "{got}");
        for (n, sh) in nums.iter().zip([16, 8, 0]) {
            assert!((n - ch(sh)).abs() < 1e-3, "{got} vs {want:06x}");
        }
        let mut dst = Model::open(":memory:").unwrap();
        crate::ifc::import_ifc(&mut dst, t.as_bytes()).unwrap();
        assert_eq!(dst.material(name).unwrap().spec.color, row, "the look colour does not become a row colour");
    }

    #[test]
    fn unset_props_not_written() {
        let t = export_ifc(&model(&spec(None, None, None, None))).unwrap();
        check_step(&t).unwrap();
        assert_eq!(count(&t, "IFCSURFACESTYLE"), 0);
        assert_eq!(count(&t, "IFCSTYLEDITEM"), 0);
        assert_eq!(count(&t, "IFCCOLOURRGB"), 0);
        assert_eq!(count(&t, "IFCMATERIALDEFINITIONREPRESENTATION"), 0);
        assert_eq!(count(&t, "IFCMATERIALPROPERTIES"), 0);
        let only_density = export_ifc(&model(&spec(None, Some(10.0), None, None))).unwrap();
        assert_eq!(count(&only_density, "IFCMATERIALPROPERTIES"), 1);
        assert!(!only_density.contains("Pset_MaterialThermal"));
    }

    #[test]
    fn ifc_roundtrip_keeps_material_props() {
        let src = spec(Some(0x33_66_99), Some(1234.5), Some(0.5), Some(900.0));
        let t = export_ifc(&model(&src)).unwrap();
        let mut dst = Model::open(":memory:").unwrap();
        crate::ifc::import_ifc(&mut dst, t.as_bytes()).unwrap();
        let got = dst.material(NAME).unwrap();
        assert!(got.id > 0, "a row was created");
        assert_eq!(got.spec.color, src.color);
        assert_eq!(got.spec.density, src.density);
        assert_eq!(got.spec.conductivity, src.conductivity);
        assert_eq!(got.spec.specific_heat, src.specific_heat);
        // one undo step removes the imported row again
        dst.undo().unwrap();
        assert_eq!(dst.material(NAME).unwrap().id, 0);
    }

    #[test]
    fn import_does_not_override_existing_material() {
        let t = export_ifc(&model(&spec(Some(0x33_66_99), Some(1234.5), Some(0.5), Some(900.0))))
            .unwrap();
        let mut dst = Model::open(":memory:").unwrap();
        let mine = spec(Some(0xAA_BB_CC), Some(7.0), None, None);
        dst.add_material(&mine).unwrap();
        crate::ifc::import_ifc(&mut dst, t.as_bytes()).unwrap();
        assert_eq!(dst.material(NAME).unwrap().spec, mine);
    }

    fn corrupt(m: &Model, col: &str, val: &str) {
        m.sql()
            .execute(&format!("UPDATE material SET {col}={val} WHERE name=?1"), [NAME])
            .unwrap();
    }

    #[test]
    fn export_skips_nonfinite_material_density() {
        let m = model(&spec(None, Some(10.0), None, None));
        corrupt(&m, "density", "9e999");
        let (t, w) = crate::ifc::export_ifc_with_warnings(&m).unwrap();
        check_step(&t).unwrap();
        assert_eq!(count(&t, "IFCMATERIALPROPERTIES"), 0);
        assert!(w.iter().any(|x| x.contains("invalid value")), "{w:?}");
    }

    #[test]
    fn export_skips_negative_conductivity() {
        let m = model(&spec(None, Some(10.0), Some(0.5), Some(900.0)));
        corrupt(&m, "conductivity", "-0.5");
        let (t, w) = crate::ifc::export_ifc_with_warnings(&m).unwrap();
        check_step(&t).unwrap();
        assert!(!t.contains("IFCTHERMALCONDUCTIVITYMEASURE"));
        assert!(t.contains("IFCSPECIFICHEATCAPACITYMEASURE(900.)"));
        assert!(w.iter().any(|x| x.contains("1 invalid")), "{w:?}");
    }

    #[test]
    fn material_with_inf_density_still_exports_valid_step() {
        let m = model(&spec(Some(0x33_66_99), Some(10.0), Some(0.5), None));
        corrupt(&m, "density", "-9e999");
        let t = export_ifc(&m).unwrap();
        check_step(&t).unwrap();
        assert_eq!(count(&t, "IFCWALL"), 1);
        assert!(t.contains("IFCTHERMALCONDUCTIVITYMEASURE(0.5)"));
    }

    #[test]
    fn export_clamps_colour_components() {
        let t = export_ifc(&model(&spec(Some(0xFF_FF_FF), None, None, None))).unwrap();
        assert!(t.contains("IFCCOLOURRGB($,1.,1.,1.)"), "{t}");
        let (t, w) =
            crate::ifc::export_ifc_with_warnings(&model(&spec(Some(0x00_00_00), None, None, None)))
                .unwrap();
        assert!(t.contains("IFCCOLOURRGB($,0.,0.,0.)"));
        assert!(w.is_empty(), "{w:?}");
    }
}
