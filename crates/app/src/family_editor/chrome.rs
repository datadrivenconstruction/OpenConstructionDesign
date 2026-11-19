// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Drawn chrome of the family editor window: document tabs, the open menu, labelled header
//! pickers. Built on the kit widgets and theme tokens only.

use super::{Action, Project, Session};
use crate::units::Units;
use ddc_ui::theme::{control, space, Role};
use ddc_ui::widgets::{self, Button, MenuTone, TextField};
use ddc_ui::{Catalog, Theme};
use egui::{self, RichText};

/// More entries than this in the open menu add a search field.
pub const SEARCH_MIN: usize = 12;

/// Label of a document tab and whether it has unsaved edits (shown as a dot, not in the text).
pub fn doc_tab(s: &Session) -> (String, bool) {
    (s.name(), s.doc.dirty)
}

/// Volume with its unit in the project units, and the solid count.
pub fn stats_text(cat: &Catalog, volume_m3: f64, parts: usize, units: Units, ru: bool) -> String {
    let v = crate::unit_text::vol(volume_m3, ru, units);
    cat.tf(
        "fe.stats",
        &[("volume", &v), ("parts", &parts.to_string())],
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Families,
    Drafts,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenRow {
    Header(Section),
    Family(i64, String),
    Draft(i64, String),
}

fn matches(name: &str, q: &str) -> bool {
    q.is_empty() || name.to_lowercase().contains(&q.to_lowercase())
}

/// Rows of the open menu: a section header followed by its matches; empty sections are dropped.
pub fn open_menu_rows(
    families: &[(i64, String)],
    drafts: &[(i64, String, Option<i64>)],
    query: &str,
) -> Vec<OpenRow> {
    let q = query.trim();
    let mut out = Vec::new();
    let fam: Vec<_> = families.iter().filter(|(_, n)| matches(n, q)).collect();
    if !fam.is_empty() {
        out.push(OpenRow::Header(Section::Families));
        out.extend(fam.into_iter().map(|(id, n)| OpenRow::Family(*id, n.clone())));
    }
    let dr: Vec<_> = drafts.iter().filter(|(_, n, _)| matches(n, q)).collect();
    if !dr.is_empty() {
        out.push(OpenRow::Header(Section::Drafts));
        out.extend(dr.into_iter().map(|(id, n, _)| OpenRow::Draft(*id, n.clone())));
    }
    out
}

fn caption(ui: &mut egui::Ui, th: &Theme, text: &str) {
    ui.label(
        RichText::new(text)
            .font(th.font(Role::Caption))
            .color(th.c("text.secondary")),
    );
}

/// Dropdown with a caption above it. Returns the new index when the pick changed.
pub fn picker(
    ui: &mut egui::Ui,
    th: &Theme,
    id: &str,
    title: &str,
    sel: usize,
    items: &[String],
) -> Option<usize> {
    let refs: Vec<&str> = items.iter().map(String::as_str).collect();
    let mut cur = sel;
    let mut changed = false;
    ui.vertical(|ui| {
        caption(ui, th, title);
        changed = widgets::field::dropdown_h(ui, th, id, &mut cur, &refs, None, None).changed();
    });
    changed.then_some(cur)
}

/// Document tabs with a dirty dot and a close button, then New and the open menu.
pub fn doc_tabs(
    ui: &mut egui::Ui,
    th: &Theme,
    cat: &Catalog,
    docs: &[Session],
    active: &mut usize,
    project: &Project,
    acts: &mut Vec<Action>,
) {
    ui.horizontal_wrapped(|ui| {
        for (i, d) in docs.iter().enumerate() {
            let (name, dirty) = doc_tab(d);
            let natural = ui
                .painter()
                .layout_no_wrap(name.clone(), th.font(Role::Tab), th.sem().text_primary)
                .size()
                .x
                + 2.0 * space::MD;
            let w = natural.min(2.0 * control::DROPDOWN_W);
            let mut sel = if i == *active { 0 } else { usize::MAX };
            ui.allocate_ui_with_layout(
                egui::vec2(w, control::TAB_H),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    if widgets::tab_bar(ui, th, &[name.as_str()], &mut sel).is_some() {
                        *active = i;
                    }
                },
            );
            if dirty {
                let (r, resp) =
                    ui.allocate_exact_size(egui::Vec2::splat(space::MD), egui::Sense::hover());
                ui.painter()
                    .circle_filled(r.center(), space::XS / 2.0, th.sem().accent);
                resp.on_hover_text(cat.t("family_editor.unsaved"));
            }
            let tip = cat.tf("family_editor.close_named", &[("name", &name)]);
            if widgets::close_button(ui, &tip).clicked() {
                acts.push(Action::Close(i));
            }
        }
        if Button::secondary(&cat.t("family_editor.new_family"))
            .show(ui, th)
            .clicked()
        {
            acts.push(Action::New);
        }
        open_menu(ui, th, cat, project, acts);
    });
}

fn open_menu(ui: &mut egui::Ui, th: &Theme, cat: &Catalog, project: &Project, acts: &mut Vec<Action>) {
    let ctx = ui.ctx().clone();
    let resp = widgets::with_icons(&ctx, |icons| {
        widgets::icon_button(ui, icons, th, "open", false, &cat.t("family_editor.open_tip"))
    });
    let total = project.families.len() + project.drafts.len();
    let qid = resp.id.with("ux51d.q");
    egui::Popup::menu(&resp)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(control::DROPDOWN_W);
            let mut q: String = ui.data_mut(|d| d.get_temp(qid)).unwrap_or_default();
            if total > SEARCH_MIN {
                let hint = cat.t("family_editor.search");
                TextField::new(&mut q)
                    .id_salt("family_editor.search")
                    .labelled_by(egui::Id::new("ux51d.search.lbl"), &hint)
                    .hint(&hint)
                    .show(ui, th);
                ui.data_mut(|d| d.insert_temp(qid, q.clone()));
            }
            let rows = open_menu_rows(&project.families, &project.drafts, &q);
            if rows.is_empty() {
                caption(ui, th, &cat.t("family_editor.nothing"));
            }
            for row in rows {
                let hit = match &row {
                    OpenRow::Header(s) => {
                        let key = match s {
                            Section::Families => "family_editor.sec_families",
                            Section::Drafts => "family_editor.sec_drafts",
                        };
                        caption(ui, th, &cat.t(key));
                        None
                    }
                    OpenRow::Family(id, n) => {
                        widgets::menu_item(ui, th, n, "", MenuTone::Normal, true)
                            .clicked()
                            .then_some(Action::OpenFamily(*id))
                    }
                    OpenRow::Draft(id, n) => {
                        widgets::menu_item(ui, th, n, "", MenuTone::Normal, true)
                            .clicked()
                            .then_some(Action::OpenDraft(*id))
                    }
                };
                if let Some(a) = hit {
                    acts.push(a);
                    ui.close();
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::component::doc::FamDoc;
    use ddc_geom::component::{Category, Hosting};
    use ddc_ui::Lang;

    fn fams() -> Vec<(i64, String)> {
        vec![(1, "Door".into()), (2, "Window".into()), (3, "Doorway".into())]
    }

    fn drafts() -> Vec<(i64, String, Option<i64>)> {
        vec![(7, "Door draft".into(), None), (8, "Chair".into(), Some(1))]
    }

    #[test]
    fn doc_tab_has_no_glued_bullet() {
        let mut s = Session::new(FamDoc::blank("Door", Category::Generic, Hosting::None), None);
        s.doc.dirty = true;
        let (label, dirty) = doc_tab(&s);
        assert!(dirty);
        assert!(!label.contains('\u{2022}') && !label.ends_with(' '), "{label}");
        s.doc.dirty = false;
        assert!(!doc_tab(&s).1);
    }

    #[test]
    fn stats_name_the_unit() {
        let cat = Catalog::new(Lang::En);
        let m = stats_text(&cat, 0.125, 3, Units::Metric, false);
        assert!(m.contains("0.125") && m.contains('\u{b3}') && m.contains('3'), "{m}");
        let i = stats_text(&cat, 0.125, 3, Units::Imperial, false);
        assert!(i.contains("ft"), "{i}");
        let ru = stats_text(&Catalog::new(Lang::Ru), 0.125, 3, Units::Metric, true);
        assert!(ru.contains('\u{43c}'), "{ru}");
    }

    #[test]
    fn open_rows_keep_sections() {
        let all = open_menu_rows(&fams(), &drafts(), "");
        assert_eq!(all.len(), 2 + 3 + 2);
        assert_eq!(all[0], OpenRow::Header(Section::Families));
        assert_eq!(all[4], OpenRow::Header(Section::Drafts));
        let hit = open_menu_rows(&fams(), &drafts(), " DOOR ");
        assert_eq!(
            hit,
            vec![
                OpenRow::Header(Section::Families),
                OpenRow::Family(1, "Door".into()),
                OpenRow::Family(3, "Doorway".into()),
                OpenRow::Header(Section::Drafts),
                OpenRow::Draft(7, "Door draft".into()),
            ]
        );
        let only = open_menu_rows(&fams(), &drafts(), "chair");
        assert_eq!(
            only,
            vec![OpenRow::Header(Section::Drafts), OpenRow::Draft(8, "Chair".into())]
        );
        assert!(open_menu_rows(&fams(), &drafts(), "zzz").is_empty());
        assert!(open_menu_rows(&[], &[], "").is_empty());
    }

    #[test]
    fn tabs_and_menu_draw_headless() {
        let th = Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Comfortable);
        let cat = Catalog::new(Lang::En);
        let docs = vec![
            Session::new(FamDoc::blank("A", Category::Generic, Hosting::None), None),
            Session::new(FamDoc::blank("B", Category::Generic, Hosting::None), None),
        ];
        let project = Project {
            families: fams(),
            drafts: drafts(),
            imperial: false,
        };
        let mut active = 1;
        let mut acts = Vec::new();
        let ctx = ddc_ui::theme::headless_ctx();
        let _ = ctx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| {
                doc_tabs(ui, &th, &cat, &docs, &mut active, &project, &mut acts);
            });
        });
        assert_eq!(active, 1);
        assert!(acts.is_empty());
    }

    #[test]
    fn strings_in_en_and_ru() {
        let cat = Catalog::new(Lang::En);
        for k in [
            "family_editor.new_family",
            "family_editor.open_tip",
            "family_editor.close_named",
            "family_editor.unsaved",
            "family_editor.search",
            "family_editor.nothing",
            "family_editor.sec_families",
            "family_editor.sec_drafts",
            "family_editor.cap_category",
            "family_editor.cap_host",
            "family_editor.cap_type",
        ] {
            for lang in [Lang::En, Lang::Ru] {
                let t = cat.table(lang).get(k).and_then(|v| v.as_str());
                assert!(t.is_some_and(|t| !t.is_empty()), "{k} {lang:?}");
            }
        }
    }

    #[test]
    fn no_raw_widgets_in_window_source() {
        let src = include_str!("mod.rs");
        let src = src.split("#[cfg(test)]").next().unwrap_or(src);
        for banned in [
            "egui::ComboBox",
            "selectable_label(",
            "ui.checkbox(",
            "ui.button(",
            "egui::Button",
        ] {
            assert!(!src.contains(banned), "{banned}");
        }
    }
}
