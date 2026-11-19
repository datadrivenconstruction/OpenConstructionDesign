//! Properties panel presentation: header card text, «differs from type» marker, per-category
//! section state, value/unit cells. Pure helpers first; the data and undo rules stay in the model.
use crate::editor::{Editor, OpFn};
use ddc_model::{ParamDef, ParamSet, ParamType, PVal};
use ddc_ui::theme::{icon_size, space};
use std::sync::Arc;

/// Text of the header card: title line, type line, id and what the copy button puts on the clipboard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderParts {
    pub title: String,
    /// «Type name · 12 of this type»; empty when the element has no type.
    pub subtitle: String,
    pub id_text: String,
    pub copy_text: String,
}

/// `count_text` is the localized «N of this type»; it is shown only next to a type name.
pub fn header_parts(title: &str, type_name: Option<&str>, count_text: &str, id: i64) -> HeaderParts {
    let subtitle = match (type_name, count_text.is_empty()) {
        (Some(t), false) => format!("{t} · {count_text}"),
        (Some(t), true) => t.to_string(),
        (None, _) => String::new(),
    };
    HeaderParts {
        title: title.to_string(),
        subtitle,
        id_text: format!("#{id}"),
        copy_text: id.to_string(),
    }
}

/// True when the element holds its own value that differs from the type's. No type value
/// (or no own value) is not an override.
pub fn overridden(own: Option<&PVal>, ty: Option<&PVal>) -> bool {
    match (own, ty) {
        (Some(PVal::Num(a)), Some(PVal::Num(b))) => {
            (a - b).abs() > 1e-9 * a.abs().max(b.abs()).max(1.0)
        }
        (Some(PVal::Text(a)), Some(PVal::Text(b))) => a.trim() != b.trim(),
        (Some(a), Some(b)) => a != b,
        _ => false,
    }
}

/// State key of a section: the same title keeps its own open/closed state per category.
pub fn section_key(category: &str, title_key: &str) -> String {
    format!("props.{category}.{title_key}")
}

/// Open state of a section (open by default); survives frames and restarts with the memory.
pub fn section_open(ctx: &egui::Context, key: &str) -> bool {
    let id = crate::ui_kit::section_id(key);
    ctx.data_mut(|d| *d.get_persisted_mut_or(id, true))
}

pub fn set_section_open(ctx: &egui::Context, key: &str, open: bool) {
    let id = crate::ui_kit::section_id(key);
    ctx.data_mut(|d| d.insert_persisted(id, open));
}

/// Number and its unit as two cells: a unit given by the caller wins, else one glued to the text.
pub fn value_cell(value: &str, unit: &str) -> (String, String) {
    if !unit.is_empty() {
        return (value.trim().to_string(), unit.to_string());
    }
    let (n, u) = crate::ui_kit::split_unit(value);
    (n.trim().to_string(), u.to_string())
}

/// Numbers sit on the right edge of the value column, text on the left.
pub fn value_align(t: ParamType) -> egui::Align {
    match t {
        ParamType::Length | ParamType::Area | ParamType::Volume | ParamType::Number | ParamType::Int => {
            egui::Align::Max
        }
        _ => egui::Align::Min,
    }
}

/// Value the element's type holds for an instance parameter, if any.
pub fn type_value(ed: &Editor, d: &ParamDef, kind: &str, id: i64) -> Option<PVal> {
    if d.spec.per_type {
        return None;
    }
    let (tk, tid) = ed.model.type_of(kind, id).ok().flatten()?;
    ed.model.param_value(d.id, tk, tid).ok().flatten()
}

/// Writes the type's value to the element: one undo step. Locked values are left alone.
pub fn reset_to_type(ed: &mut Editor, d: &ParamDef, kind: &str, id: i64) -> bool {
    if ed.model.param_locked(kind, id, &d.spec.name).unwrap_or(false) {
        return false;
    }
    let Some(tv) = type_value(ed, d, kind, id) else {
        return false;
    };
    let set = ParamSet {
        param: d.id,
        kind: kind.to_string(),
        id,
        value: Some(tv),
    };
    match ed.run_op(OpFn(Arc::new(move |m: &mut ddc_model::Model| {
        m.set_param_values(std::slice::from_ref(&set))
    }))) {
        Ok(()) => {
            ed.status = Some(crate::tr("prop_head.reset_done", &[("name", d.spec.name.as_str())]));
            true
        }
        Err(e) => {
            ed.status = Some(e.to_string());
            false
        }
    }
}

/// Quiet dot at the row end for a value that differs from the type; click resets it.
pub fn override_dot(ui: &mut egui::Ui) -> egui::Response {
    let s = ddc_ui::sem::sem_of(ui.ctx());
    let side = icon_size::XS;
    let (rect, resp) = ui.allocate_exact_size(egui::Vec2::splat(side), egui::Sense::click());
    let color = if resp.hovered() { s.accent_hover } else { s.accent };
    ui.painter().circle_filled(rect.center(), side / 4.0, color);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(crate::tr("prop_head.override_tip", &[]))
}

/// Header card: category icon, name, type line, id with a copy button.
pub fn header_card(
    ui: &mut egui::Ui,
    th: &ddc_ui::Theme,
    icons: &mut ddc_ui::icons::IconCache,
    icon: &str,
    p: &HeaderParts,
) {
    use ddc_ui::theme::Role;
    ui.add_space(space::XS);
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(egui::Vec2::splat(icon_size::MD), egui::Sense::hover());
        icons.paint(
            ui,
            &ddc_ui::widgets::icon_name(icon, false),
            icon_size::MD as u32,
            r,
            th.c("text.secondary"),
        );
        ui.vertical(|ui| {
            // name and id sit on one baseline: a row as tall as the name, both bottom-aligned
            let row_h = ui.fonts(|f| f.row_height(&th.font(Role::BodyStrong)));
            let row = egui::Layout::left_to_right(egui::Align::Max);
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), row_h), row, |ui| {
                // a long name is cut to what the id leaves, full text in the tooltip
                let idw = ui
                    .painter()
                    .layout_no_wrap(
                        p.id_text.clone(),
                        th.font(Role::Numeric),
                        th.c("text.secondary"),
                    )
                    .size()
                    .x;
                let room = (ui.available_width() - idw - ui.spacing().item_spacing.x).max(0.0);
                ui.allocate_ui_with_layout(egui::vec2(room, row_h), row, |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&p.title)
                                .font(th.font(Role::BodyStrong))
                                .color(th.c("text.primary")),
                        )
                        .truncate(),
                    )
                    .on_hover_text(&p.title);
                });
                let id = ui
                    .add(
                        egui::Label::new(
                            egui::RichText::new(&p.id_text)
                                .font(th.font(Role::Numeric))
                                .color(th.c("text.secondary")),
                        )
                        .sense(egui::Sense::click()),
                    )
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(crate::tr("prop_head.copy_id", &[]));
                if id.clicked() {
                    ui.ctx().copy_text(p.copy_text.clone());
                }
            });
            if !p.subtitle.is_empty() {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&p.subtitle)
                            .font(th.font(Role::Caption))
                            .color(th.c("text.secondary")),
                    )
                    .truncate(),
                )
                .on_hover_text(&p.subtitle);
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::ParamSpec;

    fn num(x: f64) -> PVal {
        PVal::Num(x)
    }

    #[test]
    fn override_detection() {
        assert!(!overridden(Some(&num(3.0)), Some(&num(3.0))));
        assert!(!overridden(Some(&num(3.0)), Some(&num(3.0 + 1e-12))));
        assert!(overridden(Some(&num(3.0)), Some(&num(3.1))));
        assert!(!overridden(Some(&PVal::Text("A ".into())), Some(&PVal::Text("A".into()))));
        assert!(overridden(Some(&PVal::Text("A".into())), Some(&PVal::Text("B".into()))));
        // no type value, or no own value: nothing to differ from
        assert!(!overridden(Some(&num(3.0)), None));
        assert!(!overridden(None, Some(&num(3.0))));
        assert!(!overridden(None, None));
    }

    #[test]
    fn header_text_and_copy() {
        let h = header_parts("Wall", Some("Brick 200"), "12 of this type", 41);
        assert_eq!(h.subtitle, "Brick 200 · 12 of this type");
        assert_eq!(h.id_text, "#41");
        assert_eq!(h.copy_text, "41");
        assert_eq!(header_parts("Room", None, "3 of this type", 7).subtitle, "");
        assert_eq!(header_parts("Wall", Some("T"), "", 1).subtitle, "T");
    }

    #[test]
    fn name_and_id_share_one_baseline() {
        use crate::layout_contract::{driven_ctx, pass};
        let (ctx, mut th) = driven_ctx();
        let id = egui::Id::new("prop_head.test");
        let parts = header_parts("Stair", None, "", 1);
        let mut icons = ddc_ui::icons::IconCache::default();
        let card_th = ddc_ui::Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
        let mut frame = |ctx: &egui::Context| {
            egui::Area::new(id).fixed_pos(egui::pos2(20.0, 20.0)).show(ctx, |ui| {
                ui.set_width(260.0);
                header_card(ui, &card_th, &mut icons, "stair", &parts);
            });
        };
        let p = (0..3).map(|_| pass(&ctx, &mut th, id, vec![], &mut frame)).last().unwrap();
        let at = |t: &str| p.spans.iter().find(|(s, _)| s == t).map(|(_, r)| *r);
        let (name, num) = (at("Stair").expect("name"), at("#1").expect("id"));
        assert!((name.bottom() - num.bottom()).abs() <= 1.0, "name {name:?} id {num:?}");
        assert!(num.left() >= name.right(), "the id follows the name");
    }

    #[test]
    fn of_type_count_is_localized_without_flipping_the_global_language() {
        use ddc_ui::i18n::{Catalog, Lang};
        let en = Catalog::new(Lang::En);
        assert_eq!(en.plural("prop_head.of_type", 1), "1 of this type");
        assert_eq!(en.plural("prop_head.of_type", 12), "12 of this type");
        let ru = Catalog::new(Lang::Ru);
        assert_ne!(ru.plural("prop_head.of_type", 2), "prop_head.of_type");
    }

    #[test]
    fn section_state_is_per_category_and_persists() {
        let ctx = egui::Context::default();
        let (a, b) = (section_key("wall", "inst"), section_key("door", "inst"));
        assert_ne!(a, b);
        assert!(section_open(&ctx, &a) && section_open(&ctx, &b));
        set_section_open(&ctx, &a, false);
        let _ = ctx.run(Default::default(), |_| {});
        assert!(!section_open(&ctx, &a));
        assert!(section_open(&ctx, &b));
        set_section_open(&ctx, &a, true);
        assert!(section_open(&ctx, &a));
    }

    #[test]
    fn value_and_unit_cells() {
        assert_eq!(value_cell("3000", "mm"), ("3000".into(), "mm".into()));
        assert_eq!(value_cell("12.5 m\u{b2}", ""), ("12.5".into(), "m\u{b2}".into()));
        assert_eq!(value_cell(" 7 ", ""), ("7".into(), String::new()));
        assert_eq!(value_cell("3 000 mm", ""), ("3 000".into(), "mm".into()));
        assert_eq!(value_cell("3\u{202f}000\u{a0}mm", ""), ("3\u{202f}000".into(), "mm".into()));
        assert_eq!(value_cell("12 ft 6 in", ""), ("12 ft 6 in".into(), String::new()));
        assert_eq!(value_cell("19'-8 1/4\"", ""), ("19'-8 1/4\"".into(), String::new()));
        assert_eq!(value_align(ParamType::Length), egui::Align::Max);
        assert_eq!(value_align(ParamType::Text), egui::Align::Min);
    }

    fn setup() -> (Editor, ParamDef, i64, i64) {
        let mut e = Editor::new_in_memory().unwrap();
        crate::schedule::test_room(&mut e);
        let pid = e
            .model
            .add_param(&ParamSpec {
                name: "Fire".into(),
                ptype: ParamType::Text,
                per_type: false,
                unit: String::new(),
                group: String::new(),
                guid: None,
                categories: vec!["wall".into()],
            })
            .unwrap();
        e.reload().unwrap();
        let w = e.walls[0].id;
        let (_, tid) = e.model.type_of("wall", w).unwrap().unwrap();
        let d = e.model.param_by_name("Fire").unwrap().unwrap();
        // the model refuses an instance parameter on a type row through the edit layer; the
        // raw store takes it, which is how an imported default looks
        e.model
            .sql()
            .execute(
                "INSERT INTO pparam_val(param, kind, ref, value) VALUES (?1, 'wall_type', ?2, 'EI60')",
                rusqlite::params![pid, tid],
            )
            .unwrap();
        e.model
            .set_param_values(&[ParamSet {
                param: pid,
                kind: "wall".into(),
                id: w,
                value: Some(PVal::Text("EI15".into())),
            }])
            .unwrap();
        (e, d, w, tid)
    }

    #[test]
    fn reset_to_type_is_one_undo_step() {
        let (mut e, d, w, _) = setup();
        let ty = type_value(&e, &d, "wall", w);
        let own = e.model.param_value(d.id, "wall", w).unwrap();
        assert!(overridden(own.as_ref(), ty.as_ref()));
        assert!(reset_to_type(&mut e, &d, "wall", w));
        assert_eq!(e.model.param_value(d.id, "wall", w).unwrap(), Some(PVal::Text("EI60".into())));
        e.undo();
        assert_eq!(e.model.param_value(d.id, "wall", w).unwrap(), Some(PVal::Text("EI15".into())));
        e.redo();
        assert_eq!(e.model.param_value(d.id, "wall", w).unwrap(), Some(PVal::Text("EI60".into())));
    }

    #[test]
    fn locked_value_is_not_reset() {
        let (mut e, d, w, _) = setup();
        e.model.set_param_lock(&[("wall", w, "Fire")]).unwrap();
        assert!(!reset_to_type(&mut e, &d, "wall", w));
        assert_eq!(e.model.param_value(d.id, "wall", w).unwrap(), Some(PVal::Text("EI15".into())));
    }

    #[test]
    fn no_type_value_means_no_reset() {
        let (mut e, d, w, tid) = setup();
        e.model
            .sql()
            .execute("DELETE FROM pparam_val WHERE kind='wall_type' AND ref=?1", [tid])
            .unwrap();
        assert_eq!(type_value(&e, &d, "wall", w), None);
        assert!(!reset_to_type(&mut e, &d, "wall", w));
    }
}
