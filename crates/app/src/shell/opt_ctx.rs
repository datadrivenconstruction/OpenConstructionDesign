//! Select-mode context of the option bar. Nothing selected: the level and a hint. A selection:
//! its category and count with a clear button, the wall type for walls, isolate. Pure decisions
//! first, drawing after; texts come from the catalog passed in, never from the global language.
use super::prop_rows::WallVals;
use super::{browser, tool_opts, type_dialog, type_items};
use crate::editor::{Editor, Sel};
use ddc_ui::icons::IconCache;
use ddc_ui::theme::{control, space, stroke, Role};
use ddc_ui::widgets::type_pick::{Pick, TypePick};
use ddc_ui::{widgets, Catalog, Lang, Theme};

/// Brings the control that just took keyboard focus (Tab) into the visible part of the bar;
/// only on a focus change, so the mouse wheel and the scroll bar stay free afterwards.
pub(super) fn follow_focus(ui: &egui::Ui) {
    let ctx = ui.ctx();
    let key = egui::Id::new("ddc.optbar.focus");
    let now = ctx.memory(|m| m.focused());
    let before: Option<egui::Id> = ctx.data(|d| d.get_temp(key)).flatten();
    if now == before {
        return;
    }
    ctx.data_mut(|d| d.insert_temp(key, now));
    let Some(r) = now.and_then(|id| ctx.read_response(id)) else {
        return;
    };
    if ui.min_rect().expand(1.0).contains_rect(r.rect) {
        ui.scroll_to_rect(r.rect, None);
    }
}

/// What the option bar shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::shell) enum Bar {
    /// Select mode, nothing selected.
    Idle,
    /// Select mode with a selection.
    Selected,
    /// A tool is active: its name and its options.
    Tool,
}

pub(in crate::shell) fn bar(tool_active: bool, selected: usize) -> Bar {
    if tool_active {
        Bar::Tool
    } else if selected == 0 {
        Bar::Idle
    } else {
        Bar::Selected
    }
}

/// Ids when the selection holds walls only (the wall type can be set for all of them).
pub(super) fn wall_ids(sel: &[Sel]) -> Vec<i64> {
    let ids: Vec<i64> = sel
        .iter()
        .filter_map(|s| match s {
            Sel::Wall(i) => Some(*i),
            _ => None,
        })
        .collect();
    if ids.len() == sel.len() {
        ids
    } else {
        vec![]
    }
}

/// "Walls: 3" for one category, "N elements" for a mixed selection.
pub(super) fn chip_text(c: &Catalog, sel: &[Sel]) -> String {
    let cats = super::sel_categories(sel, c.lang == Lang::Ru);
    match cats.as_slice() {
        [one] => c.tf(
            "sel_chip.sel_chip",
            &[("name", &one.1), ("n", &one.2.to_string())],
        ),
        _ => c.plural("props.multi.title", sel.len() as u64),
    }
}

/// What the user asked for in the selection context.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Out {
    pub isolate: bool,
}

fn muted(ui: &mut egui::Ui, th: &Theme, text: &str, max_w: f32) {
    let col = th.sem().text_secondary;
    let g = widgets::elide(ui, text, th.font(Role::Caption), col, max_w);
    let cut = g.elided;
    let (r, resp) = ui.allocate_exact_size(g.size(), egui::Sense::hover());
    ui.painter().galley(r.min, g, col);
    if cut {
        resp.on_hover_text(text);
    }
}

/// Quiet chip with the selection summary; true when its clear button was pressed.
fn chip(ui: &mut egui::Ui, th: &Theme, text: &str, clear_tip: &str, max_w: f32) -> bool {
    let col = th.sem().text_primary;
    let mut clear = false;
    egui::Frame::new()
        .fill(th.c("bg.sunken"))
        .stroke(egui::Stroke::new(stroke::HAIRLINE, th.c("border.subtle")))
        .corner_radius(th.radius("md"))
        .inner_margin(egui::Margin::symmetric(space::SM as i8, space::XXS as i8))
        .show(ui, |ui| {
            let g = widgets::elide(ui, text, th.font(Role::Label), col, max_w);
            let cut = g.elided;
            let (r, resp) = ui.allocate_exact_size(g.size(), egui::Sense::hover());
            ui.painter().galley(r.min, g, col);
            if cut {
                resp.on_hover_text(text);
            }
            clear = widgets::close_button(ui, clear_tip).clicked();
        });
    clear
}

/// Select mode, nothing selected: level and a hint that takes the rest of the bar.
pub(super) fn idle(ui: &mut egui::Ui, th: &Theme, c: &Catalog, ed: &mut Editor, avail: f32) {
    tool_opts::level(ui, th, c.lang == Lang::Ru, ed);
    widgets::vdivider(ui, th, th.d().control);
    let room = (avail - ui.min_rect().width()).max(control::DROPDOWN_W);
    muted(ui, th, &c.t("sel_chip.opt.sel_hint"), room);
}

/// Wall type of all selected walls; the type menu also opens the type editor.
fn wall_type(ui: &mut egui::Ui, th: &Theme, ru: bool, ed: &mut Editor, ids: &[i64]) {
    let u = ed.tool.units;
    let vals = browser::with(|s| {
        s.wall_vals
            .get((ed.revision, u), ids, || WallVals::build(&*ed, ids, u))
    });
    let rows = type_items::items(&ed.types, u, ru);
    let label = crate::tr_lang(ru, "opt.type", &[]);
    widgets::form::label(ui, th, &label, control::DROPDOWN_W);
    let pick = TypePick::new("ux65f.wtype", &rows, vals.common_type)
        .texts(type_items::texts(ru))
        .hover(crate::tr_lang(ru, "type_dialog.panels.wall_type", &[]))
        .show(ui, th);
    match pick {
        Some(Pick::Id(id)) => {
            let _ = super::edit_walls(ed, ids, Some(id), None);
        }
        Some(Pick::EditType) => {
            let id = vals.common_type.or(vals.first_type).unwrap_or(0);
            super::with_type_dialog(ed, type_dialog::Fam::Wall, id);
        }
        None => {}
    }
}

/// Select mode with a selection: chip with clear, wall type (walls only), isolate.
pub(super) fn selected(
    ui: &mut egui::Ui,
    th: &Theme,
    icons: &mut IconCache,
    c: &Catalog,
    ed: &mut Editor,
    avail: f32,
    isolate: (&str, &str),
) -> Out {
    let mut out = Out::default();
    let text = chip_text(c, &ed.selection);
    let max_w = (avail * 0.5).min(control::DROPDOWN_W + space::XXXL);
    if chip(ui, th, &text, &c.t("props.multi.clear"), max_w) {
        ed.selection.clear();
        return out;
    }
    let ids = wall_ids(&ed.selection);
    if !ids.is_empty() {
        widgets::vdivider(ui, th, th.d().control);
        wall_type(ui, th, c.lang == Lang::Ru, ed, &ids);
    }
    widgets::vdivider(ui, th, th.d().control);
    out.isolate = widgets::icon_button(ui, icons, th, isolate.0, false, isolate.1).clicked();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walls(n: i64) -> Vec<Sel> {
        (1..=n).map(Sel::Wall).collect()
    }

    #[test]
    fn bar_follows_tool_then_selection() {
        assert_eq!(bar(false, 0), Bar::Idle);
        assert_eq!(bar(false, 3), Bar::Selected);
        assert_eq!(bar(true, 0), Bar::Tool);
        assert_eq!(bar(true, 3), Bar::Tool);
    }

    #[test]
    fn type_is_offered_for_walls_only() {
        assert_eq!(wall_ids(&walls(3)), [1, 2, 3]);
        assert!(wall_ids(&[]).is_empty());
        let mixed = vec![Sel::Wall(1), Sel::Opening(2)];
        assert!(wall_ids(&mixed).is_empty());
    }

    #[test]
    fn chip_names_the_category_and_the_count_in_both_languages() {
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            let one = chip_text(&c, &walls(3));
            assert!(one.ends_with(": 3"), "{lang:?}: {one}");
            assert!(!one.starts_with(':'), "{lang:?}: category name missing");
            let mixed = chip_text(&c, &[Sel::Wall(1), Sel::Opening(2)]);
            assert!(mixed.contains('2'), "{lang:?}: {mixed}");
        }
        let ru = chip_text(&Catalog::new(Lang::Ru), &walls(2));
        let en = chip_text(&Catalog::new(Lang::En), &walls(2));
        assert_ne!(ru, en);
    }

    #[test]
    fn source_uses_kit_widgets_and_tokens_only() {
        let src = include_str!("opt_ctx.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        for bad in [
            "ComboBox",
            "DragValue",
            "selectable_label",
            "ui.label(",
            "ui.add(",
            "ui.button(",
            "Color32::",
            "from_rgb",
        ] {
            assert!(!code.contains(bad), "raw widget or colour: {bad}");
        }
    }
}
