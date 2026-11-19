// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `mod.explain` ("Explain element"): a window that says, in plain sentences, why the selected
//! element looks and behaves the way it does: what it is, where it sits, what holds its top and
//! base, how its ends join, which rules and openings it carries, whether it is pinned, grouped,
//! phased or warned about. Read only: every fact comes from the model, nothing is guessed.

use crate::editor::{Editor, Sel};
use crate::shell::EditorApp;
use crate::ui_kit as kit;
use ddc_geom::wall_edit::JoinKind;
use ddc_ui::theme::space;
use ddc_ui::Catalog;
use rusqlite::OptionalExtension;
use std::cell::RefCell;

/// One explained fact: a stable key (for tests), a short title and the sentence.
#[derive(Clone, Debug, PartialEq)]
pub struct Fact {
    pub key: &'static str,
    pub title: String,
    pub text: String,
}

#[derive(Default)]
pub struct Win {
    pub open: bool,
    /// Facts of `at`, read again when the selection or the model changes.
    at: Option<(Sel, u64, ddc_ui::Lang)>,
    facts: Vec<Fact>,
}

thread_local! {
    static WIN: RefCell<Win> = RefCell::new(Win::default());
}

fn fact(cat: &Catalog, key: &'static str, args: &[(&str, &str)]) -> Fact {
    Fact {
        key,
        title: cat.t(&format!("why.{key}")),
        text: cat.tf(&format!("why.{key}.text"), args),
    }
}

const ATTACHED_TOP: &str = "SELECT slab FROM wall_attach WHERE wall=?1 AND side='top'";
const ATTACHED_BASE: &str = "SELECT slab FROM wall_attach WHERE wall=?1 AND side='base'";
const TOP_LEVEL: &str = "SELECT level, offset FROM wall_attach_level WHERE wall=?1";

fn one_i64(ed: &Editor, sql: &str, id: i64) -> Option<i64> {
    // a table that is not in this project yet reads as "nothing attached"
    ed.model.sql().query_row(sql, [id], |r| r.get(0)).optional().ok().flatten()
}

fn level_name(ed: &Editor, id: i64) -> String {
    ed.levels().into_iter().find(|l| l.0 == id).map(|l| l.1).unwrap_or_else(|| format!("#{id}"))
}

fn join_name(cat: &Catalog, k: JoinKind) -> String {
    cat.t(match k {
        JoinKind::Butt => "why.join.butt",
        JoinKind::Miter => "why.join.miter",
        JoinKind::SquareOff => "why.join.square",
    })
}

fn wall_facts(ed: &Editor, id: i64, cat: &Catalog, out: &mut Vec<Fact>) {
    let ru = cat.lang == ddc_ui::Lang::Ru;
    let u = ed.tool.units;
    let len = |m: f64| crate::unit_text::len(m, ru, u);
    let Some(w) = ed.walls.iter().find(|w| w.id == id) else {
        return;
    };
    if let Some(r) = ed.model.wall_top_roof(id).ok().flatten() {
        out.push(fact(cat, "top_roof", &[("id", &r.to_string())]));
        if ed.model.wall_top_core(id).ok().flatten().is_some() {
            let l = format!("{:.2}", ed.model.insulation_lambda());
            out.push(fact(cat, "top_core", &[("lambda", &l)]));
        }
    } else if let Ok(Some((lv, off))) = ed
        .model
        .sql()
        .query_row(TOP_LEVEL, [id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)))
        .optional()
    {
        let (lv, off) = (level_name(ed, lv), len(off));
        out.push(fact(cat, "top_level", &[("level", &lv), ("offset", &off)]));
    } else if let Some(s) = one_i64(ed, ATTACHED_TOP, id) {
        out.push(fact(cat, "top_slab", &[("id", &s.to_string())]));
    } else if ed.model.wall_profile(id).ok().flatten().is_some() {
        out.push(fact(cat, "profile", &[]));
    } else {
        out.push(fact(cat, "top_flat", &[("h", &len(w.height))]));
    }
    if let Some(s) = one_i64(ed, ATTACHED_BASE, id) {
        out.push(fact(cat, "base_slab", &[("id", &s.to_string())]));
    } else if w.base_offset.abs() > 1e-9 {
        out.push(fact(cat, "base_offset", &[("offset", &len(w.base_offset))]));
    }
    for (end, key) in [(0u8, "why.start"), (1u8, "why.end")] {
        let Ok(p) = ed.model.wall_join(id, end) else {
            continue;
        };
        let at = cat.t(key);
        if !p.allowed {
            out.push(fact(cat, "join_off", &[("end", &at)]));
        } else if p.kind != JoinKind::Butt {
            out.push(fact(cat, "join_kind", &[("end", &at), ("kind", &join_name(cat, p.kind))]));
        }
    }
    for (_, c) in ed.model.constraints_of(id).unwrap_or_default() {
        let t = crate::constraints_ui::title_lang(ru, c.kind);
        out.push(fact(cat, "rule", &[("rule", &t)]));
    }
    let hosted = one_i64(ed, "SELECT count(*) FROM opening WHERE wall=?1", id);
    if let Some(n) = hosted.filter(|n| *n > 0) {
        out.push(fact(cat, "openings", &[("n", &n.to_string())]));
    }
}

/// Every fact about `s`, in reading order.
pub fn facts(ed: &Editor, s: Sel, cat: &Catalog) -> Vec<Fact> {
    let Some((table, id)) = crate::phase_ui::sel_ref(s) else {
        return vec![];
    };
    let mut out = vec![];
    let what = match ed.model.category_of(table, id).ok().flatten() {
        Some(c) => crate::classify_ui::category_label(cat, c),
        None => crate::phase_ui::kind_name(table, cat.lang == ddc_ui::Lang::Ru).to_string(),
    };
    out.push(fact(cat, "what", &[("what", &what), ("id", &id.to_string())]));
    // the level column is on most element tables; a table without it says nothing here
    let sql = format!("SELECT level FROM {table} WHERE id=?1");
    if let Some(lv) = one_i64(ed, &sql, id) {
        out.push(fact(cat, "level", &[("level", &level_name(ed, lv))]));
    }
    if let Sel::Wall(w) = s {
        wall_facts(ed, w, cat, &mut out);
    }
    if ed.model.is_pinned(table, id).unwrap_or(false) {
        out.push(fact(cat, "pinned", &[]));
    }
    if let Some(g) = ed.model.group_of(table, id).ok().flatten() {
        let gs = ed.model.groups().unwrap_or_default();
        if let Some(g) = gs.into_iter().find(|x| x.id == g) {
            let n = g.instances.to_string();
            out.push(fact(cat, "group", &[("name", &g.name), ("n", &n)]));
        }
    }
    if let Ok(p) = ed.model.element_phase(table, id) {
        let ps = ed.model.phases().unwrap_or_default();
        let name = |pid: i64| ps.iter().find(|x| x.id == pid).map(|x| x.name.clone());
        if let Some(c) = name(p.created) {
            out.push(fact(cat, "phase", &[("phase", &c)]));
        }
        if let Some(d) = p.demolished.and_then(name) {
            out.push(fact(cat, "demolished", &[("phase", &d)]));
        }
    }
    if matches!(s, Sel::Wall(_)) {
        for w in ed.model.wall_warnings().unwrap_or_default() {
            if w.table == table && w.id == id {
                out.push(fact(cat, "warning", &[("text", &w.message)]));
            }
        }
    }
    out
}

/// The project's roof-trimming rules: each one with its why text, editable, one undo step per edit.
fn rules_section(ui: &mut egui::Ui, ed: &mut Editor, cat: &Catalog) {
    kit::section_title(ui, &cat.t("why.rt_head"));
    for r in ddc_model::ROOF_RULES.iter() {
        let mut v = ed.model.roof_rule(r.key);
        let was = v;
        ui.label(cat.t(&format!("why.rt_{}", r.key)));
        let mut reset = false;
        let th = ddc_ui::theme::of(ui.ctx());
        let committed = ui
            .horizontal(|ui| {
                let out = ddc_ui::widgets::NumberField::new(("why.rt", r.key), &mut v, ddc_ui::widgets::NumberFormat::for_lang(cat.lang))
                    .decimals(3)
                    .step(0.01)
                    .range(r.min, r.max)
                    .show(ui, &th);
                reset = ddc_ui::widgets::Button::quiet(&cat.t("why.rt_reset")).small().show(ui, &th).clicked();
                out.committed
            })
            .inner;
        if reset {
            v = r.default;
        }
        if reset || (committed && (v - was).abs() > 1e-12) {
            if ed.model.set_roof_rule(r.key, v).is_ok() {
                let _ = ed.reload();
            }
        }
        ui.add(egui::Label::new(cat.t(&format!("why.rt_{}.text", r.key))).wrap());
        ui.add_space(space::XS);
    }
}

/// Window body (apart from the frame, for the layout probes). `true`: close.
pub fn body(ui: &mut egui::Ui, w: &mut Win, ed: &Editor, cat: &Catalog) -> bool {
    let one = match ed.selection[..] {
        [s] => Some(s),
        _ => None,
    };
    let key = one.map(|s| (s, ed.model.change_head(), cat.lang));
    if key != w.at {
        w.facts = one.map(|s| facts(ed, s, cat)).unwrap_or_default();
        w.at = key;
    }
    if w.facts.is_empty() {
        let k = if ed.selection.len() > 1 { "why.many" } else { "why.none" };
        kit::empty_note(ui, cat.t(k), Some(&cat.t("why.none_hint")));
    } else {
        for f in &w.facts {
            kit::section_title(ui, &f.title);
            ui.add(egui::Label::new(f.text.as_str()).wrap());
            ui.add_space(space::XS);
        }
    }
    let mut close = false;
    kit::dialog_footer(ui, |ui| {
        close = kit::cancel_button(ui, cat.t("dlg.close")).clicked();
    });
    close
}

fn run(_: &mut EditorApp) {
    WIN.with(|w| {
        let mut w = w.borrow_mut();
        w.open = true;
        w.at = None;
    });
}

pub fn open_for_layout() {
    WIN.with(|w| *w.borrow_mut() = Win { open: true, ..Win::default() });
}

/// Called every frame.
pub fn frame(a: &mut EditorApp, ctx: &egui::Context) {
    let mut w = WIN.with(|w| std::mem::take(&mut *w.borrow_mut()));
    if !w.open {
        return;
    }
    let (mut open, mut close) = (true, false);
    kit::Dialog::new("explain", a.sh.cat.t("cmd.mod.explain"), kit::DialogSize::Tool)
        .open(&mut open)
        .scroll_body()
        .show(ctx, |ui| {
            rules_section(ui, &mut a.ed, &a.sh.cat);
            close = body(ui, &mut w, &a.ed, &a.sh.cat);
        });
    w.open = open && !close;
    WIN.with(|s| *s.borrow_mut() = w);
}

pub fn register(a: &mut EditorApp) {
    a.register_command("mod.explain", run);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::sketch::ElevationProfile;
    use ddc_ui::Lang;

    fn room() -> (Editor, i64) {
        let mut ed = Editor::new_in_memory().unwrap();
        crate::schedule::test_room(&mut ed);
        let id = ed.walls[0].id;
        (ed, id)
    }

    fn keys(f: &[Fact]) -> Vec<&'static str> {
        f.iter().map(|f| f.key).collect()
    }

    #[test]
    fn a_plain_wall_says_what_it_is_where_and_its_flat_top() {
        let (ed, id) = room();
        let cat = Catalog::new(Lang::En);
        let f = facts(&ed, Sel::Wall(id), &cat);
        let k = keys(&f);
        assert_eq!(k[0], "what");
        assert!(f[0].text.contains(&id.to_string()), "{:?}", f[0]);
        assert!(k.contains(&"level") && k.contains(&"top_flat"), "{k:?}");
        assert!(!k.contains(&"pinned") && !k.contains(&"profile"), "{k:?}");
    }

    #[test]
    fn pin_profile_and_rules_are_explained_in_both_languages() {
        let (mut ed, id) = room();
        let len = ed.model.wall_length(id).unwrap();
        let mut p = ElevationProfile::rectangle(len, 3.0);
        p.outer.insert(3, (len / 2.0, 4.0));
        ed.model.set_wall_profile(id, Some(&p)).unwrap();
        ed.model.pin(&[("wall", id)]).unwrap();
        ed.reload().unwrap();
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            let f = facts(&ed, Sel::Wall(id), &cat);
            let k = keys(&f);
            assert!(k.contains(&"profile") && k.contains(&"pinned"), "{lang:?} {k:?}");
            for x in &f {
                assert!(!x.title.starts_with("why.") && !x.text.starts_with("why."), "{x:?}");
            }
        }
    }

    #[test]
    fn the_window_follows_the_selection() {
        let (mut ed, id) = room();
        let cat = Catalog::new(Lang::En);
        let mut w = Win { open: true, ..Win::default() };
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let draw = |ed: &Editor, w: &mut Win| {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| body(ui, w, ed, &cat));
            });
        };
        draw(&ed, &mut w);
        assert!(w.facts.is_empty());
        ed.selection = vec![Sel::Wall(id)];
        draw(&ed, &mut w);
        assert_eq!(w.facts[0].key, "what");
        ed.selection = vec![Sel::Wall(id), Sel::Wall(ed.walls[1].id)];
        draw(&ed, &mut w);
        assert!(w.facts.is_empty(), "several elements: pick one");
    }

    #[test]
    fn the_command_opens_the_window() {
        let mut a = crate::layout_contract::app_in(Lang::En);
        assert!(a.run("mod.explain"));
        assert!(WIN.with(|w| w.borrow().open));
        WIN.with(|w| w.borrow_mut().open = false);
    }

    #[test]
    fn every_text_is_in_both_languages() {
        let facts = [
            "what", "level", "top_roof", "top_core", "top_level", "top_slab", "profile", "top_flat",
            "base_slab", "base_offset", "join_off", "join_kind", "rule", "openings", "pinned",
            "group", "phase", "demolished", "warning",
        ];
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            for f in facts {
                for k in [format!("why.{f}"), format!("why.{f}.text")] {
                    assert_ne!(cat.t(&k), k, "{lang:?} {k}");
                }
            }
            for k in [
                "why.start", "why.end", "why.join.butt", "why.join.miter", "why.join.square",
                "why.none", "why.many", "why.none_hint",
            ] {
                assert_ne!(cat.t(k), k, "{lang:?} {k}");
            }
        }
    }
}
