//! Wall editing panel: joints in plan (icon on a node: click cycles the type, "forbid"
//! breaks the joint), layer cleanup by priority, composite wall elevation with profiles and openings,
//! temporary opening dimensions (to ends, sill, head) with value entry, vertical structure.
//! All geometry lives in `ddc_geom::wall_edit`; this file holds only state, hit testing and drawing.

use crate::ui_kit::Tokens;
use ddc_geom::wall_edit::{
    self as we, corner, insert_dims, stack_tiers, Band, EndPrefs, Flip, InsertDims, JoinKind,
    StackPart, Structure, Sweep, Tier, Wrap,
};
use ddc_geom::{v2, V2};
use egui::{self, Color32, Pos2, Rect, Stroke};

/// Corner of two walls with its own joint choice.
#[derive(Clone, Debug)]
pub struct Corner {
    pub primary: Band,
    pub secondary: Band,
    pub prefs: EndPrefs,
    /// Layer priorities (outside to inside) of the main and second walls.
    pub layers: (Vec<(f64, u8)>, Vec<(f64, u8)>),
    /// Layer materials of the main and second walls (cleanup across same-named layers).
    pub names: (Vec<String>, Vec<String>),
    /// Joint node (end of the main wall; for a T-junction, the middle of the second).
    pub at: V2,
    /// Number of walls at the node.
    pub degree: usize,
}

impl Corner {
    pub fn new(
        primary: Band,
        secondary: Band,
        prefs: EndPrefs,
        layers: (Vec<(f64, u8)>, Vec<(f64, u8)>),
    ) -> Self {
        let c = |p: V2| (p - secondary.a).len().min((p - secondary.b).len());
        let at = if c(primary.b) <= c(primary.a) {
            primary.b
        } else {
            primary.a
        };
        let names = (
            vec![String::new(); layers.0.len()],
            vec![String::new(); layers.1.len()],
        );
        Corner {
            primary,
            secondary,
            prefs,
            layers,
            names,
            at,
            degree: 2,
        }
    }
    pub fn node(&self) -> V2 {
        self.at
    }
    /// Layers of the main wall that run through the joint (same-named ones by priority).
    pub fn runs(&self) -> Vec<bool> {
        let a: Vec<(u8, &str)> = self
            .layers
            .0
            .iter()
            .zip(&self.names.0)
            .map(|(l, n)| (l.1, n.as_str()))
            .collect();
        let b: Vec<(u8, &str)> = self
            .layers
            .1
            .iter()
            .zip(&self.names.1)
            .map(|(l, n)| (l.1, n.as_str()))
            .collect();
        we::cleanup_runs_named(&a, &b)
    }
    pub fn footprints(&self) -> [Vec<V2>; 2] {
        corner(
            &self.primary,
            &self.secondary,
            self.prefs.kind,
            self.prefs.allowed,
        )
    }
}

/// Profile/groove field editable from a panel row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SweepField {
    From,
    To,
    Bottom,
    Left,
}

/// Opening on the elevation: offset from the end, width, sill, height, flip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Insert {
    pub along: f64,
    pub width: f64,
    pub sill: f64,
    pub height: f64,
    pub door: bool,
    pub flip: Flip,
}

impl Insert {
    pub fn dims(&self, len: f64) -> InsertDims {
        insert_dims(self.along, self.width, self.sill, self.height, len)
    }
}

#[derive(Clone)]
pub struct WallEdit {
    pub corners: Vec<Corner>,
    pub length: f64,
    pub base: f64,
    pub height: f64,
    pub stack: Vec<StackPart>,
    pub sweeps: Vec<Sweep>,
    pub inserts: Vec<Insert>,
    pub structure: Structure,
    pub selected_insert: Option<usize>,
    /// Active temporary dimension: (opening, "start"|"end"|"sill"|"head", input text).
    pub dim_edit: Option<(usize, &'static str, String)>,
    pub status: Option<String>,
    /// Model profile ids in `sweeps` order (None: new).
    pub sweep_ids: Vec<Option<i64>>,
    /// Layer outlines in plan from the model: (this wall, outline).
    pub rings: Vec<(bool, Vec<V2>)>,
    /// Wall axis and faces (for an opening by face).
    pub band: Option<Band>,
    /// Faces of neighboring elements abutting the wall (ends of T-junctions): (point, z).
    pub faces: Vec<Vec<(V2, f64)>>,
    /// New empty openings (by elevation or by face) not yet written to the model.
    pub voids: Vec<we::WallOpening>,
    /// Composite types of the model and the one assigned to the wall.
    pub stack_types: Vec<(i64, String)>,
    pub stack_id: Option<i64>,
    /// Type layers: material, thickness, cleanup priority, core.
    pub layer_info: Vec<(String, f64, u8, bool)>,
    /// "Opening by elevation" tool: first corner of the rectangle (s, z).
    pub void_from: Option<V2>,
    pub void_tool: bool,
}

impl WallEdit {
    /// Empty wall edit of length `length`, height `height` and structure `structure`.
    pub fn new(length: f64, height: f64, structure: Structure) -> Self {
        WallEdit {
            corners: vec![],
            length,
            base: 0.0,
            height,
            stack: vec![],
            sweeps: vec![],
            inserts: vec![],
            structure,
            selected_insert: None,
            dim_edit: None,
            status: None,
            sweep_ids: vec![],
            rings: vec![],
            band: None,
            faces: vec![],
            voids: vec![],
            stack_types: vec![],
            stack_id: None,
            layer_info: vec![],
            void_from: None,
            void_tool: false,
        }
    }

    /// Empty opening by face `face` (must intersect the wall; clipped at the top).
    pub fn opening_by_face(&mut self, face: &[(V2, f64)]) -> Result<(), String> {
        let b = self.band.ok_or("no wall axis")?;
        let o = we::opening_by_face(b.a, b.b, (-b.right, b.left), self.height, face)
            .ok_or("face does not hit the wall")?;
        self.push_void(o)
    }

    /// Empty opening from two corners on the elevation.
    pub fn opening_by_elevation(&mut self, p: V2, q: V2) -> Result<(), String> {
        let o =
            we::WallOpening::from_corners(p, q, self.length, self.height).ok_or("empty opening")?;
        self.push_void(o)
    }

    fn push_void(&mut self, o: we::WallOpening) -> Result<(), String> {
        let r = (o.s0, o.s1, o.z0, o.z1);
        let hit = self
            .inserts
            .iter()
            .map(|i| (i.along, i.along + i.width, i.sill, i.sill + i.height))
            .chain(self.voids.iter().map(|v| (v.s0, v.s1, v.z0, v.z1)))
            .any(|x| we::overlaps_2d(r, x));
        if hit {
            return Err("opening overlaps another insert".into());
        }
        self.voids.push(o);
        Ok(())
    }

    /// Add a profile (the default projection is a cornice under the top).
    pub fn add_sweep(&mut self, reveal: bool) {
        let z = (self.height - 0.3).max(0.0);
        let profile = if reveal {
            we::rect_profile(0.02, 0.03, z, true)
        } else {
            vec![
                v2(0.0, z),
                v2(-0.1, z),
                v2(-0.1, z + 0.1),
                v2(0.0, z + 0.15),
            ]
        };
        self.sweeps.push(Sweep {
            profile,
            s0: 0.0,
            s1: self.length,
            reveal,
            left: true,
        });
        self.sweep_ids.push(None);
    }

    pub fn delete_sweep(&mut self, k: usize) {
        if k < self.sweeps.len() {
            self.sweeps.remove(k);
            if k < self.sweep_ids.len() {
                self.sweep_ids.remove(k);
            }
        }
    }
}

impl WallEdit {
    /// Demo scene: two corners (mitered and butt), composite wall with plinth and parapet, cornice and groove.
    pub fn demo() -> Self {
        let lay = |p: &[(f64, u8)]| p.to_vec();
        let ext = lay(&[(0.02, 5), (0.1, 3), (0.2, 1), (0.015, 5)]);
        let t: f64 = ext.iter().map(|l| l.0).sum();
        let c1 = Corner::new(
            Band::centered(v2(6.0, 0.0), v2(0.0, 0.0), t),
            Band::centered(v2(0.0, 0.0), v2(0.0, 4.0), t),
            EndPrefs {
                kind: JoinKind::Miter,
                allowed: true,
            },
            (ext.clone(), ext.clone()),
        );
        let c2 = Corner::new(
            Band::centered(v2(0.0, 0.0), v2(6.0, 0.0), t),
            Band::centered(v2(6.0, 0.0), v2(6.0, 4.0), 0.2),
            EndPrefs::default(),
            (ext, lay(&[(0.2, 1)])),
        );
        let mut s = Structure::new(vec![0.02, 0.1, 0.2, 0.015], (2, 3), 3.0);
        let up = s.split(0, 0.9).unwrap_or(0);
        let _ = s.assign(up, 0);
        s.wraps[0] = true;
        s.at_inserts = Wrap::Exterior;
        let mut w = WallEdit::new(6.0, 3.6, s);
        w.band = Some(Band::centered(v2(0.0, 0.0), v2(6.0, 0.0), t));
        WallEdit {
            corners: vec![c1, c2],
            length: 6.0,
            base: 0.0,
            height: 3.6,
            stack: vec![
                StackPart {
                    name: "\u{426}\u{43e}\u{43a}\u{43e}\u{43b}\u{44c}".into(),
                    thickness: 0.45,
                    height: Some(0.6),
                    offset: 0.05,
                },
                StackPart {
                    name: "\u{421}\u{442}\u{435}\u{43d}\u{430}".into(),
                    thickness: 0.335,
                    height: None,
                    offset: 0.0,
                },
                StackPart {
                    name: "\u{41f}\u{430}\u{440}\u{430}\u{43f}\u{435}\u{442}".into(),
                    thickness: 0.25,
                    height: Some(0.5),
                    offset: -0.04,
                },
            ],
            sweeps: vec![
                Sweep {
                    profile: vec![v2(0.0, 3.0), v2(-0.12, 3.0), v2(-0.12, 3.1), v2(0.0, 3.18)],
                    s0: 0.0,
                    s1: 6.0,
                    reveal: false,
                    left: true,
                },
                Sweep {
                    profile: we::rect_profile(0.02, 0.03, 1.8, true),
                    s0: 0.0,
                    s1: 6.0,
                    reveal: true,
                    left: true,
                },
            ],
            inserts: vec![
                Insert {
                    along: 0.8,
                    width: 1.0,
                    sill: 0.0,
                    height: 2.1,
                    door: true,
                    flip: Flip::default(),
                },
                Insert {
                    along: 3.2,
                    width: 1.6,
                    sill: 0.9,
                    height: 1.5,
                    door: false,
                    flip: Flip::default(),
                },
            ],
            selected_insert: Some(1),
            sweep_ids: vec![None, None],
            ..w
        }
    }

    pub fn tiers(&self) -> Vec<Tier> {
        stack_tiers(&self.stack, self.base, self.height).unwrap_or_default()
    }

    /// Click on a joint icon: cycle the type; with Shift, allow/forbid the joint.
    pub fn click_join(&mut self, k: usize, toggle_allow: bool) {
        if let Some(c) = self.corners.get_mut(k) {
            if toggle_allow {
                c.prefs.allowed = !c.prefs.allowed;
            } else {
                c.prefs.kind = c.prefs.kind.cycle();
            }
        }
    }

    /// Value entry for the temporary dimension of the selected opening.
    pub fn commit_dim(&mut self, k: usize, which: &str, value: f64) -> Result<(), String> {
        let i = *self.inserts.get(k).ok_or("no insert")?;
        let (along, sill, height) = we::apply_dim_h(
            which,
            value,
            i.along,
            i.width,
            i.sill,
            i.height,
            self.length,
            self.base + self.height,
        )?;
        let me = (along, along + i.width, sill, sill + height);
        let others = self.inserts.iter().enumerate().any(|(j, o)| {
            j != k && we::overlaps_2d(me, (o.along, o.along + o.width, o.sill, o.sill + o.height))
        });
        if others {
            return Err("insert would overlap another insert".into());
        }
        let x = &mut self.inserts[k];
        (x.along, x.sill, x.height) = (along, sill, height);
        Ok(())
    }

    /// Projection volume and groove cut-outs per layer of the outer face (for properties).
    /// Grooves on the left face cut layers from the end (layers run from the right face to the left).
    pub fn sweep_totals(&self) -> (f64, Vec<f64>) {
        let layers = &self.structure.thickness;
        let n = layers.len();
        let mut cut = vec![0.0; n];
        let mut add = 0.0;
        let holes: Vec<we::FacadeHole> = self
            .inserts
            .iter()
            .map(|i| (i.along, i.along + i.width, i.sill, i.sill + i.height))
            .collect();
        let one = |_: f64| 1.0;
        for s in &self.sweeps {
            let on = we::SweepOn {
                sweep: s,
                len: self.length,
                h: self.height,
                holes: &holes,
            };
            add += on.added_volume(&one);
            let order: Vec<usize> = if s.left {
                (0..n).rev().collect()
            } else {
                (0..n).collect()
            };
            let from_face: Vec<f64> = order.iter().map(|&i| layers[i]).collect();
            for (v, &i) in on.cut_per_layer(&from_face, &one).iter().zip(&order) {
                cut[i] += v;
            }
        }
        (add, cut)
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        self.toolbar(ui);
        let full = ui.available_rect_before_wrap();
        let w = full.width() / 3.0;
        let cols = [0.0, 1.0, 2.0].map(|k| {
            Rect::from_min_size(
                Pos2::new(full.min.x + w * k + 8.0, full.min.y + 28.0),
                egui::vec2(w - 16.0, full.height() - 36.0),
            )
        });
        let p = ui.painter_at(full);
        p.rect_filled(full, 0.0, t.viewport);
        for (r, title) in cols.iter().zip([
            crate::tr("edit_panels.we.col.joints", &[]),
            crate::tr("edit_panels.we.col.facade", &[]),
            crate::tr("edit_panels.we.col.stack", &[]),
        ]) {
            p.rect_filled(*r, t.radius as f32, t.paper);
            p.rect_stroke(
                *r,
                t.radius as f32,
                Stroke::new(1.0, t.border),
                egui::StrokeKind::Inside,
            );
            p.text(
                Pos2::new(r.min.x, r.min.y - 6.0),
                egui::Align2::LEFT_BOTTOM,
                title,
                egui::FontId::proportional(ddc_ui::theme::type_size::TITLE),
                t.text,
            );
        }
        self.plan(ui, cols[0], t);
        self.elevation(ui, cols[1], t);
        self.structure_view(&p, cols[2], t);
        if let Some(s) = &self.status {
            p.text(
                Pos2::new(full.min.x + 8.0, full.max.y - 4.0),
                egui::Align2::LEFT_BOTTOM,
                s,
                egui::FontId::proportional(ddc_ui::theme::type_size::LABEL),
                t.accent,
            );
        }
    }

    /// Tool row: openings by elevation/face, profiles, composite type.
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        use crate::tools::opt_field;
        use ddc_ui::theme::Role;
        use ddc_ui::widgets::{self, Button};
        let th = ddc_ui::theme::of(ui.ctx());
        let th: &ddc_ui::Theme = &th;
        let u = crate::units::active_units();
        ui.horizontal_wrapped(|ui| {
            let tip = crate::tr("edit_panels.we.void_facade.tip", &[]);
            let label = crate::tr("edit_panels.we.void_facade", &[]);
            let b = if self.void_tool {
                Button::primary(&label)
            } else {
                Button::secondary(&label)
            };
            if b.small().tooltip(&tip, "").show(ui, th).clicked() {
                self.void_tool = !self.void_tool;
                self.void_from = None;
            }
            let face = self.faces.first().cloned();
            let tip = crate::tr("edit_panels.we.void_face.tip", &[]);
            let label = crate::tr("edit_panels.we.void_face", &[]);
            let clicked = ui
                .add_enabled_ui(face.is_some(), |ui| {
                    Button::secondary(&label).small().tooltip(&tip, "").show(ui, th).clicked()
                })
                .inner;
            if clicked {
                if let Some(f) = face {
                    if let Err(e) = self.opening_by_face(&f) {
                        self.status = Some(e);
                    }
                }
            }
            ui.separator();
            if opt_field::button(ui, &crate::tr("edit_panels.we.add_sweep", &[]), "") {
                self.add_sweep(false);
            }
            if opt_field::button(ui, &crate::tr("edit_panels.we.add_reveal", &[]), "") {
                self.add_sweep(true);
            }
            ui.separator();
            let cap = crate::tr("wall_stack.we.stack_caption", &[]);
            widgets::elided_label(ui, &cap, th.font(Role::Label), th.sem().text_secondary);
            let uniform = crate::tr("edit_panels.we.uniform", &[]);
            let mut names: Vec<&str> = vec![uniform.as_str()];
            names.extend(self.stack_types.iter().map(|s| s.1.as_str()));
            let mut sel = self
                .stack_id
                .and_then(|i| self.stack_types.iter().position(|s| s.0 == i))
                .map_or(0, |p| p + 1);
            if widgets::field::dropdown(ui, th, "wall_stack", &mut sel, &names, None).changed() {
                self.stack_id = sel.checked_sub(1).map(|p| self.stack_types[p].0);
            }
        });
        let mut del = None;
        let mut edits: Vec<(usize, SweepField, f64)> = vec![];
        let len = self.length;
        let height = self.height;
        for (k, sw) in self.sweeps.iter().enumerate() {
            ui.push_id(("we_sweep", k), |ui| {
                ui.horizontal(|ui| {
                    let kind = if sw.reveal {
                        crate::tr("edit_panels.we.reveal", &[])
                    } else {
                        crate::tr("edit_panels.we.profile", &[])
                    };
                    widgets::badge(ui, th, &kind, widgets::Tone::Accent);
                    let sides = [
                        crate::tr("wall_stack.we.side_left", &[]),
                        crate::tr("wall_stack.we.side_right", &[]),
                    ];
                    let names: Vec<&str> = sides.iter().map(String::as_str).collect();
                    if let Some(i) = widgets::segmented(ui, th, &names, usize::from(!sw.left)) {
                        edits.push((k, SweepField::Left, if i == 0 { 1.0 } else { 0.0 }));
                    }
                    let (mut s0, mut s1) = (sw.s0, sw.s1);
                    if opt_field::len(ui, &crate::tr("wall_stack.we.from", &[]), &mut s0, (0.0, len), u) {
                        edits.push((k, SweepField::From, s0));
                    }
                    if opt_field::len(ui, &crate::tr("wall_stack.we.to", &[]), &mut s1, (0.0, len), u) {
                        edits.push((k, SweepField::To, s1));
                    }
                    let z0 = Self::sweep_bottom(sw);
                    let mut z = z0;
                    if opt_field::len(ui, &crate::tr("wall_stack.we.bottom", &[]), &mut z, (0.0, height), u) {
                        edits.push((k, SweepField::Bottom, z));
                    }
                    let tip = crate::tr("wall_stack.we.delete_tip", &[]);
                    let ctx = ui.ctx().clone();
                    let gone = widgets::with_icons(&ctx, |icons| {
                        widgets::icon_button_danger(ui, icons, th, "delete", &tip).clicked()
                    });
                    if gone {
                        del = Some(k);
                    }
                });
            });
        }
        for (k, f, v) in edits {
            if let Err(e) = self.set_sweep(k, f, v) {
                self.status = Some(e);
            }
        }
        if let Some(k) = del {
            self.delete_sweep(k);
        }
    }

    /// Profile bottom elevation.
    pub fn sweep_bottom(sw: &Sweep) -> f64 {
        sw.profile.iter().map(|q| q.y).fold(f64::MAX, f64::min)
    }

    /// One profile field (meters): the "from"/"to" span must not invert; the bottom shifts the profile.
    pub fn set_sweep(&mut self, k: usize, f: SweepField, v: f64) -> Result<(), String> {
        let sw = self.sweeps.get_mut(k).ok_or("no sweep")?;
        if !v.is_finite() {
            return Err(crate::tr("length_input.opt.bad_length", &[]));
        }
        match f {
            SweepField::From if v >= sw.s1 => return Err(crate::tr("wall_stack.we.from_after_to", &[])),
            SweepField::To if v <= sw.s0 => return Err(crate::tr("wall_stack.we.from_after_to", &[])),
            SweepField::From => sw.s0 = v,
            SweepField::To => sw.s1 = v,
            SweepField::Left => sw.left = v > 0.5,
            SweepField::Bottom => {
                let d = v - Self::sweep_bottom(sw);
                for q in sw.profile.iter_mut() {
                    q.y += d;
                }
            }
        }
        self.status = None;
        Ok(())
    }

    /// Scale and offset to fit a set of plan points into frame `r`.
    fn fit(r: Rect, pts: impl Iterator<Item = V2>) -> impl Fn(V2) -> Pos2 {
        let (mut lo, mut hi) = (v2(f64::MAX, f64::MAX), v2(f64::MIN, f64::MIN));
        for q in pts {
            lo = v2(lo.x.min(q.x), lo.y.min(q.y));
            hi = v2(hi.x.max(q.x), hi.y.max(q.y));
        }
        if lo.x > hi.x {
            (lo, hi) = (v2(0.0, 0.0), v2(1.0, 1.0));
        }
        let pad = 36.0;
        let w = (hi.x - lo.x).max(1e-6) as f32;
        let h = (hi.y - lo.y).max(1e-6) as f32;
        let scale = ((r.width() - 2.0 * pad) / w).min((r.height() - 2.0 * pad) / h);
        let c = r.center();
        let m = v2((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0);
        move |q: V2| {
            Pos2::new(
                c.x + (q.x - m.x) as f32 * scale,
                c.y - (q.y - m.y) as f32 * scale,
            )
        }
    }

    fn plan(&mut self, ui: &mut egui::Ui, r: Rect, t: &Tokens) {
        let p = ui.painter_at(r);
        let demo: Vec<Vec<V2>> = if self.rings.is_empty() {
            self.corners.iter().flat_map(|c| c.footprints()).collect()
        } else {
            vec![]
        };
        let all: Vec<V2> = self
            .rings
            .iter()
            .flat_map(|x| x.1.iter().copied())
            .chain(demo.iter().flatten().copied())
            .collect();
        let s = Self::fit(r, all.into_iter());
        for (mine, ring) in self
            .rings
            .iter()
            .map(|x| (x.0, &x.1))
            .chain(demo.iter().map(|x| (true, x)))
        {
            let pts: Vec<Pos2> = ring.iter().map(|q| s(*q)).collect();
            if pts.len() < 3 {
                continue;
            }
            let fill = if mine { t.poche } else { t.paper };
            p.add(egui::Shape::Path(egui::epaint::PathShape {
                points: pts,
                closed: true,
                fill: Color32::TRANSPARENT,
                stroke: egui::epaint::PathStroke::new(t.lw[if mine { 0 } else { 2 }], t.cut),
            }));
            let _ = fill;
        }
        let mut clicked = None;
        for (k, c) in self.corners.iter().enumerate() {
            let runs = c.runs();
            let n = s(c.node());
            let label = match (c.prefs.allowed, c.prefs.kind) {
                (false, _) => "×",
                (_, JoinKind::Butt) => "T",
                (_, JoinKind::Miter) => "∠",
                (_, JoinKind::SquareOff) => "⊓",
            };
            let badge = Rect::from_center_size(n + egui::vec2(-22.0, 22.0), egui::vec2(20.0, 20.0));
            p.rect_filled(badge, ddc_ui::theme::radius::SM, t.accent);
            p.text(
                badge.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(ddc_ui::theme::type_size::BODY),
                t.sem().accent_text_on,
            );
            p.text(
                badge.right_bottom() + egui::vec2(4.0, 2.0),
                egui::Align2::LEFT_TOP,
                {
                    let kind = if c.prefs.allowed {
                        c.prefs.kind.as_str().to_string()
                    } else {
                        crate::tr("edit_panels.we.no_join", &[])
                    };
                    let node = if c.degree > 2 {
                        crate::tr("edit_panels.we.node", &[("n", c.degree.to_string().as_str())])
                    } else {
                        String::new()
                    };
                    crate::tr("edit_panels.we.join", &[("kind", kind.as_str()), ("a", runs.iter().filter(|x| **x).count().to_string().as_str()), ("b", runs.len().to_string().as_str()), ("node", node.as_str())])
                },
                egui::FontId::proportional(ddc_ui::theme::type_size::CAPTION),
                t.text_dim,
            );
            let resp = ui.interact(badge, ui.id().with(("join", k)), egui::Sense::click());
            if resp.clicked() {
                clicked = Some((k, ui.input(|i| i.modifiers.shift)));
            }
        }
        if let Some((k, sh)) = clicked {
            self.click_join(k, sh);
        }
    }

    fn elevation(&mut self, ui: &mut egui::Ui, r: Rect, t: &Tokens) {
        let p = ui.painter_at(r);
        let top = self.base + self.height;
        let scale = ((r.width() - 40.0) / self.length as f32).min((r.height() - 60.0) / top as f32);
        let o = Pos2::new(r.min.x + 20.0, r.max.y - 30.0);
        let s = |x: f64, z: f64| Pos2::new(o.x + x as f32 * scale, o.y - z as f32 * scale);
        for (k, tier) in self.tiers().iter().enumerate() {
            let rect = Rect::from_two_pos(s(0.0, tier.z0), s(self.length, tier.z1));
            p.rect_filled(rect, 0.0, t.mat[k % 3]);
            p.rect_stroke(
                rect,
                0.0,
                Stroke::new(t.lw[1], t.projected),
                egui::StrokeKind::Inside,
            );
            p.text(
                rect.right_center() + egui::vec2(-4.0, 0.0),
                egui::Align2::RIGHT_CENTER,
                format!("{} {:.2}", self.stack[k].name, tier.z1 - tier.z0),
                egui::FontId::proportional(ddc_ui::theme::type_size::CAPTION),
                t.text_dim,
            );
        }
        for sw in &self.sweeps {
            let zs = sw.profile.iter().map(|q| q.y);
            let (z0, z1) = (
                zs.clone().fold(f64::MAX, f64::min),
                zs.fold(f64::MIN, f64::max),
            );
            let rect = Rect::from_two_pos(s(sw.s0, z0), s(sw.s1, z1));
            if sw.reveal {
                p.rect_filled(rect, 0.0, t.beyond);
            } else {
                p.rect_filled(rect, 0.0, t.mat[5]);
                p.rect_stroke(
                    rect,
                    0.0,
                    Stroke::new(t.lw[1], t.cut),
                    egui::StrokeKind::Inside,
                );
            }
        }
        for v in &self.voids {
            let rect = Rect::from_two_pos(s(v.s0, v.z0), s(v.s1, v.z1));
            p.rect_filled(rect, 0.0, t.viewport);
            p.rect_stroke(
                rect,
                0.0,
                Stroke::new(t.lw[0], t.accent),
                egui::StrokeKind::Inside,
            );
        }
        let face = Rect::from_two_pos(s(0.0, 0.0), s(self.length, self.base + self.height));
        if self.void_tool {
            let resp = ui.interact(face, ui.id().with("void_tool"), egui::Sense::click());
            if let Some(pos) = resp.interact_pointer_pos().filter(|_| resp.clicked()) {
                let q = v2(
                    ((pos.x - o.x) / scale) as f64,
                    ((o.y - pos.y) / scale) as f64,
                );
                match self.void_from.take() {
                    None => self.void_from = Some(q),
                    Some(a) => {
                        if let Err(e) = self.opening_by_elevation(a, q) {
                            self.status = Some(e);
                        }
                        self.void_tool = false;
                    }
                }
            }
            if let Some(a) = self.void_from {
                p.circle_filled(s(a.x, a.y), 4.0, t.accent);
            }
        }
        let mut pick = None;
        let mut flip = None;
        let mut dim = None;
        for (k, i) in self.inserts.iter().enumerate() {
            let rect =
                Rect::from_two_pos(s(i.along, i.sill), s(i.along + i.width, i.sill + i.height));
            p.rect_filled(rect, 0.0, t.mat[3]);
            let sel = self.selected_insert == Some(k);
            let col = if sel { t.accent } else { t.cut };
            p.rect_stroke(
                rect,
                0.0,
                Stroke::new(t.lw[0], col),
                egui::StrokeKind::Inside,
            );
            if i.door {
                let hinge = if i.flip.hand {
                    rect.right_bottom()
                } else {
                    rect.left_bottom()
                };
                let other = if i.flip.hand {
                    rect.left_bottom()
                } else {
                    rect.right_bottom()
                };
                p.line_segment(
                    [hinge, Pos2::new(other.x, rect.top())],
                    Stroke::new(1.0, t.projected),
                );
            }
            if ui
                .interact(rect, ui.id().with(("ins", k)), egui::Sense::click())
                .clicked()
            {
                pick = Some(k);
            }
            if sel {
                for (which, r) in self.temp_dims(&p, i, k, &s, t) {
                    if ui
                        .interact(r, ui.id().with(("dim", k, which)), egui::Sense::click())
                        .clicked()
                    {
                        dim = Some((k, which));
                    }
                }
                // flip handles: ⇄ hinges, ⇅ opening side
                for (dx, lab, hand) in [(-14.0, "⇄", true), (14.0, "⇅", false)] {
                    let h = Rect::from_center_size(
                        rect.center_top() + egui::vec2(dx, -14.0),
                        egui::vec2(18.0, 18.0),
                    );
                    p.circle_filled(h.center(), 9.0, t.accent);
                    p.text(
                        h.center(),
                        egui::Align2::CENTER_CENTER,
                        lab,
                        egui::FontId::proportional(ddc_ui::theme::type_size::LABEL),
                        t.sem().accent_text_on,
                    );
                    if ui
                        .interact(h, ui.id().with(("flip", k, hand)), egui::Sense::click())
                        .clicked()
                    {
                        flip = Some((k, hand));
                    }
                }
            }
        }
        if let Some(k) = pick {
            self.selected_insert = Some(k);
        }
        if let Some((k, hand)) = flip {
            let f = &mut self.inserts[k].flip;
            *f = if hand { f.flip_hand() } else { f.flip_facing() };
        }
        if let Some((k, which)) = dim {
            let d = self.inserts[k].dims(self.length);
            let v = match which {
                "start" => d.to_start,
                "end" => d.to_end,
                "sill" => d.sill,
                _ => d.head,
            };
            self.dim_edit = Some((k, which, format!("{v:.3}")));
        }
        let mut done = None;
        if let Some((k, which, text)) = self.dim_edit.as_mut() {
            let at = Rect::from_center_size(
                r.center_top() + egui::vec2(0.0, 16.0),
                egui::vec2(120.0, 20.0),
            );
            let resp = ui.put(at, egui::TextEdit::singleline(text).desired_width(110.0));
            resp.request_focus();
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let v = crate::units::parse_locale_number(text.trim(), crate::units::ui_decimal_mark());
                done = Some((*k, *which, v));
            } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                done = Some((*k, *which, Ok(f64::NAN)));
            }
        }
        if let Some((k, which, v)) = done {
            self.dim_edit = None;
            match v {
                Ok(v) if v.is_nan() => {}
                Ok(v) => {
                    if let Err(e) = self.commit_dim(k, which, v) {
                        self.status = Some(e);
                    }
                }
                Err(_) => self.status = Some(crate::tr("edit_panels.we.enter_number", &[])),
            }
        }
        p.line_segment(
            [s(0.0, 0.0), s(self.length, 0.0)],
            Stroke::new(t.lw[3], t.cut),
        );
    }

    /// Temporary dimensions of the selected opening; clicking a label frame opens entry.
    fn temp_dims(
        &self,
        p: &egui::Painter,
        i: &Insert,
        _k: usize,
        s: &dyn Fn(f64, f64) -> Pos2,
        t: &Tokens,
    ) -> Vec<(&'static str, Rect)> {
        let d = i.dims(self.length);
        let z = i.sill + i.height / 2.0;
        let col = Stroke::new(1.0, t.accent);
        let txt = |a: Pos2, b: Pos2, v: f64| {
            p.line_segment([a, b], col);
            let m = Pos2::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
            let r = Rect::from_center_size(m, egui::vec2(40.0, 16.0));
            p.rect_filled(r, ddc_ui::theme::radius::SM, t.panel);
            p.text(
                m,
                egui::Align2::CENTER_CENTER,
                format!("{v:.3}"),
                egui::FontId::monospace(ddc_ui::theme::type_size::CAPTION),
                t.accent,
            );
            r
        };
        let mut out = vec![
            ("start", txt(s(0.0, z), s(i.along, z), d.to_start)),
            (
                "end",
                txt(s(i.along + i.width, z), s(self.length, z), d.to_end),
            ),
        ];
        let x = i.along + i.width + 0.15;
        if d.sill > 0.0 {
            out.push(("sill", txt(s(x, 0.0), s(x, d.sill), d.sill)));
        }
        out.push(("head", txt(s(x + 0.35, 0.0), s(x + 0.35, d.head), d.head)));
        out
    }

    fn structure_view(&self, p: &egui::Painter, r: Rect, t: &Tokens) {
        let st = &self.structure;
        let total: f64 = st.thickness.iter().sum();
        let sx = (r.width() * 0.5) / total as f32;
        let sz = (r.height() - 60.0) / st.height as f32;
        let o = Pos2::new(r.center().x - total as f32 * sx / 2.0, r.max.y - 30.0);
        let x0 = |l: usize| st.thickness[..l].iter().sum::<f64>() as f32 * sx;
        for reg in &st.regions {
            let a = Pos2::new(o.x + x0(reg.layer), o.y - reg.z0 as f32 * sz);
            let b = Pos2::new(o.x + x0(reg.layer + 1), o.y - reg.z1 as f32 * sz);
            let rect = Rect::from_two_pos(a, b);
            p.rect_filled(rect, 0.0, t.mat[reg.material % 6]);
            p.rect_stroke(
                rect,
                0.0,
                Stroke::new(t.lw[2], t.cut),
                egui::StrokeKind::Inside,
            );
        }
        let core = Rect::from_two_pos(
            Pos2::new(o.x + x0(st.core.0), o.y),
            Pos2::new(o.x + x0(st.core.1), o.y - st.height as f32 * sz),
        );
        p.rect_stroke(
            core,
            0.0,
            Stroke::new(t.lw[0], t.cut),
            egui::StrokeKind::Outside,
        );
        let (add, cut) = self.sweep_totals();
        let layers: String = self
            .layer_info
            .iter()
            .map(|(n, th, pr, core)| {
                let core_s = if *core { crate::tr("edit_panels.we.core", &[]) } else { String::new() };
                crate::tr("edit_panels.we.layer", &[("name", n.as_str()), ("mm", format!("{:.0}", th * 1000.0).as_str()), ("pr", pr.to_string().as_str()), ("core", core_s.as_str())])
            })
            .collect();
        p.text(
            Pos2::new(r.min.x + 8.0, r.min.y + 8.0),
            egui::Align2::LEFT_TOP,
            crate::tr("edit_panels.we.totals", &[("wrap", st.at_inserts.as_str()), ("add", format!("{add:.4}").as_str()), ("cut", cut.iter().map(|c| format!("{c:.4}")).collect::<Vec<_>>().join(" · ").as_str()), ("layers", layers.as_str())]),
            egui::FontId::proportional(ddc_ui::theme::type_size::CAPTION),
            t.text,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_cycle_and_allow() {
        let mut w = WallEdit::demo();
        let f0 = w.corners[0].footprints();
        w.click_join(0, false);
        assert_eq!(w.corners[0].prefs.kind, JoinKind::SquareOff);
        w.click_join(0, true);
        assert!(!w.corners[0].prefs.allowed);
        // without a joint the ends stop at the node: different outlines, same area
        let f1 = w.corners[0].footprints();
        assert_ne!(f1, f0);
        assert!((we::area(&f1[0]) - we::area(&f0[0])).abs() < 1e-9);
    }

    #[test]
    fn stacked_tiers_and_dims() {
        let mut w = WallEdit::demo();
        let t = w.tiers();
        assert_eq!(t.len(), 3);
        assert!((t[2].z1 - 3.6).abs() < 1e-12 && (t[1].z0 - 0.6).abs() < 1e-12);
        w.commit_dim(1, "head", 2.6).unwrap();
        assert!((w.inserts[1].height - 1.7).abs() < 1e-12);
        assert!(w.commit_dim(1, "start", 0.5).is_err(), "overlaps the door");
        w.commit_dim(1, "end", 0.4).unwrap();
        assert!((w.inserts[1].along - 4.0).abs() < 1e-12);
        let (add, cut) = w.sweep_totals();
        // a groove on the left face cuts the last layer (layers run from the right face)
        assert!(add > 0.0 && cut[3] > 0.0 && cut[1] == 0.0, "{cut:?}");
        (w.inserts[1].sill, w.inserts[1].height) = (2.2, 1.0);
        assert!(
            w.commit_dim(1, "start", 0.5).is_ok(),
            "window above the door"
        );
        assert!(w.commit_dim(1, "head", 3.7).is_err(), "head above the wall");
    }

    #[test]
    fn void_tools_and_sweep_list() {
        let mut w = WallEdit::demo();
        w.opening_by_elevation(v2(4.8, 2.6), v2(5.4, 3.2)).unwrap();
        assert!(
            w.opening_by_elevation(v2(1.0, 0.5), v2(1.5, 1.0)).is_err(),
            "door there"
        );
        let face = [
            (v2(2.0, -0.5), 0.0),
            (v2(2.4, 0.5), 0.0),
            (v2(2.4, 0.5), 9.0),
        ];
        w.opening_by_face(&face).unwrap();
        assert!((w.voids[1].z1 - 3.6).abs() < 1e-12, "clipped by the top");
        let far = [(v2(2.0, 5.0), 0.0), (v2(2.4, 6.0), 1.0)];
        assert!(w.opening_by_face(&far).is_err());
        let n = w.sweeps.len();
        w.add_sweep(true);
        w.delete_sweep(0);
        assert_eq!((w.sweeps.len(), w.sweep_ids.len()), (n, n));
    }

    use crate::tools::opt_field::from_display_mark;
    use crate::units::{DecimalMark, Units};

    const R: (f64, f64) = (0.0, 50.0);

    #[test]
    fn sweep_fields_take_project_units() {
        let mut w = WallEdit::demo();
        w.add_sweep(false);
        let k = w.sweeps.len() - 1;
        // metric: millimetres in, metres stored; English groups thousands with a comma
        let m = from_display_mark("1,000", Units::Metric, R, DecimalMark::Point).unwrap();
        assert!((m - 1.0).abs() < 1e-12, "en 1,000 is 1000 mm");
        w.set_sweep(k, SweepField::From, m).unwrap();
        assert!((w.sweeps[k].s0 - 1.0).abs() < 1e-12);
        // feet-inches in, metres stored; the shown text reads back to the same metres
        let ft = from_display_mark("10'-6\"", Units::Imperial, R, DecimalMark::Point).unwrap();
        assert!((ft - (10.0 * 12.0 + 6.0) * crate::units::INCH).abs() < 1e-9);
        w.set_sweep(k, SweepField::To, ft).unwrap();
        for u in [Units::Metric, Units::Imperial] {
            // shown in the mark it is read back in, not the thread's default one
            let shown = crate::units::edit_length_mark(ft, u, DecimalMark::Point);
            let back = from_display_mark(&shown, u, R, DecimalMark::Point).unwrap();
            assert!((back - ft).abs() < 1e-3, "{u:?}");
        }
        // bottom moves the whole profile and keeps its shape
        let h0 = w.sweeps[k].profile.iter().map(|q| q.y).fold(f64::MIN, f64::max)
            - WallEdit::sweep_bottom(&w.sweeps[k]);
        w.set_sweep(k, SweepField::Bottom, 2.0).unwrap();
        let sw = &w.sweeps[k];
        assert!((WallEdit::sweep_bottom(sw) - 2.0).abs() < 1e-12);
        let h1 = sw.profile.iter().map(|q| q.y).fold(f64::MIN, f64::max) - 2.0;
        assert!((h0 - h1).abs() < 1e-12);
    }

    #[test]
    fn sweep_invalid_input_writes_nothing() {
        let mut w = WallEdit::demo();
        w.add_sweep(true);
        let k = w.sweeps.len() - 1;
        let before = w.sweeps[k].clone();
        for bad in ["", "abc", "1..2", "--3"] {
            assert!(from_display_mark(bad, Units::Metric, R, DecimalMark::Point).is_err(), "{bad:?}");
        }
        assert!(from_display_mark("1,5", Units::Metric, R, DecimalMark::Point).is_err());
        assert!(w.set_sweep(k, SweepField::Bottom, f64::NAN).is_err());
        // From after To (and To before From) is refused with a message in the catalog
        let (s0, s1) = (before.s0, before.s1);
        let e = w.set_sweep(k, SweepField::From, s1 + 0.5).unwrap_err();
        assert!(!e.is_empty() && !e.starts_with("wall_stack."), "{e}");
        assert!(w.set_sweep(k, SweepField::To, s0).is_err());
        assert!(w.set_sweep(99, SweepField::From, 0.1).is_err());
        assert_eq!(w.sweeps[k], before, "refused values leave the profile as it was");
    }

    #[test]
    fn toolbar_renders_headless_in_both_units() {
        let ctx = ddc_ui::theme::headless_ctx();
        let t = crate::ui_kit::light();
        let shapes = |u: Units| {
            crate::units::set_active_units(u);
            let mut w = WallEdit::demo();
            w.add_sweep(true);
            let out = ctx.run(Default::default(), |c| {
                egui::CentralPanel::default().show(c, |ui| w.ui(ui, &t));
            });
            crate::units::set_active_units(Units::Metric);
            out.shapes.len()
        };
        assert!(shapes(Units::Metric) > 20);
        assert!(shapes(Units::Imperial) > 20);
    }

    #[test]
    fn edit_window_uses_the_kit_only() {
        let src = include_str!("wall_edit.rs");
        let body = src.split("#[cfg(test)]").next().unwrap();
        for raw in ["DragValue", "ComboBox", "ui.button(", "small_button", "selectable_label", ".checkbox("] {
            assert!(!body.contains(raw), "raw widget left: {raw}");
        }
        assert!(body.contains("opt_field::len"));
    }
}
