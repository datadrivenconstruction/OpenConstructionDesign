//! `app.where_is` ("Where is..."): type a concept in your own words ("schedule", "hatch",
//! "layers") and the window names our command, says where it lives on the ribbon and its keys,
//! runs it or shows its button with a ring. Matches the command names in both languages and a
//! vocabulary of common names for the same thing (`whereis.<id>`, comma separated).

use crate::shell::EditorApp;
use crate::ui_kit as kit;
use ddc_ui::theme::{space, stroke};
use ddc_ui::widgets::{self, TextField};
use ddc_ui::{Catalog, Command, Lang, Registry};
use std::cell::RefCell;

/// Commands with a vocabulary of other names (`whereis.<id>` in every language).
pub const VOCAB: &[&str] = &[
    "arch.floor", "arch.component", "arch.refplane", "arch.room", "arch.zone", "arch.opening",
    "arch.curtain_wall", "arch.shaft", "arch.section", "str.rebar", "str.footing", "mep.duct",
    "mep.pipe", "mep.tray", "mod.group", "mod.isolate", "mod.match", "mod.offset", "mod.trim",
    "mod.array", "mod.measure", "mod.align", "mod.pin", "mod.select_similar", "mod.change_type",
    "mgr.object_styles", "mgr.materials", "mgr.params", "mgr.purge", "mgr.coords", "mgr.links",
    "mgr.import_cad", "mgr.import_pdf", "mgr.phases", "mgr.units", "mgr.snaps", "mgr.workplane",
    "mgr.keymap", "data.table", "data.export_xlsx", "data.export_ifc", "data.clash",
    "data.check", "data.compare", "data.classify", "ann.dim", "ann.tag", "ann.text",
    "ann.cloud", "ann.region", "ann.detail_line", "ann.spot_elev", "app.history",
];
/// Hits shown at most.
const MAX_HITS: usize = 12;
/// How long the ring stays on a shown button, s.
const RING_S: f64 = 3.0;

#[derive(Default)]
pub struct Win {
    pub open: bool,
    pub query: String,
    /// Button to ring and until when (input time, s).
    pub ring: Option<(String, f64)>,
    focus: bool,
}

thread_local! {
    static WIN: RefCell<Win> = RefCell::new(Win::default());
}

/// What the window asks for this frame.
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    Run(String),
    Show(String),
}

fn terms(cat: &Catalog, lang: Lang, id: &str) -> Vec<String> {
    let k = format!("whereis.{id}");
    let v = cat.t_in(lang, &k);
    if v == k {
        return vec![];
    }
    v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

/// Commands matching `q`, best first: names in both languages, then the vocabulary.
pub fn find<'a>(reg: &'a Registry, cat: &Catalog, q: &str) -> Vec<&'a Command> {
    let q = q.trim();
    if q.is_empty() {
        return vec![];
    }
    let mut v: Vec<(i32, &Command)> = reg
        .all()
        .iter()
        .filter(|c| c.alias.is_none())
        .filter_map(|c| {
            let key = c.label_key();
            let names = [cat.t(&key), cat.t_in(Lang::En, &key), cat.t_in(Lang::Ru, &key)];
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            let mut words = terms(cat, Lang::En, &c.id);
            words.extend(terms(cat, Lang::Ru, &c.id));
            let words: Vec<&str> = words.iter().map(String::as_str).collect();
            // a name a person reads beats a word from the vocabulary at the same score
            let s = ddc_ui::fuzzy::best(q, &names).map(|s| s + 10);
            let s = s.max(ddc_ui::fuzzy::best(q, &words))?;
            Some((s, c))
        })
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.id.cmp(&b.1.id)));
    v.into_iter().take(MAX_HITS).map(|x| x.1).collect()
}

/// Where the command is: "Architecture > Walls", the contextual tab, or the app bar.
pub fn place(cat: &Catalog, c: &Command) -> String {
    match &c.tab {
        Some(t) => {
            let tab = cat.t(&format!("tab.{t}"));
            let gk = format!("rgroup.{}", widgets::ribbon::group_name(c));
            let g = cat.t(&gk);
            if g == gk {
                tab
            } else {
                cat.tf("where.path", &[("tab", &tab), ("group", &g)])
            }
        }
        None if !c.context.is_empty() => cat.t("where.context"),
        None => cat.t("where.app"),
    }
}

/// Keys of the command: shortcuts, then the two-letter mnemonic; empty when it has none.
pub fn keys(c: &Command) -> String {
    let mut k: Vec<String> = c.parsed_shortcuts().iter().map(|s| s.to_string()).collect();
    if let Some(m) = &c.mnemonic {
        k.push(m.clone());
    }
    k.join(" \u{b7} ")
}

/// Window body (apart from the frame, for the layout probes).
pub fn body(ui: &mut egui::Ui, w: &mut Win, reg: &Registry, cat: &Catalog) -> Option<Act> {
    let th = ddc_ui::theme::of(ui.ctx());
    let mut act = None;
    ui.add(egui::Label::new(cat.t("where.lead")).wrap());
    ui.add_space(space::XS);
    let label = cat.t("where.search");
    let out = TextField::new(&mut w.query)
        .labelled_by(egui::Id::new("where.q.label"), &label)
        .hint(&label)
        .id_salt("where.q")
        .show(ui, &th);
    if std::mem::take(&mut w.focus) {
        out.response.request_focus();
    }
    let enter = out.response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    ui.add_space(space::SM);
    let hits = find(reg, cat, &w.query);
    if enter {
        act = hits.first().map(|c| Act::Run(c.id.clone()));
    }
    if w.query.trim().is_empty() {
        kit::empty_note(ui, cat.t("where.empty"), Some(&cat.t("where.examples")));
    } else if hits.is_empty() {
        kit::empty_note(ui, cat.t("where.nothing"), Some(&cat.t("where.examples")));
    }
    for c in hits {
        kit::section_title(ui, cat.t(&c.label_key()));
        ui.add(egui::Label::new(place(cat, c)).wrap());
        let k = keys(c);
        if !k.is_empty() {
            ui.label(cat.tf("where.keys", &[("keys", &k)]));
        }
        ui.horizontal(|ui| {
            if c.tab.is_some() && kit::secondary_button(ui, cat.t("where.show")).clicked() {
                act = Some(Act::Show(c.id.clone()));
            }
            if kit::secondary_button(ui, cat.t("where.run")).clicked() {
                act = Some(Act::Run(c.id.clone()));
            }
        });
        ui.add_space(space::XS);
    }
    act
}

/// Applies `act`; `true` closes the window.
pub fn apply(a: &mut EditorApp, w: &mut Win, act: Act, now: f64) -> bool {
    match act {
        Act::Run(id) => {
            a.run(&id);
            true
        }
        Act::Show(id) => {
            if let Some(t) = a.sh.reg.get(&id).and_then(|c| c.tab.clone()) {
                let known = a.sh.reg.tabs();
                a.sh.ribbon.set_tab(&t, &known);
            }
            w.ring = Some((id, now + RING_S));
            false
        }
    }
}

fn ring(ctx: &egui::Context, w: &mut Win) {
    let Some((id, until)) = w.ring.clone() else {
        return;
    };
    let now = ctx.input(|i| i.time);
    if now > until {
        w.ring = None;
        return;
    }
    if let Some(r) = widgets::ribbon::button_rect(ctx, &id) {
        let th = ddc_ui::theme::of(ctx);
        let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("ddc.where.ring"));
        ctx.layer_painter(layer).rect_stroke(
            r.expand(space::XS),
            th.radius("md"),
            egui::Stroke::new(stroke::FOCUS, th.sem().accent),
            egui::StrokeKind::Outside,
        );
    }
    ctx.request_repaint();
}

fn run(_: &mut EditorApp) {
    WIN.with(|w| {
        let mut w = w.borrow_mut();
        w.open = true;
        w.focus = true;
    });
}

pub fn open_for_layout(query: &str) {
    WIN.with(|w| *w.borrow_mut() = Win { open: true, query: query.into(), ..Win::default() });
}

/// Called every frame.
pub fn frame(a: &mut EditorApp, ctx: &egui::Context) {
    let mut w = WIN.with(|w| std::mem::take(&mut *w.borrow_mut()));
    ring(ctx, &mut w);
    if w.open {
        let mut open = true;
        let mut act = None;
        kit::Dialog::new("where_is", a.sh.cat.t("cmd.app.where_is"), kit::DialogSize::Tool)
            .open(&mut open)
            .scroll_body()
            .show(ctx, |ui| act = body(ui, &mut w, &a.sh.reg, &a.sh.cat));
        if let Some(x) = act {
            let now = ctx.input(|i| i.time);
            if apply(a, &mut w, x, now) {
                open = false;
            }
        }
        w.open = open;
    }
    WIN.with(|s| *s.borrow_mut() = w);
}

pub fn register(a: &mut EditorApp) {
    a.register_command("app.where_is", run);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first(reg: &Registry, cat: &Catalog, q: &str) -> String {
        find(reg, cat, q).first().map(|c| c.id.clone()).unwrap_or_default()
    }

    #[test]
    fn common_names_find_our_command_in_both_languages() {
        let reg = Registry::builtin();
        let en = Catalog::new(Lang::En);
        let ru = Catalog::new(Lang::Ru);
        assert_eq!(first(&reg, &en, "schedule"), "view.schedule");
        assert_eq!(first(&reg, &en, "quantity takeoff"), "data.table");
        assert_eq!(first(&reg, &en, "layers"), "mgr.object_styles");
        assert_eq!(first(&reg, &en, "hatch"), "ann.region");
        assert_eq!(first(&reg, &en, "revision cloud"), "ann.cloud");
        let hatch = "\u{448}\u{442}\u{440}\u{438}\u{445}\u{43e}\u{432}\u{43a}\u{430}";
        assert_eq!(first(&reg, &ru, hatch), "ann.region");
        // an English word typed with the Russian interface still finds it
        assert_eq!(first(&reg, &ru, "schedule"), "view.schedule");
        assert!(find(&reg, &en, "  ").is_empty());
    }

    #[test]
    fn the_place_and_keys_are_said() {
        let reg = Registry::builtin();
        let cat = Catalog::new(Lang::En);
        let wall = reg.get("arch.wall").unwrap();
        let p = place(&cat, wall);
        assert!(p.contains(&cat.t("tab.arch")), "{p}");
        let fit = reg.get("ctx.mep.fitting").unwrap();
        assert_eq!(place(&cat, fit), cat.t("where.context"));
        let pal = reg.get("app.palette").unwrap();
        assert_eq!(place(&cat, pal), cat.t("where.app"));
        assert!(keys(pal).contains("Ctrl+K"), "{}", keys(pal));
        assert!(keys(reg.get("app.where_is").unwrap()).contains("Ctrl+Shift+K"));
    }

    #[test]
    fn show_switches_the_tab_and_run_runs() {
        let mut a = crate::layout_contract::app_in(Lang::En);
        let mut w = Win::default();
        assert!(!apply(&mut a, &mut w, Act::Show("data.table".into()), 1.0));
        let tab = a.sh.reg.get("data.table").unwrap().tab.clone().unwrap();
        assert_eq!(a.sh.ribbon.tab, tab);
        assert_eq!(w.ring, Some(("data.table".to_string(), 1.0 + RING_S)));
        assert!(apply(&mut a, &mut w, Act::Run("mod.explain".into()), 1.0));
        assert!(a.run("app.where_is"));
        assert!(WIN.with(|w| w.borrow().open));
        WIN.with(|w| w.borrow_mut().open = false);
    }

    #[test]
    fn every_vocabulary_names_a_command_in_both_languages() {
        let reg = Registry::builtin();
        for id in VOCAB {
            assert!(reg.get(id).is_some(), "{id}");
        }
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            for id in VOCAB {
                assert!(!terms(&cat, lang, id).is_empty(), "{lang:?} {id}");
            }
            for k in [
                "where.lead", "where.search", "where.empty", "where.examples", "where.nothing",
                "where.path", "where.context", "where.app", "where.keys", "where.show",
                "where.run",
            ] {
                assert_ne!(cat.t(k), k, "{lang:?} {k}");
            }
        }
    }
}
