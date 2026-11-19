// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Properties of a multi-selection: the rows every selected element has, one value or "varies",
//! one edit sets them all in one compound op (one undo step). Rows only some elements have are
//! hidden and counted; rows that cannot be read say why.

use crate::editor::{Editor, OpFn, Sel};
use crate::geom_props::{self, Row, Set, Unit};
use crate::tr;
use crate::ui_kit;
use crate::units::edit_length;
use ddc_model::{Error, Model};
use std::sync::Arc;

const SAME: f64 = 1e-9;

pub enum Cell {
    Same(f64),
    Varies,
    /// The value cannot be read from every element; the text is the reason.
    Refused(String),
}

pub struct MRow {
    pub key: &'static str,
    pub unit: Unit,
    pub lo: f64,
    pub hi: f64,
    /// The row of each selected element, with its id.
    pub per: Vec<(Row, i64)>,
    pub cell: Cell,
}

pub struct Multi {
    pub rows: Vec<MRow>,
    /// Rows that exist on some of the selected elements only.
    pub hidden: usize,
    /// Curtain wall panels in the selection: they have no rows of their own.
    pub panels: usize,
}

/// The shared rows of the selection; elements are told apart by table and id.
pub fn build(ed: &Editor) -> Multi {
    let mut items: Vec<(String, i64)> = vec![];
    let mut panels = 0;
    for s in &ed.selection {
        if matches!(s, Sel::Panel(..)) {
            panels += 1;
        } else if let Some(it) = crate::clip_tools::sel_item(*s) {
            if !items.contains(&it) {
                items.push(it);
            }
        }
    }
    let all = geom_props::rows();
    let mut tables: Vec<&str> = vec![];
    for (t, _) in &items {
        if !tables.contains(&t.as_str()) {
            tables.push(t);
        }
    }
    let mut union: Vec<&'static str> = vec![];
    for r in &all {
        if tables.contains(&r.kind) && !union.contains(&r.key) {
            union.push(r.key);
        }
    }
    let mut rows = vec![];
    for key in union.iter().copied() {
        let Some(first) = all
            .iter()
            .find(|r| r.key == key && tables.contains(&r.kind))
        else {
            continue;
        };
        let per: Option<Vec<(Row, i64)>> = items
            .iter()
            .map(|(t, id)| {
                all.iter()
                    .find(|r| r.kind == t.as_str() && r.key == key && r.unit == first.unit)
                    .map(|r| (*r, *id))
            })
            .collect();
        let Some(per) = per.filter(|p| !p.is_empty()) else {
            continue;
        };
        let mut vals = vec![];
        let mut why = None;
        for (r, id) in &per {
            match (r.get)(&ed.model, *id) {
                Ok(v) => vals.push(v),
                Err(e) => {
                    why = Some(e.to_string());
                    break;
                }
            }
        }
        let cell = match why {
            Some(e) => Cell::Refused(e),
            None if vals.windows(2).all(|w| (w[0] - w[1]).abs() < SAME) => Cell::Same(vals[0]),
            None => Cell::Varies,
        };
        rows.push(MRow {
            key,
            unit: first.unit,
            lo: first.lo,
            hi: first.hi,
            per,
            cell,
        });
    }
    let hidden = union.len() - rows.len();
    Multi {
        rows,
        hidden,
        panels,
    }
}

/// Sets one row on every selected element in one op. Returns how many changed; a refusal
/// (out of range, or any element refusing) leaves all of them as they were.
pub fn apply(ed: &mut Editor, row: &MRow, v: f64) -> Result<usize, Error> {
    let shown = match row.unit {
        Unit::Deg => v.to_degrees(),
        _ => v,
    };
    let ranged = matches!(row.unit, Unit::Len | Unit::Deg);
    if !v.is_finite() || (ranged && (shown < row.lo || shown > row.hi)) {
        return Err(Error::Invalid(tr("elem.multi.range", &[])));
    }
    let sets: Vec<(Set, i64)> = row
        .per
        .iter()
        .filter(|(r, id)| (r.get)(&ed.model, *id).map_or(true, |x| (x - v).abs() >= SAME))
        .map(|(r, id)| (r.set, *id))
        .collect();
    let n = sets.len();
    if n == 0 {
        return Ok(0);
    }
    let op = OpFn(Arc::new(move |m: &mut Model| {
        m.atomic("props_multi", |m| {
            for (set, id) in &sets {
                set(m, *id, v)?;
            }
            Ok(())
        })
    }));
    ed.run_op(op)?;
    Ok(n)
}

fn note(ui: &mut egui::Ui, text: String) {
    ui.add(egui::Label::new(egui::RichText::new(text).small().weak()).wrap());
}

/// Panel of a selection that holds a migrated, component or MEP element. False: the old
/// multi-selection panel draws instead.
pub fn show(ui: &mut egui::Ui, ed: &mut Editor) -> bool {
    let migrated = ed
        .selection
        .iter()
        .any(|s| matches!(s, Sel::Ext(..) | Sel::Comp(_) | Sel::Mep(_)));
    if ed.selection.len() < 2 || !migrated {
        return false;
    }
    let m = build(ed);
    let n = ed.selection.len().to_string();
    let title = tr("elem.multi.title", &[("n", n.as_str())]);
    let mut pending: Option<(usize, f64)> = None;
    if ui_kit::heading(ui, &title) {
        let shown: &Editor = ed;
        let sig = shown.selection.iter().fold(0u64, |h, s| {
            use std::hash::{Hash, Hasher};
            let mut x = std::collections::hash_map::DefaultHasher::new();
            s.hash(&mut x);
            h ^ x.finish()
        });
        ui_kit::props(ui, "elem.multi", |ui| {
            for (i, r) in m.rows.iter().enumerate() {
                if let Some(v) = edit_row(ui, shown, r, sig) {
                    pending = Some((i, v));
                }
            }
        });
    }
    if m.rows.is_empty() {
        note(ui, tr("elem.multi.none", &[]));
    }
    if m.hidden > 0 {
        let k = m.hidden.to_string();
        note(ui, tr("elem.multi.hidden", &[("n", k.as_str())]));
    }
    if m.panels > 0 {
        note(ui, tr("elem.panel_refused", &[]));
    }
    for r in &m.rows {
        if let Cell::Refused(why) = &r.cell {
            note(ui, format!("{}: {why}", tr(r.key, &[])));
        }
    }
    if let Some((i, v)) = pending {
        ed.status = Some(match apply(ed, &m.rows[i], v) {
            Ok(0) => tr("elem.multi.same", &[]),
            Ok(k) => tr("elem.multi.changed", &[("n", k.to_string().as_str())]),
            Err(e) => tr("geom.not_changed", &[("e", e.to_string().as_str())]),
        });
    }
    true
}

fn differs(cur: Option<f64>, v: f64) -> bool {
    cur.map_or(true, |c| (c - v).abs() >= SAME)
}

/// Number field of a row: the kit field with its unit outside the frame, the value on the
/// right, «different values» as the hint of an empty field.
fn num_field(
    ui: &mut egui::Ui,
    label: &str,
    buf: &mut String,
    unit: &str,
    hint: &str,
) -> egui::Response {
    let th = ddc_ui::theme::of(ui.ctx());
    let lid = ui.id().with(("lbl", label));
    ddc_ui::widgets::TextField::new(buf)
        .labelled_by(lid, label)
        .unit(unit)
        .hint(hint)
        .right_aligned()
        .width(ui_kit::WIDGET_W)
        .show(ui, &th)
        .response
}

/// One row; the new value when the user commits one that differs from what is shown.
fn edit_row(ui: &mut egui::Ui, ed: &Editor, r: &MRow, sig: u64) -> Option<f64> {
    let label = tr(r.key, &[]);
    let varies = tr("elem.multi.varies", &[]);
    let cur = match &r.cell {
        Cell::Refused(_) => {
            ui_kit::kv_num(ui, &label, "\u{2014}");
            return None;
        }
        Cell::Same(v) => Some(*v),
        Cell::Varies => None,
    };
    let u = ed.tool.units;
    let id = egui::Id::new(("elem.multi", r.key, sig, ed.revision));
    let stored = ui.data(|d| d.get_temp::<String>(id));
    match r.unit {
        Unit::Len => {
            let mut buf =
                stored.unwrap_or_else(|| cur.map(|v| edit_length(v, u)).unwrap_or_default());
            let tip = tr("elem.multi.tip", &[]);
            let out = ui_kit::kv_length_hint(ui, &label, &mut buf, u, &tip, &varies);
            ui.data_mut(|d| d.insert_temp(id, buf));
            out.filter(|v| differs(cur, *v))
        }
        Unit::Deg => {
            let mut buf = stored.unwrap_or_else(|| {
                cur.map(|v| format!("{:.1}", v.to_degrees()))
                    .unwrap_or_default()
            });
            let mut out = None;
            ui_kit::kv_widget(ui, &label, |ui| {
                let e = num_field(ui, &label, &mut buf, "\u{b0}", &varies);
                if ui_kit::track_field(ui, &e, &mut buf) == ui_kit::FieldEnd::Apply {
                    out = crate::units::parse_locale_number(buf.trim(), crate::units::ui_decimal_mark())
                        .ok()
                        .map(f64::to_radians);
                }
            });
            ui.data_mut(|d| d.insert_temp(id, buf));
            out.filter(|v| differs(cur, *v))
        }
        Unit::Pct | Unit::Count => {
            let count = matches!(r.unit, Unit::Count);
            let mut buf = stored.unwrap_or_else(|| {
                cur.map(|v| if count { format!("{v:.0}") } else { format!("{v:.2}") })
                    .unwrap_or_default()
            });
            let mut out = None;
            ui_kit::kv_widget(ui, &label, |ui| {
                let e = num_field(ui, &label, &mut buf, if count { "" } else { "%" }, &varies);
                if ui_kit::track_field(ui, &e, &mut buf) == ui_kit::FieldEnd::Apply {
                    out = crate::units::parse_locale_number(buf.trim(), crate::units::ui_decimal_mark())
                        .ok()
                        .map(|v| if count { v.round() } else { v })
                        .map(|v| v.clamp(r.lo, r.hi));
                }
            });
            ui.data_mut(|d| d.insert_temp(id, buf));
            out.filter(|v| differs(cur, *v))
        }
        Unit::Type(_) | Unit::Choice(_) => {
            let (opts, name): (Vec<(i64, String)>, Option<String>) = match r.unit {
                Unit::Type(t) => {
                    let o = geom_props::type_options(&ed.model, t);
                    let n = cur
                        .and_then(|c| o.iter().find(|x| x.0 == c as i64))
                        .map(|x| x.1.clone());
                    (o, n)
                }
                Unit::Choice(keys) => (
                    keys.iter()
                        .enumerate()
                        .map(|(i, k)| (i as i64, tr(k, &[])))
                        .collect(),
                    cur.and_then(|c| keys.get(c as usize))
                        .map(|k| tr(k, &[])),
                ),
                _ => (vec![], None),
            };
            let mut chosen = None;
            ui_kit::kv_widget(ui, &label, |ui| {
                let th = ddc_ui::theme::of(ui.ctx());
                let names: Vec<&str> = opts.iter().map(|(_, n)| n.as_str()).collect();
                // different values: no row is current, the closed list says so
                let at = opts.iter().position(|(o, _)| cur.is_some_and(|c| c as i64 == *o));
                let mut sel = at.unwrap_or(usize::MAX);
                let closed = if at.is_some() { name.as_deref() } else { Some(varies.as_str()) };
                let w = Some(ui_kit::WIDGET_W);
                let r = ddc_ui::widgets::field::dropdown_shown(
                    ui, &th, id, &mut sel, &names, w, None, None, closed,
                );
                if r.changed() {
                    chosen = opts.get(sel).map(|(o, _)| *o as f64);
                }
            });
            chosen.filter(|v| differs(cur, *v))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom_props::tests::fixture;
    use ddc_model::WallSpec;

    fn walls(ed: &mut Editor, hs: &[f64]) -> Vec<i64> {
        let (lv, wt): (i64, i64) = ed
            .model
            .sql()
            .query_row("SELECT level, wtype FROM wall LIMIT 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        hs.iter()
            .enumerate()
            .map(|(i, h)| {
                ed.model
                    .add_wall(&WallSpec {
                        a: (20.0, i as f64 * 2.0),
                        b: (24.0, i as f64 * 2.0),
                        level: lv,
                        base_offset: 0.0,
                        height: *h,
                        wall_type: wt,
                        offset0: None,
                    })
                    .unwrap()
            })
            .collect()
    }

    fn heights(ed: &Editor, ids: &[i64]) -> Vec<f64> {
        let r = geom_props::rows()
            .into_iter()
            .find(|r| r.kind == "wall" && r.key == "geom.height")
            .unwrap();
        ids.iter()
            .map(|i| (r.get)(&ed.model, *i).unwrap())
            .collect()
    }

    fn pick_height(ed: &mut Editor, ids: &[i64]) -> Multi {
        ed.selection = ids.iter().map(|i| Sel::Wall(*i)).collect();
        build(ed)
    }

    #[test]
    fn different_heights_vary_edit_sets_both_one_undo_restores() {
        let mut fx = fixture();
        let ids = walls(&mut fx.ed, &[2.5, 3.5]);
        let m = pick_height(&mut fx.ed, &ids);
        let h = m.rows.iter().find(|r| r.key == "geom.height").unwrap();
        assert!(matches!(h.cell, Cell::Varies));
        assert_eq!(apply(&mut fx.ed, h, 4.0).unwrap(), 2);
        assert_eq!(heights(&fx.ed, &ids), vec![4.0, 4.0]);
        let m = build(&fx.ed);
        let h = m.rows.iter().find(|r| r.key == "geom.height").unwrap();
        assert!(matches!(h.cell, Cell::Same(v) if (v - 4.0).abs() < 1e-9));
        fx.ed.undo();
        assert_eq!(heights(&fx.ed, &ids), vec![2.5, 3.5]);
    }

    #[test]
    fn mixed_kinds_show_only_shared_rows() {
        let mut fx = fixture();
        let stair = crate::elem::by_table("stair")
            .unwrap()
            .sel(fx.ids["stair"])
            .unwrap();
        fx.ed.selection = vec![Sel::Wall(fx.ids["wall"]), stair];
        let m = build(&fx.ed);
        let all = geom_props::rows();
        for r in &m.rows {
            for kind in ["wall", "stair"] {
                let both = all.iter().any(|x| x.kind == kind && x.key == r.key);
                assert!(both, "{} is not on every kind", r.key);
            }
            assert_eq!(r.per.len(), 2);
        }
        assert!(m.hidden > 0, "rows only on one kind are counted");
        assert!(!m.rows.iter().any(|r| r.key == "geom.landing"));
    }

    #[test]
    fn a_refused_row_leaves_all_unchanged() {
        let mut fx = fixture();
        let ids = walls(&mut fx.ed, &[2.5, 3.5]);
        let m = pick_height(&mut fx.ed, &ids);
        let h = m.rows.iter().find(|r| r.key == "geom.height").unwrap();
        let head = fx.ed.model.change_head();
        assert!(apply(&mut fx.ed, h, -1.0).is_err());
        assert_eq!(fx.ed.model.change_head(), head);
        assert_eq!(heights(&fx.ed, &ids), vec![2.5, 3.5]);
    }
}
