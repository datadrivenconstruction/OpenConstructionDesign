//! Pixel-grid helpers: thin UI lines and plan grid lines land on whole physical pixels at any
//! display scaling (125 %, 150 %, 175 %), so they stay one device pixel wide and never smear.
//! All coordinates are egui points; `ppp` is `ctx.pixels_per_point()`.

use egui::{Color32, Rect, Stroke};

/// Scale factors that are zero, negative or not finite fall back to 1.0.
fn sane(ppp: f32) -> f32 {
    if ppp.is_finite() && ppp > 0.0 {
        ppp
    } else {
        1.0
    }
}

/// Centre of the physical pixel that contains `v`: the position for a one-pixel line.
pub fn snap(v: f32, ppp: f32) -> f32 {
    let ppp = sane(ppp);
    ((v * ppp).floor() + 0.5) / ppp
}

/// Width of exactly one physical pixel, in points.
pub fn hairline(ppp: f32) -> f32 {
    1.0 / sane(ppp)
}

/// One-physical-pixel stroke of the given colour.
pub fn stroke(ppp: f32, color: Color32) -> Stroke {
    Stroke::new(hairline(ppp), color)
}

/// Rect whose edges lie on physical pixel boundaries (rounded to the nearest one), so an inside
/// hairline border covers whole pixels.
pub fn snap_rect(r: Rect, ppp: f32) -> Rect {
    let ppp = sane(ppp);
    let e = |v: f32| (v * ppp).round() / ppp;
    Rect::from_min_max(egui::pos2(e(r.min.x), e(r.min.y)), egui::pos2(e(r.max.x), e(r.max.y)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PPPS: [f32; 5] = [1.0, 1.25, 1.5, 1.75, 2.0];

    fn is_int(v: f32) -> bool {
        (v - v.round()).abs() < 1e-3
    }

    #[test]
    fn snapped_coordinate_is_a_pixel_centre() {
        for ppp in PPPS {
            for i in -400..400 {
                let v = i as f32 * 0.37;
                let s = snap(v, ppp);
                assert!(is_int(s * ppp - 0.5), "ppp {ppp} v {v} -> {s}");
                // stays inside the pixel that held the input
                assert!((s - v).abs() <= 1.0 / ppp + 1e-4, "ppp {ppp} v {v} -> {s}");
            }
        }
    }

    #[test]
    fn negative_coordinates_snap_downwards_like_positive_ones() {
        assert_eq!(snap(-0.3, 1.0), -0.5);
        assert_eq!(snap(-1.0, 1.0), -0.5);
        assert_eq!(snap(0.0, 1.0), 0.5);
        for ppp in PPPS {
            let s = snap(-10.2, ppp);
            assert!(s < 0.0 && is_int(s * ppp - 0.5), "ppp {ppp} -> {s}");
        }
    }

    #[test]
    fn snap_is_idempotent() {
        for ppp in PPPS {
            for i in -50..50 {
                let s = snap(i as f32 * 0.91, ppp);
                assert!((snap(s, ppp) - s).abs() < 1e-4, "ppp {ppp} {s}");
            }
        }
    }

    #[test]
    fn hairline_is_one_physical_pixel() {
        for ppp in PPPS {
            assert!((hairline(ppp) * ppp - 1.0).abs() < 1e-6, "ppp {ppp}");
            assert!((stroke(ppp, Color32::WHITE).width * ppp - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn invalid_scale_falls_back_to_one() {
        for bad in [0.0, -2.0, f32::NAN, f32::INFINITY] {
            assert_eq!(hairline(bad), 1.0);
            assert_eq!(snap(2.2, bad), 2.5);
        }
    }

    #[test]
    fn rect_edges_land_on_pixel_boundaries() {
        for ppp in PPPS {
            let r = snap_rect(Rect::from_min_max(egui::pos2(-3.3, 1.1), egui::pos2(17.77, 9.49)), ppp);
            for v in [r.min.x, r.min.y, r.max.x, r.max.y] {
                assert!(is_int(v * ppp), "ppp {ppp} edge {v}");
            }
            // already aligned rects are unchanged
            assert_eq!(snap_rect(r, ppp), r);
        }
    }

    #[test]
    fn border_hairline_inside_a_snapped_rect_covers_whole_pixels() {
        for ppp in PPPS {
            let r = snap_rect(Rect::from_min_max(egui::pos2(4.2, 4.2), egui::pos2(40.6, 20.3)), ppp);
            let inner_edge = r.min.x + hairline(ppp);
            assert!(is_int(inner_edge * ppp), "ppp {ppp}");
        }
    }

    /// Thin-line call sites must use the helpers; hand-rolled `+ 0.5` snapping is forbidden
    /// anywhere except this file.
    #[test]
    fn no_hand_rolled_half_pixel_snapping() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        for krate in ["ui/src", "app/src"] {
            let mut files = Vec::new();
            collect(&root.join(krate), &mut files);
            for f in files {
                if f.file_name().is_some_and(|n| n == "px.rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&f).unwrap_or_default();
                for (i, line) in text.lines().enumerate() {
                    let l: String = line.chars().filter(|c| !c.is_whitespace()).collect();
                    let bad = l.contains("floor()+0.5") || l.contains("round()+0.5");
                    assert!(!bad, "{}:{}: use ddc_ui::px::snap", f.display(), i + 1);
                }
            }
        }
    }

    /// Evidence sites (plan grid, level line, dividers, ribbon) call the shared helpers.
    #[test]
    fn evidence_call_sites_use_the_helpers() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let read = |p: &str| std::fs::read_to_string(root.join(p)).expect(p);
        // a one-pixel width comes from hairline() directly or through stroke()
        let thin = |s: &str| s.contains("px::hairline(") || s.contains("px::stroke(");
        let plan = read("../app/src/plan_view.rs");
        assert!(plan.matches("px::snap(").count() >= 3, "plan grid + level line");
        assert!(thin(&plan));
        assert!(!plan.contains("fn crisp("));
        let widgets = read("src/widgets/mod.rs");
        assert!(widgets.contains("px::snap(") && thin(&widgets));
        let ribbon = read("src/widgets/ribbon.rs");
        assert!(ribbon.matches("px::snap(").count() >= 7, "ribbon separators and glyphs");
        assert!(thin(&ribbon));
    }

    fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
}
