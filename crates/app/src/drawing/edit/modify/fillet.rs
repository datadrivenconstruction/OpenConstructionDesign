// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Fillet / corner of two picked drawing lines. Radius 0 trims or extends both lines to their
//! intersection; a positive radius also adds a tangent arc on the first line's layer. The sides
//! the user clicked are kept. The whole operation is one undo step ([`Step::geometry`]).

use std::f64::consts::TAU;

use ddc_geom::{v2, V2};
use ddc_io::drawing_model::{UArc, Underlay};

use super::super::tr;
use super::{cross_ll, member, Geo, MEET};
use crate::drawing::history::Step;
use crate::drawing::Drawings;

/// Radii below this make a sharp corner.
const SAME_R: f64 = 1e-9;

/// First click of the pair: the line as measured at that moment and the click.
#[derive(Clone, Copy, Debug, PartialEq)]
struct First {
    drawing: u64,
    idx: usize,
    a: (f64, f64),
    b: (f64, f64),
    at: V2,
}

#[derive(Default)]
pub(in crate::drawing::edit) struct FilletUi {
    on: bool,
    /// Fillet radius in drawing units (m); 0 makes a sharp corner.
    radius: f64,
    first: Option<First>,
    note: Option<String>,
}

/// New ends of both lines and the optional arc `(centre, radius, a0, a1)`.
#[derive(Debug, PartialEq)]
struct Plan {
    l1: (V2, V2),
    l2: (V2, V2),
    arc: Option<(V2, f64, f64, f64)>,
}

/// Geometry of the corner. `p1`, `p2` are the clicks that choose the sides to keep.
fn plan(
    (a1, b1): (V2, V2),
    p1: V2,
    (a2, b2): (V2, V2),
    p2: V2,
    r: f64,
) -> Result<Plan, &'static str> {
    if !r.is_finite() || r < 0.0 {
        return Err("dwgfillet.st.radius_bad");
    }
    let (d1, d2) = (b1 - a1, b2 - a2);
    let (n1, n2) = (d1.len(), d2.len());
    if n1 <= MEET || n2 <= MEET {
        return Err("dwgfillet.st.degenerate");
    }
    let x = *cross_ll(a1, d1, a2, d2)
        .first()
        .ok_or("dwgfillet.st.parallel")?;
    // unit directions from the corner towards the clicked sides
    let side = |d: V2, n: f64, p: V2| {
        let u = d * (1.0 / n);
        if (p - x).dot(u) < 0.0 {
            u * -1.0
        } else {
            u
        }
    };
    let (u1, u2) = (side(d1, n1, p1), side(d2, n2, p2));
    // far end of the kept side
    let far = |a: V2, b: V2, u: V2| (a - x).dot(u).max((b - x).dot(u));
    let (f1, f2) = (far(a1, b1, u1), far(a2, b2, u2));
    if f1 <= MEET || f2 <= MEET {
        return Err("dwgfillet.st.no_side");
    }
    if r <= SAME_R {
        return Ok(Plan {
            l1: (x, x + u1 * f1),
            l2: (x, x + u2 * f2),
            arc: None,
        });
    }
    let theta = u1.dot(u2).clamp(-1.0, 1.0).acos();
    let t = r / (theta / 2.0).tan();
    if !t.is_finite() || t >= f1 - MEET || t >= f2 - MEET {
        return Err("dwgfillet.st.too_big");
    }
    let bis = u1 + u2;
    let c = x + bis * (1.0 / bis.len()) * (r / (theta / 2.0).sin());
    let (t1, t2) = (x + u1 * t, x + u2 * t);
    let (q1, q2) = (t1 - c, t2 - c);
    let (mut s, mut e) = (q1.y.atan2(q1.x), q2.y.atan2(q2.x));
    if q1.cross(q2) < 0.0 {
        std::mem::swap(&mut s, &mut e);
    }
    while e <= s {
        e += TAU;
    }
    let turns = (s / TAU).floor() * TAU;
    Ok(Plan {
        l1: (t1, x + u1 * f1),
        l2: (t2, x + u2 * f2),
        arc: Some((c, r, s - turns, e - turns)),
    })
}

fn member_of_closed(u: &Underlay, layer: &str, a: V2, b: V2) -> bool {
    u.closed.iter().any(|c| member(c, layer, &Geo::Line(a, b)))
}

impl Drawings {
    /// Arm the tool with the radius (m): two line clicks follow.
    pub fn fillet_arm(&mut self, radius: f64) -> Result<String, String> {
        if !radius.is_finite() || radius < 0.0 {
            return Err(tr("dwgfillet.st.radius_bad"));
        }
        self.edit.modify.armed = false;
        self.edit.trim.disarm();
        let f = &mut self.edit.fillet;
        f.on = true;
        f.radius = radius;
        f.first = None;
        f.note = None;
        Ok(tr("dwgfillet.st.arm_first"))
    }

    /// Take one click of the pair; the second one makes the corner. A refused pair changes
    /// nothing and makes no undo step.
    pub fn fillet_pick(&mut self, level: i64, p: V2, tol: f64) -> Result<String, String> {
        if !self.edit.fillet.on {
            return Err(tr("dwgfillet.st.not_armed"));
        }
        if !(p.x.is_finite() && p.y.is_finite()) {
            return Err(tr("dwg_edit.st.path_bad"));
        }
        let hit = self.pick_at(level, p, tol).filter(|(_, hp)| {
            hp.texts.is_empty() && hp.fills.is_empty() && hp.arcs.is_empty() && hp.lines.len() == 1
        });
        let Some((k, hp)) = hit else {
            return Err(tr("dwgfillet.st.click_line"));
        };
        let i = hp.lines[0];
        let (id, l) = {
            let d = &self.list[k];
            (d.id, d.u.lines[i].clone())
        };
        let (a, b) = (v2(l.a.0, l.a.1), v2(l.b.0, l.b.1));
        if member_of_closed(&self.list[k].u, &l.layer, a, b) {
            return Err(tr("dwg_trim.st.in_poly"));
        }
        let Some(f) = self.edit.fillet.first else {
            self.edit.fillet.first = Some(First {
                drawing: id,
                idx: i,
                a: l.a,
                b: l.b,
                at: p,
            });
            return Ok(tr("dwgfillet.st.arm_second"));
        };
        if f.drawing != id {
            self.edit.fillet.first = None;
            return Err(tr("dwgfillet.st.other_drawing"));
        }
        let stale = self.list[k]
            .u
            .lines
            .get(f.idx)
            .is_none_or(|x| x.a != f.a || x.b != f.b);
        if stale {
            self.edit.fillet.first = None;
            return Err(tr("dwgfillet.st.changed"));
        }
        if f.idx == i {
            return Err(tr("dwgfillet.st.same_line"));
        }
        let r = self.edit.fillet.radius;
        let pl = plan((v2(f.a.0, f.a.1), v2(f.b.0, f.b.1)), f.at, (a, b), p, r).map_err(tr)?;
        let d = &mut self.list[k];
        self.steps.push(Step::geometry(d));
        let layer = d.u.lines[f.idx].layer.clone();
        let pen = d.u.lines[f.idx].pen;
        d.u.lines[f.idx].a = (pl.l1.0.x, pl.l1.0.y);
        d.u.lines[f.idx].b = (pl.l1.1.x, pl.l1.1.y);
        d.u.lines[i].a = (pl.l2.0.x, pl.l2.0.y);
        d.u.lines[i].b = (pl.l2.1.x, pl.l2.1.y);
        if let Some((c, rad, a0, a1)) = pl.arc {
            d.u.arcs.push(UArc {
                layer,
                c: (c.x, c.y),
                r: rad,
                a0,
                a1,
                pen,
            });
        }
        d.mark_edited();
        self.drop_recog(k);
        self.touch();
        self.edit.fillet.first = None;
        Ok(tr("dwgfillet.st.done"))
    }

    /// Canvas click while the fillet tool is armed. `true` when taken.
    pub(in crate::drawing::edit) fn fillet_click(&mut self, level: i64, p: V2, tol: f64) -> bool {
        if !self.edit.fillet.on || self.edit.draw.is_some() {
            return false;
        }
        let r = self.fillet_pick(level, p, tol);
        self.edit.fillet.note = Some(r.unwrap_or_else(|e| e));
        true
    }

    /// Stop the tool (another edit tool took over).
    pub(in crate::drawing::edit) fn fillet_disarm(&mut self) {
        self.edit.fillet.on = false;
        self.edit.fillet.first = None;
    }

    /// Fillet toggle with its radius field. Returns a status line.
    pub(in crate::drawing::edit) fn fillet_ui(&mut self, ui: &mut egui::Ui) -> Option<String> {
        let mut status = self.edit.fillet.note.take();
        if self.edit.draw.is_some() || self.edit.modify.armed || self.edit.trim.is_armed() {
            self.fillet_disarm();
        }
        let on = self.edit.fillet.on;
        let enabled = self.edit.draw.is_none();
        ui.add_enabled_ui(enabled, |ui| {
            ui.horizontal(|ui| {
                ui.label(tr("dwgfillet.radius"));
                ui.add(
                    egui::DragValue::new(&mut self.edit.fillet.radius)
                        .speed(0.01)
                        .range(0.0..=1000.0),
                )
                .on_hover_text(tr("dwgfillet.radius_tip"));
                let label = tr(if on {
                    "dwgfillet.cancel"
                } else {
                    "dwgfillet.fillet"
                });
                if ui
                    .selectable_label(on, label)
                    .on_hover_text(tr("dwgfillet.tip"))
                    .clicked()
                {
                    if on {
                        self.fillet_disarm();
                    } else {
                        let r = self.edit.fillet.radius;
                        status = Some(self.fillet_arm(r).unwrap_or_else(|e| e));
                    }
                }
            });
        });
        status
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drawing::Origin;
    use crate::editor::Editor;
    use ddc_io::drawing_model::ULine;
    use ddc_io::dxf::{Common, DxfDocument, Entity};

    fn setup() -> (Editor, Drawings) {
        let mut doc = DxfDocument::default();
        doc.entities.push(Entity::Line(ddc_io::dxf::Line {
            common: Common {
                layer: "Z".into(),
                ..Default::default()
            },
            start: [100.0, 100.0, 0.0],
            end: [101.0, 100.0, 0.0],
        }));
        let ed = Editor::new_in_memory().unwrap();
        let mut ds = Drawings::default();
        ds.add(&doc, "m", None, ed.level, Some(1.0), Origin::AsIs, v2(0.0, 0.0));
        ds.sel = 0;
        (ed, ds)
    }

    fn line(ds: &mut Drawings, layer: &str, a: (f64, f64), b: (f64, f64)) -> usize {
        let u = &mut ds.list[0].u;
        u.lines.push(ULine {
            layer: layer.into(),
            a,
            b,
            dashed: false,
            pen: 0,
        });
        u.lines.len() - 1
    }

    fn near(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
    }

    /// Both ends of a line in either order.
    fn ends(l: &ULine, p: (f64, f64), q: (f64, f64)) -> bool {
        (near(l.a, p) && near(l.b, q)) || (near(l.a, q) && near(l.b, p))
    }

    fn corner(ds: &mut Drawings, level: i64, r: f64, p1: V2, p2: V2) -> Result<String, String> {
        ds.fillet_arm(r).unwrap();
        ds.fillet_pick(level, p1, 0.05)?;
        ds.fillet_pick(level, p2, 0.05)
    }

    #[test]
    fn fillet_zero_makes_corner_one_undo() {
        let (mut ed, mut ds) = setup();
        let l1 = line(&mut ds, "Z", (0.0, 0.0), (4.0, 0.0));
        let l2 = line(&mut ds, "Z", (6.0, 3.0), (6.0, 10.0));
        ds.sync_undo(&mut ed);
        let n0 = ed.undo_len();
        let was = ds.list[0].u.clone();
        let lv = ed.level;
        corner(&mut ds, lv, 0.0, v2(1.0, 0.0), v2(6.0, 8.0)).unwrap();
        ds.sync_undo(&mut ed);
        assert_eq!(ed.undo_len(), n0 + 1, "one corner, one step");
        let u = &ds.list[0].u;
        assert!(ends(&u.lines[l1], (0.0, 0.0), (6.0, 0.0)), "{:?}", u.lines[l1]);
        assert!(ends(&u.lines[l2], (6.0, 0.0), (6.0, 10.0)), "{:?}", u.lines[l2]);
        assert_eq!(u.arcs.len(), was.arcs.len());
        ed.undo();
        ds.sync_undo(&mut ed);
        assert_eq!(ds.list[0].u, was);
    }

    #[test]
    fn fillet_radius_adds_tangent_arc_dxf_roundtrip() {
        let (mut ed, mut ds) = setup();
        let l1 = line(&mut ds, "Walls", (0.0, 0.0), (10.0, 0.0));
        let l2 = line(&mut ds, "Other", (0.0, 0.0), (0.0, 10.0));
        ds.sync_undo(&mut ed);
        let n0 = ed.undo_len();
        let lv = ed.level;
        corner(&mut ds, lv, 2.0, v2(5.0, 0.0), v2(0.0, 5.0)).unwrap();
        ds.sync_undo(&mut ed);
        assert_eq!(ed.undo_len(), n0 + 1);
        let u = &ds.list[0].u;
        assert!(ends(&u.lines[l1], (2.0, 0.0), (10.0, 0.0)));
        assert!(ends(&u.lines[l2], (0.0, 2.0), (0.0, 10.0)));
        let a = u.arcs.last().unwrap();
        assert_eq!(a.layer, "Walls");
        assert!(near(a.c, (2.0, 2.0)) && (a.r - 2.0).abs() < 1e-9);
        assert!(a.a1 > a.a0 && a.a0 >= 0.0);
        let mut doc = DxfDocument::default();
        ds.export_into(&mut doc);
        let ok = doc.entities.iter().any(|e| match e {
            Entity::Arc(x) => {
                let (s, t) = (x.start_angle.rem_euclid(360.0), x.end_angle.rem_euclid(360.0));
                x.common.layer == "Walls"
                    && (x.center[0] - 2.0).abs() < 1e-6
                    && (x.center[1] - 2.0).abs() < 1e-6
                    && (x.radius - 2.0).abs() < 1e-6
                    && (s - 180.0).abs() < 1e-6
                    && (t - 270.0).abs() < 1e-6
            }
            _ => false,
        });
        assert!(ok, "tangent arc in the DXF: {:?}", doc.entities);
    }

    #[test]
    fn fillet_parallel_refused() {
        let (mut ed, mut ds) = setup();
        line(&mut ds, "Z", (0.0, 0.0), (10.0, 0.0));
        line(&mut ds, "Z", (0.0, 3.0), (10.0, 3.0));
        ds.sync_undo(&mut ed);
        let n0 = ed.undo_len();
        let was = ds.list[0].u.clone();
        let lv = ed.level;
        let e = corner(&mut ds, lv, 1.0, v2(2.0, 0.0), v2(2.0, 3.0)).unwrap_err();
        assert_eq!(e, tr("dwgfillet.st.parallel"));
        ds.sync_undo(&mut ed);
        assert_eq!(ed.undo_len(), n0);
        assert_eq!(ds.list[0].u, was);
    }

    #[test]
    fn radius_too_large_refused() {
        let (mut ed, mut ds) = setup();
        line(&mut ds, "Z", (0.0, 0.0), (3.0, 0.0));
        line(&mut ds, "Z", (0.0, 0.0), (0.0, 3.0));
        ds.sync_undo(&mut ed);
        let n0 = ed.undo_len();
        let was = ds.list[0].u.clone();
        let lv = ed.level;
        let e = corner(&mut ds, lv, 3.0, v2(1.5, 0.0), v2(0.0, 1.5)).unwrap_err();
        assert_eq!(e, tr("dwgfillet.st.too_big"));
        ds.sync_undo(&mut ed);
        assert_eq!(ed.undo_len(), n0);
        assert_eq!(ds.list[0].u, was);
    }

    #[test]
    fn negative_radius_refused_without_arming() {
        let (ed, mut ds) = setup();
        assert_eq!(ds.fillet_arm(-1.0).unwrap_err(), tr("dwgfillet.st.radius_bad"));
        assert_eq!(ds.fillet_arm(f64::NAN).unwrap_err(), tr("dwgfillet.st.radius_bad"));
        let lv = ed.level;
        assert_eq!(
            ds.fillet_pick(lv, v2(0.0, 0.0), 0.05).unwrap_err(),
            tr("dwgfillet.st.not_armed")
        );
    }

    #[test]
    fn same_line_twice_refused_and_keeps_first() {
        let (mut ed, mut ds) = setup();
        line(&mut ds, "Z", (0.0, 0.0), (10.0, 0.0));
        ds.sync_undo(&mut ed);
        let n0 = ed.undo_len();
        let was = ds.list[0].u.clone();
        let lv = ed.level;
        let e = corner(&mut ds, lv, 1.0, v2(2.0, 0.0), v2(8.0, 0.0)).unwrap_err();
        assert_eq!(e, tr("dwgfillet.st.same_line"));
        ds.sync_undo(&mut ed);
        assert_eq!(ed.undo_len(), n0);
        assert_eq!(ds.list[0].u, was);
    }

    #[test]
    fn click_on_nothing_refused() {
        let (ed, mut ds) = setup();
        let lv = ed.level;
        ds.fillet_arm(1.0).unwrap();
        let e = ds.fillet_pick(lv, v2(50.0, 50.0), 0.05).unwrap_err();
        assert_eq!(e, tr("dwgfillet.st.click_line"));
    }

    #[test]
    fn plan_edge_cases() {
        let h = (v2(0.0, 0.0), v2(10.0, 0.0));
        // collinear lines never meet at a point
        let c = (v2(12.0, 0.0), v2(20.0, 0.0));
        assert_eq!(plan(h, v2(5.0, 0.0), c, v2(15.0, 0.0), 0.0), Err("dwgfillet.st.parallel"));
        // a line without length
        let z = (v2(3.0, 3.0), v2(3.0, 3.0));
        assert_eq!(plan(h, v2(5.0, 0.0), z, v2(3.0, 3.0), 0.0), Err("dwgfillet.st.degenerate"));
        // negative and non-finite radii
        let v = (v2(0.0, 0.0), v2(0.0, 10.0));
        assert_eq!(plan(h, v2(5.0, 0.0), v, v2(0.0, 5.0), -0.5), Err("dwgfillet.st.radius_bad"));
        assert_eq!(plan(h, v2(5.0, 0.0), v, v2(0.0, 5.0), f64::INFINITY), Err("dwgfillet.st.radius_bad"));
        // the largest radius that fits is refused at the limit and accepted just below it
        assert_eq!(plan(h, v2(5.0, 0.0), v, v2(0.0, 5.0), 10.0), Err("dwgfillet.st.too_big"));
        assert!(plan(h, v2(5.0, 0.0), v, v2(0.0, 5.0), 9.9).is_ok());
    }

    #[test]
    fn extend_to_corner_when_lines_do_not_reach() {
        // the vertical line stops short of the horizontal one; radius 0 extends it
        let p = plan(
            (v2(0.0, 0.0), v2(10.0, 0.0)),
            v2(5.0, 0.0),
            (v2(0.0, 4.0), v2(0.0, 10.0)),
            v2(0.0, 7.0),
            0.0,
        )
        .unwrap();
        assert!(p.arc.is_none());
        assert!(near((p.l2.0.x, p.l2.0.y), (0.0, 0.0)) && near((p.l2.1.x, p.l2.1.y), (0.0, 10.0)));
        assert!(near((p.l1.0.x, p.l1.0.y), (0.0, 0.0)) && near((p.l1.1.x, p.l1.1.y), (10.0, 0.0)));
    }

    #[test]
    fn picked_sides_respected_four_quadrants() {
        // two crossing lines through the origin; each click pair keeps its own quadrant
        let h = (v2(-10.0, 0.0), v2(10.0, 0.0));
        let v = (v2(0.0, -10.0), v2(0.0, 10.0));
        for (sx, sy) in [(1.0, 1.0), (-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)] {
            let p = plan(h, v2(5.0 * sx, 0.0), v, v2(0.0, 5.0 * sy), 1.0).unwrap();
            let (c, r, a0, a1) = p.arc.unwrap();
            assert!(near((c.x, c.y), (sx, sy)) && (r - 1.0).abs() < 1e-9, "{c:?}");
            // kept pieces run from the tangent point out to the picked end
            assert!(near((p.l1.0.x, p.l1.0.y), (sx, 0.0)));
            assert!(near((p.l1.1.x, p.l1.1.y), (10.0 * sx, 0.0)));
            assert!(near((p.l2.0.x, p.l2.0.y), (0.0, sy)));
            assert!(near((p.l2.1.x, p.l2.1.y), (0.0, 10.0 * sy)));
            assert!(a1 > a0 && a1 - a0 < std::f64::consts::PI + 1e-9);
        }
    }
}
