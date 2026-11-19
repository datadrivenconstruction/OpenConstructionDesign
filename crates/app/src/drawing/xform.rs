// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Mirror and uniform scale of the picked drawing primitives. A drawing as a whole cannot hold
//! a mirror in its offset and rotation, so both tools work on a pick only. Each call is one
//! undo step ([`Step::geometry`]) and marks the drawing edited.

use ddc_geom::V2;
use ddc_io::drawing_model::Affine2;
use ddc_ui::widgets::{self, Button, NumberField, NumberFormat, TextField};
use egui::{self, RichText};

use super::history::Step;
use super::Drawings;
use crate::expr_field::Scope;
use crate::units::{edit_length_mark, parse_edit_length_lang, DecimalMark, Units};

/// Width of the number and length fields.
const NUM_W: f32 = 96.0;

fn tr(key: &str) -> String {
    crate::tr(key, &[])
}

/// Field buffers of the mirror and scale tools.
pub struct XformUi {
    /// Scale factor.
    pub k: f64,
    /// Typed target length for "scale to length", in the project display unit.
    pub len_text: String,
    /// Picked line the text was prefilled for; a new pick re-prefills.
    pub len_key: Option<(usize, usize)>,
    /// Keep the originals and add the result.
    pub copy: bool,
    /// Reference length kept for "scale by reference".
    pub reference: Option<RefLen>,
    /// Scale the whole drawing instead of the current pick.
    pub whole: bool,
}

/// A measured reference: drawing index, its edit generation and the length in metres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RefLen {
    pub drawing: usize,
    pub gen: u64,
    pub len: f64,
}

impl Default for XformUi {
    fn default() -> Self {
        XformUi {
            k: 2.0,
            len_text: String::new(),
            len_key: None,
            copy: false,
            reference: None,
            whole: false,
        }
    }
}

impl Drawings {
    /// The pick with stale indices dropped. No pick at all: whole drawing refused. An empty or
    /// all-stale pick says nothing is picked and is cleared.
    fn live_pick(&mut self) -> Result<(usize, ddc_io::drawing_model::Pick), String> {
        let Some((k, pk)) = self.pick.clone() else {
            return Err(tr("dwgmir.st.whole_refused"));
        };
        let Some(d) = self.list.get(k) else {
            return Err(tr("dlg.dw.st.drawing_gone"));
        };
        let u = &d.u;
        let live = ddc_io::drawing_model::Pick {
            lines: pk.lines.iter().copied().filter(|&i| i < u.lines.len()).collect(),
            arcs: pk.arcs.iter().copied().filter(|&i| i < u.arcs.len()).collect(),
            texts: pk.texts.iter().copied().filter(|&i| i < u.texts.len()).collect(),
            closed: pk.closed.iter().copied().filter(|&i| i < u.closed.len()).collect(),
            fills: pk.fills.iter().copied().filter(|&i| i < u.fills.len()).collect(),
        };
        if live.is_empty() {
            self.pick = None;
            return Err(tr("dlg.dw.st.nothing_picked"));
        }
        Ok((k, live))
    }

    fn apply_affine(&mut self, xf: Affine2, copy: bool, key: &str) -> Result<String, String> {
        let (k, live) = self.live_pick()?;
        let Some(d) = self.list.get_mut(k) else {
            return Err(tr("dlg.dw.st.drawing_gone"));
        };
        self.steps.push(Step::geometry(d));
        d.mark_edited();
        let out = d.u.transform_pick_affine(&live, &xf, copy);
        let n = out.len();
        self.pick = Some((k, out));
        self.touch();
        Ok(crate::tr(key, &[("n", &n.to_string())]))
    }

    /// Mirror the picked primitives across the line through `a` and `b` (drawing metres).
    pub fn apply_mirror(&mut self, a: V2, b: V2, copy: bool) -> Result<String, String> {
        let Some(xf) = Affine2::mirror((a.x, a.y), (b.x, b.y)) else {
            return Err(tr("dwgmir.st.axis_bad"));
        };
        let key = if copy { "dwgmir.st.mirror_copied" } else { "dwgmir.st.mirrored" };
        self.apply_affine(xf, copy, key)
    }

    /// Scale the picked primitives by `k` about `base`. `k` must be finite and above zero.
    pub fn apply_scale(&mut self, base: V2, k: f64, copy: bool) -> Result<String, String> {
        let Some(xf) = Affine2::scale((base.x, base.y), k) else {
            return Err(tr("dwgmir.st.scale_bad"));
        };
        if (k - 1.0).abs() < 1e-12 && !copy {
            return Err(tr("dwgmir.st.scale_one"));
        }
        let key = if copy { "dwgmir.st.scaled_copied" } else { "dwgmir.st.scaled" };
        self.apply_affine(xf, copy, key)
    }

    /// Drawing and index of the single picked line, with its length in metres.
    fn picked_line_len(&self) -> Option<((usize, usize), f64)> {
        let (k, pk) = self.pick.as_ref()?;
        let d = self.list.get(*k)?;
        let lines: Vec<usize> = pk.lines.iter().copied().filter(|&i| i < d.u.lines.len()).collect();
        let [i] = lines[..] else { return None };
        let l = &d.u.lines[i];
        let n = ((l.b.0 - l.a.0).powi(2) + (l.b.1 - l.a.1).powi(2)).sqrt();
        Some(((*k, i), n))
    }

    /// Scale so that the one picked line gets length `target` (drawing metres), about `base`.
    pub fn apply_scale_to_length(&mut self, base: V2, target: f64, copy: bool) -> Result<String, String> {
        let (k, pk) = self.live_pick()?;
        let Some(d) = self.list.get(k) else {
            return Err(tr("dlg.dw.st.drawing_gone"));
        };
        let lines: Vec<usize> = pk.lines.clone();
        let [i] = lines[..] else {
            return Err(tr("dwgmir.st.len_one_line"));
        };
        let l = &d.u.lines[i];
        let cur = ((l.b.0 - l.a.0).powi(2) + (l.b.1 - l.a.1).powi(2)).sqrt();
        if !(cur.is_finite() && cur > 1e-12) {
            return Err(tr("dwgmir.st.len_zero"));
        }
        if !(target.is_finite() && target > 0.0) {
            return Err(tr("dwgmir.st.scale_bad"));
        }
        self.apply_scale(base, target / cur, copy)
    }

    /// Every primitive of drawing `k`.
    fn whole_pick(&self, k: usize) -> Option<ddc_io::drawing_model::Pick> {
        let u = &self.list.get(k)?.u;
        Some(ddc_io::drawing_model::Pick {
            lines: (0..u.lines.len()).collect(),
            arcs: (0..u.arcs.len()).collect(),
            texts: (0..u.texts.len()).collect(),
            closed: (0..u.closed.len()).collect(),
            fills: (0..u.fills.len()).collect(),
        })
    }

    /// Keep the one picked line as the reference length.
    pub fn set_reference(&mut self) -> Result<String, String> {
        let Some(((k, _), len)) = self.picked_line_len() else {
            return Err(tr("dwgmir.st.len_one_line"));
        };
        if !(len.is_finite() && len > 1e-12) {
            return Err(tr("dwgmir.st.len_zero"));
        }
        let gen = self.list[k].gen;
        self.edit.xform.reference = Some(RefLen { drawing: k, gen, len });
        Ok(tr("dwgmir.st.ref_set"))
    }

    /// The reference while its drawing is unchanged; a stale one is dropped.
    fn live_reference(&mut self) -> Option<RefLen> {
        let r = self.edit.xform.reference?;
        if self.list.get(r.drawing).is_some_and(|d| d.gen == r.gen) {
            return Some(r);
        }
        self.edit.xform.reference = None;
        None
    }

    /// Scale the whole drawing (or the current pick of the same drawing) so that the kept
    /// reference length becomes `target` metres. `base` defaults to the center of the set.
    pub fn apply_scale_by_reference(
        &mut self,
        base: Option<V2>,
        target: f64,
        whole: bool,
        copy: bool,
    ) -> Result<String, String> {
        let Some(r) = self.live_reference() else {
            return Err(tr("dwgmir.st.ref_stale"));
        };
        if !(r.len.is_finite() && r.len > 1e-12) {
            return Err(tr("dwgmir.st.len_zero"));
        }
        if !(target.is_finite() && target > 0.0) {
            return Err(tr("dwgmir.st.scale_bad"));
        }
        let pk = if whole {
            self.whole_pick(r.drawing).filter(|p| !p.is_empty())
        } else {
            self.pick.clone().filter(|(k, _)| *k == r.drawing).map(|p| p.1)
        };
        let Some(pk) = pk else {
            return Err(tr("dlg.dw.st.nothing_picked"));
        };
        let base = base.unwrap_or_else(|| self.pivot(r.drawing, Some(&pk)));
        self.pick = Some((r.drawing, pk));
        let out = self.apply_scale(base, target / r.len, copy);
        if out.is_ok() {
            self.edit.xform.reference = None;
        }
        out
    }

    /// Mirror and scale controls of the layers window. Returns a status line.
    pub fn xform_ui(&mut self, ui: &mut egui::Ui, units: Units) -> Option<String> {
        self.xform_ui_in(ui, units, crate::ui_is_ru())
    }

    /// Typed length (project units, numbers in the locale `mark`) → metres; `None` when it does
    /// not read as a length above zero.
    fn typed_len(&self, units: Units, mark: DecimalMark) -> Option<f64> {
        parse_edit_length_lang(&self.edit.xform.len_text, units, &Scope::none(), mark)
            .ok()
            .filter(|t| t.is_finite() && *t > 0.0)
    }

    /// "Scale to length" from the typed text. Unreadable text writes nothing.
    fn scale_to_typed_len(&mut self, c: V2, units: Units, loc: (bool, DecimalMark), copy: bool) -> String {
        match self.typed_len(units, loc.1) {
            Some(t) => self.apply_scale_to_length(c, t, copy).unwrap_or_else(|e| e),
            None => crate::tr_lang(loc.0, "dwgmir.st.len_parse", &[]),
        }
    }

    /// "Scale by reference" from the typed text. Unreadable text writes nothing.
    fn scale_by_ref_typed(&mut self, units: Units, loc: (bool, DecimalMark), copy: bool) -> String {
        let whole = self.edit.xform.whole;
        match self.typed_len(units, loc.1) {
            Some(t) => self.apply_scale_by_reference(None, t, whole, copy).unwrap_or_else(|e| e),
            None => crate::tr_lang(loc.0, "dwgmir.st.len_parse", &[]),
        }
    }

    /// [`Self::xform_ui`] in an explicit language: text, and the decimal mark numbers are typed in.
    pub fn xform_ui_in(&mut self, ui: &mut egui::Ui, units: Units, ru: bool) -> Option<String> {
        let (k, pk) = self.pick.clone().filter(|p| !p.1.is_empty())?;
        let c = self.pivot(k, Some(&pk));
        let th = ddc_ui::theme::of(ui.ctx());
        let mark = DecimalMark::for_ru(ru);
        let loc = (ru, mark);
        let nf = if ru { NumberFormat::RU } else { NumberFormat::EN };
        let t = |key: &str| crate::tr_lang(ru, key, &[]);
        let mut status = None;
        let res = |r: Result<String, String>| r.unwrap_or_else(|e| e);
        ui.horizontal_wrapped(|ui| {
            widgets::checkbox(ui, &th, &mut self.edit.xform.copy, &t("dwgmir.copy"));
            let copy = self.edit.xform.copy;
            let (mv, mh, mtip) = (t("dwgmir.mirror_v"), t("dwgmir.mirror_h"), t("dwgmir.mirror_tip"));
            if Button::secondary(&mv).small().tooltip(&mtip, "").show(ui, &th).clicked() {
                status = Some(res(self.apply_mirror(c, V2 { x: c.x, y: c.y + 1.0 }, copy)));
            }
            if Button::secondary(&mh).small().tooltip(&mtip, "").show(ui, &th).clicked() {
                status = Some(res(self.apply_mirror(c, V2 { x: c.x + 1.0, y: c.y }, copy)));
            }
            let (scale_l, scale_t) = (ui.label(t("dwgmir.scale")), t("dwgmir.scale"));
            NumberField::new("ux58b.xf.k", &mut self.edit.xform.k, nf)
                .labelled_by(scale_l.id, &scale_t)
                .decimals(3)
                .step(0.1)
                .range(0.0, 1000.0)
                .width(NUM_W)
                .show(ui, &th);
            let (sa, stip) = (t("dwgmir.scale_apply"), t("dwgmir.scale_tip"));
            if Button::secondary(&sa).small().tooltip(&stip, "").show(ui, &th).clicked() {
                let f = self.edit.xform.k;
                status = Some(res(self.apply_scale(c, f, copy)));
            }
            let len_l = ui.label(t("dwgmir.len"));
            if let Some((key, cur)) = self.picked_line_len() {
                if self.edit.xform.len_key != Some(key) {
                    self.edit.xform.len_key = Some(key);
                    self.edit.xform.len_text = edit_length_mark(cur, units, mark);
                }
            }
            let unit = match units {
                Units::Metric => crate::tr_str(ru, "unit_fmt.unit.mm"),
                Units::Imperial => "",
            };
            let bad = !self.edit.xform.len_text.trim().is_empty() && self.typed_len(units, mark).is_none();
            let (lbl, lerr) = (t("dwgmir.len"), t("dwgmir.st.len_parse"));
            let out = TextField::new(&mut self.edit.xform.len_text)
                .id_salt("ux58b.xf.len")
                .labelled_by(len_l.id, &lbl)
                .unit(unit)
                .right_aligned()
                .width(NUM_W)
                .error(bad)
                .error_text(&lerr)
                .show(ui, &th);
            if bad {
                out.response.on_hover_text(&lerr);
            }
            let (la, ltip) = (t("dwgmir.len_apply"), t("dwgmir.len_tip"));
            if Button::secondary(&la).small().tooltip(&ltip, "").show(ui, &th).clicked() {
                status = Some(self.scale_to_typed_len(c, units, loc, copy));
            }
            if self.picked_line_len().is_some() {
                let (rs, rtip) = (t("dwgmir.ref_set"), t("dwgmir.ref_tip"));
                if Button::secondary(&rs).small().tooltip(&rtip, "").show(ui, &th).clicked() {
                    status = Some(res(self.set_reference()));
                }
            }
            if let Some(r) = self.live_reference() {
                let shown = edit_length_mark(r.len, units, mark);
                ui.label(
                    RichText::new(crate::tr_lang(ru, "dwgmir.ref_shown", &[("v", &shown)]))
                        .color(th.sem().text_secondary),
                );
                widgets::checkbox(ui, &th, &mut self.edit.xform.whole, &t("dwgmir.ref_whole"));
                let (ra, ratip) = (t("dwgmir.ref_apply"), t("dwgmir.ref_apply_tip"));
                if Button::secondary(&ra).small().tooltip(&ratip, "").show(ui, &th).clicked() {
                    status = Some(self.scale_by_ref_typed(units, loc, copy));
                }
                if Button::quiet(&t("dwgmir.ref_clear")).small().show(ui, &th).clicked() {
                    self.edit.xform.reference = None;
                }
            }
        });
        status
    }
}

#[cfg(test)]
mod tests {
    use super::super::Origin;
    use super::*;
    use crate::editor::Editor;
    use crate::units::parse_edit_length;
    use ddc_geom::v2;
    use ddc_io::drawing_model::{Pick, UArc, ULine, UPoly, UText, Underlay};
    use std::f64::consts::FRAC_PI_2;

    fn setup() -> (Editor, Drawings) {
        let ed = Editor::new_in_memory().unwrap();
        let mut ds = Drawings::default();
        let mut u = Underlay::default();
        u.lines.push(ULine {
            layer: "0".into(),
            a: (1.0, 0.0),
            b: (3.0, 1.0),
            dashed: false,
            pen: 0,
        });
        u.arcs.push(UArc {
            layer: "0".into(),
            c: (2.0, 1.0),
            r: 1.0,
            a0: 0.0,
            a1: FRAC_PI_2,
            pen: 0,
        });
        u.texts.push(UText {
            layer: "0".into(),
            pos: (1.0, 2.0),
            height: 0.2,
            rotation: 0.0,
            text: "T".into(),
            pen: 0,
            fmt: Default::default(),
        });
        u.closed.push(UPoly {
            layer: "0".into(),
            pts: vec![(5.0, 5.0), (6.0, 5.0), (6.0, 6.0)],
            circle: None,
        });
        u.closed.push(UPoly {
            layer: "0".into(),
            pts: vec![],
            circle: Some(0.5),
        });
        ds.add_underlay(u, 1.0, "p", None, ed.level, Origin::AsIs, v2(0.0, 0.0));
        (ed, ds)
    }

    fn pick_all(ds: &mut Drawings) {
        let u = &ds.list[0].u;
        let pk = Pick {
            lines: (0..u.lines.len()).collect(),
            arcs: (0..u.arcs.len()).collect(),
            texts: (0..u.texts.len()).collect(),
            closed: (0..u.closed.len()).collect(),
            fills: vec![],
        };
        ds.pick = Some((0, pk));
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn mirror_line_and_arc_keeps_ccw() {
        let (_, mut ds) = setup();
        pick_all(&mut ds);
        // across the x axis
        ds.apply_mirror(v2(0.0, 0.0), v2(1.0, 0.0), false).unwrap();
        let u = &ds.list[0].u;
        assert!(near(u.lines[0].a.0, 1.0) && near(u.lines[0].a.1, 0.0));
        assert!(near(u.lines[0].b.0, 3.0) && near(u.lines[0].b.1, -1.0));
        let a = &u.arcs[0];
        assert!(near(a.c.1, -1.0) && near(a.r, 1.0));
        // the quarter 0..90 deg becomes 270..360 deg: still counter-clockwise, same sweep
        assert!(near(a.a1 - a.a0, FRAC_PI_2));
        assert!(near(a.a0, 3.0 * FRAC_PI_2) && near(a.a1, std::f64::consts::TAU));
        // its start point is the image of the old end point (2, 2) -> (2, -2)
        let s = (a.c.0 + a.r * a.a0.cos(), a.c.1 + a.r * a.a0.sin());
        assert!(near(s.0, 2.0) && near(s.1, -2.0));
        // a ring keeps counter-clockwise order after the mirror
        let ring = &u.closed[0].pts;
        let area: f64 = (0..ring.len())
            .map(|i| {
                let (p, q) = (ring[i], ring[(i + 1) % ring.len()]);
                p.0 * q.1 - q.0 * p.1
            })
            .sum();
        assert!(area > 0.0);
        // a text keeps reading left to right when mirrored across a vertical axis
        let (_, mut ds2) = setup();
        pick_all(&mut ds2);
        ds2.apply_mirror(v2(0.0, 0.0), v2(0.0, 1.0), false).unwrap();
        assert!(near(ds2.list[0].u.texts[0].pos.0, -1.0));
        assert!(near(ds2.list[0].u.texts[0].rotation, 0.0));
    }

    #[test]
    fn mirror_arc_a0_in_range() {
        let tau = std::f64::consts::TAU;
        let o = v2(0.0, 0.0);
        for (p, q) in [(o, v2(1.0, 0.0)), (o, v2(0.0, 1.0)), (o, v2(1.0, 1.0))] {
            let (_, mut ds) = setup();
            pick_all(&mut ds);
            ds.apply_mirror(p, q, false).unwrap();
            let a = &ds.list[0].u.arcs[0];
            assert!((0.0..tau).contains(&a.a0), "a0 = {}", a.a0);
            assert!(near(a.a1 - a.a0, FRAC_PI_2));
        }
        let (_, mut ds) = setup();
        pick_all(&mut ds);
        ds.apply_scale(v2(0.0, 0.0), 2.0, false).unwrap();
        assert!((0.0..tau).contains(&ds.list[0].u.arcs[0].a0));
    }

    #[test]
    fn stale_pick_all_kinds_no_undo_step() {
        let (mut ed, mut ds) = setup();
        let before = ds.list[0].u.clone();
        let n0 = ed.undo_len();
        let stale = Pick {
            lines: vec![99],
            arcs: vec![99],
            texts: vec![99],
            closed: vec![99],
            fills: vec![],
        };
        ds.pick = Some((0, stale));
        assert!(ds.apply_mirror(v2(0.0, 0.0), v2(1.0, 0.0), false).is_err());
        assert!(ds.apply_scale(v2(0.0, 0.0), 2.0, false).is_err());
        ds.sync_undo(&mut ed);
        assert_eq!(ed.undo_len(), n0, "stale pick leaves no step");
        assert_eq!(ds.list[0].u, before);
    }

    #[test]
    fn scale_text_height_and_circle() {
        let (_, mut ds) = setup();
        pick_all(&mut ds);
        ds.apply_scale(v2(1.0, 0.0), 2.0, false).unwrap();
        let u = &ds.list[0].u;
        assert!(near(u.texts[0].height, 0.4));
        assert!(near(u.closed[1].circle.unwrap(), 1.0));
        assert!(near(u.arcs[0].r, 2.0));
        // the base point stays, others move away from it
        assert!(near(u.lines[0].a.0, 1.0) && near(u.lines[0].a.1, 0.0));
        assert!(near(u.lines[0].b.0, 5.0) && near(u.lines[0].b.1, 2.0));
        assert!(near(u.texts[0].pos.0, 1.0) && near(u.texts[0].pos.1, 4.0));
    }

    #[test]
    fn mirror_copy_keeps_original() {
        let (_, mut ds) = setup();
        pick_all(&mut ds);
        let n = ds.list[0].u.lines.len();
        assert!(ds.apply_mirror(v2(0.0, 0.0), v2(0.0, 1.0), true).is_ok());
        let u = &ds.list[0].u;
        assert_eq!(u.lines.len(), n + 1);
        assert!(near(u.lines[0].a.0, 1.0), "the original stays");
        assert!(near(u.lines[n].a.0, -1.0), "the copy is mirrored");
        // the pick now points at the copies
        assert_eq!(ds.pick.as_ref().unwrap().1.lines, vec![n]);
    }

    #[test]
    fn undo_mirror_restores() {
        let (mut ed, mut ds) = setup();
        pick_all(&mut ds);
        let before = ds.list[0].u.clone();
        let n0 = ed.undo_len();
        ds.apply_mirror(v2(0.0, 0.0), v2(1.0, 1.0), false).unwrap();
        ds.sync_undo(&mut ed);
        assert_eq!(ed.undo_len(), n0 + 1, "one mirror, one undo step");
        assert_ne!(ds.list[0].u, before);
        ed.undo();
        ds.sync_undo(&mut ed);
        assert_eq!(ds.list[0].u, before);
        assert_eq!(ed.undo_len(), n0);
    }

    #[test]
    fn scale_zero_refused() {
        let (mut ed, mut ds) = setup();
        pick_all(&mut ds);
        let before = ds.list[0].u.clone();
        let n0 = ed.undo_len();
        for k in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(!ds.apply_scale(v2(0.0, 0.0), k, false).unwrap_err().is_empty());
        }
        assert!(ds.apply_mirror(v2(1.0, 1.0), v2(1.0, 1.0), false).is_err());
        // no pick: the whole drawing is refused with a reason
        ds.pick = None;
        assert!(ds.apply_scale(v2(0.0, 0.0), 2.0, false).is_err());
        ds.sync_undo(&mut ed);
        assert_eq!(ed.undo_len(), n0, "refusals leave no step");
        assert_eq!(ds.list[0].u, before);
    }

    #[test]
    fn scale_to_length_one_line() {
        let (_, mut ds) = setup();
        ds.pick = Some((0, Pick { lines: vec![0], ..Default::default() }));
        ds.apply_scale_to_length(v2(1.0, 0.0), 10.0, false).unwrap();
        let l = &ds.list[0].u.lines[0];
        let n = ((l.b.0 - l.a.0).powi(2) + (l.b.1 - l.a.1).powi(2)).sqrt();
        assert!(near(n, 10.0));
        assert!(near(l.a.0, 1.0) && near(l.a.1, 0.0));
    }

    fn len_of(ds: &Drawings) -> f64 {
        let l = &ds.list[0].u.lines[0];
        ((l.b.0 - l.a.0).powi(2) + (l.b.1 - l.a.1).powi(2)).sqrt()
    }

    #[test]
    fn scale_to_length_mm_input() {
        for (txt, want) in [("2500 mm", 2.5), ("2.5 m", 2.5), ("2500mm", 2.5)] {
            let (_, mut ds) = setup();
            ds.pick = Some((0, Pick { lines: vec![0], ..Default::default() }));
            let t = parse_edit_length(txt, Units::Metric).unwrap();
            ds.apply_scale_to_length(v2(1.0, 0.0), t, false).unwrap();
            assert!(near(len_of(&ds), want), "{txt}");
        }
    }

    #[test]
    fn scale_to_length_bare_number_uses_display_unit() {
        let (_, mut ds) = setup();
        ds.pick = Some((0, Pick { lines: vec![0], ..Default::default() }));
        let t = parse_edit_length("2500", Units::Metric).unwrap();
        ds.apply_scale_to_length(v2(1.0, 0.0), t, false).unwrap();
        assert!(near(len_of(&ds), 2.5));
        // prefill round-trips through the display unit
        let ((_, _), cur) = ds.picked_line_len().unwrap();
        let shown = crate::units::edit_length(cur, Units::Metric);
        assert_eq!(shown, "2\u{a0}500", "fields group thousands like the rest of the app");
        assert!(near(parse_edit_length(&shown, Units::Metric).unwrap(), 2.5));
        let t = parse_edit_length("10", Units::Imperial).unwrap();
        assert!(near(t, 10.0 * 0.0254));
    }

    #[test]
    fn scale_to_length_needs_single_line() {
        let (_, mut ds) = setup();
        ds.list[0].u.lines.push(ULine {
            layer: "0".into(),
            a: (0.0, 0.0),
            b: (1.0, 0.0),
            dashed: false,
            pen: 0,
        });
        ds.pick = Some((0, Pick { lines: vec![0, 1], ..Default::default() }));
        let before = ds.steps.len();
        assert!(ds.apply_scale_to_length(v2(0.0, 0.0), 5.0, false).is_err());
        ds.pick = Some((0, Pick { arcs: vec![0], ..Default::default() }));
        assert!(ds.apply_scale_to_length(v2(0.0, 0.0), 5.0, false).is_err());
        assert_eq!(ds.steps.len(), before);
    }

    #[test]
    fn scale_to_length_zero_refused() {
        let (_, mut ds) = setup();
        ds.list[0].u.lines[0].b = ds.list[0].u.lines[0].a;
        ds.pick = Some((0, Pick { lines: vec![0], ..Default::default() }));
        let before = ds.steps.len();
        assert!(ds.apply_scale_to_length(v2(0.0, 0.0), 5.0, false).is_err());
        ds.list[0].u.lines[0].b = (4.0, 0.0);
        for bad in [0.0, -1.0, f64::NAN] {
            assert!(ds.apply_scale_to_length(v2(0.0, 0.0), bad, false).is_err());
        }
        assert_eq!(ds.steps.len(), before);
    }

    fn set_ref(ds: &mut Drawings) {
        ds.pick = Some((0, Pick { lines: vec![0], ..Default::default() }));
        ds.set_reference().unwrap();
    }

    #[test]
    fn scale_whole_drawing_by_reference_line() {
        let (_, mut ds) = setup();
        set_ref(&mut ds);
        let cur = len_of(&ds);
        let before = ds.list[0].u.clone();
        ds.apply_scale_by_reference(Some(v2(0.0, 0.0)), cur * 3.0, true, false).unwrap();
        let u = &ds.list[0].u;
        assert!(near(len_of(&ds), cur * 3.0), "ratio target / reference");
        assert!(near(u.arcs[0].r, 3.0 * before.arcs[0].r));
        assert!(near(u.arcs[0].c.0, 3.0 * before.arcs[0].c.0));
        assert!(near(u.texts[0].height, 3.0 * before.texts[0].height));
        assert!(near(u.closed[0].pts[1].0, 18.0));
        assert!(near(u.closed[1].circle.unwrap(), 1.5));
    }

    #[test]
    fn reference_kept_when_pick_changes() {
        let (_, mut ds) = setup();
        set_ref(&mut ds);
        let cur = len_of(&ds);
        ds.pick = Some((0, Pick { arcs: vec![0], ..Default::default() }));
        assert!(ds.live_reference().is_some());
        ds.apply_scale_by_reference(Some(v2(0.0, 0.0)), 2.0 * cur, false, false).unwrap();
        let u = &ds.list[0].u;
        assert!(near(u.arcs[0].r, 2.0));
        assert!(near(u.lines[0].b.0, 3.0), "unpicked line is untouched");
    }

    #[test]
    fn reference_dropped_on_drawing_change() {
        let (_, mut ds) = setup();
        set_ref(&mut ds);
        ds.list[0].mark_edited();
        assert!(ds.live_reference().is_none());
        assert!(ds.edit.xform.reference.is_none());
        let before = ds.steps.len();
        assert!(ds.apply_scale_by_reference(None, 5.0, true, false).is_err());
        assert_eq!(ds.steps.len(), before);
    }

    #[test]
    fn reference_zero_and_bad_target_refused() {
        let (_, mut ds) = setup();
        ds.edit.xform.reference = Some(RefLen { drawing: 0, gen: ds.list[0].gen, len: 0.0 });
        let before = ds.list[0].u.clone();
        assert!(ds.apply_scale_by_reference(None, 5.0, true, false).is_err());
        set_ref(&mut ds);
        for bad in [0.0, -1.0, f64::NAN] {
            assert!(ds.apply_scale_by_reference(None, bad, true, false).is_err());
        }
        assert_eq!(ds.list[0].u, before);
        ds.list[0].u.lines[0].b = ds.list[0].u.lines[0].a;
        ds.pick = Some((0, Pick { lines: vec![0], ..Default::default() }));
        assert!(ds.set_reference().is_err(), "zero-length line is no reference");
    }

    #[test]
    fn ref_scale_one_undo_step() {
        let (mut ed, mut ds) = setup();
        set_ref(&mut ds);
        let before = ds.list[0].u.clone();
        let n0 = ed.undo_len();
        ds.apply_scale_by_reference(None, 10.0, true, false).unwrap();
        ds.sync_undo(&mut ed);
        assert_eq!(ed.undo_len(), n0 + 1);
        assert_ne!(ds.list[0].u, before);
        ed.undo();
        ds.sync_undo(&mut ed);
        assert_eq!(ds.list[0].u, before);
    }

    #[test]
    fn stale_pick_no_undo_step() {
        let (mut ed, mut ds) = setup();
        ds.pick = Some((0, Pick { lines: vec![99], ..Default::default() }));
        let before = ds.list[0].u.clone();
        let n0 = ed.undo_len();
        let steps = ds.steps.len();
        assert!(ds.apply_mirror(v2(0.0, 0.0), v2(0.0, 1.0), false).is_err());
        assert!(ds.apply_scale(v2(0.0, 0.0), 2.0, false).is_err());
        ds.sync_undo(&mut ed);
        assert_eq!(ed.undo_len(), n0);
        assert_eq!(ds.steps.len(), steps);
        assert_eq!(ds.list[0].u, before);
    }

    #[test]
    fn stale_pick_says_nothing_picked_and_clears() {
        let want = tr("dlg.dw.st.nothing_picked");
        let (_, mut ds) = setup();
        ds.pick = Some((0, Pick { lines: vec![99], ..Default::default() }));
        assert_eq!(ds.apply_scale(v2(0.0, 0.0), 2.0, false).unwrap_err(), want);
        assert!(ds.pick.is_none());
        ds.pick = Some((0, Pick::default()));
        assert_eq!(ds.apply_mirror(v2(0.0, 0.0), v2(0.0, 1.0), false).unwrap_err(), want);
        assert!(ds.pick.is_none());
        ds.pick = Some((0, Pick { lines: vec![99], ..Default::default() }));
        assert_eq!(ds.apply_scale_to_length(v2(0.0, 0.0), 5.0, false).unwrap_err(), want);
        assert!(ds.pick.is_none());
        // no pick at all keeps the whole-drawing refusal
        assert_eq!(
            ds.apply_scale(v2(0.0, 0.0), 2.0, false).unwrap_err(),
            tr("dwgmir.st.whole_refused")
        );
    }

    const EN: (bool, DecimalMark) = (false, DecimalMark::Point);
    const RU: (bool, DecimalMark) = (true, DecimalMark::Comma);

    fn one_line_pick() -> (Editor, Drawings) {
        let (ed, mut ds) = setup();
        ds.pick = Some((0, Pick { lines: vec![0], ..Default::default() }));
        (ed, ds)
    }

    #[test]
    fn typed_length_commits_by_the_ui_decimal_mark() {
        for (loc, txt) in [(EN, "2.5 m"), (RU, "2,5 m"), (EN, "2,500"), (RU, "2 500")] {
            let (_, mut ds) = one_line_pick();
            ds.edit.xform.len_text = txt.into();
            let want = 2.5;
            let msg = ds.scale_to_typed_len(v2(1.0, 0.0), Units::Metric, loc, false);
            // the success line comes from the apply path, in the UI language (not `loc`)
            assert_eq!(msg, crate::tr("dwgmir.st.scaled", &[("n", "1")]), "{txt}");
            assert!(near(len_of(&ds), want), "{txt}: {}", len_of(&ds));
        }
    }

    #[test]
    fn typed_length_is_one_undo_step() {
        let (mut ed, mut ds) = one_line_pick();
        let before = ds.list[0].u.clone();
        let n0 = ed.undo_len();
        ds.edit.xform.len_text = "2500".into();
        ds.scale_to_typed_len(v2(1.0, 0.0), Units::Metric, EN, false);
        ds.sync_undo(&mut ed);
        assert_eq!(ed.undo_len(), n0 + 1);
        ed.undo();
        ds.sync_undo(&mut ed);
        assert_eq!(ds.list[0].u, before);
        assert_eq!(ed.undo_len(), n0);
    }

    #[test]
    fn unreadable_length_writes_nothing() {
        for (loc, txt) in [(EN, "abc"), (EN, ""), (EN, "0"), (EN, "-3"), (EN, "2,5 m"), (RU, "2.5.1")] {
            let (mut ed, mut ds) = one_line_pick();
            let before = ds.list[0].u.clone();
            let (n0, steps) = (ed.undo_len(), ds.steps.len());
            ds.edit.xform.len_text = txt.into();
            let msg = ds.scale_to_typed_len(v2(1.0, 0.0), Units::Metric, loc, false);
            assert_eq!(msg, crate::tr_lang(loc.0, "dwgmir.st.len_parse", &[]), "{txt:?}");
            ds.sync_undo(&mut ed);
            assert_eq!((ed.undo_len(), ds.steps.len()), (n0, steps), "{txt:?}");
            assert_eq!(ds.list[0].u, before, "{txt:?}");
        }
    }

    #[test]
    fn scale_by_reference_reads_the_typed_length() {
        let (mut ed, mut ds) = one_line_pick();
        ds.set_reference().unwrap();
        ds.edit.xform.len_text = "x".into();
        let before = ds.list[0].u.clone();
        let msg = ds.scale_by_ref_typed(Units::Metric, EN, false);
        assert_eq!(msg, crate::tr_lang(false, "dwgmir.st.len_parse", &[]));
        assert_eq!(ds.list[0].u, before);
        assert!(ds.edit.xform.reference.is_some(), "a refused apply keeps the reference");
        // a bare metric number is millimetres, so the metre is named
        ds.edit.xform.len_text = "5,0 m".into();
        let n0 = ed.undo_len();
        ds.scale_by_ref_typed(Units::Metric, RU, false);
        ds.sync_undo(&mut ed);
        assert!(near(len_of(&ds), 5.0));
        assert_eq!(ed.undo_len(), n0 + 1);
        assert!(ds.edit.xform.reference.is_none());
    }

    fn font_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        ctx
    }

    #[test]
    fn controls_render_headless_in_both_languages_and_write_nothing() {
        for ru in [false, true] {
            for units in [Units::Metric, Units::Imperial] {
                let (mut ed, mut ds) = one_line_pick();
                ds.set_reference().unwrap();
                let before = ds.list[0].u.clone();
                let (n0, steps) = (ed.undo_len(), ds.steps.len());
                let ctx = font_ctx();
                let mut status = None;
                for _ in 0..3 {
                    let _ = ctx.run(Default::default(), |c| {
                        egui::CentralPanel::default().show(c, |ui| {
                            status = ds.xform_ui_in(ui, units, ru);
                        });
                    });
                }
                assert!(status.is_none(), "drawing the controls says nothing");
                ds.sync_undo(&mut ed);
                assert_eq!((ed.undo_len(), ds.steps.len()), (n0, steps));
                assert_eq!(ds.list[0].u, before);
                // the prefill is read back by the same locale
                let mark = if ru { DecimalMark::Comma } else { DecimalMark::Point };
                let t = ds.typed_len(units, mark).unwrap_or(f64::NAN);
                assert!((t - len_of(&ds)).abs() < 5e-3, "{units:?} ru={ru}: {t}");
            }
        }
    }

    #[test]
    fn controls_use_no_raw_widgets() {
        let src = include_str!("xform.rs");
        let prod = &src[..src.find("#[cfg(test)]").unwrap_or(src.len())];
        for raw in [
            "ui.button(",
            "small_button(",
            "ui.checkbox(",
            "egui::DragValue",
            "egui::ComboBox",
            "egui::TextEdit",
            "selectable_value",
            "selectable_label",
            "radio_value",
            "ui.add(egui::",
        ] {
            assert!(!prod.contains(raw), "raw widget {raw} in the transform controls");
        }
    }
}
