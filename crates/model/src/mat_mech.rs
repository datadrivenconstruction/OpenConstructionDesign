//! Mechanical defaults of a material by its class (from the name, English or Russian), for
//! the structural analysis when the material row has no E, G, nu yet. Typical textbook
//! values, SI units (Pa, 1/K); a project must verify them. Strength `fy` only for metals,
//! the lowest common structural grade; other classes get None (checks report `na`).

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MechDefaults {
    /// Class key: concrete_aerated, concrete_light, concrete, steel, aluminium, timber,
    /// masonry, glass.
    pub class: &'static str,
    /// Young's modulus, Pa.
    pub e: f64,
    /// Shear modulus, Pa (E / (2 (1 + nu)) except for timber, which is orthotropic).
    pub g: f64,
    pub nu: f64,
    /// Yield strength, Pa.
    pub fy: Option<f64>,
    /// Thermal expansion, 1/K.
    pub alpha: f64,
}

fn iso(class: &'static str, e: f64, nu: f64, fy: Option<f64>, alpha: f64) -> MechDefaults {
    MechDefaults {
        class,
        e,
        g: e / (2.0 * (1.0 + nu)),
        nu,
        fy,
        alpha,
    }
}

/// Defaults for a material name; None when the class is unknown (`no_material`).
/// Concretes are matched before steel so that fibre-reinforced concrete stays concrete.
pub fn mech_defaults(name: &str) -> Option<MechDefaults> {
    let m = name.to_lowercase();
    let has = |keys: &[&str]| keys.iter().any(|k| m.contains(k));
    Some(
        if has(&[
            "aerated",
            "autoclaved",
            "\u{433}\u{430}\u{437}\u{43e}\u{431}\u{435}\u{442}\u{43e}\u{43d}",
            "\u{43f}\u{435}\u{43d}\u{43e}\u{431}\u{435}\u{442}\u{43e}\u{43d}",
        ]) {
            iso("concrete_aerated", 2.0e9, 0.2, None, 8e-6)
        } else if has(&[
            "lightweight",
            "light-weight",
            "\u{43a}\u{435}\u{440}\u{430}\u{43c}\u{437}\u{438}\u{442}",
        ]) {
            iso("concrete_light", 18e9, 0.2, None, 8e-6)
        } else if has(&["concrete", "\u{431}\u{435}\u{442}\u{43e}\u{43d}"]) {
            iso("concrete", 30e9, 0.2, None, 10e-6)
        } else if has(&["steel", "\u{441}\u{442}\u{430}\u{43b}", "s235", "s355"]) {
            iso("steel", 210e9, 0.3, Some(235e6), 12e-6)
        } else if has(&["alum", "\u{430}\u{43b}\u{44e}\u{43c}"]) {
            iso("aluminium", 70e9, 0.33, Some(110e6), 23e-6)
        } else if has(&[
            "timber",
            "wood",
            "glulam",
            "\u{434}\u{435}\u{440}\u{435}\u{432}",
            "\u{431}\u{440}\u{443}\u{441}",
        ]) {
            // along the grain; G parallel to the grain is about E / 16
            MechDefaults {
                class: "timber",
                e: 11e9,
                g: 11e9 / 16.0,
                nu: 0.3,
                fy: None,
                alpha: 5e-6,
            }
        } else if has(&[
            "masonry",
            "brick",
            "\u{43a}\u{438}\u{440}\u{43f}\u{438}\u{447}",
            "\u{43a}\u{43b}\u{430}\u{434}\u{43a}",
        ]) {
            iso("masonry", 5e9, 0.2, None, 6e-6)
        } else if has(&["glass", "\u{441}\u{442}\u{435}\u{43a}\u{43b}"]) {
            iso("glass", 70e9, 0.22, None, 9e-6)
        } else {
            return None;
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_by_name_in_both_languages() {
        let s = mech_defaults("Structural steel").unwrap();
        assert_eq!(s.class, "steel");
        assert!((s.e - 210e9).abs() < 1.0 && (s.g - 210e9 / 2.6).abs() < 1.0);
        assert_eq!(s.fy, Some(235e6));
        // "stal'" and "beton" in Cyrillic
        let ru_steel = mech_defaults("\u{421}\u{442}\u{430}\u{43b}\u{44c} C245").unwrap();
        assert_eq!(ru_steel.class, "steel");
        let ru_conc = mech_defaults("\u{411}\u{435}\u{442}\u{43e}\u{43d} B25").unwrap();
        assert_eq!(ru_conc.class, "concrete");
        assert_eq!(ru_conc.fy, None);
        // fibre concrete ("stalefibrobeton") stays concrete
        let fibre = "\u{441}\u{442}\u{430}\u{43b}\u{435}\u{444}\u{438}\u{431}\u{440}\u{43e}\
                     \u{431}\u{435}\u{442}\u{43e}\u{43d}";
        assert_eq!(mech_defaults(fibre).unwrap().class, "concrete");
        assert_eq!(mech_defaults("Aerated concrete").unwrap().class, "concrete_aerated");
        assert_eq!(mech_defaults("Glulam GL24h").unwrap().class, "timber");
        assert_eq!(mech_defaults("Brick").unwrap().class, "masonry");
        assert!(mech_defaults("Paint").is_none());
    }

    #[test]
    fn values_are_physical_and_in_pascal() {
        for n in ["concrete", "steel", "aluminium", "timber", "masonry", "glass", "aerated"] {
            let d = mech_defaults(n).unwrap();
            assert!(d.e > 1e9 && d.e < 1e12, "{n}");
            assert!(d.g > 0.0 && d.g < d.e && d.nu > 0.0 && d.nu < 0.5, "{n}");
            assert!(d.alpha > 1e-6 && d.alpha < 3e-5, "{n}");
        }
    }
}
