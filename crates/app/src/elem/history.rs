// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Feature history of a parametric mass: named parameters with formula cells, the feature
//! list with reorder, suppress, roll-back marker and argument edits. Every change is one
//! model edit (one undo step); a refused one leaves the mass as it was and says why.
//! "Timeline to family" (`mass.timeline_family`) turns the timeline into a component family
//! whose parameters are the timeline's, read back from the stored family to confirm.

use crate::editor::{Editor, OpFn, Sel};
use crate::shell::EditorApp;
use crate::units::display_length;
use crate::{tr, ui_kit};
use ddc_geom::component::expr::{Q, ANGLE, LENGTH};
use ddc_geom::component::Category;
use ddc_geom::feature::{standard, FeatureOp, Kind as FKind, Kind0, NodeId, Status, Timeline};
use ddc_model::feature_tl::{feature_args, modifier_kinds};
use ddc_model::{Error, Model};
use ddc_ui::widgets::formula::{formula, Check};
use eframe::egui;
use std::sync::Arc;

const CELL_W: f32 = 150.0;

/// One history edit.
#[derive(Clone, Debug, PartialEq)]
pub enum TlEdit {
    /// Plain form history to an editable timeline.
    Convert,
    Param(String, String),
    DropParam(String),
    Arg(NodeId, String, String),
    Suppress(NodeId, bool),
    Move(NodeId, usize),
    Marker(Option<NodeId>),
    Remove(NodeId),
    /// A registry modifier (`modifier_kinds`) on the body of a feature, default arguments.
    Modify(NodeId, String),
}

/// Ribbon and palette command: the selected mass's timeline as a component family.
pub const TO_FAMILY: &str = "mass.timeline_family";

/// Modifiers the history menu offers: every registry kind, and whether it can run without
/// picked faces (the others are added from the Forms tab, where faces are picked in 3D).
pub fn modifier_menu() -> Vec<(&'static str, bool)> {
    modifier_kinds()
        .into_iter()
        .map(|k| (k, standard().get(k).is_some_and(|o| o.faces().0 == 0)))
        .collect()
}

/// The mass name, or that name with the first free number, as a family name.
pub fn family_name(m: &Model, base: &str) -> Result<String, Error> {
    let base = match base.trim() {
        "" => "Mass",
        b => b,
    };
    if m.family_id(base)?.is_none() {
        return Ok(base.to_string());
    }
    for k in 2.. {
        let n = format!("{base} {k}");
        if m.family_id(&n)?.is_none() {
            return Ok(n);
        }
    }
    unreachable!("an unbounded range always yields a free name")
}

/// The mass's timeline as a new generic family named `name` (one undo step); returns the
/// family and the parameter count read back from the stored family's own timeline.
pub fn to_family(m: &mut Model, id: i64, name: &str) -> Result<(i64, usize), Error> {
    let t = m
        .mass_timeline(id)?
        .ok_or_else(|| Error::Invalid(format!("mass {id} has no timeline")))?;
    let fam = m.load_timeline_family(name, Category::Generic, &t)?;
    let back = m
        .family_timeline(fam)?
        .ok_or_else(|| Error::Invalid(format!("family {name} lost its timeline")))?;
    if back.params != t.params {
        return Err(Error::Invalid(format!("family {name} does not carry the timeline parameters")));
    }
    Ok((fam, back.params.len()))
}

/// Runs "Timeline to family" for mass `id` as one undo step and reports in the status bar.
pub fn run_to_family(ed: &mut Editor, id: i64) {
    let name = match ed.model.mass(id).and_then(|s| family_name(&ed.model, &s.name)) {
        Ok(n) => n,
        Err(e) => {
            ed.status = Some(tr("mass_family.refused", &[("e", e.to_string().as_str())]));
            return;
        }
    };
    let n = Arc::new(std::sync::Mutex::new(0usize));
    let (nm, nn) = (name.clone(), n.clone());
    let op = OpFn(Arc::new(move |m: &mut Model| {
        let (_, k) = to_family(m, id, &nm)?;
        *nn.lock().unwrap_or_else(|p| p.into_inner()) = k;
        Ok(())
    }));
    ed.status = Some(match ed.run_op(op) {
        Ok(()) => {
            let k = *n.lock().unwrap_or_else(|p| p.into_inner());
            tr("mass_family.done", &[("name", name.as_str()), ("n", k.to_string().as_str())])
        }
        Err(e) => tr("mass_family.refused", &[("e", e.to_string().as_str())]),
    });
}

fn h_to_family(app: &mut EditorApp) {
    let mass = app.ed.selection.iter().find_map(|s| match s {
        Sel::Mass(id) => Some(*id),
        _ => None,
    });
    match mass {
        Some(id) => run_to_family(&mut app.ed, id),
        None => app.ed.status = Some(tr("mass_family.no_mass", &[])),
    }
}

pub fn register(app: &mut EditorApp) {
    app.register_command(TO_FAMILY, h_to_family);
}

pub fn apply(m: &mut Model, id: i64, e: &TlEdit) -> Result<(), Error> {
    let reg = standard();
    match e {
        TlEdit::Convert => m.mass_to_timeline(id),
        TlEdit::Param(n, x) => m.edit_mass_timeline(id, |t| t.set_param(n, x)),
        TlEdit::DropParam(n) => m.edit_mass_timeline(id, |t| t.remove_param(n, reg)),
        TlEdit::Arg(node, k, x) => m.edit_mass_timeline(id, |t| t.set_arg(*node, k, x, reg)),
        TlEdit::Suppress(node, on) => m.edit_mass_timeline(id, |t| t.suppress(*node, *on)),
        TlEdit::Move(node, to) => m.edit_mass_timeline(id, |t| t.move_to(*node, *to)),
        TlEdit::Marker(at) => m.edit_mass_timeline(id, |t| t.set_marker(*at)),
        TlEdit::Remove(node) => m.edit_mass_timeline(id, |t| t.remove(*node).map(|_| ())),
        TlEdit::Modify(node, op) => m.edit_mass_timeline(id, |t| {
            let kind = FKind::Modify {
                op: op.clone(),
                input: *node,
                faces: vec![],
                args: vec![],
            };
            t.add("", kind).map(|_| ())
        }),
    }
}

fn commit(ed: &mut Editor, id: i64, e: TlEdit) {
    let op = OpFn(Arc::new(move |m: &mut Model| apply(m, id, &e)));
    ed.status = Some(match ed.run_op(op) {
        Ok(()) => tr("geom.changed", &[]),
        Err(e) => tr("geom.not_changed", &[("e", e.to_string().as_str())]),
    });
}

/// Timeline and per-feature state, read once per model revision.
#[derive(Clone)]
struct Seen {
    t: Option<Timeline>,
    state: Vec<(NodeId, Status)>,
}

fn seen(ui: &egui::Ui, ed: &Editor, id: i64) -> Seen {
    let key = egui::Id::new(("features.history", id));
    let rev = ed.revision;
    if let Some(s) = ui
        .data(|d| d.get_temp::<(u64, Seen)>(key))
        .filter(|c| c.0 == rev)
    {
        return s.1;
    }
    let t = ed.model.mass_timeline(id).ok().flatten();
    let state = match ed.model.mass_timeline_build(id) {
        Ok(Some(r)) => r.nodes.iter().map(|n| (n.id, n.status.clone())).collect(),
        _ => vec![],
    };
    let s = Seen { t, state };
    ui.data_mut(|d| d.insert_temp(key, (rev, s.clone())));
    s
}

pub fn show_q(q: Q, u: crate::units::Units) -> String {
    if q.d == LENGTH {
        display_length(q.v, u)
    } else if q.d == ANGLE {
        format!("{:.2}\u{b0}", q.v.to_degrees())
    } else {
        format!("{:.4}", q.v)
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

/// Preview of a parameter's text: its value with the change applied, or why it is refused.
pub fn param_check(t: &Timeline, name: &str, text: &str, u: crate::units::Units) -> Check {
    let mut c = t.clone();
    match c.set_param(name, text).and_then(|_| c.values()) {
        Ok(v) => Check::Ok(v.get(name).map(|q| show_q(*q, u)).unwrap_or_default()),
        Err(e) => Check::Err {
            pos: None,
            msg: e.to_string(),
        },
    }
}

pub fn panel(ui: &mut egui::Ui, ed: &mut Editor, id: i64) {
    let th = ddc_ui::theme::of(ui.ctx());
    let s = seen(ui, ed, id);
    let Some(t) = s.t else {
        if ui_kit::heading(ui, &tr("features.history", &[])) {
            ui.colored_label(ui_kit::weak(ui), tr("features.plain", &[]));
            if ddc_ui::widgets::Button::secondary(&tr("features.convert", &[])).show(ui, &th).clicked() {
                commit(ed, id, TlEdit::Convert);
            }
        }
        return;
    };
    let mut todo = None;
    params(ui, ed, id, &t, &mut todo);
    features(ui, ed, id, &t, &s.state, &mut todo);
    let fam = ddc_ui::widgets::Button::secondary(&tr("cmd.mass.timeline_family", &[]))
        .show(ui, &th)
        .on_hover_text(tr("cmd.mass.timeline_family.desc", &[]))
        .clicked();
    if let Some(e) = todo {
        commit(ed, id, e);
    } else if fam {
        run_to_family(ed, id);
    }
}

fn buf(ui: &egui::Ui, key: egui::Id, rev: u64, init: &str) -> String {
    ui.data(|d| d.get_temp::<(u64, String)>(key))
        .filter(|b| b.0 == rev)
        .map_or_else(|| init.to_string(), |b| b.1)
}

fn params(ui: &mut egui::Ui, ed: &Editor, id: i64, t: &Timeline, todo: &mut Option<TlEdit>) {
    if !ui_kit::heading_keyed(ui, "features.params", &tr("features.params", &[]), Some(t.params.len())) {
        return;
    }
    let (u, rev) = (ed.tool.units, ed.revision);
    let th = ddc_ui::theme::of(ui.ctx());
    ui_kit::props(ui, &format!("param1.params.{id}"), |ui| {
        for p in &t.params {
            let key = egui::Id::new(("param1.param", id, p.name.as_str()));
            let mut b = buf(ui, key, rev, &p.expr);
            let check = param_check(t, &p.name, &b, u);
            ui_kit::kv_widget(ui, &p.name, |ui| {
                ui.horizontal(|ui| {
                    let r = formula(ui, &th, key, &mut b, CELL_W, &check);
                    if ui_kit::track_field(ui, &r, &mut b) == ui_kit::FieldEnd::Apply
                        && b != p.expr
                        && !check.is_err()
                    {
                        *todo = Some(TlEdit::Param(p.name.clone(), b.clone()));
                    }
                    if ddc_ui::widgets::Button::quiet("\u{d7}")
                        .small()
                        .show(ui, &th)
                        .on_hover_text(tr("features.drop_param", &[]))
                        .clicked()
                    {
                        *todo = Some(TlEdit::DropParam(p.name.clone()));
                    }
                });
            });
            ui.data_mut(|d| d.insert_temp(key, (rev, b)));
        }
        let key = egui::Id::new(("features.new_param", id));
        let mut b = buf(ui, key, rev, "");
        let tip = tr("features.new_param_tip", &[]);
        if let Some(text) = ui_kit::kv_edit(ui, &tr("features.new_param", &[]), &mut b, "", &tip) {
            match text.split_once('=') {
                Some((n, x)) if !n.trim().is_empty() && !x.trim().is_empty() => {
                    *todo = Some(TlEdit::Param(n.trim().into(), x.trim().into()));
                    b.clear();
                }
                _ => {}
            }
        }
        ui.data_mut(|d| d.insert_temp(key, (rev, b)));
    });
}

fn status_mark(s: Option<&Status>) -> (&'static str, String) {
    match s {
        Some(Status::Built) | Some(Status::Reused) => ("\u{2713}", tr("features.st.ok", &[])),
        Some(Status::Suppressed) => ("\u{2013}", tr("features.st.suppressed", &[])),
        Some(Status::Skipped(_)) => ("?", tr("features.st.skipped", &[])),
        Some(Status::Failed(m)) => ("!", tr("features.st.failed", &[("e", m.as_str())])),
        Some(Status::Rolled) | None => ("\u{22ef}", tr("features.st.rolled", &[])),
    }
}

const KNOWN_ARGS: &[&str] = &[
    "offset", "cx", "cy", "w", "h", "r", "dist", "start", "angle", "dx", "dy", "dz", "turn", "d",
    "t", "seg", "through", "round",
];

fn arg_label(key: &str) -> String {
    if KNOWN_ARGS.contains(&key) {
        tr(&format!("features.arg.{key}"), &[])
    } else {
        key.to_string()
    }
}

const KINDS: &[&str] = &[
    "sketch", "extrude", "revolve", "sweep", "loft", "boolean", "move", "chamfer", "fillet",
    "shell", "offset", "push_pull", "push_region", "blend", "draft",
];

fn kind_label(kind: &str) -> String {
    if KINDS.contains(&kind) {
        tr(&format!("features.kind.{kind}"), &[])
    } else {
        kind.to_string()
    }
}

fn features(
    ui: &mut egui::Ui,
    ed: &Editor,
    id: i64,
    t: &Timeline,
    state: &[(NodeId, Status)],
    todo: &mut Option<TlEdit>,
) {
    if !ui_kit::heading_keyed(ui, "features.features", &tr("features.features", &[]), Some(t.nodes.len())) {
        return;
    }
    let rev = ed.revision;
    let th = ddc_ui::theme::of(ui.ctx());
    let marker = t.marker_index();
    let last = t.nodes.len().saturating_sub(1);
    for (i, n) in t.nodes.iter().enumerate() {
        let st = state.iter().find(|s| s.0 == n.id).map(|s| &s.1);
        let (mark, why) = status_mark(st);
        ui.horizontal(|ui| {
            let c = match st {
                Some(Status::Failed(_)) => ui.visuals().error_fg_color,
                Some(Status::Built) | Some(Status::Reused) => ui.visuals().text_color(),
                _ => ui_kit::weak(ui),
            };
            ui.colored_label(c, mark).on_hover_text(why);
            egui::CollapsingHeader::new(format!("{}  \u{b7} {}", n.name, kind_label(n.kind.name())))
                .id_salt(("param1.node", id, n.id))
                .show(ui, |ui| {
                    ui_kit::props(ui, &format!("param1.args.{id}.{}", n.id), |ui| {
                        for (key, text, dim) in feature_args(&n.kind) {
                            let bk = egui::Id::new(("features.arg", id, n.id, key.as_str()));
                            let mut b = buf(ui, bk, rev, &text);
                            let tip = match dim.as_str() {
                                "length" => tr("features.tip.length", &[]),
                                "angle" => tr("features.tip.angle", &[]),
                                _ => tr("features.tip.number", &[]),
                            };
                            if let Some(v) = ui_kit::kv_edit(ui, &arg_label(&key), &mut b, "", &tip) {
                                if v != text {
                                    *todo = Some(TlEdit::Arg(n.id, key.clone(), v));
                                }
                            }
                            ui.data_mut(|d| d.insert_temp(bk, (rev, b)));
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(i > 0, egui::Button::new("\u{25b2}"))
                            .on_hover_text(tr("features.up", &[]))
                            .clicked()
                        {
                            *todo = Some(TlEdit::Move(n.id, i - 1));
                        }
                        if ui
                            .add_enabled(i < last, egui::Button::new("\u{25bc}"))
                            .on_hover_text(tr("features.down", &[]))
                            .clicked()
                        {
                            *todo = Some(TlEdit::Move(n.id, i + 1));
                        }
                        let mut off = n.suppressed;
                        if ddc_ui::widgets::checkbox(ui, &th, &mut off, &tr("features.suppress", &[])).changed() {
                            *todo = Some(TlEdit::Suppress(n.id, off));
                        }
                        if ddc_ui::widgets::Button::secondary(&tr("features.roll_here", &[])).show(ui, &th).clicked() {
                            *todo = Some(TlEdit::Marker(Some(n.id)));
                        }
                        if ddc_ui::widgets::Button::secondary(&tr("features.remove", &[])).show(ui, &th).clicked() {
                            *todo = Some(TlEdit::Remove(n.id));
                        }
                    });
                    if n.kind.class() != Kind0::Sketch {
                        let add = ddc_ui::widgets::Button::quiet(&tr("mass_family.add_modifier", &[])).small().show(ui, &th);
                        egui::Popup::menu(&add).show(|ui| {
                            for (k, free) in modifier_menu() {
                                let r = ddc_ui::widgets::menu_item_icon(
                                    ui,
                                    &th,
                                    None,
                                    &kind_label(k),
                                    "",
                                    ddc_ui::widgets::MenuTone::Normal,
                                    free,
                                );
                                let r = if free { r } else { r.on_disabled_hover_text(tr("mass_family.needs_faces", &[])) };
                                if r.clicked() {
                                    *todo = Some(TlEdit::Modify(n.id, k.to_string()));
                                    ui.close();
                                }
                            }
                        });
                    }
                });
        });
        if i + 1 == marker && marker < t.nodes.len() {
            ui.horizontal(|ui| {
                ui.colored_label(ui.visuals().warn_fg_color, tr("features.marker", &[]));
                if ddc_ui::widgets::Button::secondary(&tr("features.roll_end", &[])).show(ui, &th).clicked() {
                    *todo = Some(TlEdit::Marker(None));
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::feature::{Kind, PlaneKind, Shape};
    use crate::units::Units;

    fn s(x: &str) -> String {
        x.to_string()
    }

    fn model() -> (Model, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L0", 0.0).unwrap();
        let mut t = Timeline::default();
        t.set_param("Depth", "1m").unwrap();
        t.set_param("Width", "2*Depth").unwrap();
        let sk = t
            .add("", Kind::Sketch {
                plane: PlaneKind::Xy,
                offset: s("0"),
                shape: Shape::Rect {
                    cx: s("0"),
                    cy: s("0"),
                    w: s("Width"),
                    h: s("Depth"),
                },
            })
            .unwrap();
        t.add("", Kind::Extrude {
            sketch: sk,
            dist: s("3m"),
            start: s("0"),
        })
        .unwrap();
        let id = m.add_mass_timeline(l, "P", t).unwrap();
        (m, id)
    }

    #[test]
    fn panel_edits_are_single_undo_steps() {
        let (mut m, id) = model();
        let v = |m: &Model| m.mass_body(id).unwrap().volume();
        let v0 = v(&m);
        apply(&mut m, id, &TlEdit::Param(s("Depth"), s("2m"))).unwrap();
        assert!((v(&m) - 4.0 * v0).abs() < 1e-9);
        apply(&mut m, id, &TlEdit::Arg(2, s("dist"), s("Width"))).unwrap();
        assert!((v(&m) - 4.0 * 4.0 * 2.0).abs() < 1e-9);
        assert!(apply(&mut m, id, &TlEdit::DropParam(s("Width"))).is_err());
        assert!(apply(&mut m, id, &TlEdit::Move(2, 0)).is_err());
        m.undo().unwrap();
        m.undo().unwrap();
        assert!((v(&m) - v0).abs() < 1e-9);
    }

    #[test]
    fn formula_preview_shows_values_and_refusals() {
        let (m, id) = model();
        let t = m.mass_timeline(id).unwrap().unwrap();
        let u = Units::Metric;
        assert_eq!(param_check(&t, "Depth", "1.5m", u), Check::Ok(display_length(1.5, u)));
        assert!(param_check(&t, "Depth", "Width", u).is_err());
        assert!(param_check(&t, "Depth", "2*", u).is_err());
        assert_eq!(param_check(&t, "Width", "90deg", u), Check::Ok("90.00\u{b0}".into()));
    }

    #[test]
    fn timeline_to_family_carries_the_parameters_and_undoes_in_one_step() {
        let (mut m, id) = model();
        let t = m.mass_timeline(id).unwrap().unwrap();
        let before = m.families().unwrap().len();
        let name = family_name(&m, "P").unwrap();
        assert_eq!(name, "P");
        let (fam, n) = to_family(&mut m, id, &name).unwrap();
        assert_eq!(n, 2, "Depth and Width");
        assert_eq!(m.family_timeline(fam).unwrap(), Some(t));
        assert_eq!(m.family_id("P").unwrap(), Some(fam));
        // the name is taken now: the next family gets the first free number
        assert_eq!(family_name(&m, "P").unwrap(), "P 2");
        assert_eq!(family_name(&m, "  ").unwrap(), "Mass");
        m.undo().unwrap();
        assert_eq!(m.families().unwrap().len(), before);
        assert_eq!(m.family_id("P").unwrap(), None);
    }

    #[test]
    fn command_is_registered_and_one_undo_step_on_the_selected_mass() {
        let mut ed = Editor::new_in_memory().unwrap();
        let l = ed.level;
        let (src, sid) = model();
        let t = src.mass_timeline(sid).unwrap().unwrap();
        let id = ed.model.add_mass_timeline(l, "Block", t).unwrap();
        ed.reload().unwrap();
        let mut app = EditorApp::new(ed, None, "test".into());
        // nothing selected: refused with a reason, nothing written
        let n0 = app.ed.undo_len();
        assert!(app.run(TO_FAMILY));
        assert_eq!(app.ed.undo_len(), n0);
        assert_eq!(app.ed.status.as_deref(), Some(tr("mass_family.no_mass", &[]).as_str()));
        app.ed.selection = vec![Sel::Mass(id)];
        assert!(app.run(TO_FAMILY));
        assert_eq!(app.ed.undo_len(), n0 + 1);
        let fam = app.ed.model.family_id("Block").unwrap().expect("family made");
        assert_eq!(app.ed.model.family_timeline(fam).unwrap().unwrap().params.len(), 2);
        assert!(app.ed.status.as_deref().is_some_and(|s| s.contains("Block")));
        app.ed.undo();
        assert_eq!(app.ed.model.family_id("Block").unwrap(), None);
    }

    #[test]
    fn command_has_registry_entry_and_strings_in_both_languages() {
        let reg = ddc_ui::Registry::builtin();
        let c = reg.get(TO_FAMILY).expect("registry entry");
        assert!(!c.icon.is_empty() && ddc_ui::icons::source(&c.icon).is_some(), "icon {}", c.icon);
        for lang in [ddc_ui::i18n::Lang::En, ddc_ui::i18n::Lang::Ru] {
            for k in [
                "cmd.mass.timeline_family",
                "cmd.mass.timeline_family.desc",
                "mass_family.done",
                "mass_family.refused",
                "mass_family.no_mass",
                "mass_family.add_modifier",
                "mass_family.needs_faces",
            ] {
                let s = crate::tr_in(lang, k, &[]);
                assert!(!s.is_empty() && s != k, "{k} {lang:?}");
            }
        }
    }

    #[test]
    fn modifier_menu_lists_the_registry_and_adds_one_as_a_single_step() {
        let menu = modifier_menu();
        let kinds = modifier_kinds();
        assert_eq!(menu.iter().map(|m| m.0).collect::<Vec<_>>(), kinds);
        assert!(kinds.contains(&"shell") && kinds.contains(&"chamfer"));
        // shell runs on the whole body; chamfer needs two faces picked in 3D
        assert!(menu.contains(&("shell", true)));
        assert!(menu.contains(&("chamfer", false)));
        for k in &kinds {
            assert!(KINDS.contains(k), "{k} has a kind label");
        }
        let (mut m, id) = model();
        let n = m.mass_timeline(id).unwrap().unwrap().nodes.len();
        apply(&mut m, id, &TlEdit::Modify(2, s("shell"))).unwrap();
        let t = m.mass_timeline(id).unwrap().unwrap();
        assert_eq!(t.nodes.len(), n + 1);
        assert!(matches!(&t.nodes[n].kind, FKind::Modify { op, input: 2, .. } if op == "shell"));
        m.undo().unwrap();
        assert_eq!(m.mass_timeline(id).unwrap().unwrap().nodes.len(), n);
    }
}
