//! State of the window chrome (start screen, settings, tasks, docks, tree rename) kept as one
//! field of [`EditorApp`], and the status-line to toast bridge both frame paths share.
use super::dock_ui::DockUi;
use super::settings_win::SettingsUi;
use super::start_win::StartUi;
use super::tasks_ui::TasksUi;
use super::tree_ux::TreeUx;
use super::EditorApp;
use ddc_ui::notify::Level;
use ddc_ui::theme::space;
use ddc_ui::widgets::layout_cards::{self as lc, PickOut, Picker, TitleFit};
use ddc_ui::workspace::{ViewArrangement, Workspace, PRESET_IDS};
use ddc_ui::Lang;

/// Command behind the layout toast's button and `Shift+F4`.
pub const LAYOUT_BACK: &str = "ui.layout.back";

#[derive(Default)]
pub struct Chrome {
    pub start: StartUi,
    pub settings: SettingsUi,
    pub tasks: TasksUi,
    /// The loading window of opens and imports.
    pub load: super::load_win::LoadWin,
    pub dock: DockUi,
    /// Folded chrome of this session: clean screen, the hidden ribbon shown, rail holds.
    pub fold: super::fold::FoldUi,
    pub tree: TreeUx,
    /// Room edit buffers: (id, model revision, number, name); undo drops them by revision.
    pub room_edit: Option<(i64, u64, String, String)>,
    /// Crash note of the open recovery dialog, read once.
    pub recover_note: Option<Option<std::path::PathBuf>>,
    /// `Editor::status_seq` already turned into a toast.
    pub seen_seq: u64,
    /// Model journal head at the last status check; a move means an op committed.
    pub seen_head: u64,
    /// Hash of (selection, revision) the ribbon block list was computed for.
    pub caps_key: Option<u64>,
    /// Layout popover under the title bar button.
    pub mode: Picker,
    /// Title bar geometry of the last frame (search field, work-mode group, layout button).
    pub title: Option<TitleGeom>,
    /// Full-screen wanted (F11) and what was last sent to the window.
    pub full: bool,
    pub full_sent: bool,
    /// Work mode whose panel layout is on screen (None before the first frame).
    pub work_mode: Option<super::mode_layout::WorkMode>,
    /// «Save layout as…» name being typed on the interface page.
    pub layout_name: String,
    /// The setup wizard (first start, or on demand).
    pub onboard: super::onboard::Wizard,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TitleGeom {
    pub fit: TitleFit,
    pub left_end: f32,
    pub button: egui::Rect,
}

/// Strings of the work-mode group, built once per frame.
pub struct ModeText {
    label: [String; 3],
    title: [String; 3],
    desc: [String; 3],
    keys: [String; 3],
    icon: String,
    tip: String,
    tip_desc: String,
    cycle: String,
}

impl ModeText {
    fn segs(&self) -> [lc::Seg<'_>; 3] {
        let icons = ["plan-view", "3d-view", "layout-split"];
        std::array::from_fn(|i| lc::Seg {
            icon: icons[i],
            label: &self.label[i],
            title: &self.title[i],
            desc: &self.desc[i],
            keys: &self.keys[i],
        })
    }
}

fn is_split(ws: &Workspace) -> bool {
    matches!(ws.views, ViewArrangement::SplitPlan3d { .. })
}

/// Severity of a status line. The lines are plain text from everywhere, so words decide.
fn status_level(m: &str) -> Level {
    let low = m.to_lowercase();
    let bad = low
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .any(|w| {
            matches!(
                w,
                "\u{43d}\u{435}" | "\u{43d}\u{435}\u{442}" | "\u{43d}\u{435}\u{43b}\u{44c}\u{437}\u{44f}" | "not" | "cannot" | "can't" | "failed" | "error"
            ) || w.starts_with("\u{43e}\u{448}\u{438}\u{431}")
                || w.starts_with("\u{43d}\u{435}\u{432}\u{43e}\u{437}\u{43c}\u{43e}\u{436}")
        });
    if bad {
        Level::Warning
    } else {
        Level::Info
    }
}

impl EditorApp {
    /// Display name: the catalog's for presets, the saved name for custom layouts.
    pub fn layout_name(&self, ws: &Workspace) -> String {
        let k = format!("workspace.name.{}", ws.id);
        let t = self.sh.cat.t(&k);
        if t == k {
            ws.name.clone()
        } else {
            t
        }
    }

    /// Preset a layout is, or is derived from.
    fn preset_of(ws: &Workspace) -> Option<usize> {
        let id = if PRESET_IDS.contains(&ws.id.as_str()) {
            ws.id.as_str()
        } else {
            ws.base.as_deref()?
        };
        PRESET_IDS.iter().position(|p| *p == id)
    }

    fn layout_purpose(&self, ws: &Workspace) -> String {
        if PRESET_IDS.contains(&ws.id.as_str()) {
            self.sh.cat.t(&format!("workspace.purpose.{}", ws.id))
        } else {
            self.sh.cat.t("workspace.purpose.custom")
        }
    }

    fn layout_icon(&self, ws: &Workspace) -> String {
        Self::preset_of(ws)
            .and_then(|i| self.sh.reg.get(&format!("ui.layout.set.{}", i + 1)))
            .map(|c| c.icon.clone())
            .filter(|i| !i.is_empty())
            .unwrap_or_else(|| "layout-settings".into())
    }

    fn layout_keys(&self, ws: &Workspace) -> String {
        match PRESET_IDS.iter().position(|p| *p == ws.id) {
            Some(i) => crate::quick::keys_text(self, &format!("ui.layout.set.{}", i + 1)),
            None => String::new(),
        }
    }

    pub fn layout_index(&self) -> usize {
        self.layouts
            .iter()
            .position(|w| w.id == self.layout)
            .unwrap_or(0)
    }

    /// Selected view segment: plan, 3D, or plan + 3D (the split layout).
    pub fn view_mode(&self) -> usize {
        if is_split(&self.workspace()) {
            2
        } else {
            usize::from(self.view3d)
        }
    }

    pub(super) fn mode_text(&self) -> ModeText {
        let c = &self.sh.cat;
        let ws = self.workspace();
        let name = self.layout_name(&ws);
        let k = |id: &str| crate::quick::keys_text(self, id);
        ModeText {
            label: ["workspace.view.plan", "workspace.view.3d", "workspace.view.split"].map(|x| c.t(x)),
            title: ["workspace.view.plan.tip", "workspace.view.3d.tip", "workspace.view.split.tip"].map(|x| c.t(x)),
            desc: ["workspace.view.plan.desc", "workspace.view.3d.desc", "workspace.view.split.desc"]
                .map(|x| c.t(x)),
            keys: [k("view.plan"), k("view.3d"), k("ui.layout.set.3")],
            icon: self.layout_icon(&ws),
            tip: c.tf("workspace.layout.tip", &[("name", &name)]),
            tip_desc: c.t("workspace.layout.desc"),
            cycle: k("ui.layout.next"),
        }
    }

    /// Where the search field and the work-mode group go in a title bar `bar` whose menus end
    /// at `left_end`.
    pub(super) fn title_fit(
        &self,
        ui: &egui::Ui,
        t: &ModeText,
        bar: egui::Rect,
        left_end: f32,
    ) -> TitleFit {
        let th = &self.sh.theme;
        let w = lc::group_widths(ui, th, &t.segs());
        let h = th.d().control + space::XS;
        lc::title_fit(bar, left_end, w, th.tokens.breakpoint.icons_below, h)
    }

    /// Right end of the title bar: view segments and the layout button, then its popover.
    pub(super) fn title_mode(
        &mut self,
        ui: &mut egui::Ui,
        t: &ModeText,
        fit: TitleFit,
        left_end: f32,
        fire: &mut Vec<String>,
    ) {
        let segs = t.segs();
        let sel = self.view_mode();
        let open = self.chrome.mode.open;
        let enabled = !self.project_ui.modal();
        let btn = lc::LayoutBtn {
            icon: &t.icon,
            title: &t.tip,
            desc: &t.tip_desc,
            keys: &t.cycle,
        };
        let s = &mut self.sh;
        let (mut view, mut clicked, mut button) = (None, false, egui::Rect::NOTHING);
        let b = egui::UiBuilder::new()
            .max_rect(fit.group)
            .layout(egui::Layout::left_to_right(egui::Align::Center));
        ui.scope_builder(b, |ui| {
            ui.add_enabled_ui(enabled, |ui| {
                let g = lc::mode_group(ui, &s.theme, &mut s.icons, &segs, sel, &btn, open, fit.fit);
                view = g.view;
                clicked = g.button.clicked();
                button = g.button.rect;
            });
        });
        if let Some(cmd) = view.and_then(|i| self.pick_view(i)) {
            fire.push(cmd.into());
        }
        if clicked {
            let cur = self.layout_index();
            self.chrome.mode.toggle(cur);
        }
        if !enabled {
            self.chrome.mode.close();
        }
        self.chrome.title = Some(TitleGeom {
            fit,
            left_end,
            button,
        });
        let ctx = ui.ctx().clone();
        self.mode_popover(&ctx, button, fire);
    }

    /// A view segment was picked: plan and 3D leave the split layout, plan + 3D enters it.
    /// Returns the command to run.
    pub fn pick_view(&mut self, i: usize) -> Option<&'static str> {
        match i {
            0 | 1 => {
                self.leave_split();
                Some(if i == 0 { "view.plan" } else { "view.3d" })
            }
            2 => Some("ui.layout.set.3"),
            _ => None,
        }
    }

    /// From the split layout back to the one used before it (classic when there was none).
    fn leave_split(&mut self) {
        if !is_split(&self.workspace()) {
            return;
        }
        let back = self
            .layout_prev
            .clone()
            .filter(|p| self.layouts.iter().any(|w| w.id == *p && !is_split(w)))
            .unwrap_or_else(|| PRESET_IDS[0].into());
        self.set_layout(&back);
    }

    fn mode_popover(&mut self, ctx: &egui::Context, anchor: egui::Rect, fire: &mut Vec<String>) {
        if !self.chrome.mode.open {
            return;
        }
        let rows: Vec<(String, String, String)> = self
            .layouts
            .iter()
            .map(|w| (self.layout_name(w), self.layout_purpose(w), self.layout_keys(w)))
            .collect();
        let c = &self.sh.cat;
        let (title, cycle, current, settings) = (
            c.t("workspace.pop.title"),
            c.t("workspace.pop.cycle"),
            c.t("workspace.pop.current"),
            c.t("workspace.pop.settings"),
        );
        let cycle_keys = crate::quick::keys_text(self, "ui.layout.next");
        let cur = self.layout_index();
        let out = {
            let cards: Vec<lc::Card> = self
                .layouts
                .iter()
                .zip(&rows)
                .map(|(ws, r)| lc::Card {
                    ws,
                    name: &r.0,
                    purpose: &r.1,
                    keys: &r.2,
                })
                .collect();
            let text = lc::PopText {
                title: &title,
                cycle: &cycle,
                cycle_keys: &cycle_keys,
                current: &current,
                settings: &settings,
            };
            let s = &mut self.sh;
            lc::popover(
                ctx,
                &s.theme,
                &mut s.icons,
                anchor,
                &cards,
                cur,
                &mut self.chrome.mode,
                &text,
            )
        };
        match out {
            Some(lc::PopOut::Pick(i)) => {
                if let Some(id) = self.layouts.get(i).map(|w| w.id.clone()) {
                    self.set_layout(&id);
                }
            }
            Some(lc::PopOut::Settings) => fire.push("ui.layout.settings".into()),
            None => {}
        }
    }

    /// Popover keys, taken before tools and views see them (Esc must not cancel a tool).
    pub(super) fn mode_keys(&mut self, ctx: &egui::Context) {
        if !self.chrome.mode.open {
            return;
        }
        if self.chrome.start.open || self.project_ui.modal() {
            self.chrome.mode.close();
            return;
        }
        let n = self.layouts.len();
        for k in lc::take_picker_keys(ctx) {
            if let PickOut::Picked(i) = self.chrome.mode.key(k, n) {
                if let Some(id) = self.layouts.get(i).map(|w| w.id.clone()) {
                    self.set_layout(&id);
                }
            }
        }
    }

    /// One toast per switch: the new layout and the key that returns, with a button for it.
    pub(super) fn layout_toast(&mut self) {
        let name = self.layout_name(&self.workspace());
        let back = crate::quick::keys_text(self, LAYOUT_BACK);
        let c = &self.sh.cat;
        let text = if back.is_empty() {
            c.tf("workspace.toast", &[("name", &name)])
        } else {
            c.tf("workspace.toast_keys", &[("name", &name), ("keys", &back)])
        };
        let label = c.t("workspace.back");
        self.sh
            .toasts
            .live
            .retain(|t| t.command.as_deref() != Some(LAYOUT_BACK));
        self.sh.toasts.push_cmd(Level::Info, text, label, LAYOUT_BACK);
    }

    /// A new status line becomes a toast, once.
    /// A step toast whose entry is no longer the one its button would step (a later edit
    /// came on top, or the entry left the stack) keeps its message and loses the button.
    fn drop_stale_steps(&mut self) {
        let (undo, redo) = (self.ed.top_entry(), self.ed.redo_entry_top());
        for t in &mut self.sh.toasts.live {
            let top = match t.command.as_deref() {
                Some("edit.undo") => undo,
                Some("edit.redo") => redo,
                _ => continue,
            };
            if t.entry.is_none() || t.entry != top {
                (t.action, t.command, t.entry) = (None, None, None);
            }
        }
    }

    pub(super) fn status_to_toast(&mut self) {
        // the step is over: a room warning a later status assignment replaced comes back
        self.ed.settle_status();
        self.drop_stale_steps();
        let offered = crate::elem::panel_rule::take_offer();
        let fresh = self.ed.status_seq != self.chrome.seen_seq;
        self.chrome.seen_seq = self.ed.status_seq;
        let head = self.ed.model.change_head();
        let moved = head != self.chrome.seen_head;
        self.chrome.seen_head = head;
        if moved {
            crate::ai::pin_toast::after_change(self);
        }
        // Undo only for the entry the reported op wrote, and only while it is still on top
        let undo = std::mem::take(&mut self.ed.undo_offer)
            .filter(|e| moved && self.ed.top_entry() == Some(*e));
        let cmd = self.ed.status_cmd.take();
        let entry = self.ed.status_entry.take();
        if cmd.is_none() && self.ed.status == self.last_status && !fresh {
            return;
        }
        if let Some(m) = &self.ed.status {
            let lvl = self
                .ed
                .status_severity()
                .map_or_else(|| status_level(m), |s| s.level());
            let ru = self.sh.cat.lang == Lang::Ru;
            let text = crate::ui_err::humanize(m, ru);
            if let Some(e) = undo {
                let label = self.undo_label();
                self.sh
                    .toasts
                    .push_step(lvl, text, label, "edit.undo", Some(e));
            } else if offered {
                let label = self.sh.cat.t("elem.panel_select_wall");
                self.sh
                    .toasts
                    .push_cmd(lvl, text, label, crate::elem::panel_rule::CMD);
            } else {
                match cmd {
                    // Redo after Undo (and back): the toast steps the entry it reports
                    Some(c) if ddc_ui::notify::STEP_CMDS.contains(&c) => {
                        let label = self.sh.cat.t(&format!("undo.action.{c}"));
                        self.sh.toasts.push_step(lvl, text, label, c, entry)
                    }
                    Some(c) => {
                        let label = self.sh.cat.t(&format!("undo.action.{c}"));
                        self.sh.toasts.push_cmd(lvl, text, label, c)
                    }
                    None => self.sh.toasts.push(lvl, text, None),
                }
            }
        }
        self.last_status = self.ed.status.clone();
    }

    /// «Undo (Ctrl+Z)» for a toast button, the key as the person set it.
    pub(crate) fn undo_label(&self) -> String {
        let keys = crate::quick::keys_text(self, "edit.undo");
        let c = &self.sh.cat;
        if keys.is_empty() {
            c.t("undo.action")
        } else {
            c.tf("undo.action_keys", &[("keys", &keys)])
        }
    }

    /// A toast button stepped history entry `entry`: exactly that one, or a refusal.
    pub(crate) fn step_toast_entry(&mut self, undo: bool, entry: u64) {
        let ok = if undo {
            self.ed.undo_entry(entry)
        } else {
            self.ed.redo_entry(entry)
        };
        if ok {
            self.plan.drawings.sync_undo(&mut self.ed);
        }
    }

    /// Disables the transform buttons the selection cannot take, with the reason as tooltip.
    /// Recomputed only when the selection or the model changed.
    pub(super) fn sync_caps_block(&mut self) {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.ed.selection.hash(&mut h);
        self.ed.revision.hash(&mut h);
        self.ed.grip.focus.len().hash(&mut h);
        let key = h.finish();
        if self.chrome.caps_key == Some(key) {
            return;
        }
        self.chrome.caps_key = Some(key);
        self.sh.ribbon.blocked.clear();
        use crate::modify_tools::Op;
        for op in [Op::Move, Op::Copy, Op::Rotate, Op::Mirror, Op::MirrorDraw, Op::Array] {
            if let Some(tip) = crate::elem::caps::blocked_tip(&self.ed, op) {
                self.sh.ribbon.blocked.insert(op.command().to_string(), tip);
            }
        }
        let ru = self.sh.cat.lang == ddc_ui::i18n::Lang::Ru;
        for cmd in crate::modify_tools::NEEDS_SELECTION {
            if let Some(tip) = crate::modify_tools::select_first_tip(&self.ed, cmd, ru) {
                self.sh.ribbon.blocked.insert(cmd.to_string(), tip);
            }
        }
    }

    /// Resolves `prefs.lang` (`auto` = OS language) and sets it for the catalog and for `tr()` together.
    pub fn apply_lang(&mut self) {
        let lang = self.prefs.ui_lang(super::settings_win::system_lang());
        self.set_lang(lang);
    }

    /// One place that changes the UI language: catalog lookups and `crate::tr` always agree.
    pub fn set_lang(&mut self, lang: ddc_ui::i18n::Lang) {
        self.sh.cat.lang = lang;
        crate::set_ui_lang(lang);
    }

    /// Saved preferences that live outside `Prefs::apply`: language, scale, ribbon state.
    /// Called on a real start only, so tests and screenshots keep their defaults.
    pub fn apply_startup_prefs(&mut self) {
        self.apply_lang();
        self.sh.theme.set_scale(self.prefs.ui_scale);
        self.sh.ribbon.collapsed = self.prefs.ribbon_collapsed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> EditorApp {
        EditorApp::new(crate::editor::Editor::new_in_memory().unwrap(), None, "test".into())
    }

    #[test]
    fn an_identical_status_toasts_again() {
        let mut a = app();
        a.ed.say(crate::status::Severity::Refusal, "same");
        a.status_to_toast();
        a.status_to_toast();
        assert_eq!(a.sh.toasts.live.len(), 1, "no new say, no new toast");
        assert_eq!(a.sh.toasts.history.len(), 1);
        a.ed.say(crate::status::Severity::Refusal, "same");
        a.status_to_toast();
        assert_eq!(a.sh.toasts.history.len(), 2, "a repeat shows again");
        assert_eq!(a.sh.toasts.live[0].count, 2);
    }

    fn one_wall(a: &mut EditorApp) {
        let t = a.ed.types[1].id;
        a.ed.model
            .add_wall(&ddc_model::WallSpec {
                a: (0.0, 0.0),
                b: (4.0, 0.0),
                level: a.ed.level,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        a.ed.reload().unwrap();
        a.status_to_toast();
    }

    #[test]
    fn a_committed_delete_toasts_with_undo_and_its_key() {
        let mut a = app();
        one_wall(&mut a);
        a.ed.selection = vec![crate::editor::Sel::Wall(a.ed.walls[0].id)];
        a.ed.delete_selection();
        a.status_to_toast();
        let t = a.sh.toasts.live.last().unwrap();
        assert_eq!(t.command.as_deref(), Some("edit.undo"));
        assert!(t.entry.is_some() && t.entry == a.ed.top_entry(), "the delete's own entry");
        let keys = crate::quick::keys_text(&a, "edit.undo");
        if !keys.is_empty() {
            assert!(t.action.as_deref().unwrap().contains(&keys));
        }
    }

    #[test]
    fn a_refusal_without_a_journal_move_has_no_action() {
        let mut a = app();
        one_wall(&mut a);
        a.ed.say(crate::status::Severity::Refusal, "nothing deleted");
        a.status_to_toast();
        assert!(a.sh.toasts.live.last().unwrap().command.is_none());
        // a stale offer without a journal move is dropped too
        a.ed.offer_undo();
        a.ed.say(crate::status::Severity::Info, "x");
        a.status_to_toast();
        assert!(a.sh.toasts.live.last().unwrap().command.is_none());
        assert!(a.ed.undo_offer.is_none());
    }

    #[test]
    fn severity_comes_from_the_caller_not_the_words() {
        use crate::status::Severity::*;
        let mut a = app();
        a.ed.say(Refusal, "Moves only along its host");
        a.ed.say(Error, "all fine here");
        a.status_to_toast();
        assert_eq!(a.sh.toasts.live[0].level, Level::Error);
        let mut b = app();
        b.ed.say(Refusal, "Moves only along its host");
        b.status_to_toast();
        assert_eq!(b.sh.toasts.live[0].level, Level::Warning);
        b.ed.status = Some("plain".into());
        b.status_to_toast();
        assert_eq!(b.sh.toasts.live[1].level, Level::Info);
    }

    #[test]
    fn ribbon_blocks_follow_the_selection() {
        use crate::editor::Sel;
        let mut a = app();
        // a319e363 gave the stair every transform: a rebar set is the kind still refused alone
        a.ed.selection = vec![Sel::Ext(crate::scene_ids::EXT_REBAR, 1)];
        a.sync_caps_block();
        assert!(a.sh.ribbon.blocked.contains_key("mod.rotate"));
        assert!(!a.sh.ribbon.blocked.contains_key("mod.offset"));
        a.ed.selection.clear();
        a.sync_caps_block();
        // with nothing selected only the "select first" commands stay blocked, each with its tip
        let want: std::collections::BTreeSet<&str> = crate::modify_tools::NEEDS_SELECTION
            .iter()
            .copied()
            .filter(|c| crate::modify_tools::select_first_tip(&a.ed, c, false).is_some())
            .collect();
        let got: std::collections::BTreeSet<&str> =
            a.sh.ribbon.blocked.keys().map(String::as_str).collect();
        assert_eq!(got, want);
        assert!(!a.sh.ribbon.blocked.contains_key("mod.rotate"));
    }

    #[test]
    fn status_level_reads_both_languages() {
        assert_eq!(status_level(&crate::app2_fx::fx("chrome_load_failed")), Level::Warning);
        assert_eq!(status_level(&crate::app2_fx::fx("chrome_write_error")), Level::Warning);
        assert_eq!(status_level("cannot load 'x'"), Level::Warning);
        assert_eq!(status_level("Import failed: bad file"), Level::Warning);
        assert_eq!(status_level("family 'x' loaded"), Level::Info);
        assert_eq!(status_level(&crate::app2_fx::fx("chrome_added_to_wall")), Level::Info);
    }

    fn frame_at(a: &mut EditorApp, ctx: &egui::Context, w: f32, ev: Vec<egui::Event>) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(w, 1080.0),
            )),
            events: ev,
            ..Default::default()
        };
        let _ = ctx.run(input, |c| a.ui(c, None));
    }

    /// Press and release, or egui reports the next press as a repeat.
    fn press(k: egui::Key, m: egui::Modifiers) -> Vec<egui::Event> {
        let ev = |pressed| egui::Event::Key {
            key: k,
            physical_key: Some(k),
            pressed,
            repeat: false,
            modifiers: m,
        };
        vec![ev(true), ev(false)]
    }

    /// A selected wall, the wall tool holding its first point, two edits on the undo stack.
    fn busy() -> (EditorApp, egui::Context) {
        use crate::wall_tool::Event;
        let mut a = app();
        let ctx = egui::Context::default();
        a.ed.add_curtains(&[(ddc_geom::v2(0.0, 0.0), ddc_geom::v2(6.0, 0.0), 0.0)]);
        a.ed.add_curtains(&[(ddc_geom::v2(0.0, 3.0), ddc_geom::v2(6.0, 3.0), 0.0)]);
        a.ed.selection = vec![crate::editor::Sel::Curtain(a.ed.curtains[0].id)];
        assert!(a.run("arch.wall"));
        a.ed.handle(Event::Click(ddc_geom::v2(1.0, 1.0)));
        for _ in 0..3 {
            frame_at(&mut a, &ctx, 1920.0, vec![]);
        }
        (a, ctx)
    }

    /// What LAYOUTS 2.2 says survives a switch: selection, tool with its unfinished polyline,
    /// undo, plan and 3D cameras, schedule tab and scroll, the open view.
    fn snap(a: &EditorApp) -> String {
        let c = &a.view.camera;
        format!(
            "{:?}",
            (
                &a.ed.selection,
                a.ed.wall_mode(),
                &a.ed.chain(),
                a.plan.center,
                a.plan.scale,
                (c.target, c.yaw, c.pitch, c.distance, c.ortho),
                (a.ed.can_undo(), a.ed.can_redo(), a.ed.curtains.len()),
                (a.table.cat, a.table.scroll_to),
                a.view3d,
            )
        )
    }

    #[test]
    fn popover_keys_switch_layout_and_keep_state() {
        let (mut a, ctx) = busy();
        a.view.camera.yaw += 0.7;
        a.table.cat = crate::schedule::Cat::Doors;
        let before = snap(&a);
        let none = egui::Modifiers::NONE;
        a.chrome.mode.toggle(a.layout_index());
        assert_eq!(a.chrome.mode.hi, 0, "opens on the current layout");
        frame_at(&mut a, &ctx, 1920.0, press(egui::Key::ArrowDown, none));
        assert_eq!(a.layout, "classic", "arrows only move the highlight");
        assert_eq!(a.chrome.mode.hi, 1);
        frame_at(&mut a, &ctx, 1920.0, press(egui::Key::Enter, none));
        assert_eq!(a.layout, "context");
        assert!(!a.chrome.mode.open);
        assert_eq!(snap(&a), before);
        // Esc closes the popover and nothing else: the wall tool keeps its first point
        a.chrome.mode.toggle(a.layout_index());
        assert_eq!(a.chrome.mode.hi, 1);
        frame_at(&mut a, &ctx, 1920.0, press(egui::Key::Escape, none));
        assert!(!a.chrome.mode.open);
        assert_eq!(a.layout, "context");
        assert_eq!(snap(&a), before);
        a.ed.set_mode(crate::wall_tool::Mode::Select);
        a.ed.undo();
        assert_eq!(
            a.ed.curtains.len(),
            1,
            "undo reverts the same edit after the switches"
        );
    }

    #[test]
    fn f4_cycles_and_shift_f4_switches_back() {
        let (mut a, ctx) = busy();
        // the first visit builds the 3D scene and fits the camera; state is compared from here on
        for i in [2, 0] {
            if let Some(c) = a.pick_view(i) {
                assert!(a.run(c));
            }
            frame_at(&mut a, &ctx, 1920.0, vec![]);
        }
        assert_eq!(a.layout, "classic");
        let before = snap(&a);
        let none = egui::Modifiers::NONE;
        frame_at(&mut a, &ctx, 1920.0, press(egui::Key::F4, none));
        assert_eq!(a.layout, "context");
        frame_at(&mut a, &ctx, 1920.0, press(egui::Key::F4, none));
        assert_eq!(a.layout, "split");
        let toast = a
            .sh
            .toasts
            .live
            .iter()
            .rev()
            .find(|t| t.command.as_deref() == Some(LAYOUT_BACK))
            .expect("layout toast");
        assert!(toast.text.contains("Shift+F4"), "{}", toast.text);
        let n = a
            .sh
            .toasts
            .live
            .iter()
            .filter(|t| t.command.as_deref() == Some(LAYOUT_BACK))
            .count();
        assert_eq!(n, 1, "a new switch replaces the old layout toast");
        frame_at(&mut a, &ctx, 1920.0, press(egui::Key::F4, egui::Modifiers::SHIFT));
        assert_eq!(a.layout, "context");
        frame_at(&mut a, &ctx, 1920.0, press(egui::Key::F4, egui::Modifiers::SHIFT));
        assert_eq!(a.layout, "split", "back twice returns");
        assert!(a.run(LAYOUT_BACK));
        assert_eq!(a.layout, "context");
        assert_eq!(snap(&a), before);
        assert_eq!(a.mode_text().cycle, "F4", "tooltips show the cycle key");
        let mine: Vec<String> = a
            .sh
            .reg
            .conflicts()
            .iter()
            .map(|c| format!("{c:?}"))
            .filter(|c| c.contains("ui.layout") || c.contains("F4"))
            .collect();
        assert!(mine.is_empty(), "{mine:?}");
    }

    #[test]
    fn view_segments_enter_and_leave_split() {
        let (mut a, ctx) = busy();
        // the first visit builds the 3D scene; state is compared from here on
        for i in [2, 0] {
            if let Some(c) = a.pick_view(i) {
                assert!(a.run(c));
            }
            frame_at(&mut a, &ctx, 1920.0, vec![]);
        }
        assert!(a.run("ui.layout.set.4"));
        frame_at(&mut a, &ctx, 1920.0, vec![]);
        a.view.camera.pitch *= 0.5;
        let before = snap(&a);
        assert_eq!(a.view_mode(), 0);
        assert_eq!(a.pick_view(2), Some("ui.layout.set.3"));
        assert!(a.run("ui.layout.set.3"));
        frame_at(&mut a, &ctx, 1920.0, vec![]);
        assert_eq!((a.layout.as_str(), a.view_mode()), ("split", 2));
        // plan leaves the split for the layout used before it, not for classic
        let c = a.pick_view(0).unwrap();
        assert_eq!(a.layout, "focus");
        assert!(a.run(c));
        frame_at(&mut a, &ctx, 1920.0, vec![]);
        assert_eq!(a.view_mode(), 0);
        assert_eq!(snap(&a), before);
        // 3D from the split: the same layout back, with 3D on
        assert!(a.run("ui.layout.set.3"));
        let c = a.pick_view(1).unwrap();
        assert!(a.run(c));
        assert_eq!((a.layout.as_str(), a.view_mode()), ("focus", 1));
    }

    /// Real frames in both languages: the work-mode group is icon-only on narrow windows,
    /// labelled on wide ones, and never covers the search field or the menus.
    #[test]
    fn title_group_never_covers_search() {
        for lang in [Lang::En, Lang::Ru] {
            for w in [1024.0_f32, 1280.0, 1920.0, 2560.0] {
                let mut a = app();
                a.sh.cat.lang = lang;
                let ctx = egui::Context::default();
                for _ in 0..3 {
                    frame_at(&mut a, &ctx, w, vec![]);
                }
                let g = a.chrome.title.expect("title bar drawn");
                let tag = format!("{w} {lang:?}");
                assert!(g.fit.group.left() > g.left_end, "{tag}");
                assert!(g.fit.group.right() <= w, "{tag}");
                assert!(g.fit.group.expand(1.0).contains_rect(g.button), "{tag}: {:?}", g.button);
                let s = g.fit.search.expect("search shown");
                assert!(s.left() >= g.left_end, "{tag}");
                assert!(s.right() + space::SM <= g.fit.group.left(), "{tag}");
                if w <= 1024.0 {
                    assert_eq!(g.fit.fit, lc::Fit::Icons, "{tag}");
                }
                if w >= 1920.0 {
                    assert_eq!(g.fit.fit, lc::Fit::Full, "{tag}");
                }
            }
        }
    }

    #[test]
    fn layout_names_and_purposes_are_localized() {
        let mut a = app();
        for lang in [Lang::En, Lang::Ru] {
            a.sh.cat.lang = lang;
            for ws in ddc_ui::workspace::presets() {
                let name = a.layout_name(&ws);
                assert!(!name.starts_with("workspace."), "{name}");
                assert!(!a.layout_purpose(&ws).starts_with("workspace."));
                assert!(!a.layout_icon(&ws).is_empty());
                assert!(a.layout_keys(&ws).starts_with("Ctrl+Alt+"));
            }
        }
        a.sh.cat.lang = Lang::En;
        let ws = a.workspace();
        assert_eq!(a.layout_name(&ws), "Classic ribbon");
    }
}
