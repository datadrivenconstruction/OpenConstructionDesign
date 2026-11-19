// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Joints panel of a component instance: the assembly's degrees of freedom (rank deficit of
//! the joint equations), its joints with drive fields inside their limits, limit edits and a
//! new joint to the ground (one instance selected) or to the other selected instance.

use crate::editor::{Editor, OpFn};
use crate::units::{display_length, parse_typed_signed, ui_decimal_mark, unit_label, Units};
use crate::{tr, ui_kit};
use ddc_geom::joint::JFrame;
use ddc_model::joints::{Coord, JointKind};
use ddc_model::{AsmDof, Error, JointRow, JointSpec, Model};
use eframe::egui;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Vertical,
    X,
    Y,
}

impl Axis {
    pub const ALL: [Axis; 3] = [Axis::Vertical, Axis::X, Axis::Y];

    pub fn dir(self) -> [f64; 3] {
        match self {
            Axis::Vertical => [0.0, 0.0, 1.0],
            Axis::X => [1.0, 0.0, 0.0],
            Axis::Y => [0.0, 1.0, 0.0],
        }
    }

    fn label(self) -> String {
        match self {
            Axis::Vertical => tr("features.axis.z", &[]),
            Axis::X => tr("features.axis.x", &[]),
            Axis::Y => tr("features.axis.y", &[]),
        }
    }
}

fn kind_label(k: JointKind) -> String {
    tr(&format!("features.joint.{}", k.name()), &[])
}

/// A joint at the instances' anchors: on `a`'s origin for the ground, midway between two.
pub fn new_joint(m: &Model, kind: JointKind, a: i64, b: Option<i64>, axis: Axis) -> Result<JointSpec, Error> {
    let pa = m.joint_anchor(a)?;
    let o = match b {
        Some(b) => {
            let pb = m.joint_anchor(b)?;
            [0, 1, 2].map(|k| (pa[k] + pb[k]) / 2.0)
        }
        None => pa,
    };
    let frame = JFrame::new(o, axis.dir(), [1.0, 0.0, 0.0])
        .ok_or_else(|| Error::Invalid("joint axis".into()))?;
    Ok(JointSpec {
        name: String::new(),
        kind,
        a,
        b,
        frame,
        rot: None,
        slide: None,
    })
}

/// Limits typed as `low .. high` (degrees for angles, display length for slides); empty = free.
pub fn parse_limits(s: &str, angle: bool, u: Units) -> Option<Option<(f64, f64)>> {
    if s.trim().is_empty() {
        return Some(None);
    }
    let (lo, hi) = s.split_once("..")?;
    let one = |t: &str| -> Option<f64> {
        if angle {
            t.trim().parse::<f64>().ok().map(f64::to_radians)
        } else {
            // a slide limit may lie below the joint origin: signed, unlike a length field
            parse_typed_signed(t.trim(), u, ui_decimal_mark()).ok()
        }
    };
    Some(Some((one(lo)?, one(hi)?)))
}

fn limits_text(l: Option<(f64, f64)>, angle: bool, u: Units) -> String {
    l.map_or(String::new(), |(lo, hi)| {
        if angle {
            format!("{:.2} .. {:.2}", lo.to_degrees(), hi.to_degrees())
        } else {
            format!("{} .. {}", display_length(lo, u), display_length(hi, u))
        }
    })
}

#[derive(Clone, Debug, PartialEq)]
enum Act {
    Drive(i64, Coord, f64),
    Limits(i64, String, Option<(f64, f64)>, Option<(f64, f64)>),
    Delete(i64),
    Add(JointKind, Option<i64>, Axis),
}

fn act(ed: &mut Editor, id: i64, a: Act) {
    let op = OpFn(Arc::new(move |m: &mut Model| match &a {
        Act::Drive(j, c, v) => m.joint_drive(*j, *c, *v).map(|_| ()),
        Act::Limits(j, n, r, s) => m.joint_edit(*j, n, *r, *s),
        Act::Delete(j) => m.joint_delete(*j),
        Act::Add(k, b, ax) => {
            let s = new_joint(m, *k, id, *b, *ax)?;
            m.joint_add(&s).map(|_| ())
        }
    }));
    ed.status = Some(match ed.run_op(op) {
        Ok(()) => tr("geom.changed", &[]),
        Err(e) => tr("geom.not_changed", &[("e", e.to_string().as_str())]),
    });
}

type Values = Vec<(JointRow, Option<f64>, Option<f64>)>;

#[derive(Clone)]
struct Seen {
    dof: Result<AsmDof, String>,
    joints: Values,
}

fn seen(ui: &egui::Ui, ed: &Editor, id: i64) -> Seen {
    let key = egui::Id::new(("features.joints", id));
    let rev = ed.revision;
    if let Some(s) = ui
        .data(|d| d.get_temp::<(u64, Seen)>(key))
        .filter(|c| c.0 == rev)
    {
        return s.1;
    }
    let m = &ed.model;
    let joints = m
        .joints_of(id)
        .unwrap_or_default()
        .into_iter()
        .map(|j| {
            let a = m.joint_value(j.id, Coord::Angle).ok().flatten();
            let s = m.joint_value(j.id, Coord::Slide).ok().flatten();
            (j, a, s)
        })
        .collect();
    let s = Seen {
        dof: m.assembly_dof(id).map_err(|e| e.to_string()),
        joints,
    };
    ui.data_mut(|d| d.insert_temp(key, (rev, s.clone())));
    s
}

fn temp<T: Clone + Send + Sync + 'static>(ui: &egui::Ui, key: egui::Id, rev: u64, init: T) -> T {
    ui.data(|d| d.get_temp::<(u64, T)>(key))
        .filter(|b| b.0 == rev)
        .map_or(init, |b| b.1)
}

pub fn panel(ui: &mut egui::Ui, ed: &mut Editor, id: i64) {
    if !ui_kit::heading_keyed(ui, "features.joints", &tr("features.joints", &[]), None) {
        return;
    }
    let s = seen(ui, ed, id);
    let (u, rev) = (ed.tool.units, ed.revision);
    let mut todo = None;
    ui_kit::props(ui, &format!("param1.joints.{id}"), |ui| {
        match &s.dof {
            Ok(d) => {
                ui_kit::kv_num(ui, &tr("features.dof", &[]), d.dof.to_string());
                ui_kit::kv_num(ui, &tr("features.rank", &[]), format!("{} / {}", d.rank, d.equations));
                if !d.redundant.is_empty() {
                    ui_kit::kv(ui, &tr("features.redundant", &[]), d.redundant.len().to_string());
                }
                if d.residual > 1e-6 {
                    ui_kit::kv(ui, &tr("features.broken", &[]), display_length(d.residual, u));
                }
            }
            Err(e) => ui_kit::kv(ui, &tr("features.dof", &[]), e.clone()),
        }
        for (j, ang, sl) in &s.joints {
            joint_rows(ui, id, j, *ang, *sl, u, rev, &mut todo);
        }
    });
    add_row(ui, ed, id, &mut todo);
    if let Some(a) = todo {
        act(ed, id, a);
    }
}

#[allow(clippy::too_many_arguments)]
fn joint_rows(
    ui: &mut egui::Ui,
    id: i64,
    j: &JointRow,
    ang: Option<f64>,
    sl: Option<f64>,
    u: Units,
    rev: u64,
    todo: &mut Option<Act>,
) {
    let partner = match j.b {
        None => tr("features.ground", &[]),
        Some(b) => format!("#{}", if b == id { j.a } else { b }),
    };
    ui_kit::kv_widget(ui, &j.name, |ui| {
        ui.horizontal(|ui| {
            ui.label(format!("{} \u{b7} {partner}", kind_label(j.kind)));
            if ddc_ui::widgets::Button::quiet("\u{d7}")
                .small()
                .show(ui, &ddc_ui::theme::of(ui.ctx()))
                .on_hover_text(tr("features.joint_delete", &[]))
                .clicked()
            {
                *todo = Some(Act::Delete(j.id));
            }
        });
    });
    if let Some(a) = ang {
        let key = egui::Id::new(("param1.jangle", j.id));
        let mut v = temp(ui, key, rev, a.to_degrees());
        let (lo, hi) = j.rot.map_or((-360.0, 360.0), |l| (l.0.to_degrees(), l.1.to_degrees()));
        let done = ui_kit::kv_field(ui, key, &tr("features.angle", &[]), &mut v, ui_kit::NumKind::DEG, lo, hi, "\u{b0}");
        ui.data_mut(|d| d.insert_temp(key, (rev, v)));
        if done && (v - a.to_degrees()).abs() > 1e-9 {
            *todo = Some(Act::Drive(j.id, Coord::Angle, v.to_radians()));
        }
    }
    if let Some(x) = sl {
        let key = egui::Id::new(("param1.jslide", j.id));
        let mut v = temp(ui, key, rev, x);
        let (lo, hi) = j.slide.unwrap_or((-1000.0, 1000.0));
        let done = ui_kit::kv_field(ui, key, &tr("features.slide", &[]), &mut v, ui_kit::NumKind::Len(u), lo, hi, unit_label(u));
        ui.data_mut(|d| d.insert_temp(key, (rev, v)));
        if done && (v - x).abs() > 1e-9 {
            *todo = Some(Act::Drive(j.id, Coord::Slide, v));
        }
    }
    for (angle, on, cur) in [(true, j.kind.turns(), j.rot), (false, j.kind.slides(), j.slide)] {
        if !on {
            continue;
        }
        let key = egui::Id::new(("param1.jlim", j.id, angle));
        let mut b = temp(ui, key, rev, limits_text(cur, angle, u));
        let label = tr(if angle { "features.rot_limits" } else { "features.slide_limits" }, &[]);
        if let Some(t) = ui_kit::kv_edit(ui, &label, &mut b, "", &tr("features.limits_tip", &[])) {
            match parse_limits(&t, angle, u) {
                Some(l) => {
                    let (r, s) = if angle { (l, j.slide) } else { (j.rot, l) };
                    *todo = Some(Act::Limits(j.id, j.name.clone(), r, s));
                }
                None => b = limits_text(cur, angle, u),
            }
        }
        ui.data_mut(|d| d.insert_temp(key, (rev, b)));
    }
}

fn add_row(ui: &mut egui::Ui, ed: &Editor, id: i64, todo: &mut Option<Act>) {
    let others: Vec<i64> = ed
        .selection
        .iter()
        .filter_map(|s| super::migrated(*s))
        .filter(|(f, c)| f.table == "comp" && *c != id)
        .map(|(_, c)| c)
        .collect();
    let partner = (others.len() == 1).then(|| others[0]);
    let kk = egui::Id::new(("features.new_kind", id));
    let ak = egui::Id::new(("features.new_axis", id));
    let mut kind = ui.data(|d| d.get_temp::<JointKind>(kk)).unwrap_or(JointKind::Revolute);
    let mut axis = ui.data(|d| d.get_temp::<Axis>(ak)).unwrap_or(Axis::Vertical);
    let th = ddc_ui::theme::of(ui.ctx());
    ui_kit::props(ui, &format!("param1.new_joint.{id}"), |ui| {
        ui_kit::kv_widget(ui, &tr("features.new_kind", &[]), |ui| {
            let labels: Vec<String> = JointKind::ALL.iter().map(|k| kind_label(*k)).collect();
            let items: Vec<&str> = labels.iter().map(String::as_str).collect();
            let mut sel = JointKind::ALL.iter().position(|k| *k == kind).unwrap_or(0);
            ddc_ui::widgets::dropdown(ui, &th, kk, &mut sel, &items, None);
            kind = JointKind::ALL[sel];
        });
        ui_kit::kv_widget(ui, &tr("features.new_axis", &[]), |ui| {
            let labels: Vec<String> = Axis::ALL.iter().map(|a| a.label()).collect();
            let items: Vec<&str> = labels.iter().map(String::as_str).collect();
            let mut sel = Axis::ALL.iter().position(|a| *a == axis).unwrap_or(0);
            ddc_ui::widgets::dropdown(ui, &th, ak, &mut sel, &items, None);
            axis = Axis::ALL[sel];
        });
        let to = partner.map_or_else(|| tr("features.ground", &[]), |b| format!("#{b}"));
        ui_kit::kv(ui, &tr("features.new_partner", &[]), to);
    });
    ui.data_mut(|d| {
        d.insert_temp(kk, kind);
        d.insert_temp(ak, axis);
    });
    if ddc_ui::widgets::Button::secondary(&tr("features.add_joint", &[]))
        .show(ui, &th)
        .on_hover_text(tr("features.add_joint_tip", &[]))
        .clicked()
    {
        *todo = Some(Act::Add(kind, partner, axis));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::CompSpec;

    #[test]
    fn limits_text_round_trips() {
        let u = Units::Metric;
        assert_eq!(parse_limits("", true, u), Some(None));
        let r = parse_limits("-90 .. 45", true, u).unwrap().unwrap();
        assert!((r.0 + std::f64::consts::FRAC_PI_2).abs() < 1e-12);
        assert_eq!(parse_limits(&limits_text(Some(r), true, u), true, u).map(|l| l.is_some()), Some(true));
        assert_eq!(parse_limits("10", true, u), None);
        let s = parse_limits(&limits_text(Some((-0.5, 1.25)), false, u), false, u).unwrap().unwrap();
        assert!((s.0 + 0.5).abs() < 1e-9 && (s.1 - 1.25).abs() < 1e-9, "{s:?}");
    }

    #[test]
    fn new_joint_sits_between_the_instances() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 2.0).unwrap();
        let fam = m.load_library_family("Sink").unwrap();
        let ct = m.comp_types(fam).unwrap()[0].0;
        let a = m.add_component(&CompSpec::at(lv, ct, 0.0, 0.0)).unwrap();
        let b = m.add_component(&CompSpec::at(lv, ct, 2.0, 4.0)).unwrap();
        let s = new_joint(&m, JointKind::Revolute, a, Some(b), Axis::Vertical).unwrap();
        assert_eq!(s.frame.o, [1.0, 2.0, 2.0]);
        let j = m.joint_add(&s).unwrap();
        assert_eq!(m.assembly_dof(a).unwrap().dof, 7);
        m.joint_drive(j, Coord::Angle, 0.5).unwrap();
        let g = new_joint(&m, JointKind::Rigid, a, None, Axis::X).unwrap();
        assert_eq!(g.frame.z, [1.0, 0.0, 0.0]);
    }
}
