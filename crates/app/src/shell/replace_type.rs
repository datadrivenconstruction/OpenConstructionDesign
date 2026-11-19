// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! "Replace type…": all instances of a type in the view (current level) or in the whole
//! project → another type of the same category; preview "N elements on M levels", a single
//! operation (one undo step), summary in the status bar.
use super::browser::Act;
use super::type_dialog::{self, Fam};
use crate::editor::{Editor, Sel};
use crate::ui_kit as kit;
use ddc_model::Model;
use ddc_ui::theme::{space, Role};
use ddc_ui::widgets::type_pick::{Pick, PickItem, PickTexts, TypePick};
use ddc_ui::widgets;
use ddc_ui::Theme;
use egui::{self, RichText};
use std::cell::RefCell;
use std::collections::BTreeSet;

/// Open dialog: what is replaced, with what, and where.
#[derive(Clone, Debug, PartialEq)]
pub struct Replace {
    pub act: Act,
    pub to: Option<i64>,
    pub project: bool,
    /// Snapshot for the window (no SQL every frame): key, type name, targets, instances, level.
    cache: Option<Snapshot>,
}

/// (key, source type, targets, "N elem. on M lvl." or empty, level name).
type Snapshot = (Key, String, Vec<(i64, String)>, String, String);

/// Model connection, its changes, revision, level, "whole project".
type Key = (usize, u64, u64, i64, bool);

/// Instance: model table, id, level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Inst {
    pub table: &'static str,
    pub id: i64,
    pub level: i64,
}

thread_local! {
    static OPEN: RefCell<Option<Replace>> = const { RefCell::new(None) };
}

pub fn open(act: Act) {
    OPEN.with(|o| {
        *o.borrow_mut() = Some(Replace {
            act,
            to: None,
            project: true,
            cache: None,
        })
    });
}

fn pairs(m: &Model, q: &str, id: i64) -> Vec<(i64, String)> {
    let Ok(mut st) = m.sql().prepare(q) else {
        return vec![];
    };
    st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map(|it| it.flatten().collect())
        .unwrap_or_default()
}

/// Type name for the title.
pub fn type_name(m: &Model, act: Act) -> String {
    match act {
        Act::Type(f, id) => type_dialog::types_of(m, f)
            .into_iter()
            .find(|t| t.0 == id)
            .map(|t| t.1)
            .unwrap_or_default(),
        Act::Comp(fid, tid) => {
            let fam = pairs(m, "SELECT id, name FROM comp_family WHERE id=?1", fid);
            let ty = m.comp_types(fid).unwrap_or_default();
            let t = ty.into_iter().find(|t| t.0 == tid).map(|t| t.1);
            match (fam.first(), t) {
                (Some(f), Some(t)) => format!("{}: {t}", f.1),
                (Some(f), None) => f.1.clone(),
                _ => String::new(),
            }
        }
        _ => String::new(),
    }
}

/// Compatible types: those of the same system category; for components, types of families of the same
/// category (`Family: Type`). The source type is excluded.
pub fn targets(m: &Model, act: Act) -> Vec<(i64, String)> {
    match act {
        Act::Type(Fam::Mep, _) => vec![],
        Act::Type(f, id) => type_dialog::types_of(m, f)
            .into_iter()
            .filter(|t| t.0 != id)
            .collect(),
        Act::Comp(fid, tid) => {
            let cat: String = m
                .sql()
                .query_row("SELECT category FROM comp_family WHERE id=?1", [fid], |r| {
                    r.get(0)
                })
                .unwrap_or_default();
            let host: String = m
                .sql()
                .query_row("SELECT host FROM comp_family WHERE id=?1", [fid], |r| {
                    r.get(0)
                })
                .unwrap_or_default();
            // same category and same kind of host (wall/floor/free-standing)
            let fams: Vec<(i64, String)> = {
                let Ok(mut st) = m.sql().prepare(
                    "SELECT id, name FROM comp_family WHERE category=?1 AND host=?2 ORDER BY name",
                ) else {
                    return vec![];
                };
                st.query_map([&cat, &host], |r| Ok((r.get(0)?, r.get(1)?)))
                    .map(|it| it.flatten().collect())
                    .unwrap_or_default()
            };
            fams.into_iter()
                .flat_map(|(f, fname)| {
                    m.comp_types(f)
                        .unwrap_or_default()
                        .into_iter()
                        .map(move |(t, n)| (t, format!("{fname}: {n}")))
                })
                .filter(|t| t.0 != tid)
                .collect()
        }
        _ => vec![],
    }
}

/// Instances of the type: all, or only those on `level`.
pub fn instances(m: &Model, act: Act, level: Option<i64>) -> Vec<Inst> {
    let q: &[(&'static str, &str)] = match act {
        Act::Type(Fam::Wall, _) => &[("wall", "SELECT id, level FROM wall WHERE wtype=?1")],
        Act::Type(Fam::Slab, _) => &[("slab", "SELECT id, level FROM slab WHERE stype=?1")],
        Act::Type(Fam::Member, _) => &[
            ("col", "SELECT id, level FROM col WHERE mtype=?1"),
            ("beam", "SELECT id, level FROM beam WHERE mtype=?1"),
        ],
        Act::Comp(_, _) => &[("comp", "SELECT id, level FROM comp WHERE ctype=?1")],
        _ => &[],
    };
    let ty = match act {
        Act::Type(_, id) | Act::Comp(_, id) => id,
        _ => return vec![],
    };
    let mut out = vec![];
    for (table, sql) in q {
        let Ok(mut st) = m.sql().prepare(&format!("{sql} ORDER BY id")) else {
            continue;
        };
        let rows = st
            .query_map([ty], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
            .map(|it| it.flatten().collect::<Vec<_>>())
            .unwrap_or_default();
        out.extend(
            rows.into_iter()
                .filter(|(_, l)| level.is_none() || level == Some(*l))
                .map(|(id, level)| Inst { table, id, level }),
        );
    }
    out
}

/// Instances selectable in the plan (walls, floors) as a selection.
pub fn as_selection(insts: &[Inst]) -> Vec<Sel> {
    insts
        .iter()
        .filter_map(|i| match i.table {
            "wall" => Some(Sel::Wall(i.id)),
            "slab" => Some(Sel::Slab(i.id)),
            _ => None,
        })
        .collect()
}

/// "N elements on M levels".
pub fn preview(insts: &[Inst], ru: bool) -> String {
    let lv: BTreeSet<i64> = insts.iter().map(|i| i.level).collect();
    let (n, m) = (insts.len(), lv.len());
    crate::tr_lang(ru, "type_dialog.rt.preview", &[("n", n.to_string().as_str()), ("m", m.to_string().as_str())])
}

/// Replace the type on all instances in one operation (one undo step); returns the number changed.
pub fn apply(
    ed: &mut Editor,
    act: Act,
    to: i64,
    project: bool,
    ru: bool,
) -> Result<usize, String> {
    let level = (!project).then_some(ed.level);
    let insts = instances(&ed.model, act, level);
    if insts.is_empty() {
        ed.status = Some(crate::tr_lang(ru, "type_dialog.rt.no_instances", &[]));
        return Ok(0);
    }
    if !targets(&ed.model, act).iter().any(|t| t.0 == to) {
        return Err(crate::tr_lang(ru, "type_dialog.rt.incompatible", &[]));
    }
    let summary = preview(&insts, ru);
    let n = if let Act::Comp(..) = act {
        // these are instances of the source type and `to` is a different type: every one is changed
        let ids: Vec<i64> = insts.iter().map(|i| i.id).collect();
        let cnt = ids.len();
        let rec = crate::tools::Rec::new("replace_type", move |m| {
            let mut done = 0;
            for id in &ids {
                let r = m.component(*id).and_then(|mut s| {
                    if s.ctype == to {
                        return Ok(false);
                    }
                    s.ctype = to;
                    m.update_component(*id, &s).map(|_| true)
                });
                match r {
                    Ok(ch) => done += usize::from(ch),
                    Err(e) => {
                        for _ in 0..done {
                            m.undo()?;
                        }
                        return Err(e);
                    }
                }
            }
            Ok(done)
        });
        ed.apply_ext(rec).map_err(|e| e.to_string())?;
        cnt
    } else {
        let edits = type_edits(&ed.model, &insts, to)?;
        let n = edits.len();
        if n > 0 {
            ed.apply_xlsx(edits, "")?;
        }
        n
    };
    if n == 0 {
        ed.status = Some(crate::tr_lang(ru, "type_dialog.rt.nothing", &[]));
        return Ok(0);
    }
    ed.status = Some(crate::tr_lang(
        ru,
        "type_dialog.rt.replaced",
        &[("n", n.to_string().as_str()), ("summary", summary.as_str())],
    ));
    Ok(n)
}

/// "type = `to`" edits by type id (walls, floors, columns, beams).
pub(crate) fn type_edits(
    m: &Model,
    insts: &[Inst],
    to: i64,
) -> Result<Vec<ddc_model::Edit>, String> {
    use ddc_model::Edit;
    let e = |e: ddc_model::Error| e.to_string();
    let mut out = vec![];
    for i in insts {
        match i.table {
            "col" => {
                let mut spec = m.column(i.id).map_err(e)?;
                if spec.member_type != to {
                    spec.member_type = to;
                    out.push(Edit::Column { id: i.id, spec });
                }
            }
            "beam" => {
                let mut spec = m.beam(i.id).map_err(e)?;
                if spec.member_type != to {
                    spec.member_type = to;
                    out.push(Edit::Beam { id: i.id, spec });
                }
            }
            "wall" => {
                let (mut spec, bulge) = crate::query::wall_spec(m, i.id).map_err(e)?;
                if spec.wall_type != to {
                    spec.wall_type = to;
                    out.push(Edit::Wall {
                        id: i.id,
                        spec,
                        bulge,
                    });
                }
            }
            "slab" => {
                let (level, st, offset) = crate::query::slab_spec(m, i.id).map_err(e)?;
                if st != to {
                    out.push(Edit::Slab {
                        id: i.id,
                        level,
                        slab_type: to,
                        offset,
                    });
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Types above this count get a picker with search; fewer fit a plain dropdown.
const SEARCH_OVER: usize = 12;

pub(crate) fn searchable(types: usize) -> bool {
    types > SEARCH_OVER
}

/// Why "Replace" is off, in the dialog's language; `None` when it can run.
pub(crate) fn blocked(to: Option<i64>, no_instances: bool, ru: bool) -> Option<&'static str> {
    if to.is_none() {
        Some(crate::tr_str(ru, "type_dialog.rt.pick_a_type2"))
    } else if no_instances {
        Some(crate::tr_str(ru, "type_dialog.rt.no_instances_to_replace"))
    } else {
        None
    }
}

/// The "With" control: dropdown, or a searchable picker for a long list.
fn pick_type(ui: &mut egui::Ui, th: &Theme, ru: bool, tg: &[(i64, String)], to: &mut Option<i64>) {
    let tr = |k: &'static str| crate::tr_str(ru, k);
    if searchable(tg.len()) {
        let items: Vec<PickItem> = tg
            .iter()
            .map(|(id, name)| PickItem {
                id: *id,
                name: name.clone(),
                detail: String::new(),
            })
            .collect();
        let texts = PickTexts {
            search: tr("type_dialog.rt.search").to_string(),
            empty: tr("type_dialog.rt.no_match").to_string(),
            edit: String::new(),
            mixed: tr("type_dialog.rt.pick_a_type").to_string(),
        };
        if let Some(Pick::Id(id)) = TypePick::new("u2.rt.to", &items, *to)
            .texts(texts)
            .show(ui, th)
        {
            *to = Some(id);
        }
    } else {
        // row 0 is the placeholder: nothing picked yet
        let mut names: Vec<&str> = vec![tr("type_dialog.rt.pick_a_type")];
        names.extend(tg.iter().map(|t| t.1.as_str()));
        let mut sel = to
            .and_then(|t| tg.iter().position(|x| x.0 == t))
            .map_or(0, |i| i + 1);
        if widgets::dropdown(ui, th, "u2.rt.to", &mut sel, &names, None).changed() {
            *to = sel.checked_sub(1).map(|i| tg[i].0);
        }
    }
}

/// "Replace type" window; called every frame from the window dispatcher.
pub fn show(ctx: &egui::Context, th: &Theme, ru: bool, ed: &mut Editor) {
    let Some(mut r) = OPEN.with(|o| o.borrow().clone()) else {
        return;
    };
    let tr = |k: &'static str| crate::tr_str(ru, k);
    let m = &ed.model;
    let conn = m.sql() as *const rusqlite::Connection as usize;
    // a different project (new model connection): the dialog closes
    if r.cache.as_ref().is_some_and(|c| c.0 .0 != conn) {
        OPEN.with(|o| *o.borrow_mut() = None);
        return;
    }
    let key: Key = (
        conn,
        m.sql().total_changes(),
        ed.revision,
        ed.level,
        r.project,
    );
    if r.cache.as_ref().map(|c| &c.0) != Some(&key) {
        let insts = instances(m, r.act, (!r.project).then_some(ed.level));
        let pv = if insts.is_empty() {
            String::new()
        } else {
            preview(&insts, ru)
        };
        r.cache = Some((
            key,
            type_name(m, r.act),
            targets(m, r.act),
            pv,
            ed.level_name(),
        ));
    }
    let Some((_, from, tg, pv, level_name)) = r.cache.clone() else {
        return;
    };
    // the type was deleted: close
    if from.is_empty() {
        OPEN.with(|o| *o.borrow_mut() = None);
        return;
    }
    let (mut open, mut ok, mut cancel) = (true, false, false);
    kit::Dialog::new(
        "replace_type",
        tr("type_dialog.rt.replace_type"),
        kit::DialogSize::Form,
    )
    .open(&mut open)
    .show(ctx, |ui| {
        kit::labeled_row(ui, tr("type_dialog.rt.replace"), |ui| {
            kit::name_with_tip(ui, &from, &from, true);
        });
        if tg.is_empty() {
            kit::empty_note(ui, tr("type_dialog.rt.no_other_types_in"), None);
        } else {
            kit::labeled_row(ui, tr("type_dialog.rt.with"), |ui| {
                pick_type(ui, th, ru, &tg, &mut r.to)
            });
        }
        kit::labeled_row(ui, tr("type_dialog.rt.where"), |ui| {
            let here = format!("{} ({level_name})", tr("type_dialog.rt.in_view"));
            let items = [tr("type_dialog.rt.whole_project"), here.as_str()];
            if let Some(i) = widgets::segmented(ui, th, &items, usize::from(!r.project)) {
                r.project = i == 0;
            }
        });
        kit::labeled_row(ui, tr("type_dialog.rt.will_change"), |ui| {
            let txt = if pv.is_empty() {
                tr("type_dialog.rt.no_instances").to_string()
            } else {
                pv.clone()
            };
            ui.label(RichText::new(txt).font(th.font(Role::Numeric)));
        });
        ui.add_space(space::SM);
        ui.label(
            RichText::new(tr("type_dialog.rt.one_operation_a_single"))
                .font(th.font(Role::Label))
                .color(th.c("text.secondary")),
        );
        kit::dialog_footer(ui, |ui| {
            let why = blocked(r.to, pv.is_empty(), ru);
            ok = kit::primary_action(ui, tr("type_dialog.rt.replace"), why);
            cancel = kit::cancel_button(ui, tr("type_dialog.rt.cancel")).clicked();
        });
    });
    let keep = open && !cancel && !ok;
    if ok {
        if let Some(to) = r.to {
            if let Err(e) = apply(ed, r.act, to, r.project, ru) {
                ed.status = Some(crate::tr_lang(ru, "type_dialog.rt.not_replaced", &[("e", e.as_str())]));
            }
        }
    }
    OPEN.with(|o| *o.borrow_mut() = keep.then_some(r));
}

/// Walls of the default types on two levels, with neutral type and level names.
#[cfg(test)]
pub(crate) fn two_levels() -> Editor {
    use crate::modify_tools::tests::walls;
    let mut ed = Editor::new_in_memory().unwrap();
    ed.current_type = 1;
    walls(
        &mut ed,
        &[((0.0, 0.0), (6.0, 0.0)), ((0.0, 4.0), (6.0, 4.0))],
    );
    let l2 = ed.model.add_level("Upper", 3.0).unwrap();
    ed.reload().unwrap();
    let l1 = ed.level;
    ed.set_level(l2);
    walls(&mut ed, &[((0.0, 0.0), (6.0, 0.0))]);
    ed.current_type = 0;
    walls(&mut ed, &[((0.0, 2.0), (6.0, 2.0))]);
    ed.set_level(l1);
    ed.model.sql().execute("UPDATE level SET name='Ground' WHERE id=?1", [l1]).unwrap();
    ed.model.sql().execute("UPDATE wall_type SET name='Type '||id", []).unwrap();
    ed
}

/// What the layout probe opens the dialog on.
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Case {
    /// Source type with two targets, none picked.
    Fresh,
    /// A target picked.
    Chosen,
    /// More targets than the dropdown holds.
    Many,
    /// A category with no other type.
    NoOther,
}

/// The dialog drawn headless in an explicit language, for the layout contract and its tests.
#[cfg(test)]
pub(crate) fn layout_probe(ru: bool, case: Case) -> crate::layout_contract::Probe {
    let mut ed = two_levels();
    let (from, second) = (ed.types[1].id, ed.types[2].id);
    let act = if case == Case::NoOther {
        let id = ed.model.add_mep_system("Sys", ddc_model::SystemKind::ColdWater).unwrap();
        Act::Type(Fam::Mep, id)
    } else {
        Act::Type(Fam::Wall, from)
    };
    if case == Case::Many {
        for i in 0..13 {
            let lay = ddc_model::LayerSpec { material: "Brick".into(), thickness: 0.2 };
            ed.model.add_wall_type(&format!("Extra {i}"), &[lay]).unwrap();
        }
    }
    open(act);
    if case == Case::Chosen {
        OPEN.with(|o| o.borrow_mut().as_mut().unwrap().to = Some(second));
    }
    let th = Theme::new(ddc_ui::Mode::Dark, ddc_ui::Density::Compact);
    let p = crate::layout_contract::probe("replace_type", |ctx| show(ctx, &th, ru, &mut ed));
    OPEN.with(|o| *o.borrow_mut() = None);
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::accesskit::Role as Ak;

    fn cyr(s: &str) -> bool {
        s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c))
    }

    fn lefts(p: &crate::layout_contract::Probe, role: Ak) -> Vec<f32> {
        p.controls.iter().filter(|c| c.role == role).map(|c| c.rect.left()).collect()
    }

    #[test]
    fn controls_share_one_left_edge_in_both_languages() {
        for ru in [false, true] {
            let p = layout_probe(ru, Case::Chosen);
            let combo = lefts(&p, Ak::ComboBox);
            let pills = lefts(&p, Ak::RadioButton);
            assert!(!combo.is_empty(), "ru={ru}: {:?}", p.controls);
            assert_eq!(pills.len(), 2, "ru={ru}: {:?}", p.controls);
            let x = combo.iter().copied().fold(f32::MAX, f32::min);
            // the sunken track shares the edge; its first pill sits one inset inside it
            let inset = space::XXS;
            // the tree lists nodes by id, not by place: the first pill is the leftmost one
            let first = pills.iter().copied().fold(f32::MAX, f32::min);
            assert!((first - inset - x).abs() < 0.5, "ru={ru}: dropdown {x} pills {pills:?}");
        }
    }

    #[test]
    fn replace_is_off_with_a_reason_until_a_type_is_picked() {
        for ru in [false, true] {
            let pick = blocked(None, false, ru).expect("no type picked");
            let none = blocked(Some(1), true, ru).expect("no instances");
            assert_ne!(pick, none);
            assert_eq!(pick, crate::tr_str(ru, "type_dialog.rt.pick_a_type2"));
            assert_eq!(blocked(Some(1), false, ru), None);
            // the button is drawn in the language of the dialog
            let p = layout_probe(ru, Case::Fresh);
            let want = crate::tr_str(ru, "type_dialog.rt.replace");
            let b = p.controls.iter().find(|c| c.role == Ak::Button && c.label == want);
            assert!(b.is_some(), "ru={ru}: {:?}", p.controls);
        }
        assert_ne!(blocked(None, false, false), blocked(None, false, true));
    }

    #[test]
    fn no_other_types_shows_the_note_and_no_dropdown() {
        for ru in [false, true] {
            let p = layout_probe(ru, Case::NoOther);
            assert!(lefts(&p, Ak::ComboBox).is_empty(), "ru={ru}: {:?}", p.controls);
            assert_eq!(lefts(&p, Ak::RadioButton).len(), 2, "ru={ru}");
            let cancel = crate::tr_str(ru, "type_dialog.rt.cancel");
            assert!(p.controls.iter().any(|c| c.label == cancel), "ru={ru}");
        }
    }

    #[test]
    fn long_lists_get_search_and_still_one_control() {
        assert!(!searchable(SEARCH_OVER));
        assert!(searchable(SEARCH_OVER + 1));
        for ru in [false, true] {
            let p = layout_probe(ru, Case::Many);
            assert!(!lefts(&p, Ak::ComboBox).is_empty(), "ru={ru}: {:?}", p.controls);
        }
    }

    #[test]
    fn dialog_and_status_speak_the_given_language() {
        for ru in [false, true] {
            for case in [Case::Fresh, Case::Chosen, Case::NoOther] {
                let p = layout_probe(ru, case);
                let any = p.controls.iter().any(|c| cyr(&c.label));
                let en_bad: Vec<&str> =
                    p.controls.iter().map(|c| c.label.as_str()).filter(|l| cyr(l)).collect();
                if ru {
                    assert!(any, "{case:?}: no Russian label");
                } else {
                    assert!(en_bad.is_empty(), "{case:?}: {en_bad:?}");
                }
            }
            let mut ed = two_levels();
            let (from, to) = (ed.types[1].id, ed.types[2].id);
            apply(&mut ed, Act::Type(Fam::Wall, from), to, true, ru).unwrap();
            let msg = ed.status.clone().unwrap();
            assert_eq!(cyr(&msg), ru, "status {msg:?}");
        }
    }

    #[test]
    fn source_has_no_raw_grid_combo_literal_width_or_global_language() {
        let src = include_str!("replace_type.rs");
        let code = &src[..src.find("#[cfg(test)]\nmod tests").unwrap()];
        let banned = [
            "egui::Grid",
            "ComboBox",
            "radio_value",
            "selectable_label",
            ".width(260",
            "add_space(6.0)",
            "ui_is_ru",
            "ui_lang_now",
            "crate::tr(",
            "tr_n(",
        ];
        for b in banned {
            assert!(!code.contains(b), "replace_type still uses {b}");
        }
    }

    #[test]
    fn replace_type_in_project_is_one_undo_step() {
        let mut ed = two_levels();
        let from = ed.types[1].id;
        let to = ed.types[2].id;
        let act = Act::Type(Fam::Wall, from);
        let all = instances(&ed.model, act, None);
        assert_eq!(all.len(), 3);
        let pv = crate::tr_lang(true, "type_dialog.rt.preview", &[("n", "3"), ("m", "2")]);
        assert_eq!(preview(&all, true), pv);
        assert_eq!(instances(&ed.model, act, Some(ed.level)).len(), 2);
        let tg = targets(&ed.model, act);
        assert_eq!(tg.len(), 2);
        assert!(!tg.iter().any(|t| t.0 == from));
        let undo0 = ed.can_undo();
        assert_eq!(apply(&mut ed, act, to, true, true).unwrap(), 3);
        assert!(instances(&ed.model, act, None).is_empty());
        assert_eq!(
            instances(&ed.model, Act::Type(Fam::Wall, to), None).len(),
            3
        );
        ed.undo();
        assert_eq!(instances(&ed.model, act, None).len(), 3);
        assert_eq!(ed.can_undo(), undo0);
        ed.redo();
        assert_eq!(instances(&ed.model, act, None).len(), 0);
        ed.undo();
        // in the view only; an incompatible type (floors) is rejected
        assert_eq!(apply(&mut ed, act, to, false, true).unwrap(), 2);
        assert_eq!(instances(&ed.model, act, None).len(), 1);
        assert!(apply(&mut ed, act, from, true, true).is_err());
        assert_eq!(as_selection(&all).len(), 3);
    }

    /// Snapshots Q1–Q4 (light+dark) in docs/design/shots/query/:
    /// `cargo test -p ddc-app shots_query -- --ignored`
    #[test]
    #[ignore]
    fn shots_query() {
        use crate::shell::EditorApp;
        let (pick, desc) = crate::gfx::choose(&crate::gfx::candidates(None, false), &mut |_| {})
            .expect("graphics adapter");
        let rs = crate::shot::render_state(pick).unwrap();
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/design/shots/query");
        std::fs::create_dir_all(&dir).unwrap();
        let size = (1440u32, 900u32);
        let render = |app: &mut EditorApp| {
            let ctx = egui::Context::default();
            crate::shot::frame(&rs, &ctx, size, 1.0, 8, None, |c, _| app.ui(c, Some(&rs)))
        };
        for theme in ["light", "dark"] {
            for id in ["Q1", "Q2", "Q3", "Q4"] {
                let mut ed = crate::query::demo_scene();
                ed.status = None;
                ed.set_mode(crate::wall_tool::Mode::Select);
                let (from, to) = (ed.types[1].id, ed.types[2].id);
                let mut app = EditorApp::new(ed, Some(&rs), desc.clone());
                app.prefs.theme = theme.into();
                app.plan.fit(&app.ed);
                render(&mut app);
                super::super::browser::with(|s| s.expand = false);
                crate::query::prepare_demo(&mut app, id);
                if id == "Q4" {
                    open(Act::Type(Fam::Wall, from));
                    OPEN.with(|o| o.borrow_mut().as_mut().unwrap().to = Some(to));
                    super::super::browser::with(|s| s.expand = true);
                }
                let px = render(&mut app);
                OPEN.with(|o| *o.borrow_mut() = None);
                crate::shot::save_png(&dir.join(format!("{id}-{theme}.png")), size, &px).unwrap();
            }
        }
    }
}
