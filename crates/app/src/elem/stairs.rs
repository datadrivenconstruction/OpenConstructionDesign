//! Stairs and railings on the element path: plan outlines for selection, one-step delete of a
//! mixed selection, and the properties panel. Rows live in `geom_props`.

use crate::editor::{structx, Editor, OpFn, Sel};
use crate::units::display_length;
use crate::{geom_props, tr, ui_kit};
use ddc_model::{Model, RailingHost};
use std::sync::Arc;

type Fp = Vec<(&'static str, i64, Vec<Vec<(f64, f64)>>)>;

/// Plan cut height over the level, m (as the plan layer).
const CUT: f64 = 1.2;
/// Half side of the mark of an element whose body does not build, m.
const MARK: f64 = 0.25;

fn ids(m: &Model, sql: &str, level: i64) -> Vec<i64> {
    let Ok(mut st) = m.sql().prepare(sql) else {
        return vec![];
    };
    st.query_map([level], |r| r.get(0))
        .map(|it| it.flatten().collect())
        .unwrap_or_default()
}

fn mark(x: f64, y: f64) -> Vec<(f64, f64)> {
    vec![
        (x - MARK, y - MARK),
        (x + MARK, y - MARK),
        (x + MARK, y + MARK),
        (x - MARK, y + MARK),
    ]
}

/// A path as a closed ring that doubles back, so its edges are the path and its area is zero.
pub(crate) fn path_ring(pts: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut ring = pts.to_vec();
    if pts.len() > 2 {
        ring.extend(pts[1..pts.len() - 1].iter().rev().copied());
    }
    ring
}

/// Outlines for plan selection: a stair on both of its levels, a railing on its own.
/// A stair that does not fit its levels stays selectable by a mark at its origin.
pub fn footprints(m: &Model, level: i64) -> Fp {
    let mut out: Fp = vec![];
    for id in ids(m, "SELECT id FROM stair WHERE level=?1 OR top_level=?1", level) {
        let mut rings: Vec<Vec<(f64, f64)>> = m
            .stair_plan(id, level, CUT)
            .map(|p| {
                p.outline
                    .iter()
                    .filter(|r| r.len() >= 3)
                    .map(|r| r.iter().map(|q| (q.x, q.y)).collect())
                    .collect()
            })
            .unwrap_or_default();
        if rings.is_empty() {
            let Ok(s) = m.stair(id) else { continue };
            rings.push(mark(s.origin.0, s.origin.1));
        }
        out.push(("stair", id, rings));
    }
    for id in ids(m, "SELECT id FROM railing WHERE level=?1", level) {
        let mut pts: Vec<(f64, f64)> = m
            .railing_layout(id)
            .map(|l| l.rail.iter().map(|q| (q.x, q.y)).collect())
            .unwrap_or_default();
        if pts.len() < 2 {
            pts = m
                .railing_path(id)
                .unwrap_or_default()
                .iter()
                .map(|q| (q.0, q.1))
                .collect();
        }
        pts.dedup();
        let ring = match pts.len() {
            0 => continue,
            1 => mark(pts[0].0, pts[0].1),
            _ => path_ring(&pts),
        };
        out.push(("railing", id, vec![ring]));
    }
    out
}

fn is_stair_or_railing(s: &Sel) -> bool {
    matches!(s, Sel::Ext(c, _) if matches!(structx::kind_of(*c), Some("stair" | "railing")))
}

/// "2 Stairs, Railing": what a delete took, by family.
fn what(items: &[(String, i64)]) -> String {
    let mut by: Vec<(&str, usize)> = vec![];
    for (t, _) in items {
        match by.iter_mut().find(|b| b.0 == t.as_str()) {
            Some(b) => b.1 += 1,
            None => by.push((t.as_str(), 1)),
        }
    }
    by.iter()
        .map(|(t, n)| {
            if *n == 1 {
                tr(&format!("elem.{t}.one"), &[])
            } else {
                format!("{n} {}", tr(&format!("elem.{t}.many"), &[]))
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Items of the selection with the railings hosted on a deleted stair, railings first so a stair
/// is never blocked by its own railing. Returns the list and how many railings were added.
pub(crate) fn delete_list(m: &Model, items: Vec<(String, i64)>) -> (Vec<(String, i64)>, usize) {
    let mut extra: Vec<(String, i64)> = vec![];
    for (t, id) in &items {
        if t != "stair" {
            continue;
        }
        let Ok(mut st) = m.sql().prepare("SELECT id FROM railing WHERE stair=?1") else {
            continue;
        };
        let hosted: Vec<i64> = st
            .query_map([*id], |r| r.get(0))
            .map(|it| it.flatten().collect())
            .unwrap_or_default();
        for r in hosted {
            let pair = ("railing".to_string(), r);
            if !items.contains(&pair) && !extra.contains(&pair) {
                extra.push(pair);
            }
        }
    }
    let added = extra.len();
    let (mut rails, rest): (Vec<_>, Vec<_>) = extra
        .into_iter()
        .chain(items)
        .partition(|(t, _)| t == "railing");
    rails.extend(rest);
    (rails, added)
}

/// Delete when a stair or railing is selected: the whole selection in one undo step, also mixed
/// with walls. false: none selected, the other paths run.
pub fn delete_selected(ed: &mut Editor) -> bool {
    if !ed.selection.iter().any(is_stair_or_railing) {
        return false;
    }
    let items: Vec<(String, i64)> = ed
        .selection
        .iter()
        .filter(|s| !matches!(s, Sel::Panel(..) | Sel::Mass(_) | Sel::Anno(_)))
        .filter_map(|s| crate::clip_tools::sel_item(*s))
        .collect();
    let shown = what(&items);
    let (list, hosted) = delete_list(&ed.model, items);
    let op = OpFn(Arc::new(move |m: &mut Model| {
        let refs: Vec<(&str, i64)> = list.iter().map(|(t, i)| (t.as_str(), *i)).collect();
        m.delete_items("elem_delete", &refs)
    }));
    ed.status = Some(match ed.run_op(op) {
        Ok(()) => {
            ed.selection
                .retain(|s| crate::clip_tools::sel_item(*s).is_none());
            ed.grip.focus.clear();
            let mut t = tr("elem.deleted_what", &[("what", shown.as_str())]);
            if hosted > 0 {
                let n = hosted.to_string();
                t = format!("{t} {}", tr("stair.deleted_with", &[("n", n.as_str())]));
            }
            t
        }
        Err(e) => tr("elem.not_deleted_what", &[("what", e.to_string().as_str())]),
    });
    true
}

fn selected(ed: &Editor) -> Option<(&'static str, i64)> {
    ed.selection.iter().find_map(|s| match s {
        Sel::Ext(c, id) => match structx::kind_of(*c) {
            Some("stair") => Some(("stair", *id)),
            Some("railing") => Some(("railing", *id)),
            _ => None,
        },
        _ => None,
    })
}

fn stair_info(ui: &mut egui::Ui, ed: &Editor, id: i64) {
    let u = ed.tool.units;
    match ed.model.stair_quantities(id) {
        Ok(q) => ui_kit::figures(
            ui,
            &[
                (tr("stair.risers", &[]), q.risers.to_string()),
                (tr("stair.riser", &[]), display_length(q.riser, u)),
                (tr("stair.comfort", &[]), display_length(q.comfort, u)),
            ],
        ),
        Err(_) => ui_kit::hint(ui, tr("stair.broken", &[])),
    }
}

fn railing_info(ui: &mut egui::Ui, ed: &Editor, id: i64) {
    let Ok(s) = ed.model.railing(id) else { return };
    let host = match s.host {
        RailingHost::Stair { stair, .. } => {
            tr("railing.host_stair", &[("id", stair.to_string().as_str())])
        }
        RailingHost::Path(_) => tr("railing.host_path", &[]),
    };
    ui_kit::hint(ui, host);
    if let Ok(q) = ed.model.railing_quantities(id) {
        let u = ed.tool.units;
        ui_kit::figures(ui, &[(tr("railing.length", &[]), display_length(q.length, u))]);
    }
}

/// Drops a typed tread: the 2R + T rule sets it again.
fn tread_by_rule(ed: &mut Editor, id: i64) {
    let op = OpFn(Arc::new(move |m: &mut Model| {
        let mut s = m.stair(id)?;
        s.tread = None;
        m.update_stair(id, &s)
    }));
    ed.status = Some(match ed.run_op(op) {
        Ok(()) => tr("geom.changed", &[]),
        Err(e) => tr("geom.not_changed", &[("e", e.to_string().as_str())]),
    });
}

/// Properties of the selected stair or railing; false when none is selected.
pub fn props(ui: &mut egui::Ui, ed: &mut Editor) -> bool {
    let Some((kind, id)) = selected(ed) else {
        return false;
    };
    let title = tr(
        if kind == "stair" {
            "geom.title.stair"
        } else {
            "geom.title.railing"
        },
        &[],
    );
    ui_kit::element_header(ui, kind, &title, "", id);
    if kind == "stair" {
        stair_info(ui, ed, id);
    } else {
        railing_info(ui, ed, id);
    }
    geom_props::show(ui, ed, kind, id);
    let typed = kind == "stair" && ed.model.stair(id).is_ok_and(|s| s.tread.is_some());
    if typed
        && ui
            .button(tr("stair.tread_auto", &[]))
            .on_hover_text(tr("stair.tread_auto_tip", &[]))
            .clicked()
    {
        tread_by_rule(ed, id);
    }
    true
}

#[cfg(test)]
mod tests;
