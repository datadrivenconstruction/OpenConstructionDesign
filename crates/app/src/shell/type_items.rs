//! Data for the shared type picker: rows, subtitle and texts of the wall type fields.
use crate::editor::WallType;
use crate::units::{display_length_lang, Units};
use ddc_ui::i18n::{Catalog, Lang};
use ddc_ui::widgets::type_pick::{PickItem, PickTexts};

fn plural(ru: bool, key: &str, n: u64) -> String {
    static RU: std::sync::OnceLock<Catalog> = std::sync::OnceLock::new();
    static EN: std::sync::OnceLock<Catalog> = std::sync::OnceLock::new();
    let c = if ru {
        RU.get_or_init(|| Catalog::new(Lang::Ru))
    } else {
        EN.get_or_init(|| Catalog::new(Lang::En))
    };
    c.plural(key, n)
}

/// `300 mm · 3 layers`, with the unit and the noun in the given language.
pub(super) fn detail(t: &WallType, u: Units, ru: bool) -> String {
    format!(
        "{} \u{b7} {}",
        display_length_lang(t.thickness(), u, ru),
        plural(ru, "type_pick.pick.layers", t.layers.len() as u64)
    )
}

pub(crate) fn items(types: &[WallType], u: Units, ru: bool) -> Vec<PickItem> {
    types
        .iter()
        .map(|t| PickItem {
            id: t.id,
            name: t.name.clone(),
            detail: detail(t, u, ru),
        })
        .collect()
}

pub(crate) fn texts(ru: bool) -> PickTexts {
    PickTexts {
        search: crate::tr_lang(ru, "type_pick.pick.search", &[]),
        empty: crate::tr_lang(ru, "type_pick.pick.empty", &[]),
        edit: crate::tr_lang(ru, "type_pick.pick.edit", &[]),
        mixed: crate::tr_lang(ru, "type_pick.pick.mixed", &[]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    use super::super::{browser, edit_walls, type_dialog, wall_type_editor, with_type_dialog};
    use crate::wall_tool::{Event, Mode};
    use ddc_geom::v2;
    use ddc_ui::widgets::type_pick::filter;

    fn ty(id: i64, name: &str, layers: &[f64]) -> WallType {
        WallType {
            id,
            name: name.into(),
            layers: layers.to_vec(),
        }
    }

    #[test]
    fn subtitle_metric_en_and_ru() {
        let t = ty(1, "A", &[0.1, 0.1, 0.1]);
        assert_eq!(detail(&t, Units::Metric, false), "300\u{a0}mm \u{b7} 3 layers");
        assert_eq!(
            detail(&t, Units::Metric, true),
            "300\u{a0}\u{43c}\u{43c} \u{b7} 3 \u{441}\u{43b}\u{43e}\u{44f}"
        );
        let one = ty(2, "B", &[0.2]);
        assert!(detail(&one, Units::Metric, false).ends_with("1 layer"));
        assert!(detail(&one, Units::Metric, true).ends_with("1 \u{441}\u{43b}\u{43e}\u{439}"));
        let five = ty(3, "C", &[0.05; 5]);
        assert!(detail(&five, Units::Metric, true).ends_with("5 \u{441}\u{43b}\u{43e}\u{451}\u{432}"));
    }

    #[test]
    fn subtitle_imperial_uses_feet_and_inches() {
        let t = ty(1, "A", &[0.3048]);
        let s = detail(&t, Units::Imperial, false);
        assert!(s.contains('\'') && s.ends_with("1 layer"), "{s}");
    }

    #[test]
    fn search_by_name_and_by_thickness_over_real_rows() {
        let types = vec![ty(1, "Brick", &[0.1, 0.2]), ty(2, "Gypsum", &[0.0125])];
        let it = items(&types, Units::Metric, false);
        assert_eq!(filter(&it, "gyp"), vec![1]);
        assert_eq!(filter(&it, "300"), vec![0]);
        assert_eq!(filter(&it, "2 layers"), vec![0]);
        assert!(filter(&it, "nothing").is_empty());
    }

    #[test]
    fn mixed_label_in_both_languages() {
        assert_eq!(texts(false).mixed, "Mixed");
        assert_eq!(texts(true).mixed, "\u{420}\u{430}\u{437}\u{43d}\u{44b}\u{435}");
        assert_eq!(texts(false).edit, "Edit type\u{2026}");
        assert!(!texts(true).empty.is_empty() && !texts(true).search.is_empty());
    }

    fn room() -> Editor {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.set_mode(Mode::Rect);
        ed.handle(Event::Move(v2(0.0, 0.0)));
        ed.handle(Event::Press(v2(0.0, 0.0)));
        ed.handle(Event::Move(v2(6.0, 4.0)));
        ed.handle(Event::Release(v2(6.0, 4.0)));
        ed.set_mode(Mode::Select);
        ed
    }

    #[test]
    fn picking_a_type_is_one_undo_step() {
        let mut ed = room();
        let ids: Vec<i64> = ed.walls.iter().map(|w| w.id).collect();
        let first = ed.walls[0].wall_type;
        let t = ed.types.iter().find(|t| t.id != first).expect("a second type").id;
        let before = ed.can_undo();
        assert!(edit_walls(&mut ed, &ids, Some(t), None));
        assert!(ed.walls.iter().all(|w| w.wall_type == t));
        ed.undo();
        assert!(ed.walls.iter().all(|w| w.wall_type != t));
        assert_eq!(ed.can_undo(), before);
    }

    #[test]
    fn edit_type_entry_opens_dialog_for_current_type() {
        let ed = room();
        let id = ed.walls[0].wall_type;
        browser::with(|s| s.type_dlg = None);
        wall_type_editor::with(|o| *o = None);
        with_type_dialog(&ed, type_dialog::Fam::Wall, id);
        // walls open the layer editor, not the generic type dialog
        assert!(browser::with(|s| s.type_dlg.is_none()));
        let open = wall_type_editor::with(|o| o.as_ref().map(|d| d.id));
        assert_eq!(open, Some(id));
        wall_type_editor::with(|o| *o = None);
    }

    #[test]
    fn call_sites_no_longer_use_combo_boxes() {
        let panels = include_str!("panels.rs");
        let tool = include_str!("tool_opts.rs");
        assert!(!panels.contains("from_id_salt(\"sel.wtype\")"));
        assert!(!panels.contains("from_id_salt(\"sel.mtype\")"));
        assert!(!tool.contains("\"length_input.wtype\""));
        // one "Edit type" entry: the picker footer, not a second button in the type block
        assert!(!include_str!("prop_rows.rs").contains("\"props.edit_type\""));
        assert!(tool.contains(".hover(tr(ru, \"type_dialog.panels.wall_type\"))"));
    }
}
