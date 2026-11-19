//! Sheet properties and revision issue dialogs on the kit: commit path, undo, render, source.
use super::props::{self, Props};
use super::*;

/// Context with the named font weights (section headers, table headers): they exist from the
/// pass after install, a bare context panics on them.
fn font_ctx() -> egui::Context {
    let ctx = egui::Context::default();
    ddc_ui::theme::install_fonts(&ctx);
    let _ = ctx.run(Default::default(), |_| {});
    ctx
}

#[test]
fn font_ctx_binds_the_named_weights() {
    let ctx = font_ctx();
    let _ = ctx.run(Default::default(), |c| {
        for role in [Role::Section, Role::TableHeader] {
            let w = c.fonts(|f| f.layout_no_wrap("A".into(), ddc_ui::typo::font(role), Color32::WHITE).size().x);
            assert!(w > 0.0);
        }
    });
}

fn frames(mut draw: impl FnMut(&egui::Context)) {
    let ctx = font_ctx();
    for _ in 0..3 {
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 720.0),
                )),
                ..Default::default()
            },
            |c| draw(c),
        );
    }
}

fn rev(mark: &str, text: &str) -> Revision {
    Revision {
        mark: mark.into(),
        date: "01.01.2026".into(),
        description: text.into(),
        author: "AB".into(),
    }
}

#[test]
fn duplicate_number_keeps_the_inline_error_and_refuses_the_save() {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.model.add_sheet(&SheetDef::new("A-101", "Plan")).unwrap();
    let b = ed.model.add_sheet(&SheetDef::new("A-102", "Section")).unwrap();
    for ru in [false, true] {
        let mut p = Props::load(&ed, ed.model.sheet(b).unwrap(), Some(b));
        p.def.number = "A-101".into();
        let why = props::number_block(&ed, ru, p.id, &p.def.number).expect("taken");
        assert!(why.contains("A-101"), "{why}");
        // the window still draws the error and returns no result
        frames(|c| {
            assert!(props::props_window(c, &mut ed, ru, &mut p).is_none());
        });
        let revision = ed.revision;
        assert!(props::save_props(&mut ed, ru, &p).is_err());
        assert_eq!(ed.revision, revision);
        assert_eq!(ed.model.sheet(b).unwrap().number, "A-102");
    }
}

#[test]
fn revision_add_and_remove_round_trip_in_one_undo_step_each() {
    let mut ed = Editor::new_in_memory().unwrap();
    let a = ed.model.add_sheet(&SheetDef::new("A-101", "Plan")).unwrap();
    let mut p = Props::load(&ed, ed.model.sheet(a).unwrap(), Some(a));
    p.revs.push(rev("1", "first"));
    p.revs.push(rev("2", "second"));
    p.def.name = "Plan 2".into();
    props::save_props(&mut ed, false, &p).unwrap();
    assert_eq!(ed.model.revisions(a).unwrap().len(), 2);
    assert_eq!(ed.model.sheet(a).unwrap().name, "Plan 2");
    // name and revisions are one operation
    ed.undo();
    assert!(ed.model.revisions(a).unwrap().is_empty());
    assert_eq!(ed.model.sheet(a).unwrap().name, "Plan");
    ed.redo();

    let mut p = Props::load(&ed, ed.model.sheet(a).unwrap(), Some(a));
    p.revs.remove(0);
    let revision = ed.revision;
    props::save_props(&mut ed, false, &p).unwrap();
    assert_ne!(ed.revision, revision);
    let left = ed.model.revisions(a).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].description, "second");
    ed.undo();
    assert_eq!(ed.model.revisions(a).unwrap().len(), 2);

    // nothing changed: no undo step
    ed.redo();
    let p = Props::load(&ed, ed.model.sheet(a).unwrap(), Some(a));
    let revision = ed.revision;
    props::save_props(&mut ed, false, &p).unwrap();
    assert_eq!(ed.revision, revision);
}

#[test]
fn dialogs_render_headless_in_both_languages() {
    let mut ed = Editor::new_in_memory().unwrap();
    let a = ed.model.add_sheet(&SheetDef::new("A-101", "Plan")).unwrap();
    for ru in [false, true] {
        for gost in [false, true] {
            let mut d = ed.model.sheet(a).unwrap();
            d.standard = if gost { "gost" } else { "iso" }.into();
            let mut p = Props::load(&ed, d, Some(a));
            p.revs = vec![
                rev("1", "A very long revision description that must be elided in the table"),
                rev("2", "short"),
            ];
            p.sel = Some(1);
            frames(|c| {
                assert!(props::props_window(c, &mut ed, ru, &mut p).is_none());
            });
        }
    }
    let def = ed.model.sheet(a).unwrap();
    let at = {
        let cells = io::stamp_cells(&def.standard, &def.paper, def.landscape).unwrap();
        let c = cells.iter().find(|c| c.key == "legal_owner").unwrap();
        [c.rect[0] + 1.0, c.rect[1] + 1.0]
    };
    for ru in [false, true] {
        let mut e = issue::stamp_edit_at(&ed, a, &def, at, ru).unwrap();
        frames(|c| {
            assert!(issue::stamp_window(c, &mut ed, ru, &mut e));
        });
    }
}

#[test]
fn revision_headers_fit_the_form_width_in_ru_and_en() {
    let th = ddc_ui::Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
    let ctx = font_ctx();
    let avail = ddc_ui::theme::layout::DIALOG_FORM - 2.0 * space::XL;
    for ru in [false, true] {
        let cols = props::rev_cols(ru);
        let widths = ddc_ui::widgets::table::column_widths(&cols, avail);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            for (c, w) in cols.iter().zip(&widths) {
                let tw = ctx.fonts(|f| {
                    f.layout_no_wrap(c.title.to_string(), th.font(Role::TableHeader), Color32::WHITE)
                        .size()
                        .x
                });
                assert!(tw + 2.0 * th.d().pad_x <= *w, "{} ru={ru}: {tw} > {w}", c.title);
            }
        });
    }
}

/// Production part of a source file (tests excluded).
fn prod(src: &str) -> &str {
    &src[..src.find("#[cfg(test)]").unwrap_or(src.len())]
}

#[test]
fn props_and_issue_dialogs_use_no_raw_widgets_or_literal_widths() {
    for (name, src) in [
        ("props.rs", include_str!("props.rs")),
        ("issue.rs", include_str!("issue.rs")),
    ] {
        let s = prod(src);
        for raw in [
            "egui::TextEdit",
            "text_edit_singleline",
            "egui::ComboBox",
            "selectable_value",
            "radio_value",
            "ui.button(",
            "egui::Grid",
            "desired_width(",
            "close_button(",
            ".fixed(",
        ] {
            assert!(!s.contains(raw), "{name}: raw widget {raw}");
        }
        // every `.width(` takes a computed value, never a number
        for (i, _) in s.match_indices(".width(") {
            let next = s[i + 7..].chars().next().unwrap_or(' ');
            assert!(!next.is_ascii_digit(), "{name}: literal width at byte {i}");
        }
    }
    let p = prod(include_str!("props.rs"));
    assert!(p.contains("Table::new("));
    assert!(p.contains("widgets::dropdown("));
    assert!(p.contains("TextField::new("));
    assert!(p.contains("Button::quiet("));
}

#[test]
fn bare_sheet_demo_flag_opens_the_canvas() {
    let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(super::demo_arg(&a(&["x", "--sheet-demo"])), Some("canvas"));
    assert_eq!(super::demo_arg(&a(&["--sheet-demo", "--lang", "ru"])), Some("canvas"));
    assert_eq!(super::demo_arg(&a(&["--sheet-demo", "print"])), Some("print"));
    assert_eq!(super::demo_arg(&a(&["--screenshot", "o.png"])), None);
}

/// "Publish PDF" is findable by its own words in the palette and runs the sheet export.
#[test]
fn publish_pdf_is_found_by_name_and_runs_sheet_export() {
    let reg = ddc_ui::Registry::builtin();
    assert_eq!(reg.resolve("file.publish_pdf"), "data.export_pdf");
    let ru_q = crate::app2_fx::fx("publish_pdf_query");
    for (lang, q) in [(ddc_ui::Lang::Ru, ru_q.as_str()), (ddc_ui::Lang::En, "publish pdf")] {
        let cat = ddc_ui::Catalog::new(lang);
        let hits = ddc_ui::fuzzy::search(&reg, &cat, q);
        let top: Vec<&str> = hits.iter().take(3).map(|c| reg.resolve(&c.id)).collect();
        assert!(top.contains(&"data.export_pdf"), "{lang:?}: {top:?}");
    }
}
