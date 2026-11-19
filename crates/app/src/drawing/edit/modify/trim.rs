//! Trim and extend of drawing lines and arcs against picked boundaries (lines, arcs, circles,
//! closed outlines). One click is one undo step ([`Step::geometry`]) and marks the drawing
//! edited like the other modify tools. Nothing is written when nothing crosses.

use std::f64::consts::TAU;

use ddc_geom::{v2, V2};
use ddc_io::drawing_model::{Pick, UArc, ULine, Underlay};

use super::super::{tr, SAME};
use super::{angle, circle_of, cross_cc, cross_lc, cross_ll, member, Geo, MEET};
use crate::drawing::history::Step;
use crate::drawing::{seg_dist, Drawings};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TrimMode {
    #[default]
    Off,
    Trim,
    Extend,
}

/// A boundary piece; a full circle spans `TAU`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Bound {
    Seg(V2, V2),
    Arc { c: V2, r: f64, a0: f64, a1: f64 },
}

/// The armed mode, its boundary (as picked and as measured) and the last click's result.
#[derive(Default)]
pub(in crate::drawing::edit) struct TrimUi {
    mode: TrimMode,
    drawing: u64,
    pick: Pick,
    bounds: Vec<Bound>,
    /// Geometry signatures of the picked pieces: the armed one and each one our own cuts made.
    /// An index shifted onto another piece (delete, explode) matches none of them.
    hist: Vec<Vec<String>>,
    note: Option<String>,
}

impl TrimUi {
    pub(in crate::drawing::edit) fn disarm(&mut self) {
        self.mode = TrimMode::Off;
    }

    pub(in crate::drawing::edit) fn is_armed(&self) -> bool {
        self.mode != TrimMode::Off
    }
}

enum Change {
    Line(usize, Vec<(V2, V2)>),
    Arc(usize, Vec<(f64, f64)>),
}

fn fin(v: V2) -> bool {
    v.x.is_finite() && v.y.is_finite()
}

/// Boundary pieces of a pick: lines, arcs, and the edges or circles of closed outlines.
fn bounds_of(u: &Underlay, pk: &Pick) -> Vec<Bound> {
    let mut out = vec![];
    for &i in &pk.lines {
        let Some(l) = u.lines.get(i) else { continue };
        let (a, b) = (v2(l.a.0, l.a.1), v2(l.b.0, l.b.1));
        if fin(a) && fin(b) && (b - a).len() > SAME {
            out.push(Bound::Seg(a, b));
        }
    }
    for &i in &pk.arcs {
        let Some(x) = u.arcs.get(i) else { continue };
        let ok = [x.c.0, x.c.1, x.r, x.a0, x.a1].iter().all(|v| v.is_finite());
        if ok && x.r > SAME && x.a1 > x.a0 {
            out.push(Bound::Arc {
                c: v2(x.c.0, x.c.1),
                r: x.r,
                a0: x.a0,
                a1: x.a1,
            });
        }
    }
    for &i in &pk.closed {
        let Some(p) = u.closed.get(i) else { continue };
        let n = p.pts.len();
        if n == 0 {
            continue;
        }
        if let Some(r) = p.circle {
            let (sx, sy) = p.pts.iter().fold((0.0, 0.0), |s, q| (s.0 + q.0, s.1 + q.1));
            let mean = v2(sx / n as f64, sy / n as f64);
            let c = exact_centre(u, p, r, mean).unwrap_or(mean);
            if fin(c) && r.is_finite() && r > SAME {
                out.push(Bound::Arc { c, r, a0: 0.0, a1: TAU });
            }
            continue;
        }
        for j in 0..n {
            let (q, w) = (p.pts[j], p.pts[(j + 1) % n]);
            let (a, b) = (v2(q.0, q.1), v2(w.0, w.1));
            if fin(a) && fin(b) && (b - a).len() > SAME {
                out.push(Bound::Seg(a, b));
            }
        }
    }
    out
}

/// Centre of the full-circle arc paired with a circle outline: same layer and radius, and the
/// ring points lie on it (the mean of a ring sampled unevenly is off the true centre).
fn exact_centre(u: &Underlay, p: &ddc_io::drawing_model::UPoly, r: f64, mean: V2) -> Option<V2> {
    u.arcs
        .iter()
        .filter(|a| a.layer == p.layer && a.a1 - a.a0 >= TAU - 1e-9 && (a.r - r).abs() <= SAME)
        .map(|a| v2(a.c.0, a.c.1))
        .find(|c| {
            fin(*c)
                && ((*c - mean).len() <= SAME
                    || p.pts
                        .iter()
                        .all(|q| ((v2(q.0, q.1) - *c).len() - r).abs() <= MEET))
        })
}

/// Kind, layer and pen of every picked piece; `None` when an index no longer resolves.
fn pick_sig(u: &Underlay, pk: &Pick) -> Option<Vec<String>> {
    let mut out = vec![];
    for &i in &pk.lines {
        let l = u.lines.get(i)?;
        out.push(format!(
            "L|{}|{:?}|{:.6}|{:.6}|{:.6}|{:.6}",
            l.layer, l.pen, l.a.0, l.a.1, l.b.0, l.b.1
        ));
    }
    for &i in &pk.arcs {
        let a = u.arcs.get(i)?;
        out.push(format!(
            "A|{}|{:?}|{:.6}|{:.6}|{:.6}|{:.6}|{:.6}",
            a.layer, a.pen, a.c.0, a.c.1, a.r, a.a0, a.a1
        ));
    }
    for &i in &pk.closed {
        let p = u.closed.get(i)?;
        let pts: Vec<String> = p
            .pts
            .iter()
            .map(|q| format!("{:.6},{:.6}", q.0, q.1))
            .collect();
        out.push(format!("C|{}|{}|{}", p.layer, p.circle.is_some(), pts.join(";")));
    }
    Some(out)
}

/// Every picked index still names a piece of the drawing.
fn pick_resolves(u: &Underlay, pk: &Pick) -> bool {
    pk.lines.iter().all(|&i| i < u.lines.len())
        && pk.arcs.iter().all(|&i| i < u.arcs.len())
        && pk.closed.iter().all(|&i| i < u.closed.len())
}

/// `x` (on the circle) lies on the arc from `a0` to `a1`.
fn on_arc(c: V2, r: f64, a0: f64, a1: f64, x: V2) -> bool {
    let sweep = a1 - a0;
    if sweep >= TAU - 1e-9 {
        return true;
    }
    let e = MEET / r;
    let off = (angle(c, x) - a0).rem_euclid(TAU);
    off <= sweep + e || off >= TAU - e
}

/// Where the infinite line `p + t d` meets the boundary piece.
fn hits_line(p: V2, d: V2, b: &Bound) -> Vec<V2> {
    match *b {
        Bound::Seg(q, e) => cross_ll(p, d, q, e - q)
            .into_iter()
            .filter(|x| seg_dist(*x, q, e) <= MEET)
            .collect(),
        Bound::Arc { c, r, a0, a1 } => cross_lc(p, d, c, r)
            .into_iter()
            .filter(|x| on_arc(c, r, a0, a1, *x))
            .collect(),
    }
}

/// Where the whole circle (`c0`, `r0`) meets the boundary piece.
fn hits_circle(c0: V2, r0: f64, b: &Bound) -> Vec<V2> {
    match *b {
        Bound::Seg(q, e) => cross_lc(q, e - q, c0, r0)
            .into_iter()
            .filter(|x| seg_dist(*x, q, e) <= MEET)
            .collect(),
        Bound::Arc { c, r, a0, a1 } => cross_cc(c0, r0, c, r)
            .into_iter()
            .filter(|x| on_arc(c, r, a0, a1, *x))
            .collect(),
    }
}

/// The part of the line between the crossings around `click` goes; one side without a crossing
/// goes to the end. Pieces kept, in line order.
fn trim_line(a: V2, b: V2, click: V2, bounds: &[Bound]) -> Result<Vec<(V2, V2)>, &'static str> {
    let d = b - a;
    let l2 = d.dot(d);
    let len = l2.sqrt();
    if !(len > SAME) {
        return Err("dwg_trim.st.trim_none");
    }
    let t_of = |x: V2| (x - a).dot(d) / l2;
    let e = MEET / len;
    let ts: Vec<f64> = bounds
        .iter()
        .flat_map(|bd| hits_line(a, d, bd))
        .map(t_of)
        .filter(|t| *t > e && *t < 1.0 - e)
        .collect();
    let tc = t_of(click).clamp(0.0, 1.0);
    let lo = ts.iter().copied().filter(|t| *t <= tc).max_by(|x, y| x.total_cmp(y));
    let hi = ts.iter().copied().filter(|t| *t > tc).min_by(|x, y| x.total_cmp(y));
    let mut out = vec![];
    if let Some(t) = lo {
        out.push((a, a + d * t));
    }
    if let Some(t) = hi {
        out.push((a + d * t, b));
    }
    if out.is_empty() {
        return Err("dwg_trim.st.trim_none");
    }
    Ok(out)
}

/// Same for an arc, in angles. A full circle needs two crossings and keeps the long way round.
fn trim_arc(
    c: V2,
    r: f64,
    a0: f64,
    a1: f64,
    click: V2,
    bounds: &[Bound],
) -> Result<Vec<(f64, f64)>, &'static str> {
    let sweep = a1 - a0;
    let full = sweep >= TAU - 1e-9;
    let e = MEET / r;
    let off = |x: V2| (angle(c, x) - a0).rem_euclid(TAU);
    let mut os: Vec<f64> = bounds
        .iter()
        .flat_map(|bd| hits_circle(c, r, bd))
        .map(off)
        .filter(|o| full || (*o > e && *o < sweep - e))
        .collect();
    os.sort_by(|x, y| x.total_cmp(y));
    os.dedup_by(|x, y| (*x - *y).abs() <= e);
    if full && os.len() > 1 && os[0] + TAU - os[os.len() - 1] <= e {
        os.pop();
    }
    let o = off(click);
    let oc = if full || o <= sweep {
        o
    } else if o - sweep < TAU - o {
        sweep
    } else {
        0.0
    };
    let lo = os.iter().copied().filter(|v| *v <= oc).last();
    let hi = os.iter().copied().find(|v| *v > oc);
    if full {
        if os.len() < 2 {
            return Err("dwg_trim.st.trim_none");
        }
        let (Some(lo), Some(hi)) = (lo.or(os.last().copied()), hi.or(os.first().copied())) else {
            return Err("dwg_trim.st.trim_none");
        };
        let span = (lo - hi).rem_euclid(TAU);
        if span <= e {
            return Err("dwg_trim.st.trim_none");
        }
        return Ok(vec![(a0 + hi, a0 + hi + span)]);
    }
    let mut out = vec![];
    if let Some(v) = lo {
        out.push((a0, a0 + v));
    }
    if let Some(v) = hi {
        out.push((a0 + v, a1));
    }
    if out.is_empty() {
        return Err("dwg_trim.st.trim_none");
    }
    Ok(out)
}

/// The end of the line nearer `click` runs on to the nearest boundary crossing beyond it.
fn extend_line(a: V2, b: V2, click: V2, bounds: &[Bound]) -> Result<(V2, V2), &'static str> {
    let d = b - a;
    let l2 = d.dot(d);
    let len = l2.sqrt();
    if !(len > SAME) {
        return Err("dwg_trim.st.extend_none");
    }
    let t_of = |x: V2| (x - a).dot(d) / l2;
    let e = MEET / len;
    let ts: Vec<f64> = bounds
        .iter()
        .flat_map(|bd| hits_line(a, d, bd))
        .map(t_of)
        .collect();
    let at_b = t_of(click) >= 0.5;
    if ts.iter().any(|t| (if at_b { *t - 1.0 } else { *t }).abs() <= e) {
        return Err("dwg_trim.st.extend_already");
    }
    if at_b {
        let t = ts
            .iter()
            .copied()
            .filter(|t| *t > 1.0 + e)
            .min_by(|x, y| x.total_cmp(y));
        t.map(|t| (a, a + d * t)).ok_or("dwg_trim.st.extend_none")
    } else {
        let t = ts
            .iter()
            .copied()
            .filter(|t| *t < -e)
            .max_by(|x, y| x.total_cmp(y));
        t.map(|t| (a + d * t, b)).ok_or("dwg_trim.st.extend_none")
    }
}

/// The end of the arc nearer `click` runs on along its circle to the nearest crossing.
fn extend_arc(
    c: V2,
    r: f64,
    a0: f64,
    a1: f64,
    click: V2,
    bounds: &[Bound],
) -> Result<(f64, f64), &'static str> {
    let sweep = a1 - a0;
    if sweep >= TAU - 1e-9 {
        return Err("dwg_trim.st.extend_full");
    }
    let e = MEET / r;
    let o = (angle(c, click) - a0).rem_euclid(TAU);
    let at_end = if o <= sweep {
        o >= sweep * 0.5
    } else {
        o - sweep < TAU - o
    };
    let gaps: Vec<f64> = bounds
        .iter()
        .flat_map(|bd| hits_circle(c, r, bd))
        .map(|x| {
            let an = angle(c, x);
            if at_end {
                (an - a1).rem_euclid(TAU)
            } else {
                (a0 - an).rem_euclid(TAU)
            }
        })
        .collect();
    if gaps.iter().any(|g| *g <= e || *g >= TAU - e) {
        return Err("dwg_trim.st.extend_already");
    }
    let room = TAU - sweep;
    let g = gaps
        .iter()
        .copied()
        .filter(|g| *g > e && *g < room - e)
        .min_by(|x, y| x.total_cmp(y))
        .ok_or("dwg_trim.st.extend_none")?;
    Ok(if at_end { (a0, a1 + g) } else { (a0 - g, a1) })
}

/// Start angle back into one turn from zero, end shifted along.
fn norm(a0: f64, a1: f64) -> (f64, f64) {
    let turns = (a0 / TAU).floor() * TAU;
    (a0 - turns, a1 - turns)
}

impl Drawings {
    /// Take the picked lines, arcs and outlines as the boundary and wait for clicks that trim
    /// or extend. Refused with a reason when the pick holds no curve.
    pub fn trim_arm(&mut self, mode: TrimMode) -> Result<String, String> {
        if mode == TrimMode::Off {
            self.edit.trim.mode = TrimMode::Off;
            return Ok(String::new());
        }
        let Some((k, pk)) = self.pick.clone() else {
            return Err(tr("dlg.dw.st.nothing_picked"));
        };
        let Some(d) = self.list.get(k) else {
            return Err(tr("dlg.dw.st.drawing_gone"));
        };
        if pk.is_empty() {
            return Err(tr("dlg.dw.st.nothing_picked"));
        }
        let bounds = bounds_of(&d.u, &pk);
        if bounds.is_empty() {
            return Err(tr("dwg_modify.st.only_curves"));
        }
        let id = d.id;
        let sig = pick_sig(&d.u, &pk).unwrap_or_default();
        self.edit.modify.armed = false;
        self.fillet_disarm();
        let t = &mut self.edit.trim;
        t.hist = vec![sig];
        t.mode = mode;
        t.drawing = id;
        t.pick = pk;
        t.bounds = bounds;
        t.note = None;
        let key = if mode == TrimMode::Trim {
            "dwg_trim.st.arm_trim"
        } else {
            "dwg_trim.st.arm_extend"
        };
        Ok(tr(key))
    }

    /// Trim or extend (per the armed mode) the line or arc at `p` against the boundary.
    /// Nothing is changed, and no undo step made, when it is refused.
    pub fn cut_at(&mut self, p: V2, tol: f64) -> Result<String, String> {
        let mode = self.edit.trim.mode;
        if mode == TrimMode::Off {
            return Err(tr("dwg_trim.st.not_armed"));
        }
        if !fin(p) {
            return Err(tr("dwg_edit.st.path_bad"));
        }
        let id = self.edit.trim.drawing;
        let Some(k) = self.list.iter().position(|d| d.id == id) else {
            self.edit.trim.mode = TrimMode::Off;
            return Err(tr("dlg.dw.st.drawing_gone"));
        };
        let mut updated = false;
        let now = bounds_of(&self.list[k].u, &self.edit.trim.pick);
        if now != self.edit.trim.bounds {
            // an undo or a cut moved the boundary: follow it while the picked pieces exist
            let same = pick_resolves(&self.list[k].u, &self.edit.trim.pick)
                && pick_sig(&self.list[k].u, &self.edit.trim.pick)
                    .is_some_and(|s| self.edit.trim.hist.contains(&s));
            if now.is_empty() || !same {
                self.edit.trim.mode = TrimMode::Off;
                return Err(tr("dwg_trim.st.bound_changed"));
            }
            self.edit.trim.bounds = now;
            updated = true;
        }
        let level = self.list[k].level;
        let hit = self.pick_at(level, p, tol).filter(|(hk, hp)| {
            *hk == k
                && hp.texts.is_empty()
                && hp.fills.is_empty()
                && hp.lines.len() + hp.arcs.len() == 1
        });
        let Some((_, hp)) = hit else {
            return Err(tr("dwg_trim.st.click_curve"));
        };
        let bounds = self.edit.trim.bounds.clone();
        let u = &self.list[k].u;
        let change = if let Some(&i) = hp.lines.first() {
            let l = &u.lines[i];
            let (a, b) = (v2(l.a.0, l.a.1), v2(l.b.0, l.b.1));
            if u.closed.iter().any(|c| member(c, &l.layer, &Geo::Line(a, b))) {
                return Err(tr("dwg_trim.st.in_poly"));
            }
            let segs = if mode == TrimMode::Trim {
                trim_line(a, b, p, &bounds)
            } else {
                extend_line(a, b, p, &bounds).map(|s| vec![s])
            };
            Change::Line(i, segs.map_err(tr)?)
        } else if let Some(&i) = hp.arcs.first() {
            let x = &u.arcs[i];
            let (c, r) = (v2(x.c.0, x.c.1), x.r);
            let g = Geo::Arc {
                c,
                r,
                a0: x.a0,
                a1: x.a1,
                fwd: true,
            };
            if u
                .closed
                .iter()
                .any(|q| member(q, &x.layer, &g) || circle_of(q, &x.layer, &g))
            {
                return Err(tr("dwg_trim.st.in_poly"));
            }
            let spans = if mode == TrimMode::Trim {
                trim_arc(c, r, x.a0, x.a1, p, &bounds)
            } else {
                extend_arc(c, r, x.a0, x.a1, p, &bounds).map(|s| vec![s])
            };
            Change::Arc(i, spans.map_err(tr)?)
        } else {
            return Err(tr("dwg_trim.st.click_curve"));
        };
        let Some(d) = self.list.get_mut(k) else {
            return Err(tr("dlg.dw.st.drawing_gone"));
        };
        self.steps.push(Step::geometry(d));
        match change {
            Change::Line(i, segs) => {
                let base = d.u.lines[i].clone();
                for (j, (a, b)) in segs.into_iter().enumerate() {
                    if j == 0 {
                        d.u.lines[i].a = (a.x, a.y);
                        d.u.lines[i].b = (b.x, b.y);
                    } else {
                        d.u.lines.push(ULine {
                            layer: base.layer.clone(),
                            a: (a.x, a.y),
                            b: (b.x, b.y),
                            dashed: base.dashed,
                            pen: base.pen,
                        });
                    }
                }
            }
            Change::Arc(i, spans) => {
                let base = d.u.arcs[i].clone();
                for (j, (s0, s1)) in spans.into_iter().enumerate() {
                    let (n0, n1) = norm(s0, s1);
                    if j == 0 {
                        d.u.arcs[i].a0 = n0;
                        d.u.arcs[i].a1 = n1;
                    } else {
                        d.u.arcs.push(UArc {
                            layer: base.layer.clone(),
                            c: base.c,
                            r: base.r,
                            a0: n0,
                            a1: n1,
                            pen: base.pen,
                        });
                    }
                }
            }
        }
        d.mark_edited();
        self.drop_recog(k);
        self.touch();
        // a boundary piece that was cut keeps serving with its new shape
        self.edit.trim.bounds = bounds_of(&self.list[k].u, &self.edit.trim.pick);
        if let Some(s) = pick_sig(&self.list[k].u, &self.edit.trim.pick) {
            if !self.edit.trim.hist.contains(&s) {
                self.edit.trim.hist.push(s);
            }
        }
        let done = tr(if mode == TrimMode::Trim {
            "dwg_trim.st.trim_done"
        } else {
            "dwg_trim.st.extend_done"
        });
        Ok(if updated {
            format!("{done}. {}", tr("dwg_trim.st.bound_updated"))
        } else {
            done
        })
    }

    /// Canvas click while trim or extend is armed. `true` when taken.
    pub(in crate::drawing::edit) fn trim_click(&mut self, level: i64, p: V2, tol: f64) -> bool {
        if self.edit.trim.mode == TrimMode::Off || self.edit.draw.is_some() {
            return false;
        }
        let id = self.edit.trim.drawing;
        if !self
            .list
            .iter()
            .any(|d| d.id == id && d.level == level && d.visible)
        {
            return false;
        }
        let r = self.cut_at(p, tol);
        self.edit.trim.note = Some(r.unwrap_or_else(|e| e));
        true
    }

    /// Trim and extend buttons. Returns a status line.
    pub(in crate::drawing::edit) fn trim_ui(&mut self, ui: &mut egui::Ui) -> Option<String> {
        let mut status = self.edit.trim.note.take();
        if self.edit.draw.is_some() || self.edit.modify.armed {
            self.edit.trim.mode = TrimMode::Off;
        }
        let has = self.pick.as_ref().is_some_and(|(_, p)| !p.is_empty());
        let mode = self.edit.trim.mode;
        let enabled = self.edit.draw.is_none() && (has || mode != TrimMode::Off);
        ui.add_enabled_ui(enabled, |ui| {
            ui.horizontal(|ui| {
                let rows = [
                    (TrimMode::Trim, "dwg_trim.trim", "dwg_trim.trim_cancel", "dwg_trim.trim_tip"),
                    (
                        TrimMode::Extend,
                        "dwg_trim.extend",
                        "dwg_trim.extend_cancel",
                        "dwg_trim.extend_tip",
                    ),
                ];
                for (m, on, off, tip) in rows {
                    let armed = mode == m;
                    if ui
                        .selectable_label(armed, tr(if armed { off } else { on }))
                        .on_hover_text(tr(tip))
                        .clicked()
                    {
                        if armed {
                            self.edit.trim.mode = TrimMode::Off;
                        } else {
                            status = Some(self.trim_arm(m).unwrap_or_else(|e| e));
                        }
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
    use ddc_io::drawing_model::UPoly;
    use ddc_io::dxf::{Common, DxfDocument, Entity};
    use std::f64::consts::PI;

    const EPS: f64 = 1e-9;

    fn near(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < EPS && (a.1 - b.1).abs() < EPS
    }

    fn empty() -> (Editor, Drawings) {
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

    fn line(ds: &mut Drawings, a: (f64, f64), b: (f64, f64)) -> usize {
        let u = &mut ds.list[0].u;
        u.lines.push(ULine {
            layer: "Z".into(),
            a,
            b,
            dashed: false,
            pen: 0,
        });
        u.lines.len() - 1
    }

    fn arc(ds: &mut Drawings, c: (f64, f64), r: f64, a0: f64, a1: f64) -> usize {
        let u = &mut ds.list[0].u;
        u.arcs.push(UArc {
            layer: "Z".into(),
            c,
            r,
            a0,
            a1,
            pen: 0,
        });
        u.arcs.len() - 1
    }

    /// Pick the boundary and arm `mode`.
    fn arm(ds: &mut Drawings, lines: &[usize], arcs: &[usize], mode: TrimMode) {
        ds.pick = Some((
            0,
            Pick {
                lines: lines.to_vec(),
                arcs: arcs.to_vec(),
                ..Default::default()
            },
        ));
        ds.trim_arm(mode).unwrap();
    }

    fn export(ds: &Drawings) -> DxfDocument {
        let mut doc = DxfDocument::default();
        ds.export_into(&mut doc);
        doc
    }

    fn has_line(doc: &DxfDocument, a: (f64, f64), b: (f64, f64)) -> bool {
        let close =
            |p: [f64; 3], q: (f64, f64)| (p[0] - q.0).abs() < 1e-6 && (p[1] - q.1).abs() < 1e-6;
        doc.entities.iter().any(|e| match e {
            Entity::Line(l) => {
                (close(l.start, a) && close(l.end, b)) || (close(l.start, b) && close(l.end, a))
            }
            _ => false,
        })
    }

    #[test]
    fn trim_line_at_boundary_click_side() {
        // two boundaries: the part between them goes, the outer parts stay
        let (_ed, mut ds) = empty();
        let li = line(&mut ds, (0.0, 0.0), (10.0, 0.0));
        let b1 = line(&mut ds, (3.0, -1.0), (3.0, 1.0));
        let b2 = line(&mut ds, (7.0, -1.0), (7.0, 1.0));
        arm(&mut ds, &[b1, b2], &[], TrimMode::Trim);
        let n = ds.list[0].u.lines.len();
        ds.cut_at(v2(5.0, 0.0), 0.05).unwrap();
        let u = &ds.list[0].u;
        assert_eq!(u.lines.len(), n + 1);
        assert!(near(u.lines[li].a, (0.0, 0.0)) && near(u.lines[li].b, (3.0, 0.0)));
        let l = u.lines.last().unwrap();
        assert!(near(l.a, (7.0, 0.0)) && near(l.b, (10.0, 0.0)), "{l:?}");
        assert_eq!((l.layer.as_str(), l.pen, l.dashed), ("Z", 0, false));

        // one boundary: the clicked side goes, the other stays
        let (_ed, mut ds) = empty();
        let li = line(&mut ds, (0.0, 0.0), (10.0, 0.0));
        let b1 = line(&mut ds, (3.0, -1.0), (3.0, 1.0));
        arm(&mut ds, &[b1], &[], TrimMode::Trim);
        ds.cut_at(v2(8.0, 0.0), 0.05).unwrap();
        let l = &ds.list[0].u.lines[li];
        assert!(near(l.a, (0.0, 0.0)) && near(l.b, (3.0, 0.0)), "{l:?}");
        let (_ed, mut ds) = empty();
        let li = line(&mut ds, (0.0, 0.0), (10.0, 0.0));
        let b1 = line(&mut ds, (3.0, -1.0), (3.0, 1.0));
        arm(&mut ds, &[b1], &[], TrimMode::Trim);
        ds.cut_at(v2(1.0, 0.0), 0.05).unwrap();
        let l = &ds.list[0].u.lines[li];
        assert!(near(l.a, (3.0, 0.0)) && near(l.b, (10.0, 0.0)), "{l:?}");
    }

    #[test]
    fn trim_arc_by_line() {
        // the line y = 1 cuts the half circle at pi/6 and 5 pi/6
        let (_ed, mut ds) = empty();
        let ai = arc(&mut ds, (0.0, 0.0), 2.0, 0.0, PI);
        let b = line(&mut ds, (-5.0, 1.0), (5.0, 1.0));
        arm(&mut ds, &[b], &[], TrimMode::Trim);
        let n = ds.list[0].u.arcs.len();
        ds.cut_at(v2(0.0, 2.0), 0.05).unwrap();
        let u = &ds.list[0].u;
        assert_eq!(u.arcs.len(), n + 1);
        assert!(u.arcs[ai].a0.abs() < EPS && (u.arcs[ai].a1 - PI / 6.0).abs() < EPS);
        let t = u.arcs.last().unwrap();
        assert!((t.a0 - 5.0 * PI / 6.0).abs() < EPS && (t.a1 - PI).abs() < EPS, "{t:?}");
        assert!(near(t.c, (0.0, 0.0)) && (t.r - 2.0).abs() < EPS);

        // a click near the start removes the piece up to the crossing
        let (_ed, mut ds) = empty();
        let ai = arc(&mut ds, (0.0, 0.0), 2.0, 0.0, PI);
        let b = line(&mut ds, (-5.0, 1.0), (5.0, 1.0));
        arm(&mut ds, &[b], &[], TrimMode::Trim);
        ds.cut_at(v2(2.0 * 0.1_f64.cos(), 2.0 * 0.1_f64.sin()), 0.05)
            .unwrap();
        let a = &ds.list[0].u.arcs[ai];
        assert!((a.a0 - PI / 6.0).abs() < EPS && (a.a1 - PI).abs() < EPS, "{a:?}");
    }

    #[test]
    fn extend_line_to_arc() {
        let (_ed, mut ds) = empty();
        let li = line(&mut ds, (0.0, 0.0), (1.0, 0.0));
        let ai = arc(&mut ds, (5.0, 0.0), 2.0, PI / 2.0, 3.0 * PI / 2.0);
        arm(&mut ds, &[], &[ai], TrimMode::Extend);
        // the start end looks away from the arc: nothing there
        let before = ds.list[0].u.lines[li].clone();
        assert!(ds.cut_at(v2(0.1, 0.0), 0.05).is_err());
        assert_eq!(ds.list[0].u.lines[li], before);
        // the far end runs on to the arc at (3, 0)
        ds.cut_at(v2(0.9, 0.0), 0.05).unwrap();
        let l = &ds.list[0].u.lines[li];
        assert!(near(l.a, (0.0, 0.0)) && near(l.b, (3.0, 0.0)), "{l:?}");
        // now it ends on the boundary: refused
        assert!(ds.cut_at(v2(2.9, 0.0), 0.05).is_err());
    }

    #[test]
    fn no_crossing_refuses_and_keeps() {
        let (mut ed, mut ds) = empty();
        let li = line(&mut ds, (0.0, 0.0), (10.0, 0.0));
        let b = line(&mut ds, (20.0, -1.0), (20.0, 1.0));
        arm(&mut ds, &[b], &[], TrimMode::Trim);
        ds.sync_undo(&mut ed);
        let n0 = ed.undo_len();
        let was = ds.list[0].u.clone();
        let dirty = ds.list[0].dirty;
        let e = ds.cut_at(v2(5.0, 0.0), 0.05).unwrap_err();
        assert!(!e.is_empty());
        // a click on nothing is refused too
        assert!(!ds.cut_at(v2(5.0, 5.0), 0.05).unwrap_err().is_empty());
        ds.trim_arm(TrimMode::Extend).unwrap();
        // the start end looks away from the boundary (the far end would reach it at x = 20)
        assert!(!ds.cut_at(v2(2.0, 0.0), 0.05).unwrap_err().is_empty());
        ds.sync_undo(&mut ed);
        assert_eq!(ed.undo_len(), n0, "no step for a refusal");
        assert_eq!(ds.list[0].u, was);
        assert_eq!(ds.list[0].dirty, dirty);
        assert_eq!(ds.list[0].u.lines[li].b, (10.0, 0.0));
    }

    #[test]
    fn trim_refuses_closed_outline_member() {
        let (_ed, mut ds) = empty();
        let e1 = line(&mut ds, (0.0, 0.0), (4.0, 0.0));
        line(&mut ds, (4.0, 0.0), (4.0, 4.0));
        line(&mut ds, (4.0, 4.0), (0.0, 4.0));
        line(&mut ds, (0.0, 4.0), (0.0, 0.0));
        ds.list[0].u.closed.push(UPoly {
            layer: "Z".into(),
            pts: vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)],
            circle: None,
        });
        let b = line(&mut ds, (2.0, -1.0), (2.0, 1.0));
        arm(&mut ds, &[b], &[], TrimMode::Trim);
        let was = ds.list[0].u.clone();
        assert!(!ds.cut_at(v2(1.0, 0.0), 0.05).unwrap_err().is_empty());
        assert_eq!(ds.list[0].u, was);
        assert_eq!(ds.list[0].u.lines[e1].b, (4.0, 0.0));
    }

    #[test]
    fn trim_one_undo_and_dxf_export() {
        let (mut ed, mut ds) = empty();
        let li = line(&mut ds, (0.0, 0.0), (10.0, 0.0));
        let b1 = line(&mut ds, (3.0, -1.0), (3.0, 1.0));
        let b2 = line(&mut ds, (7.0, -1.0), (7.0, 1.0));
        arm(&mut ds, &[b1, b2], &[], TrimMode::Trim);
        ds.sync_undo(&mut ed);
        let n0 = ed.undo_len();
        let lines0 = ds.list[0].u.lines.len();
        assert!(has_line(&export(&ds), (0.0, 0.0), (10.0, 0.0)));
        ds.cut_at(v2(5.0, 0.0), 0.05).unwrap();
        ds.sync_undo(&mut ed);
        assert_eq!(ed.undo_len(), n0 + 1, "one trim, one step");
        assert!(ds.list[0].dirty);
        let doc = export(&ds);
        assert!(has_line(&doc, (0.0, 0.0), (3.0, 0.0)));
        assert!(has_line(&doc, (7.0, 0.0), (10.0, 0.0)));
        assert!(!has_line(&doc, (0.0, 0.0), (10.0, 0.0)));
        ed.undo();
        ds.sync_undo(&mut ed);
        assert_eq!(ds.list[0].u.lines.len(), lines0);
        assert!(near(ds.list[0].u.lines[li].b, (10.0, 0.0)));
        let doc = export(&ds);
        assert!(has_line(&doc, (0.0, 0.0), (10.0, 0.0)));
        assert!(!has_line(&doc, (7.0, 0.0), (10.0, 0.0)));
    }

    #[test]
    fn circle_bound_uses_exact_centre() {
        let (_ed, mut ds) = empty();
        // three ring points at 0, 90 and 180 degrees: their mean is off the centre
        let ring = vec![(7.0, 5.0), (5.0, 7.0), (3.0, 5.0)];
        let u = &mut ds.list[0].u;
        u.closed.push(UPoly {
            layer: "Z".into(),
            pts: ring.clone(),
            circle: Some(2.0),
        });
        let pk = Pick {
            closed: vec![0],
            ..Default::default()
        };
        let mean_only = bounds_of(&ds.list[0].u, &pk);
        assert!(matches!(mean_only[0], Bound::Arc { c, .. } if (c.y - 17.0 / 3.0).abs() < EPS));
        arc(&mut ds, (5.0, 5.0), 2.0, 0.0, TAU);
        let b = bounds_of(&ds.list[0].u, &pk);
        match b[0] {
            Bound::Arc { c, r, .. } => {
                assert!((c.x - 5.0).abs() < EPS && (c.y - 5.0).abs() < EPS && r == 2.0)
            }
            _ => panic!("circle expected"),
        }
    }

    #[test]
    fn undo_after_cut_rearms_with_new_bounds() {
        let (mut ed, mut ds) = empty();
        // the horizontal piece is both boundary and target
        let h = line(&mut ds, (0.0, 0.0), (10.0, 0.0));
        let v = line(&mut ds, (3.0, -1.0), (3.0, 1.0));
        arm(&mut ds, &[h, v], &[], TrimMode::Trim);
        ds.sync_undo(&mut ed);
        let first = ds.edit.trim.bounds.clone();
        ds.cut_at(v2(8.0, 0.0), 0.05).unwrap();
        ds.sync_undo(&mut ed);
        assert_ne!(ds.edit.trim.bounds, first);
        ed.undo();
        ds.sync_undo(&mut ed);
        let msg = ds.cut_at(v2(8.0, 0.0), 0.05).unwrap();
        assert!(msg.contains(&tr("dwg_trim.st.bound_updated")), "{msg}");
        assert_eq!(ds.edit.trim.mode, TrimMode::Trim);
        assert!(near(ds.list[0].u.lines[h].b, (3.0, 0.0)));
    }

    #[test]
    fn cut_changes_geometry_still_follows() {
        let (_ed, mut ds) = empty();
        let h = line(&mut ds, (0.0, 0.0), (10.0, 0.0));
        let v = line(&mut ds, (3.0, -1.0), (3.0, 1.0));
        arm(&mut ds, &[h, v], &[], TrimMode::Trim);
        ds.cut_at(v2(8.0, 0.0), 0.05).unwrap();
        // the boundary piece h was cut; a second cut keeps following its new shape
        line(&mut ds, (1.0, 2.0), (1.0, -2.0));
        let msg = ds.cut_at(v2(1.0, 1.5), 0.05);
        assert!(msg.is_ok(), "{msg:?}");
        assert_eq!(ds.edit.trim.mode, TrimMode::Trim);
    }

    #[test]
    fn same_layer_neighbour_after_delete_disarms() {
        let (_ed, mut ds) = empty();
        line(&mut ds, (0.0, 0.0), (10.0, 0.0));
        let lo = line(&mut ds, (0.0, 5.0), (1.0, 5.0));
        let b = line(&mut ds, (3.0, -1.0), (3.0, 1.0));
        line(&mut ds, (20.0, -1.0), (20.0, 1.0));
        arm(&mut ds, &[b], &[], TrimMode::Trim);
        // a delete below the pick shifts it onto another primitive
        ds.list[0].u.lines.remove(lo);
        assert!(ds.cut_at(v2(8.0, 0.0), 0.05).is_err());
        assert_eq!(ds.edit.trim.mode, TrimMode::Off);
    }

    #[test]
    fn pick_gone_disarms() {
        let (_ed, mut ds) = empty();
        line(&mut ds, (0.0, 0.0), (10.0, 0.0));
        let b = line(&mut ds, (3.0, -1.0), (3.0, 1.0));
        arm(&mut ds, &[b], &[], TrimMode::Trim);
        ds.list[0].u.lines.truncate(b);
        assert!(!ds.cut_at(v2(8.0, 0.0), 0.05).unwrap_err().is_empty());
        assert_eq!(ds.edit.trim.mode, TrimMode::Off);
    }
}
