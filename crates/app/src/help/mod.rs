//! Help with no external documents: an F1 window with search and a context topic (`panel`), the
//! "First steps" tutorial (`tutorial`), "Open sample" (`sample`), "What's new".
//! Shell hooks: `register` in `shell::REGISTER`, `frame` in `EditorApp::ui`.
pub mod first_card;
pub mod panel;
pub mod refdock;
pub mod sample;
pub mod tutorial;

use crate::shell::EditorApp;
use crate::ui_kit as kit;
use ddc_ui::help::{self as h, Library};
use ddc_ui::notify::Level;
use ddc_ui::Lang;
use std::cell::RefCell;

#[derive(Default)]
struct State {
    panel: panel::Panel,
    tour: tutorial::Tour,
    news: bool,
    lib: Option<Library>,
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State::default());
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    ST.with(|s| f(&mut s.borrow_mut()))
}

/// Open the help window with the search box filled.
pub fn search(q: &str) {
    with(|s| {
        s.panel.open = true;
        s.panel.query = q.to_string();
        s.panel.focus_search();
    })
}

pub fn register(app: &mut EditorApp) {
    app.register_command("app.help", |_| {
        with(|s| {
            s.panel.open = true;
            s.panel.want_context = true;
        })
    });
    app.register_command("help.tutorial", |_| with(|s| s.tour.open()));
    app.register_command("help.sample", |app| open_sample(app));
    app.register_command("help.whats_new", |_| with(|s| s.news = true));
    app.register_command("help.reference", |_| refdock::toggle());
}

/// Context keys for F1 by importance: the button under the cursor, tool, selection, view, general.
pub fn context_keys(app: &EditorApp, hovered: Option<String>) -> Vec<String> {
    let mut k: Vec<String> = hovered.into_iter().collect();
    if let Some(c) = app.active_command() {
        k.push(c.to_string());
    }
    if let Some((cat, _)) = app.selection_category() {
        k.push(format!("sel:{cat}"));
    }
    let view = if crate::sheet_view::active().is_some() {
        "view:sheet"
    } else if app.section.is_some() {
        "view:section"
    } else if app.view3d {
        "view:3d"
    } else {
        "view:plan"
    };
    k.push(view.into());
    k.push("app.help".into());
    k
}

/// The ribbon command under the cursor (its button in the previous frame).
fn hovered_command(app: &EditorApp, ctx: &egui::Context) -> Option<String> {
    let p = ctx.pointer_hover_pos()?;
    app.sh
        .reg
        .all()
        .iter()
        .find(|c| ddc_ui::widgets::ribbon::button_rect(ctx, &c.id).is_some_and(|r| r.contains(p)))
        .map(|c| app.sh.reg.resolve(&c.id).to_string())
}

/// Help, tutorial and sample windows per frame; commands from them run after the frame.
pub fn frame(app: &mut EditorApp, ctx: &egui::Context) {
    first_card::frame(app, ctx);
    let lang = app.sh.cat.lang;
    let mut st = ST.with(|s| std::mem::take(&mut *s.borrow_mut()));
    if !matches!(&st.lib, Some(l) if l.lang == lang) {
        st.lib = Some(Library::builtin(lang));
    }
    let mut acts: Vec<panel::Act> = vec![];
    // F1 during a tool: the keys of its current step, from the hint table
    let mut tool_topic = None;
    if st.panel.want_context {
        if let (Some(t), Some(lib)) = (crate::interact::hints::help_topic(&app.ed), st.lib.as_mut()) {
            lib.topics.retain(|x| x.id != t.id);
            tool_topic = Some(t.id.clone());
            lib.topics.push(t);
        }
    }
    if let Some(lib) = &st.lib {
        if std::mem::take(&mut st.panel.want_context) {
            let keys = context_keys(app, hovered_command(app, ctx));
            let id = tool_topic
                .or_else(|| lib.for_keys(keys.iter().map(String::as_str)).map(|t| t.id.clone()));
            if let Some(id) = id {
                st.panel.show_topic(&id);
            }
            st.panel.query.clear();
            st.panel.focus_search();
        }
        panel::show(ctx, &mut st.panel, lib, &app.sh.cat, &app.sh.reg, &mut acts);
    }
    if st.tour.open {
        let n = st.tour.snap(&app.ed.model, app.ed.revision, app.ed.level);
        let now = tutorial::Snap {
            n,
            view3d: app.view3d,
            active: app.active_command().map(String::from),
            sel: app.selection_category().map(|c| c.0),
            palette: app.sh.palette.open,
            xlsx: tutorial::xlsx_counts(),
        };
        st.tour.tick(&now);
        if std::mem::take(&mut st.tour.entered) {
            let tab = st
                .tour
                .current()
                .and_then(|s| s.cmd)
                .and_then(|id| app.sh.reg.get(id))
                .and_then(|c| c.tab.clone());
            if let Some(tab) = tab {
                let known = app.sh.reg.tabs();
                app.sh.ribbon.set_tab(&tab, &known);
            }
        }
        if let tutorial::Out::Run(id) = tutorial::show(ctx, &mut st.tour, &app.sh.cat, &app.sh.reg)
        {
            acts.push(panel::Act::Run(id));
        }
    }
    if st.news {
        news(ctx, &mut st.news, lang, app, &mut acts);
    }
    ST.with(|s| *s.borrow_mut() = st);
    for a in acts {
        match a {
            panel::Act::Run(id) => {
                app.run(&id);
            }
        }
    }
}

pub(crate) fn news(
    ctx: &egui::Context,
    open: &mut bool,
    lang: Lang,
    app: &EditorApp,
    acts: &mut Vec<panel::Act>,
) {
    let t = h::whats_new(lang);
    let lib = Library::from_files(lang, &[]);
    let c = &app.sh.cat;
    let mut closed = false;
    kit::Dialog::new(
        "help_news",
        c.t("cmd.help.whats_new"),
        kit::DialogSize::Tool,
    )
    .stable()
    .open(open)
    .show(ctx, |ui| {
        kit::pinned_footer(ui, |ui, footer| {
            if footer {
                closed |= kit::cancel_button(ui, c.t("dlg.close")).clicked();
            } else {
                panel::body(ui, &t, &lib, c, &app.sh.reg, acts);
            }
        });
    });
    if closed {
        *open = false;
    }
}

/// Replace the model with the sample, after asking about unsaved edits; refuses while a save or
/// open is busy. "Something to lose" is decided by the journal (`Editor::is_dirty`): it sees every table.
pub fn open_sample(app: &mut EditorApp) {
    crate::project_ui::request_sample(app);
}

/// The replacement itself: only from `project_ui` (the question is already asked). Lowest-level plan, view fit to size.
pub(crate) fn load_sample(app: &mut EditorApp) {
    let ru = app.sh.cat.lang == Lang::Ru;
    let r = sample::build(ru).and_then(|m| {
        let level = sample::first_level(&m);
        app.ed.adopt_untitled(m, level)
    });
    match r {
        Ok(()) => {
            crate::project_ui::after_swap(app);
            first_card::offer(app);
            let msg = app.sh.cat.t("help.sample.opened");
            app.sh.toasts.push(Level::Info, msg, None);
        }
        Err(e) => {
            let e = e.to_string();
            let msg = app.sh.cat.tf("help.sample.failed", &[("e", e.as_str())]);
            app.sh.toasts.push(Level::Warning, msg, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::{Catalog, Registry};

    #[test]
    fn help_commands_and_strings_exist() {
        let reg = Registry::builtin();
        for id in ["app.help", "help.tutorial", "help.sample", "help.whats_new"] {
            assert!(reg.get(id).is_some(), "{id}");
        }
        for lang in [Lang::Ru, Lang::En] {
            let c = Catalog::new(lang);
            for k in [
                "help.title",
                "help.search",
                "help.back",
                "help.empty",
                "help.commands",
                "help.see",
                "help.sec.steps",
                "help.sec.keys",
                "help.sec.tips",
                "help.cancel",
                "help.sample.replace",
                "help.sample.open",
                "help.sample.opened",
                "help.sample.failed",
                "cmd.help.tutorial",
                "cmd.help.sample",
                "cmd.help.whats_new",
            ] {
                assert_ne!(c.t_in(lang, k), k, "{lang:?} {k}");
            }
        }
    }

    #[test]
    fn f1_topic_follows_tool_selection_and_view() {
        let lib = Library::builtin(Lang::Ru);
        let pick = |keys: &[&str]| lib.for_keys(keys.iter().copied()).map(|t| t.id.clone());
        assert_eq!(
            pick(&["arch.wall", "view:plan", "app.help"]).as_deref(),
            Some("walls")
        );
        assert_eq!(
            pick(&["sel:opening", "view:plan"]).as_deref(),
            Some("doors_windows")
        );
        assert_eq!(pick(&["view:3d", "app.help"]).as_deref(), Some("views_3d"));
        assert_eq!(pick(&["nope", "app.help"]).as_deref(), Some("interface"));
        // every wired command with a ribbon tab finds a topic via @context or a mention
        let reg = Registry::builtin();
        let missing: Vec<&str> = crate::shell::WIRED
            .iter()
            .copied()
            .filter(|id| reg.get(id).is_some_and(|c| c.tab.is_some()))
            .filter(|id| lib.for_context(id).is_none() && lib.mentioning(id).is_empty())
            .collect();
        assert!(missing.is_empty(), "missing help: {missing:?}");
    }
}
