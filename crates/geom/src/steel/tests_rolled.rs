//! Rolled section properties: closed forms by hand, cross-checks between the two derivations,
//! a few well-known published values, and integrity of every table row.

use super::rolled::{self, i_closed_form, outline, plastic_modulus, poly_props, RolledDims, RolledKind};
use super::sections;
use crate::math::v2;
use crate::member::{catalog, Shape};

fn rel(a: f64, b: f64) -> f64 {
    (a / b - 1.0).abs()
}

fn i_dims(h: f64, b: f64, tw: f64, tf: f64, r: f64) -> RolledDims {
    let m = 1e-3;
    RolledDims { kind: RolledKind::I, h: h * m, b: b * m, tw: tw * m, tf: tf * m, r1: r * m, r2: 0.0, slope: 0.0 }
}

#[test]
fn rectangle_polygon_matches_closed_form() {
    let (b, h) = (0.2, 0.5);
    let r = [v2(-b / 2.0, -h / 2.0), v2(b / 2.0, -h / 2.0), v2(b / 2.0, h / 2.0), v2(-b / 2.0, h / 2.0)];
    let g = poly_props(&r);
    assert!(rel(g.area, b * h) < 1e-12);
    assert!(rel(g.iy, b * h.powi(3) / 12.0) < 1e-12);
    assert!(rel(g.iz, h * b.powi(3) / 12.0) < 1e-12);
    assert!(rel(g.iy / g.cz, b * h * h / 6.0) < 1e-12);
    // a rectangle is an I with flanges as wide as the web and no fillets
    let c = i_closed_form(h, b, b, 0.1, 0.0);
    assert!(rel(c.wpl_y, b * h * h / 4.0) < 1e-9, "{}", c.wpl_y);
    assert!(rel(c.wpl_z, h * b * b / 4.0) < 1e-9, "{}", c.wpl_z);
}

#[test]
fn sharp_i_matches_hand_calculation() {
    // h 300, b 150, tw 7, tf 10 mm, no fillets
    let c = i_closed_form(0.3, 0.15, 0.007, 0.010, 0.0);
    let mm = |v: f64, p: i32| v * 1e-3f64.powi(p);
    assert!(rel(c.area, mm(4960.0, 2)) < 1e-12, "A {}", c.area);
    // (150 * 300^3 - 143 * 280^3) / 12
    assert!(rel(c.iy, mm(75_905_333.333_333, 4)) < 1e-9, "Iy {}", c.iy);
    // (2 * 10 * 150^3 + 280 * 7^3) / 12
    assert!(rel(c.iz, mm(5_633_003.333_333, 4)) < 1e-9, "Iz {}", c.iz);
    // b tf (h - tf) + tw (h - 2 tf)^2 / 4
    assert!(rel(c.wpl_y, mm(572_200.0, 3)) < 1e-12, "Wpl,y {}", c.wpl_y);
    // tf b^2 / 2 + (h - 2 tf) tw^2 / 4
    assert!(rel(c.wpl_z, mm(115_930.0, 3)) < 1e-12, "Wpl,z {}", c.wpl_z);
    // the polygon route gives the same numbers
    let d = i_dims(300.0, 150.0, 7.0, 10.0, 0.0);
    let g = poly_props(&outline(&d, 8));
    assert!(rel(g.area, c.area) < 1e-10 && rel(g.iy, c.iy) < 1e-10 && rel(g.iz, c.iz) < 1e-10);
}

#[test]
fn filleted_i_closed_form_agrees_with_polygon() {
    for d in [i_dims(300.0, 150.0, 7.1, 10.7, 15.0), i_dims(1008.0, 302.0, 21.0, 40.0, 30.0), i_dims(127.0, 76.0, 4.0, 7.6, 7.6)] {
        let c = i_closed_form(d.h, d.b, d.tw, d.tf, d.r1);
        let g = poly_props(&outline(&d, 256));
        assert!(rel(g.area, c.area) < 1e-5, "A {} {}", g.area, c.area);
        assert!(rel(g.iy, c.iy) < 1e-5, "Iy {} {}", g.iy, c.iy);
        assert!(rel(g.iz, c.iz) < 1e-5, "Iz {} {}", g.iz, c.iz);
        assert!(g.yc.abs() < 1e-12 && g.zc.abs() < 1e-12);
        // the channel route (clipping and bisection for the plastic modulus) on the I outline
        let o = outline(&d, 256);
        assert!(rel(plastic_modulus(&o, 1), c.wpl_y) < 1e-5, "Wpl,y {}", c.wpl_y);
        assert!(rel(plastic_modulus(&o, 0), c.wpl_z) < 1e-5, "Wpl,z {}", c.wpl_z);
    }
    // sharp-cornered channel: the polygon route against plates by hand
    let m = 1e-3;
    let ch = RolledDims { kind: RolledKind::Channel, h: 0.2, b: 0.075, tw: 8.5 * m, tf: 11.5 * m, r1: 0.0, r2: 0.0, slope: 0.0 };
    let p = rolled::props(&ch);
    let a = 2.0 * 0.075 * ch.tf + (0.2 - 2.0 * ch.tf) * ch.tw;
    assert!(rel(p.area, a) < 1e-10, "A {}", p.area);
    let iy = (0.075 * 0.2f64.powi(3) - (0.075 - ch.tw) * (0.2 - 2.0 * ch.tf).powi(3)) / 12.0;
    assert!(rel(p.iy, iy) < 1e-10, "Iy {}", p.iy);
    let ey = (2.0 * ch.tf * 0.075 * 0.075 / 2.0 + (0.2 - 2.0 * ch.tf) * ch.tw * ch.tw / 2.0) / a;
    assert!(rel(p.ey, ey) < 1e-10, "ey {}", p.ey);
    let wply = 2.0 * (0.075 * ch.tf * (0.2 - ch.tf) / 2.0 + ch.tw * (0.1 - ch.tf).powi(2) / 2.0);
    assert!(rel(p.wpl_y, wply) < 1e-10, "Wpl,y {}", p.wpl_y);
}

#[test]
fn published_values_within_tolerance() {
    let cm = |v: f64, p: i32| v * 1e-2f64.powi(p);
    let get = |n: &str| rolled::find(n).unwrap_or_else(|| panic!("{n}")).props;
    // IPE 300: A 53.8 cm2, Iy 8356, Iz 604 cm4, Wel,y 557, Wpl,y 628 cm3, It 20.1 cm4, Iw 125.9e3 cm6
    let p = get("IPE 300");
    assert!(rel(p.area, cm(53.81, 2)) < 0.002, "A {}", p.area);
    assert!(rel(p.iy, cm(8356.0, 4)) < 0.002 && rel(p.iz, cm(603.8, 4)) < 0.002);
    assert!(rel(p.wel_y, cm(557.1, 3)) < 0.002 && rel(p.wpl_y, cm(628.4, 3)) < 0.002);
    assert!(rel(p.it, cm(20.12, 4)) < 0.03, "It {}", p.it);
    assert!(rel(p.iw, cm(125.9e3, 6)) < 0.01, "Iw {}", p.iw);
    assert!(rel(p.ry, cm(12.46, 1)) < 0.002 && rel(p.rz, cm(3.35, 1)) < 0.003);
    // HEB 300: A 149.1, Iy 25170, Wpl,y 1869, It 185, Iw 1688e3
    let p = get("HEB 300");
    assert!(rel(p.area, cm(149.1, 2)) < 0.002 && rel(p.iy, cm(25170.0, 4)) < 0.002);
    assert!(rel(p.wpl_y, cm(1869.0, 3)) < 0.002, "Wpl,y {}", p.wpl_y);
    assert!(rel(p.it, cm(185.0, 4)) < 0.04, "It {}", p.it);
    assert!(rel(p.iw, cm(1688e3, 6)) < 0.01);
    // UPN 200: A 32.2, Iy 1910, Iz 148, Wel,y 191, Wel,z 27.0, Wpl,y 228, e 2.01 cm, It 11.9
    let p = get("UPN 200");
    assert!(rel(p.area, cm(32.2, 2)) < 0.005 && rel(p.iy, cm(1910.0, 4)) < 0.005);
    assert!(rel(p.iz, cm(148.0, 4)) < 0.01 && rel(p.ey, cm(2.01, 1)) < 0.01, "ey {}", p.ey);
    assert!(rel(p.wel_y, cm(191.0, 3)) < 0.005 && rel(p.wel_z, cm(27.0, 3)) < 0.01);
    assert!(rel(p.wpl_y, cm(228.0, 3)) < 0.005, "Wpl,y {}", p.wpl_y);
    assert!(p.wpl_z > p.wel_z && p.wpl_z < 2.5 * p.wel_z);
    assert!(rel(p.it, cm(11.9, 4)) < 0.08, "It {}", p.it);
    // UB 305x165x40: A 51.3, Iy 8503, Iz 764 cm4, Wpl,y 623 cm3, It 14.7 cm4
    let p = get("ub 305x165x40");
    assert!(rel(p.area, cm(51.3, 2)) < 0.002 && rel(p.iy, cm(8503.0, 4)) < 0.002);
    assert!(rel(p.iz, cm(764.0, 4)) < 0.002 && rel(p.wpl_y, cm(623.0, 3)) < 0.003);
    assert!(rel(p.it, cm(14.7, 4)) < 0.03, "It {}", p.it);
    // W12x26: A 7.65 in2, Ix 204 in4, Iy 17.3 in4, Zx 37.2 in3, J 0.300 in4
    let inch = |v: f64, p: i32| v * 0.0254f64.powi(p);
    let p = get("W12X26");
    assert!(rel(p.area, inch(7.65, 2)) < 0.003 && rel(p.iy, inch(204.0, 4)) < 0.005);
    assert!(rel(p.iz, inch(17.3, 4)) < 0.005 && rel(p.wpl_y, inch(37.2, 3)) < 0.005);
    assert!(rel(p.it, inch(0.300, 4)) < 0.03, "J {}", p.it);
    // GOST 57837 20B1: 22.4 kg/m
    assert!(rel(get("20B1").mass, 22.4) < 0.005);
}

#[test]
fn every_row_is_consistent() {
    let t = rolled::table();
    assert!(t.len() >= 300, "{}", t.len());
    let mut names = std::collections::HashSet::new();
    for r in t {
        assert!(names.insert(r.name.to_lowercase()), "duplicate {}", r.name);
        if !r.alias.is_empty() {
            assert!(names.insert(r.alias.to_lowercase()), "duplicate alias {}", r.alias);
        }
        let p = r.props;
        assert!(p.area > 0.0 && p.iy > p.iz && p.iz > 0.0, "{}", r.name);
        assert!(p.wpl_y > p.wel_y && p.wpl_z > p.wel_z && p.it > 0.0 && p.iw > 0.0, "{}", r.name);
        // I: fillets add area to the plates; channel: taper and toe radii take a little back
        let plates = sections::area(&r.shape);
        match r.dims.kind {
            RolledKind::I => assert!(p.area > plates && p.area < r.dims.h * r.dims.b, "{}", r.name),
            RolledKind::Channel => assert!(rel(p.area, plates) < 0.03, "{}", r.name),
        }
        // the designation carries the mass per length: kg/m (BS 4-1), lb/ft (ASTM A6)
        let nominal = match r.standard {
            "BS 4-1" => r.name.rsplit('x').next().and_then(|m| m.parse::<f64>().ok()),
            "ASTM A6" => r.name.rsplit('x').next().and_then(|m| m.parse::<f64>().ok()).map(|m| m * 1.488_164),
            _ => None,
        };
        if let Some(m) = nominal {
            assert!(rel(p.mass, m) < 0.025, "{}: {} kg/m vs {m}", r.name, p.mass);
        }
    }
    for s in ["EN 10365", "BS 4-1", "GOST 57837", "ASTM A6"] {
        assert!(t.iter().filter(|r| r.standard == s).count() >= 40, "{s}");
    }
}

#[test]
fn rolled_sections_reach_the_catalogues() {
    assert!(matches!(sections::find("HEM 300"), Some(Shape::I { .. })));
    assert!(matches!(sections::find("UB 305x165x40"), Some(Shape::I { .. })));
    assert!(matches!(sections::find("UPN 400"), Some(Shape::Channel { .. })));
    let cyr = sections::find("20\u{411}1");
    assert!(cyr.is_some() && cyr == sections::find("20b1"));
    // names already in the section table keep their old shape
    assert_eq!(sections::find("IPE 300"), Some(rolled::find("IPE 300").unwrap().shape.clone()));
    let cat = catalog();
    let mut seen = std::collections::HashSet::new();
    for (n, _) in &cat {
        assert!(seen.insert(*n), "duplicate catalogue entry {n}");
    }
    for n in ["HEM 300", "W14x90", "UC 203x203x46", "30\u{41a}1", "IPE 300", "Pipe 6 STD"] {
        assert!(seen.contains(n), "{n} missing from the catalogue");
    }
    let w = cat.iter().find(|c| c.0 == "W12x26").unwrap();
    assert!(matches!(w.1, Shape::I { h, .. } if (h - 12.2 * 0.0254).abs() < 1e-12));
}
