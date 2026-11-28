// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Structural element properties: kind, quantities, key numbers. Committed on
//! release/focus loss (choices commit immediately) as a single model operation (`Editor::run_op`).
//! Also hosts plan highlighting of the selection and beam-system layout rules for the options.

use crate::editor::structx::of_sel;
use crate::editor::{Editor, OpFn, Sel};
use crate::ui_kit;
use crate::unit_text::{area, vol};
use crate::units::{
    display_length_lang, fmt_num_lang, unit_label, Units,
};
use ddc_geom::V2;
use ddc_model::{BeamLayout, Error, Model, TrussChord, TrussWeb};
use ddc_ui::i18n::Catalog;
use egui::{self, Color32, Pos2, Stroke};
use std::sync::Arc;

/// Field value kind.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Unit {
    /// Length, m (shown in the current units).
    Len,
    /// Angle, degrees (radians in the model).
    Deg,
    /// Percent (a fraction in the model).
    Pct,
    Count,
    /// Option index; labels are i18n keys.
    Choice(&'static [&'static str]),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Field {
    pub key: &'static str,
    pub unit: Unit,
    pub lo: f64,
    pub hi: f64,
}

const fn fd(key: &'static str, unit: Unit, lo: f64, hi: f64) -> Field {
    Field { key, unit, lo, hi }
}

pub const WEB_KEYS: [&str; 3] = ["opt.web.pratt", "opt.web.howe", "opt.web.warren"];
pub const CHORD_KEYS: [&str; 2] = ["sp.chord.flat", "opt.pitched"];
/// Layout rules: index ↔ `BeamLayout` (`layout_of` / `layout_index`).
pub const LAYOUT_KEYS: [&str; 4] = [
    "opt.layout.fixed_start",
    "opt.layout.fixed_center",
    "opt.layout.max_equal",
    "opt.layout.fixed_count",
];

const WEBS: [TrussWeb; 3] = [TrussWeb::Pratt, TrussWeb::Howe, TrussWeb::Warren];

pub fn layout_of(i: usize, count: usize) -> BeamLayout {
    match i {
        0 => BeamLayout::FixedStart,
        1 => BeamLayout::FixedCenter,
        3 => BeamLayout::FixedCount(count.max(1)),
        _ => BeamLayout::MaxEqual,
    }
}

pub fn layout_index(l: BeamLayout) -> usize {
    match l {
        BeamLayout::FixedStart => 0,
        BeamLayout::FixedCenter => 1,
        BeamLayout::MaxEqual => 2,
        BeamLayout::FixedCount(_) => 3,
    }
}

const FOOTING: [Field; 7] = [
    fd("opt.rotation", Unit::Deg, -360.0, 360.0),
    fd("opt.top_off", Unit::Len, -10.0, 10.0),
    fd("sp.step_w", Unit::Len, 0.1, 20.0),
    fd("sp.step_l", Unit::Len, 0.1, 20.0),
    fd("sp.step_h", Unit::Len, 0.05, 5.0),
    fd("sp.at_x", Unit::Len, -1e4, 1e4),
    fd("sp.at_y", Unit::Len, -1e4, 1e4),
];
const STRIP: [Field; 4] = [
    fd("opt.width", Unit::Len, 0.1, 10.0),
    fd("opt.depth", Unit::Len, 0.05, 5.0),
    fd("sp.ecc", Unit::Len, -5.0, 5.0),
    fd("opt.top_off", Unit::Len, -10.0, 10.0),
];
const RAFT: [Field; 4] = [
    fd("sp.thickness", Unit::Len, 0.05, 5.0),
    fd("opt.edge_width", Unit::Len, 0.0, 10.0),
    fd("opt.edge_depth", Unit::Len, 0.0, 5.0),
    fd("opt.top_off", Unit::Len, -10.0, 10.0),
];
const RAMP: [Field; 7] = [
    fd("opt.width", Unit::Len, 0.3, 20.0),
    fd("opt.slope_pct", Unit::Pct, 0.5, 100.0),
    fd("sp.thickness", Unit::Len, 0.02, 2.0),
    fd("core_tools.landing", Unit::Len, 0.3, 20.0),
    fd("core_tools.max_run", Unit::Len, 0.5, 200.0),
    fd("opt.base_off", Unit::Len, -10.0, 50.0),
    fd("opt.top_off", Unit::Len, -10.0, 100.0),
];
const ROOM_SEP: [Field; 4] = [
    fd("core_tools.sep.ax", Unit::Len, -1e5, 1e5),
    fd("core_tools.sep.ay", Unit::Len, -1e5, 1e5),
    fd("core_tools.sep.bx", Unit::Len, -1e5, 1e5),
    fd("core_tools.sep.by", Unit::Len, -1e5, 1e5),
];
const SHAFT: [Field; 2] = [
    fd("opt.base_off", Unit::Len, -10.0, 50.0),
    fd("opt.top_off", Unit::Len, -10.0, 100.0),
];
/// End points of a grid line or a reference plane (the same rows as a room separator).
const LINE_ENDS: [Field; 4] = ROOM_SEP;
const BRACE: [Field; 1] = [fd("opt.rotation", Unit::Deg, -360.0, 360.0)];
const TRUSS: [Field; 5] = [
    fd("opt.height", Unit::Len, 0.2, 20.0),
    fd("sp.heel", Unit::Len, 0.0, 10.0),
    fd("opt.panels", Unit::Count, 2.0, 200.0),
    fd("sp.web", Unit::Choice(&WEB_KEYS), 0.0, 2.0),
    fd("sp.chord", Unit::Choice(&CHORD_KEYS), 0.0, 1.0),
];
const SYSTEM: [Field; 6] = [
    fd("opt.spacing", Unit::Len, 0.1, 20.0),
    fd("opt.direction", Unit::Deg, -360.0, 360.0),
    fd("opt.layout", Unit::Choice(&LAYOUT_KEYS), 0.0, 3.0),
    fd("opt.count", Unit::Count, 1.0, 1000.0),
    fd("opt.end_gap", Unit::Len, 0.0, 2.0),
    fd("opt.z", Unit::Len, -10.0, 50.0),
];

/// Editable fields of a kind (order matches `values` / `apply`).
pub fn fields(kind: &str) -> &'static [Field] {
    match kind {
        "footing" => &FOOTING,
        "strip_footing" => &STRIP,
        "raft" => &RAFT,
        "brace" => &BRACE,
        "truss" => &TRUSS,
        "beam_system" => &SYSTEM,
        "ramp" => &RAMP,
        "room_sep" => &ROOM_SEP,
        "shaft" => &SHAFT,
        "grid" | "ref_plane" => &LINE_ENDS,
        _ => &[],
    }
}

/// Current field values (angles in degrees, choices as an index).
pub fn values(m: &Model, kind: &str, id: i64) -> Result<Vec<f64>, Error> {
    Ok(match kind {
        "ramp" => {
            let s = m.ramp(id)?;
            vec![
                s.width,
                s.slope * 100.0,
                s.thickness,
                s.landing,
                s.max_run,
                s.base_offset,
                s.top_offset,
            ]
        }
        "room_sep" => {
            let s = m.room_sep(id)?;
            vec![s.a.0, s.a.1, s.b.0, s.b.1]
        }
        "zone" => {
            m.zone(id)?;
            vec![]
        }
        "shaft" => {
            let s = m.shaft(id)?;
            vec![s.base_offset, s.top_offset]
        }
        "grid" => {
            let s = m.grid(id)?.spec;
            vec![s.a.0, s.a.1, s.b.0, s.b.1]
        }
        "ref_plane" => {
            let (_, a, b) = m.ref_plane(id)?;
            vec![a.0, a.1, b.0, b.1]
        }
        "footing" => {
            let s = m.footing(id)?;
            let (w, l, h) = s.steps.first().map_or((0.0, 0.0, 0.0), |t| (t.w, t.l, t.h));
            vec![
                s.rotation.to_degrees(),
                s.top_offset,
                w,
                l,
                h,
                s.at.0,
                s.at.1,
            ]
        }
        "strip_footing" => {
            let s = m.strip_footing(id)?;
            vec![s.width, s.depth, s.ecc, s.top_offset]
        }
        "raft" => {
            let s = m.raft(id)?;
            vec![s.thickness, s.edge_width, s.edge_depth, s.top_offset]
        }
        "brace" => vec![m.brace(id)?.rotation.to_degrees()],
        "truss" => {
            let s = m.truss(id)?;
            let web = WEBS.iter().position(|w| *w == s.web).unwrap_or(0);
            let chord = usize::from(s.chord == TrussChord::Pitched);
            vec![s.height, s.heel, s.panels as f64, web as f64, chord as f64]
        }
        "beam_system" => {
            let s = m.beam_system(id)?;
            let n = match s.layout {
                BeamLayout::FixedCount(n) => n,
                _ => 5,
            };
            vec![
                s.spacing,
                s.angle.to_degrees(),
                layout_index(s.layout) as f64,
                n as f64,
                s.end_gap,
                s.z,
            ]
        }
        _ => return Err(Error::Invalid(format!("unknown kind {kind}"))),
    })
}

/// New name of a grid line or a reference plane, one operation.
pub(crate) fn rename_datum(m: &mut Model, kind: &str, id: i64, name: &str) -> Result<(), Error> {
    match kind {
        "grid" => {
            let mut s = m.grid(id)?.spec;
            s.name = Some(name.to_string());
            m.update_grid(id, &s)
        }
        _ => {
            let (_, a, b) = m.ref_plane(id)?;
            m.update_ref_plane(id, name, a, b)
        }
    }
}

fn idx(v: f64) -> usize {
    v.round().max(0.0) as usize
}

/// Write field values as a single model operation (`update_*`).
pub fn apply(m: &mut Model, kind: &str, id: i64, v: &[f64]) -> Result<(), Error> {
    let n = fields(kind).len();
    if v.len() != n {
        return Err(Error::Invalid(format!(
            "{kind}: {} values, need {n}",
            v.len()
        )));
    }
    match kind {
        "footing" => {
            let mut s = m.footing(id)?;
            s.rotation = v[0].to_radians();
            s.top_offset = v[1];
            if let Some(t) = s.steps.first_mut() {
                (t.w, t.l, t.h) = (v[2], v[3], v[4]);
            }
            s.at = (v[5], v[6]);
            m.update_footing(id, &s)
        }
        "strip_footing" => {
            let mut s = m.strip_footing(id)?;
            (s.width, s.depth, s.ecc, s.top_offset) = (v[0], v[1], v[2], v[3]);
            m.update_strip_footing(id, &s)
        }
        "raft" => {
            let mut s = m.raft(id)?;
            (s.thickness, s.edge_width, s.edge_depth, s.top_offset) = (v[0], v[1], v[2], v[3]);
            m.update_raft(id, &s)
        }
        "brace" => {
            let mut s = m.brace(id)?;
            s.rotation = v[0].to_radians();
            m.update_brace(id, &s)
        }
        "truss" => {
            let mut s = m.truss(id)?;
            s.height = v[0];
            s.heel = v[1];
            s.web = WEBS[idx(v[3]).min(2)];
            s.chord = if idx(v[4]) == 1 {
                TrussChord::Pitched
            } else {
                TrussChord::Flat
            };
            let mut panels = idx(v[2]).clamp(2, 200);
            if s.chord == TrussChord::Pitched && panels % 2 == 1 {
                panels += 1;
            }
            s.panels = panels;
            m.update_truss(id, &s)
        }
        "ramp" => {
            let mut s = m.ramp(id)?;
            s.width = v[0];
            s.slope = v[1] / 100.0;
            s.thickness = v[2];
            s.landing = v[3];
            s.max_run = v[4];
            s.base_offset = v[5];
            s.top_offset = v[6];
            m.update_ramp(id, &s)
        }
        "room_sep" => {
            let mut s = m.room_sep(id)?;
            s.a = (v[0], v[1]);
            s.b = (v[2], v[3]);
            m.update_room_sep(id, &s)
        }
        "shaft" => {
            let mut s = m.shaft(id)?;
            (s.base_offset, s.top_offset) = (v[0], v[1]);
            m.update_shaft(id, &s)
        }
        "grid" => {
            let mut s = m.grid(id)?.spec;
            s.a = (v[0], v[1]);
            s.b = (v[2], v[3]);
            m.update_grid(id, &s)
        }
        "ref_plane" => {
            let (name, _, _) = m.ref_plane(id)?;
            m.update_ref_plane(id, &name, (v[0], v[1]), (v[2], v[3]))
        }
        "beam_system" => {
            let mut s = m.beam_system(id)?;
            s.spacing = v[0];
            s.angle = v[1].to_radians();
            s.layout = layout_of(idx(v[2]), idx(v[3]));
            s.end_gap = v[4];
            s.z = v[5];
            m.update_beam_system(id, &s)
        }
        _ => Err(Error::Invalid(format!("unknown kind {kind}"))),
    }
}

/// Field row; true — edit finished (choices finish immediately on change).
fn field(ui: &mut egui::Ui, cat: &Catalog, f: &Field, v: &mut f64, u: Units, id: i64) -> bool {
    let name = cat.t(f.key);
    let salt = egui::Id::new(("g6.field", id, f.key));
    let kv = |ui: &mut egui::Ui, v: &mut f64, kind, unit: &str| {
        ui_kit::kv_field(ui, salt, &name, v, kind, f.lo, f.hi, unit)
    };
    match f.unit {
        Unit::Len => kv(ui, v, ui_kit::NumKind::Len(u), unit_label(u)),
        Unit::Deg => kv(ui, v, ui_kit::NumKind::DEG, "°"),
        Unit::Pct => kv(ui, v, ui_kit::NumKind::PCT, "%"),
        Unit::Count => kv(ui, v, ui_kit::NumKind::COUNT, ""),
        Unit::Choice(keys) => {
            let before = idx(*v).min(keys.len() - 1);
            let mut i = before;
            let items: Vec<String> = keys.iter().map(|k| cat.t(k)).collect();
            let items: Vec<&str> = items.iter().map(String::as_str).collect();
            let th = ddc_ui::theme::of(ui.ctx());
            ui_kit::kv_widget(ui, &name, |ui| {
                ddc_ui::widgets::field::dropdown(
                    ui,
                    &th,
                    ("g6.choice", f.key),
                    &mut i,
                    &items,
                    Some(ui_kit::WIDGET_W),
                );
            });
            *v = i as f64;
            i != before
        }
    }
}

/// Edit buffer between frames: the selection and values before commit.
type Buf = (Sel, Vec<f64>);

/// Quantity rows (catalog key, shown value) in the language of `cat`: parts, length when the
/// element has one, volume, area, then weight when the density is known.
fn qty_rows(cat: &Catalog, q: &ddc_model::StructQty, u: Units) -> Vec<(&'static str, String)> {
    let ru = cat.lang == ddc_ui::Lang::Ru;
    let mut rows = vec![("sp.parts", q.parts.to_string())];
    if q.length > 0.0 {
        rows.push(("sp.length", display_length_lang(q.length, u, cat.lang)));
    }
    rows.push(("sp.volume", vol(q.volume, ru, u)));
    rows.push(("sp.area", area(q.area, ru, u)));
    if let Some(w) = q.weight {
        rows.push(("sp.weight", format!("{}\u{a0}{}", fmt_num_lang(w, 0, cat.lang), cat.t("sp.kg"))));
    }
    rows
}

/// true when a structural element is selected (the panel is drawn).
pub fn show(ui: &mut egui::Ui, cat: &Catalog, ed: &mut Editor) -> bool {
    let Some((sel, (kind, id))) = ed
        .selection
        .iter()
        .find_map(|s| of_sel(*s).map(|k| (*s, k)))
    else {
        return false;
    };
    let Ok(init) = values(&ed.model, kind, id) else {
        return false;
    };
    let q = ed.model.struct_quantities(kind, id).ok();
    let u = ed.tool.units;
    let bid = egui::Id::new("ddc.g6.struct_props");
    let mut cur = ui
        .data(|d| d.get_temp::<Buf>(bid))
        .filter(|b| b.0 == sel && b.1.len() == init.len())
        .map_or_else(|| init.clone(), |b| b.1);
    let mut commit = false;
    let mut rename = None;
    ui_kit::props(ui, "g6.sel", |ui| {
        ui_kit::kv(ui, &cat.t(&format!("tool.{kind}")), format!("#{id}"));
        if let Some(q) = &q {
            let rows = qty_rows(cat, q, u);
            let (head, weight) = rows.split_at(rows.len() - usize::from(q.weight.is_some()));
            for (k, v) in head {
                ui_kit::kv_num(ui, &cat.t(k), v.clone());
            }
            if !q.material.is_empty() {
                crate::view_gfx_ui::mat_swatch::material_row(ui, &cat.t("sp.material"), &q.material);
            }
            for (k, v) in weight {
                ui_kit::kv_num(ui, &cat.t(k), v.clone());
            }
        }
        rename = crate::tools::core_rows(ui, cat, &ed.model, kind, id, u);
        for (f, v) in fields(kind).iter().zip(cur.iter_mut()) {
            commit |= field(ui, cat, f, v, u, id);
        }
    });
    if let Some(edit) = rename {
        let r = ed.run_op(OpFn(Arc::new(move |m: &mut Model| match &edit {
            crate::tools::CoreEdit::Zone(z) => m.update_zone(id, z),
            crate::tools::CoreEdit::Ramp(s) => m.update_ramp(id, s),
            crate::tools::CoreEdit::Shaft(s) => m.update_shaft(id, s),
            crate::tools::CoreEdit::Name(n) => rename_datum(m, kind, id, n),
        })));
        if let Err(e) = r {
            ed.status = Some(format!("{}: {e}", cat.t("tool.err.model")));
        }
    }
    if commit && cur != init {
        ui.data_mut(|d| d.remove::<Buf>(bid));
        let vals = cur;
        let r = ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
            apply(m, kind, id, &vals)
        })));
        if let Err(e) = r {
            ed.status = Some(format!("{}: {e}", cat.t("tool.err.model")));
        }
    } else {
        ui.data_mut(|d| d.insert_temp(bid, (sel, cur)));
    }
    ui_kit::hint(ui, cat.t("sp.hint"));
    true
}

/// Outlines of selected structural elements in the plan (accent color, like other selection).
pub fn paint_sel(p: &egui::Painter, ed: &Editor, accent: Color32, to_s: &dyn Fn(V2) -> Pos2) {
    for s in ed.selection.iter().filter(|s| of_sel(**s).is_some()) {
        for o in crate::selection::outlines(ed, *s) {
            if o.len() < 2 {
                continue;
            }
            let mut line: Vec<Pos2> = o.iter().map(|q| to_s(*q)).collect();
            if o.len() >= 3 {
                line.push(line[0]);
            }
            p.add(egui::Shape::line(line, Stroke::new(2.0, accent)));
        }
        // the zone is only a label point: its fill is the rooms it holds
        if let Some(("zone", id)) = of_sel(*s) {
            let Ok(z) = ed.model.zone(id) else { continue };
            for room in ed.rooms.iter().filter(|r| z.rooms.contains(&r.id)) {
                for o in room.rings.iter().take(1).filter(|o| o.len() >= 3) {
                    let mut line: Vec<Pos2> = o.iter().map(|q| to_s(*q)).collect();
                    line.push(line[0]);
                    p.add(egui::Shape::line(line, Stroke::new(1.5, accent.gamma_multiply(0.6))));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::{FootingSpec, FootingStep};

    #[test]
    fn quantity_rows_follow_the_catalog_language() {
        let q = ddc_model::StructQty {
            parts: 2,
            length: 1234.5,
            volume: 1234.0,
            area: 0.0,
            material: String::new(),
            weight: Some(12345.0),
        };
        let m = Units::Metric;
        for (lang, len, kg, v) in [
            (ddc_ui::Lang::En, "1,234,500\u{a0}", "12,345\u{a0}", "1,234"),
            (ddc_ui::Lang::Ru, "1\u{a0}234\u{a0}500\u{a0}", "12\u{a0}345\u{a0}", "1\u{a0}234"),
        ] {
            // the global UI language stays as it is: only the catalog decides (any precision)
            let rows = qty_rows(&Catalog::new(lang), &q, m);
            let keys: Vec<_> = rows.iter().map(|r| r.0).collect();
            assert_eq!(keys, ["sp.parts", "sp.length", "sp.volume", "sp.area", "sp.weight"]);
            assert!(rows[1].1.starts_with(len), "{lang:?} {:?}", rows[1].1);
            assert!(rows[2].1.starts_with(v), "{lang:?} {:?}", rows[2].1);
            assert!(rows[3].1.starts_with('0') && !rows[3].1.contains('-'), "{:?}", rows[3].1);
            assert!(rows[4].1.starts_with(kg), "{lang:?} {:?}", rows[4].1);
        }
        let bare = ddc_model::StructQty { length: 0.0, weight: None, ..q };
        let rows = qty_rows(&Catalog::new(ddc_ui::Lang::En), &bare, m);
        let keys: Vec<_> = rows.iter().map(|r| r.0).collect();
        assert_eq!(keys, ["sp.parts", "sp.volume", "sp.area"]);
    }

    #[test]
    fn footing_fields_round_trip_one_undo() {
        let mut e = Editor::new_in_memory().unwrap();
        let id = e
            .model
            .add_footing(&FootingSpec {
                level: e.level,
                column: None,
                at: (1.0, 2.0),
                rotation: 0.0,
                round: false,
                steps: vec![FootingStep {
                    w: 1.2,
                    l: 1.2,
                    h: 0.5,
                }],
                top_offset: 0.0,
                material: "Concrete".into(),
            })
            .unwrap();
        e.reload().unwrap();
        let mut v = values(&e.model, "footing", id).unwrap();
        assert_eq!(v.len(), fields("footing").len());
        v[2] = 1.5;
        v[3] = 1.5;
        let vals = v.clone();
        e.run_op(OpFn(Arc::new(move |m: &mut Model| {
            apply(m, "footing", id, &vals)
        })))
        .unwrap();
        let q = e.model.struct_quantities("footing", id).unwrap();
        assert!((q.volume - 1.5 * 1.5 * 0.5).abs() < 1e-9, "{}", q.volume);
        assert_eq!(values(&e.model, "footing", id).unwrap(), v);
        e.undo();
        let q = e.model.struct_quantities("footing", id).unwrap();
        assert!((q.volume - 1.2 * 1.2 * 0.5).abs() < 1e-9);
    }

    #[test]
    fn layouts_map_both_ways() {
        for (i, k) in LAYOUT_KEYS.iter().enumerate() {
            let l = layout_of(i, 4);
            assert_eq!(layout_index(l), i);
            assert_eq!(l.text(), k.trim_start_matches("opt.layout."));
        }
        assert_eq!(layout_of(3, 0), BeamLayout::FixedCount(1));
    }

    #[test]
    fn every_kind_has_values_matching_fields() {
        for k in ddc_model::STRUCT_KINDS {
            assert!(!fields(k).is_empty(), "{k}");
        }
        let m = Model::open(":memory:").unwrap();
        assert!(apply(&mut Model::open(":memory:").unwrap(), "footing", 1, &[]).is_err());
        assert!(values(&m, "nope", 1).is_err());
    }
}
