//! Schedule toolbar on the design kit: category tabs with counts, icon buttons with tooltips,
//! kit menus (columns, groups, formulas, export), phase dropdown and the "N of M shown" chip.
//! What the bar offers in each state is a pure function (`toolbar_items`, `visible_tabs`), so it is
//! tested without a frame.

use super::*;
use ddc_ui::theme::{control, space};
use ddc_ui::widgets::{self, Button, MenuTone, Tone};

/// A control of the toolbar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Filter,
    Columns,
    Groups,
    Formulas,
    Phase,
    Copy,
    Import,
    Export,
}

/// One control as the current state allows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolItem {
    pub id: Tool,
    pub enabled: bool,
    /// Catalog key of the tooltip body.
    pub tip_key: &'static str,
    /// Catalog key of the line shown while the control is disabled.
    pub reason_key: Option<&'static str>,
}

impl Tool {
    /// Catalog key of the name (tooltip title and menu button text).
    pub fn label_key(self) -> &'static str {
        match self {
            Tool::Filter => "sch.ui.filter",
            Tool::Columns => "sch.ui.columns",
            Tool::Groups => "sch.ui.groups",
            Tool::Formulas => "sch.ui.formulas",
            Tool::Phase => "sched_toolbar.phase",
            Tool::Copy => "sch.ui.copy",
            Tool::Import => "sched_import.import",
            Tool::Export => "sch.ui.export",
        }
    }

    pub fn icon(self) -> Option<&'static str> {
        match self {
            Tool::Filter => Some("filter"),
            Tool::Columns => Some("table"),
            Tool::Groups => Some("group"),
            Tool::Formulas => None,
            Tool::Phase => Some("phases"),
            Tool::Copy => Some("copy"),
            Tool::Import => Some("import"),
            Tool::Export => Some("save-as"),
        }
    }

    /// Shortcut shown in the tooltip.
    pub fn keys(self) -> &'static str {
        match self {
            Tool::Copy => "Ctrl+C",
            _ => "",
        }
    }

    /// Controls on the right edge of the bar: file actions.
    pub fn trailing(self) -> bool {
        matches!(self, Tool::Copy | Tool::Import | Tool::Export)
    }

    pub const ALL: [Tool; 8] = [
        Tool::Filter,
        Tool::Columns,
        Tool::Groups,
        Tool::Formulas,
        Tool::Phase,
        Tool::Copy,
        Tool::Import,
        Tool::Export,
    ];
}

/// The bar left to right: view controls, then file actions (the last three sit on the right).
/// Phase appears only when the model has phases. A control that needs a table is disabled without
/// one; Import waits for a running file dialog, Export for a running save dialog.
pub fn toolbar_items(
    has_table: bool,
    has_phases: bool,
    saving: bool,
    opening: bool,
) -> Vec<ToolItem> {
    let mut v = Vec::new();
    for id in Tool::ALL {
        if id == Tool::Phase && !has_phases {
            continue;
        }
        let (tip_key, needs_table) = match id {
            Tool::Filter => ("sched_toolbar.filter_tip", false),
            Tool::Columns => ("sched_toolbar.columns_tip", true),
            Tool::Groups => ("sched_toolbar.groups_tip", true),
            Tool::Formulas => ("sched_toolbar.formulas_tip", true),
            Tool::Phase => ("sched_toolbar.phase_tip", true),
            Tool::Copy => ("sched_toolbar.copy_tip", true),
            Tool::Import => ("sched_toolbar.import_tip", true),
            Tool::Export => ("sched_toolbar.export_tip", false),
        };
        let reason_key = if needs_table && !has_table {
            Some("sched_toolbar.need_table")
        } else if id == Tool::Import && opening {
            Some("sched_toolbar.busy_open")
        } else if id == Tool::Export && saving {
            Some("sched_toolbar.busy_save")
        } else {
            None
        };
        v.push(ToolItem {
            id,
            enabled: reason_key.is_none(),
            tip_key,
            reason_key,
        });
    }
    v
}

/// Category tabs shown: the basic ones, and the others that have rows or are current.
pub fn visible_tabs(cur: Cat, counts: &[usize]) -> Vec<(Cat, usize)> {
    Cat::ALL
        .iter()
        .enumerate()
        .filter_map(|(k, c)| {
            let n = counts.get(k).copied().unwrap_or(0);
            (c.basic() || n > 0 || *c == cur).then_some((*c, n))
        })
        .collect()
}

/// Phase choices in the dropdown order: all, "in phase" per phase, "demolished in" per phase.
pub fn phase_options(phases: &[(i64, String)]) -> Vec<PhaseMode> {
    let mut v = vec![PhaseMode::All];
    v.extend(phases.iter().map(|(id, _)| PhaseMode::In(*id)));
    v.extend(phases.iter().map(|(id, _)| PhaseMode::Demolished(*id)));
    v
}

/// Whether any column filter has text.
pub fn filters_active(filters: &[String]) -> bool {
    filters.iter().any(|f| !f.trim().is_empty())
}

const CALC_KINDS: [Kind; 6] = [
    Kind::Num,
    Kind::Len,
    Kind::Area,
    Kind::Vol,
    Kind::Int,
    Kind::Pct,
];

fn tool_tip(
    resp: egui::Response,
    th: &Theme,
    it: &ToolItem,
    title: &str,
) -> egui::Response {
    let desc = crate::tr(it.tip_key, &[]);
    let reason = match (it.enabled, it.reason_key) {
        (false, Some(k)) => crate::tr(k, &[]),
        _ => String::new(),
    };
    widgets::rich_tooltip_reason(resp, th, title, &desc, it.id.keys(), &reason)
}

/// Quiet button with the tool's icon that opens a menu.
fn menu_trigger(ui: &mut egui::Ui, th: &Theme, it: &ToolItem, label: &str) -> egui::Response {
    let ctx = ui.ctx().clone();
    let r = ui
        .add_enabled_ui(it.enabled, |ui| {
            widgets::with_icons(&ctx, |icons| {
                let b = Button::quiet(label).small();
                match it.id.icon() {
                    Some(n) => b.show_icon(ui, th, icons, n),
                    None => b.show(ui, th),
                }
            })
        })
        .inner;
    tool_tip(r, th, it, label)
}

/// Square icon button; `on` fills it for a toggle that is active.
fn icon_tool(
    ui: &mut egui::Ui,
    th: &Theme,
    it: &ToolItem,
    on: bool,
    title: &str,
) -> egui::Response {
    let ctx = ui.ctx().clone();
    let icon = it.id.icon().unwrap_or("close");
    let r = ui
        .add_enabled_ui(it.enabled, |ui| {
            widgets::with_icons(&ctx, |icons| {
                widgets::icon_button(ui, icons, th, icon, on, "")
            })
        })
        .inner;
    tool_tip(r, th, it, title)
}

fn keep_open_menu(resp: &egui::Response) -> egui::Popup<'_> {
    egui::Popup::menu(resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
}

impl View {
    /// Category tabs; the category picked this frame, if it changed.
    pub(super) fn tabs_row(&mut self, ui: &mut egui::Ui, th: &Theme) -> Option<Cat> {
        let tabs = visible_tabs(self.cat, &self.counts);
        let labels: Vec<String> = tabs
            .iter()
            .map(|(c, n)| format!("{} {}", c.title(), fmt_num(*n as f64, 0)))
            .collect();
        let names: Vec<&str> = labels.iter().map(String::as_str).collect();
        let mut sel = tabs.iter().position(|(c, _)| *c == self.cat).unwrap_or(0);
        widgets::tab_bar(ui, th, &names, &mut sel)
            .and_then(|i| tabs.get(i))
            .map(|(c, _)| *c)
            .filter(|c| *c != self.cat)
    }

    /// Actions row. Menus may change grouping (`dirty`) or formulas (`key` reset).
    pub(super) fn toolbar(
        &mut self,
        ui: &mut egui::Ui,
        th: &Theme,
        u: Units,
        acts: &mut Vec<Action>,
    ) {
        let (cols, phases) = self
            .table
            .as_ref()
            .map(|t| (t.cols.clone(), t.phases.clone()))
            .unwrap_or_default();
        let items = toolbar_items(
            self.table.is_some(),
            !phases.is_empty(),
            self.save_rx.is_some(),
            self.open_rx.is_some(),
        );
        let ti = self.cat.index();
        let mut rebuild = false;
        ui.horizontal(|ui| {
            for it in items.iter().filter(|i| !i.id.trailing()) {
                let title = crate::tr(it.id.label_key(), &[]);
                match it.id {
                    Tool::Filter => {
                        if icon_tool(ui, th, it, self.show_filters, &title).clicked() {
                            self.show_filters = !self.show_filters;
                        }
                    }
                    Tool::Columns => {
                        let r = menu_trigger(ui, th, it, &title);
                        self.columns_menu(&r, th, &cols, u);
                    }
                    Tool::Groups => {
                        let n = self.tabs[ti].group.len();
                        let label = if n == 0 {
                            title.clone()
                        } else {
                            crate::tr("sch.ui.groups_n", &[("n", &n.to_string())])
                        };
                        let r = menu_trigger(ui, th, it, &label);
                        self.groups_menu(&r, th, &cols, u);
                    }
                    Tool::Formulas => {
                        let r = menu_trigger(ui, th, it, &title);
                        rebuild |= self.formulas_menu(&r, th);
                    }
                    Tool::Phase => self.phase_dropdown(ui, th, it, &phases),
                    _ => {}
                }
            }
            self.shown_chip(ui, th);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                for it in items.iter().rev().filter(|i| i.id.trailing()) {
                    let title = crate::tr(it.id.label_key(), &[]);
                    let r = icon_tool(ui, th, it, false, &title);
                    match it.id {
                        Tool::Copy => {
                            if r.clicked() {
                                acts.push(Action::Copy);
                            }
                        }
                        Tool::Import => {
                            if r.clicked() {
                                self.start_open(ui.ctx());
                            }
                        }
                        Tool::Export => self.export_menu(&r, th, acts),
                        _ => {}
                    }
                }
            });
        });
        if rebuild {
            self.key = None;
        }
    }

    /// "N of M shown" as a quiet chip, with a way back when a column filter causes it.
    fn shown_chip(&mut self, ui: &mut egui::Ui, th: &Theme) {
        let shown = self.ord.len();
        let all = self.table.as_ref().map_or(0, |t| t.rows.len());
        if shown == all {
            return;
        }
        let text = crate::tr(
            "sch.ui.shown",
            &[("shown", &shown.to_string()), ("all", &all.to_string())],
        );
        widgets::badge(ui, th, &text, Tone::Neutral);
        let ti = self.cat.index();
        if filters_active(&self.tabs[ti].filters)
            && Button::quiet(&crate::tr("sched_toolbar.clear_filter", &[]))
                .small()
                .show(ui, th)
                .clicked()
        {
            self.tabs[ti].filters.iter_mut().for_each(String::clear);
            self.dirty = true;
        }
    }

    fn phase_dropdown(
        &mut self,
        ui: &mut egui::Ui,
        th: &Theme,
        it: &ToolItem,
        phases: &[(i64, String)],
    ) {
        let name = |p: i64| {
            phases
                .iter()
                .find(|x| x.0 == p)
                .map_or("?".to_string(), |x| x.1.clone())
        };
        let opts = phase_options(phases);
        let labels: Vec<String> = opts
            .iter()
            .map(|m| match m {
                PhaseMode::All => crate::tr("sch.ui.phase_all", &[]),
                PhaseMode::In(p) => crate::tr("sch.ui.phase_in", &[("name", &name(*p))]),
                PhaseMode::Demolished(p) => {
                    crate::tr("sch.ui.phase_demo", &[("name", &name(*p))])
                }
            })
            .collect();
        let names: Vec<&str> = labels.iter().map(String::as_str).collect();
        let t = &mut self.tabs[self.cat.index()];
        let mut i = opts.iter().position(|m| *m == t.phase).unwrap_or(0);
        let r = ui
            .add_enabled_ui(it.enabled, |ui| {
                widgets::dropdown(ui, th, "ddc.sch.phase", &mut i, &names, None)
            })
            .inner;
        let changed = r.changed();
        let title = crate::tr(it.id.label_key(), &[]);
        tool_tip(r, th, it, &title);
        if changed && opts.get(i).is_some_and(|m| *m != t.phase) {
            t.phase = opts[i];
            self.dirty = true;
        }
    }

    fn columns_menu(&mut self, resp: &egui::Response, th: &Theme, cols: &[Col], u: Units) {
        let ti = self.cat.index();
        let t = &mut self.tabs[ti];
        keep_open_menu(resp).show(|ui| {
            ui.set_min_width(control::DROPDOWN_W);
            for (j, c) in cols.iter().enumerate() {
                let mut on = !t.hidden[j];
                if widgets::checkbox(ui, th, &mut on, &header(c, u)).changed() {
                    t.hidden[j] = !on;
                }
            }
        });
    }

    fn groups_menu(&mut self, resp: &egui::Response, th: &Theme, cols: &[Col], u: Units) {
        let ti = self.cat.index();
        let t = &mut self.tabs[ti];
        let mut regroup = false;
        keep_open_menu(resp).show(|ui| {
            ui.set_min_width(control::DROPDOWN_W);
            ui.label(th.rich(
                ddc_ui::theme::Role::Section,
                &crate::tr("sch.ui.group_by", &[]),
            ));
            for c in cols {
                let mut on = t.group.contains(&c.key);
                if widgets::checkbox(ui, th, &mut on, &header(c, u)).changed() {
                    if on {
                        t.group.push(c.key.clone());
                    } else {
                        t.group.retain(|k| *k != c.key);
                    }
                    regroup = true;
                }
            }
            widgets::menu_divider(ui, th);
            if widgets::checkbox(ui, th, &mut t.subtotals, &crate::tr("sch.ui.subtotals", &[]))
                .changed()
            {
                regroup = true;
            }
            if !t.group.is_empty()
                && widgets::menu_item(
                    ui,
                    th,
                    &crate::tr("sch.ui.ungroup", &[]),
                    "",
                    MenuTone::Normal,
                    true,
                )
                .clicked()
            {
                t.group.clear();
                regroup = true;
            }
        });
        if regroup {
            self.dirty = true;
        }
    }

    /// Calculated columns editor; `true` when the table has to be rebuilt.
    fn formulas_menu(&mut self, resp: &egui::Response, th: &Theme) -> bool {
        let ti = self.cat.index();
        let t = &mut self.tabs[ti];
        let errs = &self.calc_errs;
        let mut rebuild = false;
        keep_open_menu(resp).show(|ui| {
            ui.label(th.rich(
                ddc_ui::theme::Role::Body,
                &crate::tr("sch.ui.formula_help", &[]),
            ));
            let mut drop = None;
            for (n, c) in t.calcs.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    let name_hint = crate::tr("sch.ui.name_hint", &[]);
                    let expr_hint = crate::tr("sch.ui.formula_hint", &[]);
                    widgets::TextField::new(&mut c.name)
                        .id_salt(("ddc.calc.name", n))
                        .hint(&name_hint)
                        .labelled_by(egui::Id::new(("ddc.calc.name.lbl", n)), &name_hint)
                        .width(control::DROPDOWN_W)
                        .show(ui, th);
                    widgets::TextField::new(&mut c.expr)
                        .id_salt(("ddc.calc.expr", n))
                        .hint(&expr_hint)
                        .labelled_by(egui::Id::new(("ddc.calc.expr.lbl", n)), &expr_hint)
                        .width(2.0 * control::DROPDOWN_W)
                        .show(ui, th);
                    let labels: Vec<String> = CALC_KINDS.iter().map(|k| kind_name(*k)).collect();
                    let names: Vec<&str> = labels.iter().map(String::as_str).collect();
                    let mut i = CALC_KINDS.iter().position(|k| *k == c.kind).unwrap_or(0);
                    if widgets::dropdown(ui, th, ("ddc.calc.kind", n), &mut i, &names, None)
                        .changed()
                    {
                        c.kind = CALC_KINDS[i];
                    }
                    if widgets::glyph_button(ui, "delete", true, &crate::tr("sch.ui.delete_col", &[]))
                        .clicked()
                    {
                        drop = Some(n);
                    }
                });
                if let Some(Some(e)) = errs.get(n) {
                    ui.label(
                        egui::RichText::new(crate::tr("sch.ui.error", &[("e", e)]))
                            .color(th.c("error")),
                    );
                }
            }
            if let Some(n) = drop {
                t.calcs.remove(n);
                rebuild = true;
            }
            ui.horizontal(|ui| {
                if Button::secondary(&crate::tr("sch.ui.add_col", &[]))
                    .small()
                    .show(ui, th)
                    .clicked()
                {
                    t.calcs.push(Calc {
                        name: String::new(),
                        expr: String::new(),
                        kind: Kind::Num,
                    });
                }
                if Button::primary(&crate::tr("sch.ui.apply", &[]))
                    .small()
                    .show(ui, th)
                    .clicked()
                {
                    rebuild = true;
                }
            });
        });
        rebuild
    }

    fn export_menu(&mut self, resp: &egui::Response, th: &Theme, acts: &mut Vec<Action>) {
        let idle = self.save_rx.is_none();
        let mut save_as = false;
        egui::Popup::menu(resp).show(|ui| {
            ui.set_min_width(2.0 * control::DROPDOWN_W);
            if widgets::menu_item(
                ui,
                th,
                &crate::tr("sch.ui.save_as", &[]),
                "",
                MenuTone::Normal,
                idle,
            )
            .clicked()
            {
                save_as = true;
                ui.close();
            }
            widgets::menu_divider(ui, th);
            let path_lbl = crate::tr("sch.ui.or_path", &[]);
            let path_lr = ui.label(th.rich(ddc_ui::theme::Role::Body, &path_lbl));
            ui.add_space(space::XS);
            widgets::TextField::new(&mut self.export_path)
                .id_salt("ddc.sch.export_path")
                .labelled_by(path_lr.id, &path_lbl)
                .show(ui, th);
            ui.add_space(space::XS);
            if Button::primary(&crate::tr("sch.ui.save", &[]))
                .small()
                .show(ui, th)
                .clicked()
            {
                acts.push(Action::Export(self.export_path.clone().into()));
                ui.close();
            }
        });
        if save_as {
            self.start_save(&resp.ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::Lang;

    fn item(v: &[ToolItem], id: Tool) -> ToolItem {
        v.iter().find(|i| i.id == id).cloned().unwrap()
    }

    #[test]
    fn everything_is_on_with_a_table_and_no_dialog() {
        let v = toolbar_items(true, true, false, false);
        assert_eq!(v.len(), Tool::ALL.len());
        assert!(v.iter().all(|i| i.enabled && i.reason_key.is_none()));
    }

    #[test]
    fn phase_only_with_phases() {
        assert!(toolbar_items(true, false, false, false)
            .iter()
            .all(|i| i.id != Tool::Phase));
        assert!(toolbar_items(true, true, false, false)
            .iter()
            .any(|i| i.id == Tool::Phase));
    }

    #[test]
    fn without_a_table_only_filter_and_export_stay_on() {
        let v = toolbar_items(false, true, false, false);
        for i in &v {
            let on = matches!(i.id, Tool::Filter | Tool::Export);
            assert_eq!(i.enabled, on, "{:?}", i.id);
            if !on {
                assert_eq!(i.reason_key, Some("sched_toolbar.need_table"), "{:?}", i.id);
            }
        }
    }

    #[test]
    fn import_waits_for_the_file_dialog_and_export_for_the_save_dialog() {
        let v = toolbar_items(true, false, false, true);
        let imp = item(&v, Tool::Import);
        assert!(!imp.enabled);
        assert_eq!(imp.reason_key, Some("sched_toolbar.busy_open"));
        assert!(item(&v, Tool::Export).enabled);
        let v = toolbar_items(true, false, true, false);
        let exp = item(&v, Tool::Export);
        assert!(!exp.enabled);
        assert_eq!(exp.reason_key, Some("sched_toolbar.busy_save"));
        assert!(item(&v, Tool::Import).enabled);
    }

    #[test]
    fn file_actions_are_the_trailing_group() {
        let v = toolbar_items(true, true, false, false);
        let lead = v.iter().take_while(|i| !i.id.trailing()).count();
        assert!(v[lead..].iter().all(|i| i.id.trailing()));
        assert_eq!(v.len() - lead, 3);
    }

    #[test]
    fn every_icon_exists() {
        for t in Tool::ALL {
            if let Some(n) = t.icon() {
                assert!(ddc_ui::icons::source(n).is_some(), "{t:?}: icon {n}");
            }
        }
        // icon-only buttons need an icon
        for t in [Tool::Filter, Tool::Copy, Tool::Import, Tool::Export] {
            assert!(t.icon().is_some(), "{t:?}");
        }
    }

    #[test]
    fn tooltips_and_reasons_exist_in_both_languages() {
        let c = crate::catalog();
        let mut keys = vec!["sched_toolbar.clear_filter", "sched_toolbar.fields_title"];
        for t in Tool::ALL {
            keys.push(t.label_key());
        }
        for has_table in [true, false] {
            for (s, o) in [(false, false), (true, true)] {
                for i in toolbar_items(has_table, true, s, o) {
                    keys.push(i.tip_key);
                    keys.extend(i.reason_key);
                }
            }
        }
        for k in keys {
            for lang in [Lang::En, Lang::Ru] {
                let t = c.table(lang).get(k).and_then(|v| v.as_str()).unwrap_or("");
                assert!(!t.trim().is_empty(), "{k} missing in {lang:?}");
            }
        }
    }

    #[test]
    fn only_copy_has_a_shortcut() {
        for t in Tool::ALL {
            assert_eq!(t.keys() == "Ctrl+C", t == Tool::Copy, "{t:?}");
        }
    }

    #[test]
    fn tabs_hide_empty_extra_categories_but_keep_the_current_one() {
        let n = Cat::ALL.len();
        let counts = vec![0usize; n];
        let base = visible_tabs(Cat::Walls, &counts);
        assert!(base.iter().all(|(c, _)| c.basic()));
        let extra = Cat::ALL.iter().copied().find(|c| !c.basic()).unwrap();
        let cur = visible_tabs(extra, &counts);
        assert!(cur.iter().any(|(c, _)| *c == extra));
        let k = Cat::ALL.iter().position(|c| *c == extra).unwrap();
        let mut counts = counts;
        counts[k] = 3;
        let with = visible_tabs(Cat::Walls, &counts);
        assert!(with.contains(&(extra, 3)));
    }

    #[test]
    fn phase_options_cover_all_in_and_demolished() {
        let ph = vec![(1, "A".to_string()), (2, "B".to_string())];
        let o = phase_options(&ph);
        assert_eq!(
            o,
            vec![
                PhaseMode::All,
                PhaseMode::In(1),
                PhaseMode::In(2),
                PhaseMode::Demolished(1),
                PhaseMode::Demolished(2)
            ]
        );
        assert_eq!(phase_options(&[]), vec![PhaseMode::All]);
    }

    #[test]
    fn filters_active_ignores_blank_text() {
        assert!(!filters_active(&[]));
        assert!(!filters_active(&["".into(), "  ".into()]));
        assert!(filters_active(&["".into(), ">5".into()]));
    }

    #[test]
    fn toolbar_and_menus_use_no_raw_egui_widgets() {
        let src = include_str!("toolbar.rs");
        let prod = &src[..src.find("#[cfg(test)]").unwrap()];
        for pat in RAW {
            assert!(!prod.contains(pat), "raw widget {pat} in toolbar.rs");
        }
    }

    const RAW: [&str; 13] = [
        "ui.button(",
        ".small_button(",
        "egui::Button",
        "ComboBox",
        "selectable_label",
        "selectable_value",
        "TextEdit",
        "text_edit_singleline",
        "DragValue",
        "ui.checkbox(",
        ".separator()",
        "menu_button(",
        "toggle_value(",
    ];

    #[test]
    fn schedule_view_keeps_raw_widgets_only_in_the_cell_editor() {
        let src = include_str!("../ui.rs");
        let prod = &src[..src.find("\nmod tests").unwrap()];
        for pat in RAW {
            let n = prod.matches(pat).count();
            let allowed = match pat {
                "TextEdit" | "selectable_label" => 1,
                _ => 0,
            };
            assert_eq!(n, allowed, "raw widget {pat} in ui.rs: {n}");
        }
    }
}
