//! Project parameters of a multi-selection: the parameters of the active category with the
//! shared value or `<varies>`; one edit writes every selected element in one undo step. A mixed
//! value is never written unless its field is edited.
use crate::editor::{Editor, Sel};
use crate::shell::prop_head;
use crate::ui_kit as kit;
use ddc_model::{PVal, ParamDef, ParamType};

/// One parameter over the selection.
#[derive(Clone)]
pub struct MRow {
    pub def: ParamDef,
    /// The value all elements hold (`Some(None)`: none set); `None` when they differ.
    pub common: Option<Option<PVal>>,
    /// Elements whose value of this parameter is locked.
    pub locked: usize,
}

/// The parameter category of the first element and the selected elements that share it.
pub fn targets(ed: &Editor, sels: &[Sel]) -> (String, Vec<(&'static str, i64)>) {
    let refs: Vec<(&'static str, i64)> =
        sels.iter().filter_map(|s| crate::phase_ui::sel_ref(*s)).collect();
    let cat_of = |r: &(&'static str, i64)| ed.model.category_of(r.0, r.1).ok().flatten();
    let Some(cat) = refs.iter().find_map(cat_of) else {
        return (String::new(), vec![]);
    };
    let out = refs.into_iter().filter(|r| cat_of(r) == Some(cat)).collect();
    (cat.to_string(), out)
}

/// Rows of the category's parameters over `targets`.
pub fn rows(ed: &Editor, cat: &str, targets: &[(&'static str, i64)]) -> Vec<MRow> {
    let defs = ed.model.params_for(cat).unwrap_or_default();
    defs.into_iter()
        .map(|def| {
            let vals: Vec<Option<PVal>> = targets
                .iter()
                .map(|&(k, id)| ed.model.param_value_of(&def, k, id).ok().flatten())
                .collect();
            let common = match vals.split_first() {
                Some((f, rest)) if rest.iter().any(|v| v != f) => None,
                Some((f, _)) => Some(f.clone()),
                None => Some(None),
            };
            let locked = targets.iter().filter(|&&(k, id)| locked(ed, &def, k, id)).count();
            MRow { def, common, locked }
        })
        .collect()
}

/// The value is locked on the row that holds it (the type row for a type parameter).
fn locked(ed: &Editor, d: &ParamDef, kind: &str, id: i64) -> bool {
    let at = if d.spec.per_type {
        ed.model.type_of(kind, id).ok().flatten().map(|(tk, tid)| (tk.to_string(), tid))
    } else {
        Some((kind.to_string(), id))
    };
    at.is_some_and(|(k, i)| ed.model.param_locked(&k, i, &d.spec.name).unwrap_or(false))
}

fn unit_of(d: &ParamDef, u: crate::units::Units) -> &str {
    match d.spec.ptype {
        ParamType::Length => crate::schedule::Kind::Len.unit(u),
        ParamType::Area => crate::schedule::Kind::Area.unit(u),
        ParamType::Volume => crate::schedule::Kind::Vol.unit(u),
        ParamType::Number => d.spec.unit.as_str(),
        _ => "",
    }
}

/// Project parameters block of the multi panel; nothing when the category has none.
pub fn show(ui: &mut egui::Ui, ed: &mut Editor, ru: bool, sels: &[Sel]) {
    let (cat, tg) = targets(ed, sels);
    if tg.is_empty() {
        return;
    }
    let rk = ed.cache_key();
    let ck = egui::Id::new(("ddc.pparam.multi", &tg));
    let hit = ui.data(|d| d.get_temp::<((u64, u64), Vec<MRow>)>(ck)).filter(|c| c.0 == rk);
    let rows = match hit {
        Some(c) => c.1,
        None => {
            let r = rows(ed, &cat, &tg);
            ui.data_mut(|d| d.insert_temp(ck, (rk, r.clone())));
            r
        }
    };
    if rows.is_empty() {
        return;
    }
    let u = ed.tool.units;
    let th = ddc_ui::theme::of(ui.ctx());
    let varies = crate::tr_str(ru, "type_dialog.panels.varies");
    let tip = crate::tr_str(ru, "lnp.params_tip");
    let fk = egui::Id::new("ddc.pparam.multi.focus");
    let mut focus: Option<(i64, String)> = ui.data(|d| d.get_temp(fk));
    let mut commit: Option<(i64, String)> = None;
    let open = kit::heading_keyed(
        ui,
        &prop_head::section_key(&cat, "sketch_edit.props_title"),
        crate::tr_str(ru, "sketch_edit.props_title"),
        Some(rows.len()),
    );
    if open {
        kit::props(ui, "sel.pparam.multi", |ui| {
            for r in &rows {
                let d = &r.def;
                let mut key = d.spec.name.clone();
                if d.spec.per_type {
                    key += crate::tr_str(ru, "sketch_edit.per_type_suffix");
                }
                let cur = match &r.common {
                    Some(v) => crate::params_ui::shown(v, d.spec.ptype, u, ru),
                    None => String::new(),
                };
                if r.locked >= tg.len() {
                    // every value is locked: read only, as on one element
                    let v = if r.common.is_none() { varies.to_string() } else { cur };
                    kit::kv(ui, &key, v);
                    continue;
                }
                if d.spec.ptype == ParamType::YesNo {
                    let mut names = vec![
                        crate::tr_str(ru, "sketch_edit.yes"),
                        crate::tr_str(ru, "sketch_edit.no"),
                        crate::tr_str(ru, "params_list.not_set"),
                    ];
                    let mut at = match &r.common {
                        Some(v) => crate::params_ui::yn_index(v),
                        None => {
                            names.push(varies);
                            3
                        }
                    };
                    kit::kv_widget(ui, &key, |ui| {
                        let w = ui.available_width().min(kit::WIDGET_W);
                        let id = ("ddc.pparam.multi.yn", d.id);
                        let c = ddc_ui::widgets::dropdown(ui, &th, id, &mut at, &names, Some(w));
                        if c.changed() && at < 3 {
                            commit = Some((d.id, crate::params_ui::yn_commit(at).to_string()));
                        }
                    });
                    continue;
                }
                let mut buf = match &focus {
                    Some((k, b)) if *k == d.id => b.clone(),
                    _ => cur.clone(),
                };
                let hint = if r.common.is_none() { varies } else { "" };
                let unit = unit_of(d, u);
                let align = if d.spec.ptype == ParamType::Text {
                    egui::Align::LEFT
                } else {
                    egui::Align::RIGHT
                };
                let resp = kit::kv_text(ui, &key, &mut buf, unit, tip, hint, align);
                if resp.has_focus() {
                    focus = Some((d.id, buf));
                } else if focus.as_ref().is_some_and(|f| f.0 == d.id) {
                    // Enter, Tab and a click elsewhere write a changed text; Esc drops it
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
                    if kit::field_end(resp.lost_focus(), enter, esc, buf != cur)
                        == kit::FieldEnd::Apply
                    {
                        commit = Some((d.id, buf));
                    }
                    focus = None;
                }
            }
        });
        if let Some(n) = rows.iter().map(|r| r.locked).max().filter(|n| *n > 0) {
            kit::hint(ui, crate::tr_lang(ru, "lnp.locked_hint", &[("n", &n.to_string())]));
        }
    }
    ui.data_mut(|d| match &focus {
        Some(f) => {
            d.insert_temp(fk, f.clone());
        }
        None => {
            d.remove::<(i64, String)>(fk);
        }
    });
    if let Some((pid, text)) = commit {
        crate::schedule::extra::edit_param_many(ed, pid, &tg, text.trim());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wall_tool::{Event, Mode};
    use ddc_geom::v2;

    fn room() -> Editor {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.set_mode(Mode::Rect);
        ed.handle(Event::Move(v2(0.0, 0.0)));
        ed.handle(Event::Press(v2(0.0, 0.0)));
        ed.handle(Event::Move(v2(6.0, 4.0)));
        ed.handle(Event::Release(v2(6.0, 4.0)));
        ed.set_mode(Mode::Select);
        ed
    }

    fn mark(ed: &mut Editor, ptype: ParamType) -> i64 {
        let pid = ed
            .model
            .add_param(&ddc_model::ParamSpec {
                name: "Mark".into(),
                ptype,
                per_type: false,
                unit: String::new(),
                group: String::new(),
                guid: None,
                categories: vec!["wall".into()],
            })
            .unwrap();
        ed.reload().unwrap();
        pid
    }

    #[test]
    fn three_walls_mark_writes_three_values_in_one_undo_step() {
        let mut ed = room();
        let pid = mark(&mut ed, ParamType::Text);
        let ids: Vec<i64> = ed.walls.iter().map(|w| w.id).take(3).collect();
        let sels: Vec<Sel> = ids.iter().map(|i| Sel::Wall(*i)).collect();
        let (cat, tg) = targets(&ed, &sels);
        assert_eq!((cat.as_str(), tg.len()), ("wall", 3));
        let d0 = ed.undo_len();
        assert_eq!(crate::schedule::extra::edit_param_many(&mut ed, pid, &tg, "A-1"), 3);
        let text = Some(PVal::Text("A-1".into()));
        for &id in &ids {
            assert_eq!(ed.model.param_value(pid, "wall", id).unwrap(), text);
        }
        assert_eq!(ed.undo_len(), d0 + 1);
        let r = rows(&ed, &cat, &tg);
        assert_eq!(r.len(), 1);
        assert!(r[0].common == Some(text.clone()), "shared value shown");
        ed.undo();
        for &id in &ids {
            assert_eq!(ed.model.param_value(pid, "wall", id).unwrap(), None);
        }
        // the same value again writes nothing and leaves no step
        ed.redo();
        let d1 = ed.undo_len();
        assert_eq!(crate::schedule::extra::edit_param_many(&mut ed, pid, &tg, "A-1"), 0);
        assert_eq!(ed.undo_len(), d1);
    }

    #[test]
    fn mixed_values_read_as_varies_and_stay_until_edited() {
        let mut ed = room();
        let pid = mark(&mut ed, ParamType::Number);
        let ids: Vec<i64> = ed.walls.iter().map(|w| w.id).take(2).collect();
        crate::schedule::extra::edit_param(&mut ed, pid, "wall", ids[0], "5");
        let sels: Vec<Sel> = ids.iter().map(|i| Sel::Wall(*i)).collect();
        let (cat, tg) = targets(&ed, &sels);
        let r = rows(&ed, &cat, &tg);
        assert!(r[0].common.is_none(), "5 and nothing differ");
        // invalid text: refused, nothing written, no step
        let d0 = ed.undo_len();
        assert_eq!(crate::schedule::extra::edit_param_many(&mut ed, pid, &tg, "abc"), 0);
        assert_eq!(ed.undo_len(), d0);
        assert_eq!(ed.status_severity(), Some(crate::status::Severity::Refusal));
        assert_eq!(ed.model.param_value(pid, "wall", ids[0]).unwrap(), Some(PVal::Num(5.0)));
        assert_eq!(ed.model.param_value(pid, "wall", ids[1]).unwrap(), None);
    }

    #[test]
    fn locked_values_are_kept_and_counted() {
        let mut ed = room();
        let pid = mark(&mut ed, ParamType::Text);
        let ids: Vec<i64> = ed.walls.iter().map(|w| w.id).take(2).collect();
        ed.model.set_param_lock(&[("wall", ids[0], "Mark")]).unwrap();
        let sels: Vec<Sel> = ids.iter().map(|i| Sel::Wall(*i)).collect();
        let (cat, tg) = targets(&ed, &sels);
        assert_eq!(rows(&ed, &cat, &tg)[0].locked, 1);
        assert_eq!(crate::schedule::extra::edit_param_many(&mut ed, pid, &tg, "B"), 1);
        assert_eq!(ed.model.param_value(pid, "wall", ids[0]).unwrap(), None);
        let b = Some(PVal::Text("B".into()));
        assert_eq!(ed.model.param_value(pid, "wall", ids[1]).unwrap(), b);
        let st = ed.status.clone().unwrap_or_default();
        assert!(st.contains(&crate::tr("locked_values.kept", &[("n", "1")])), "{st}");
        assert!(st.contains(&crate::tr("lnp.params_changed", &[("n", "1")])), "{st}");
    }

    #[test]
    fn mixed_category_keeps_the_first_category() {
        let mut ed = room();
        mark(&mut ed, ParamType::Text);
        let w = ed.walls[0].id;
        let sels = [Sel::Wall(w), Sel::Wall(ed.walls[1].id)];
        let (cat, tg) = targets(&ed, &sels);
        assert_eq!(cat, "wall");
        assert_eq!(tg, vec![("wall", w), ("wall", ed.walls[1].id)]);
        assert!(targets(&ed, &[]).1.is_empty());
    }

    #[test]
    fn texts_exist_in_both_languages() {
        for key in ["lnp.params_changed", "lnp.params_same", "lnp.params_tip", "lnp.locked_hint"] {
            let (en, ru) = (crate::tr_str(false, key), crate::tr_str(true, key));
            assert_ne!(en, key);
            assert_ne!(ru, key);
            assert_ne!(en, ru, "{key}");
        }
    }
}
