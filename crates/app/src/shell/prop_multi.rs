// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Properties header of a multi-selection: element count, one chip per category (icon, name,
//! count). A chip click shows that category's rows; Ctrl+click narrows the selection to it
//! (the model stays untouched, the toast restores the previous selection).
//! Pure helpers first (chip model, narrowing, common-value detection), drawing last.
use super::panels::{cat_of, sel_categories};
use crate::editor::{Editor, Sel};
use ddc_ui::theme::{icon_size, space, stroke, ControlSize, Role, Theme};
use ddc_ui::{Catalog, Lang};

/// One category of the selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chip {
    pub cat: &'static str,
    pub label: String,
    pub count: usize,
}

/// What the user did in the header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    /// Keep only this category in the selection.
    Narrow(&'static str),
    /// Show the properties of this category; the selection stays.
    View(&'static str),
    Clear,
}

/// Chips in a stable order: larger count first, then name. A single category gives no chips.
pub fn chips(sel: &[Sel], lang: Lang) -> Vec<Chip> {
    let mut v: Vec<Chip> = sel_categories(sel, lang == Lang::Ru)
        .into_iter()
        .map(|(cat, label, count)| Chip { cat, label, count })
        .collect();
    if v.len() < 2 {
        return vec![];
    }
    v.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(&b.label)));
    v
}

/// Category whose rows are shown: the requested one if the selection holds it, else the largest.
pub fn active(sel: &[Sel], want: Option<&str>, lang: Lang) -> &'static str {
    let cats = sel_categories(sel, lang == Lang::Ru);
    if let Some(c) = want.and_then(|w| cats.iter().find(|c| c.0 == w)) {
        return c.0;
    }
    cats.iter()
        .max_by(|a, b| a.2.cmp(&b.2).then_with(|| b.1.cmp(&a.1)))
        .map_or("", |c| c.0)
}

/// The selection reduced to one category, in the original order.
pub fn narrow(sel: &[Sel], cat: &str) -> Vec<Sel> {
    sel.iter().filter(|s| cat_of(s) == cat).copied().collect()
}

/// Narrows the editor selection. A selection change only: no model operation, no revision bump.
/// False when nothing would change.
pub fn apply_narrow(ed: &mut Editor, cat: &str) -> bool {
    let kept = narrow(&ed.selection, cat);
    if kept.is_empty() || kept.len() == ed.selection.len() {
        return false;
    }
    ed.selection = kept;
    true
}

/// Icon of a category chip.
pub fn chip_icon(cat: &str) -> &'static str {
    match cat {
        "wall" => "wall",
        "opening" => "opening",
        "curtain" => "curtain",
        "floor" => "floor",
        "room" => "room",
        "ceiling" => "ceiling",
        "column" => "column",
        "beam" => "beam",
        "stair" => "stair",
        "railing" => "railing",
        "shaft" => "shaft",
        "ramp" => "ramp",
        "room_sep" => "room-sep",
        "zone" => "zone",
        "grid" => "grid",
        "ref_plane" => "workplane",
        "mep" => "pipe",
        "anno" => "text",
        "imported" => "import",
        _ => "component",
    }
}

/// Value of a property across the selected elements.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Common<T> {
    /// No elements, nothing to compare.
    Empty,
    Same(T),
    /// This many distinct values.
    Varies(usize),
}

pub fn common<T: PartialEq + Clone>(vals: &[T]) -> Common<T> {
    let mut distinct: Vec<&T> = vec![];
    for v in vals {
        if !distinct.contains(&v) {
            distinct.push(v);
        }
    }
    match distinct.as_slice() {
        [] => Common::Empty,
        [one] => Common::Same((*one).clone()),
        many => Common::Varies(many.len()),
    }
}

/// Text of a value cell: the shared value, a dash for nothing, or «varies (N)».
pub fn common_text<T: ToString>(c: &Catalog, v: &Common<T>) -> String {
    match v {
        Common::Empty => "\u{2014}".to_string(),
        Common::Same(s) => s.to_string(),
        Common::Varies(n) => c.plural("props.multi.varies", *n as u64),
    }
}

/// Title and subtitle of the header card; `single` is the one category when there are no chips.
pub fn header_text(c: &Catalog, total: usize, ncats: usize, single: &str) -> (String, String) {
    let title = c.plural("props.multi.title", total as u64);
    let sub = if ncats > 1 {
        c.plural("props.multi.cats", ncats as u64)
    } else {
        single.to_string()
    };
    (title, sub)
}

pub fn narrowed_text(c: &Catalog, label: &str, n: usize) -> String {
    c.tf("props.multi.narrowed", &[("name", label), ("n", &n.to_string())])
}

fn chip_button(ui: &mut egui::Ui, th: &Theme, c: &Catalog, chip: &Chip, on: bool) -> egui::Response {
    let s = th.sem();
    let h = th.control_h(ControlSize::Small);
    let pad = space::SM;
    let label = ui.painter().layout_no_wrap(
        chip.label.clone(),
        th.font(Role::Caption),
        if on { s.accent } else { s.text_primary },
    );
    let num = ui.painter().layout_no_wrap(
        chip.count.to_string(),
        th.font(Role::Numeric),
        if on { s.accent } else { s.text_secondary },
    );
    let w = pad + icon_size::SM + space::XS + label.size().x + space::XS + num.size().x + pad;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::click());
    let name = chip.label.clone();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, &name));
    let (fill, line) = if on {
        (s.accent_subtle, s.accent)
    } else if resp.hovered() {
        (s.bg_hover, s.border_strong)
    } else {
        (egui::Color32::TRANSPARENT, s.border_subtle)
    };
    ui.painter().rect(
        rect,
        th.radius("md"),
        fill,
        egui::Stroke::new(stroke::HAIRLINE, line),
        egui::StrokeKind::Inside,
    );
    let mid = rect.center().y;
    let ic = egui::Rect::from_center_size(
        egui::pos2(rect.min.x + pad + icon_size::SM / 2.0, mid),
        egui::Vec2::splat(icon_size::SM),
    );
    let tint = if on { s.accent } else { s.text_secondary };
    ddc_ui::widgets::with_icons(ui.ctx(), |icons| {
        icons.paint(ui, chip_icon(chip.cat), icon_size::SM as u32, ic, tint);
    });
    let x = ic.max.x + space::XS;
    let lp = egui::pos2(x, mid - label.size().y / 2.0);
    ui.painter().galley(lp, label.clone(), s.text_primary);
    let np = egui::pos2(x + label.size().x + space::XS, mid - num.size().y / 2.0);
    ui.painter().galley(np, num, s.text_secondary);
    let n = chip.count.to_string();
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(c.tf("props.multi.chip_tip", &[("name", &chip.label), ("n", &n)]))
}

/// Header card: icon, «N elements», category line, clear button, then the chip row.
pub fn header(
    ui: &mut egui::Ui,
    th: &Theme,
    c: &Catalog,
    total: usize,
    single: &str,
    chips: &[Chip],
    active_cat: &str,
) -> Option<Act> {
    let mut act = None;
    let (title, sub) = header_text(c, total, chips.len(), single);
    ui.add_space(space::XS);
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(egui::Vec2::splat(icon_size::MD), egui::Sense::hover());
        ddc_ui::widgets::with_icons(ui.ctx(), |icons| {
            icons.paint(ui, "layers", icon_size::MD as u32, r, th.c("text.secondary"));
        });
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new(title)
                    .font(th.font(Role::BodyStrong))
                    .color(th.c("text.primary")),
            );
            if !sub.is_empty() {
                ui.label(
                    egui::RichText::new(sub)
                        .font(th.font(Role::Caption))
                        .color(th.c("text.secondary")),
                );
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ddc_ui::widgets::glyph_button(ui, "close", true, &c.t("props.multi.clear")).clicked() {
                act = Some(Act::Clear);
            }
        });
    });
    if !chips.is_empty() {
        ui.add_space(space::XS);
        let ctrl = ui.input(|i| i.modifiers.command);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(space::XS, space::XS);
            for ch in chips {
                if chip_button(ui, th, c, ch, ch.cat == active_cat).clicked() {
                    act = Some(if ctrl { Act::Narrow(ch.cat) } else { Act::View(ch.cat) });
                }
            }
        });
    }
    ui.add_space(space::XS);
    act
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walls(n: i64) -> Vec<Sel> {
        (1..=n).map(Sel::Wall).collect()
    }

    fn mixed() -> Vec<Sel> {
        let mut v = vec![Sel::Opening(10), Sel::Room(20)];
        v.extend(walls(3));
        v.push(Sel::Opening(11));
        v
    }

    #[test]
    fn counts_per_category() {
        let c = chips(&mixed(), Lang::En);
        let got: Vec<(&str, usize)> = c.iter().map(|x| (x.cat, x.count)).collect();
        assert_eq!(got, vec![("wall", 3), ("opening", 2), ("room", 1)]);
    }

    #[test]
    fn order_is_count_then_name_and_stable() {
        let sel = vec![Sel::Room(1), Sel::Opening(2), Sel::Wall(3)];
        let a = chips(&sel, Lang::En);
        let mut rev = sel.clone();
        rev.reverse();
        assert_eq!(a, chips(&rev, Lang::En));
        let names: Vec<&str> = a.iter().map(|x| x.label.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "equal counts sort by name");
    }

    #[test]
    fn single_category_has_no_chips() {
        assert!(chips(&walls(4), Lang::En).is_empty());
        assert!(chips(&[], Lang::En).is_empty());
    }

    #[test]
    fn labels_follow_the_catalog_language() {
        let en = chips(&mixed(), Lang::En);
        let ru = chips(&mixed(), Lang::Ru);
        assert_eq!(en.len(), ru.len());
        for (e, r) in en.iter().zip(&ru) {
            assert_eq!(e.cat, r.cat);
            assert_ne!(e.label, r.label, "{}", e.cat);
            assert!(!e.label.starts_with("type_dialog."), "{}", e.label);
        }
    }

    #[test]
    fn active_prefers_request_then_largest() {
        let sel = mixed();
        assert_eq!(active(&sel, None, Lang::En), "wall");
        assert_eq!(active(&sel, Some("room"), Lang::En), "room");
        assert_eq!(active(&sel, Some("beam"), Lang::En), "wall", "absent request falls back");
        assert_eq!(active(&[], None, Lang::En), "");
    }

    #[test]
    fn narrow_keeps_one_category_in_order() {
        let got = narrow(&mixed(), "opening");
        assert_eq!(got, vec![Sel::Opening(10), Sel::Opening(11)]);
        assert!(narrow(&mixed(), "beam").is_empty());
    }

    #[test]
    fn click_narrows_selection_without_touching_the_model() {
        let mut e = Editor::new_in_memory().unwrap();
        let t = e.types[1].id;
        let specs: Vec<_> = (0..3)
            .map(|i| ddc_model::WallSpec {
                a: (0., i as f64 * 10.),
                b: (5., i as f64 * 10.),
                level: e.level,
                base_offset: 0.0,
                height: e.height,
                wall_type: t,
                offset0: None,
            })
            .collect();
        let ids = e.model.add_walls(&specs).unwrap();
        e.selection = ids.iter().map(|&i| Sel::Wall(i)).collect();
        e.selection.push(Sel::Room(99));
        let before = e.selection.clone();
        let (hash, rev) = (e.model.content_hash().unwrap(), e.revision);
        assert!(apply_narrow(&mut e, "wall"));
        assert_eq!(e.selection.len(), 3);
        assert!(e.selection.iter().all(|s| matches!(s, Sel::Wall(_))));
        assert_eq!(e.model.content_hash().unwrap(), hash, "model unchanged");
        assert_eq!(e.revision, rev, "no model operation ran");
        // restoring the remembered selection is a plain assignment
        e.selection = before.clone();
        assert_eq!(e.selection, before);
    }

    #[test]
    fn narrow_to_everything_or_nothing_is_a_no_op() {
        let mut e = Editor::new_in_memory().unwrap();
        e.selection = walls(2);
        assert!(!apply_narrow(&mut e, "wall"), "already only walls");
        assert!(!apply_narrow(&mut e, "beam"), "absent category");
        assert_eq!(e.selection, walls(2));
    }

    #[test]
    fn common_detects_varies() {
        assert_eq!(common::<i32>(&[]), Common::Empty);
        assert_eq!(common(&[3, 3, 3]), Common::Same(3));
        assert_eq!(common(&[3, 4, 3, 5]), Common::Varies(3));
        assert_eq!(common(&["a".to_string()]), Common::Same("a".to_string()));
    }

    #[test]
    fn common_text_in_both_languages() {
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            assert_eq!(common_text(&c, &Common::Same("2.80 m".to_string())), "2.80 m");
            assert_eq!(common_text::<String>(&c, &Common::Empty), "\u{2014}");
            for n in [2usize, 3, 5, 11, 21] {
                let t = common_text::<String>(&c, &Common::Varies(n));
                assert!(t.contains(&n.to_string()), "{lang:?} {n}: {t}");
                assert!(!t.starts_with("props.multi"), "{t}");
            }
        }
    }

    #[test]
    fn header_and_toast_texts_exist_in_both_languages() {
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            let (title, sub) = header_text(&c, 7, 3, "");
            assert!(title.contains('7') && !title.starts_with("props.multi"), "{title}");
            assert!(sub.contains('3') && !sub.starts_with("props.multi"), "{sub}");
            let (_, one) = header_text(&c, 4, 1, "Walls");
            assert_eq!(one, "Walls");
            let t = narrowed_text(&c, "Walls", 3);
            assert!(t.contains("Walls") && t.contains('3'), "{t}");
            for k in ["props.multi.clear", "props.multi.undo", "props.multi.chip_tip"] {
                assert_ne!(c.t_in(lang, k), k, "{k}");
            }
        }
        // the two languages differ
        let (en, _) = header_text(&Catalog::new(Lang::En), 5, 2, "");
        let (ru, _) = header_text(&Catalog::new(Lang::Ru), 5, 2, "");
        assert_ne!(en, ru);
    }

    #[test]
    fn every_chip_icon_exists() {
        for cat in [
            "wall", "opening", "curtain", "floor", "room", "ceiling", "column", "beam", "stair",
            "railing", "shaft", "ramp", "room_sep", "zone", "grid", "ref_plane", "mep", "anno",
            "imported", "mass", "comp", "structure",
        ] {
            assert!(ddc_ui::icons::source(chip_icon(cat)).is_some(), "{cat}");
        }
        assert!(ddc_ui::icons::source("layers").is_some());
        assert!(ddc_ui::icons::source("close").is_some());
    }
}
