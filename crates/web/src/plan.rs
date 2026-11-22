// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Level plan drawn from the model's own plan primitives (`Model::plan_view`), with pan, zoom and pick.

use ddc_model::{Model, PlanViewSpec, ViewPrim};
use egui::{pos2, Color32, Pos2, Rect, Stroke, Vec2};

/// Height of the plan cut above the level, m (the editor's default).
pub const CUT: f64 = 1.2;

pub struct Plan {
    pub level: i64,
    pub prims: Vec<ViewPrim>,
    pub bbox: [f64; 4],
    /// Screen pixels per metre and the world point at the view centre.
    pub scale: f32,
    pub centre: (f64, f64),
    fitted: bool,
}

impl Plan {
    pub fn build(m: &Model, level: i64) -> Result<Plan, String> {
        let spec = PlanViewSpec {
            level,
            cut_height: CUT,
            grids: vec![],
        };
        let v = m.plan_view(&spec).map_err(|e| e.to_string())?;
        let mut prims = v.prims;
        if let Ok(c) = m.components_plan(level, ddc_geom::component::Lod::Coarse) {
            prims.extend(c);
        }
        Ok(Plan {
            level,
            prims,
            bbox: v.bbox,
            scale: 10.0,
            centre: ((v.bbox[0] + v.bbox[2]) * 0.5, (v.bbox[1] + v.bbox[3]) * 0.5),
            fitted: false,
        })
    }

    fn fit(&mut self, r: Rect) {
        let w = (self.bbox[2] - self.bbox[0]).max(1.0) as f32;
        let h = (self.bbox[3] - self.bbox[1]).max(1.0) as f32;
        self.scale = (r.width() / w).min(r.height() / h) * 0.9;
        self.centre = ((self.bbox[0] + self.bbox[2]) * 0.5, (self.bbox[1] + self.bbox[3]) * 0.5);
        self.fitted = true;
    }

    pub fn refit(&mut self) {
        self.fitted = false;
    }

    fn to_screen(&self, r: Rect, p: (f64, f64)) -> Pos2 {
        let c = r.center();
        pos2(
            c.x + ((p.0 - self.centre.0) as f32) * self.scale,
            c.y - ((p.1 - self.centre.1) as f32) * self.scale,
        )
    }

    fn to_world(&self, r: Rect, s: Pos2) -> (f64, f64) {
        let c = r.center();
        (
            self.centre.0 + ((s.x - c.x) / self.scale) as f64,
            self.centre.1 - ((s.y - c.y) / self.scale) as f64,
        )
    }

    /// Draws the plan; returns the element clicked, if any.
    pub fn ui(&mut self, ui: &mut egui::Ui, sel: Option<(&str, i64)>, dark: bool) -> Option<Option<(String, i64)>> {
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        if !self.fitted {
            self.fit(rect);
        }
        if resp.dragged() {
            let d: Vec2 = resp.drag_delta();
            self.centre.0 -= (d.x / self.scale) as f64;
            self.centre.1 += (d.y / self.scale) as f64;
        }
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                if let Some(hp) = resp.hover_pos() {
                    let before = self.to_world(rect, hp);
                    self.scale = (self.scale * (scroll * 0.002).exp()).clamp(0.2, 2000.0);
                    let after = self.to_world(rect, hp);
                    self.centre.0 += before.0 - after.0;
                    self.centre.1 += before.1 - after.1;
                }
            }
        }
        let p = ui.painter_at(rect);
        let (bg, ink, cut, sel_c) = if dark {
            (Color32::from_rgb(24, 26, 30), Color32::from_gray(200), Color32::from_gray(150), Color32::from_rgb(255, 140, 26))
        } else {
            (Color32::WHITE, Color32::from_gray(40), Color32::from_gray(95), Color32::from_rgb(230, 110, 0))
        };
        p.rect_filled(rect, 0.0, bg);
        let thin = Stroke::new(1.0_f32, ink);
        let is_sel = |t: &str, id: i64| sel.is_some_and(|s| s.0 == t && s.1 == id);
        for pr in &self.prims {
            match pr {
                ViewPrim::Cut { wall, ring, .. } => {
                    let pts: Vec<Pos2> = ring.iter().map(|q| self.to_screen(rect, *q)).collect();
                    let fill = if is_sel("wall", *wall) { sel_c } else { cut };
                    fill_ring(&p, &pts, fill);
                    p.add(egui::Shape::closed_line(pts, Stroke::new(1.0_f32, ink)));
                }
                ViewPrim::DoorLeaf { opening, a, b } => {
                    let c = if is_sel("opening", *opening) { sel_c } else { ink };
                    p.line_segment([self.to_screen(rect, *a), self.to_screen(rect, *b)], Stroke::new(1.5_f32, c));
                }
                ViewPrim::DoorSwing { opening, center, radius, a0, a1 } => {
                    let c = if is_sel("opening", *opening) { sel_c } else { ink };
                    let n = 16;
                    let pts: Vec<Pos2> = (0..=n)
                        .map(|k| {
                            let a = (a0 + (a1 - a0) * k as f64 / n as f64).to_radians();
                            self.to_screen(rect, (center.0 + radius * a.cos(), center.1 + radius * a.sin()))
                        })
                        .collect();
                    p.add(egui::Shape::line(pts, Stroke::new(0.8_f32, c)));
                }
                ViewPrim::WindowLine { opening, a, b } => {
                    let c = if is_sel("opening", *opening) { sel_c } else { ink };
                    p.line_segment([self.to_screen(rect, *a), self.to_screen(rect, *b)], Stroke::new(1.0_f32, c));
                }
                ViewPrim::CurtainLine { a, b, .. } => {
                    p.line_segment([self.to_screen(rect, *a), self.to_screen(rect, *b)], thin);
                }
                ViewPrim::Edge { table, element, a, b, .. } => {
                    let c = if is_sel(table, *element) { sel_c } else { ink.gamma_multiply(0.7) };
                    p.line_segment([self.to_screen(rect, *a), self.to_screen(rect, *b)], Stroke::new(0.8_f32, c));
                }
                ViewPrim::Grid { label, a, b, .. } => {
                    let s = Stroke::new(0.6_f32, Color32::from_rgb(90, 140, 200));
                    let (sa, sb) = (self.to_screen(rect, *a), self.to_screen(rect, *b));
                    p.line_segment([sa, sb], s);
                    p.text(sa, egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(11.0), s.color);
                }
                _ => {}
            }
        }
        for pr in &self.prims {
            if let ViewPrim::RoomTag { room, pos, name, number, area, .. } = pr {
                if self.scale < 4.0 {
                    continue;
                }
                let c = if is_sel("room", *room) { sel_c } else { ink };
                let s = self.to_screen(rect, *pos);
                let head = if number.is_empty() { name.clone() } else { format!("{number} {name}") };
                p.text(s, egui::Align2::CENTER_BOTTOM, head, egui::FontId::proportional(11.0), c);
                p.text(s, egui::Align2::CENTER_TOP, format!("{area:.1} m²"), egui::FontId::proportional(10.0), c);
            }
        }
        if resp.clicked() {
            let at = resp.interact_pointer_pos()?;
            return Some(self.hit(rect, at));
        }
        None
    }

    fn hit(&self, rect: Rect, s: Pos2) -> Option<(String, i64)> {
        let w = self.to_world(rect, s);
        let tol = (6.0 / self.scale) as f64;
        let mut best: Option<(f64, String, i64)> = None;
        let mut take = |d: f64, t: &str, id: i64| {
            if d <= tol && best.as_ref().is_none_or(|b| d < b.0) {
                best = Some((d, t.to_string(), id));
            }
        };
        for pr in &self.prims {
            match pr {
                ViewPrim::Cut { wall, ring, .. } if inside(ring, w) => take(0.0, "wall", *wall),
                ViewPrim::DoorLeaf { opening, a, b } | ViewPrim::WindowLine { opening, a, b } => {
                    take(seg_dist(w, *a, *b), "opening", *opening)
                }
                ViewPrim::Edge { table, element, a, b, .. } => take(seg_dist(w, *a, *b), table, *element),
                _ => {}
            }
        }
        if let Some(b) = best {
            return Some((b.1, b.2));
        }
        // a room by its tag: the nearest within a few metres
        self.prims
            .iter()
            .filter_map(|pr| match pr {
                ViewPrim::RoomTag { room, pos, .. } => Some((((pos.0 - w.0).powi(2) + (pos.1 - w.1).powi(2)).sqrt(), *room)),
                _ => None,
            })
            .filter(|(d, _)| *d < 1.5)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, r)| ("room".to_string(), r))
    }
}

fn inside(ring: &[(f64, f64)], p: (f64, f64)) -> bool {
    let mut c = false;
    let n = ring.len();
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + n - 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0 {
            c = !c;
        }
    }
    c
}

fn seg_dist(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l = dx * dx + dy * dy;
    let t = if l > 0.0 { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l).clamp(0.0, 1.0) } else { 0.0 };
    ((a.0 + t * dx - p.0).powi(2) + (a.1 + t * dy - p.1).powi(2)).sqrt()
}

/// Fills a simple polygon by ear clipping (wall cuts are often not convex).
fn fill_ring(p: &egui::Painter, pts: &[Pos2], c: Color32) {
    let n = pts.len();
    if n < 3 {
        return;
    }
    let area: f32 = (0..n).map(|i| pts[i].x * pts[(i + 1) % n].y - pts[(i + 1) % n].x * pts[i].y).sum();
    let mut idx: Vec<usize> = (0..n).collect();
    if area < 0.0 {
        idx.reverse();
    }
    let mut mesh = egui::Mesh::default();
    for q in pts {
        mesh.colored_vertex(*q, c);
    }
    let cr = |a: Pos2, b: Pos2, q: Pos2| (b.x - a.x) * (q.y - a.y) - (b.y - a.y) * (q.x - a.x);
    let mut guard = 0;
    while idx.len() > 3 && guard < n * n {
        guard += 1;
        let m = idx.len();
        let mut cut = false;
        for i in 0..m {
            let (ia, ib, ic) = (idx[(i + m - 1) % m], idx[i], idx[(i + 1) % m]);
            let (a, b, q) = (pts[ia], pts[ib], pts[ic]);
            if cr(a, b, q) <= 0.0 {
                continue;
            }
            let ear = idx.iter().all(|&k| {
                k == ia || k == ib || k == ic || !(cr(a, b, pts[k]) > 0.0 && cr(b, q, pts[k]) > 0.0 && cr(q, a, pts[k]) > 0.0)
            });
            if ear {
                mesh.add_triangle(ia as u32, ib as u32, ic as u32);
                idx.remove(i);
                cut = true;
                break;
            }
        }
        if !cut {
            break;
        }
    }
    if idx.len() == 3 {
        mesh.add_triangle(idx[0] as u32, idx[1] as u32, idx[2] as u32);
    }
    p.add(egui::Shape::mesh(mesh));
}
