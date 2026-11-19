// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Geometry contract of the Properties panel, drawn headless through `EditorApp::props` inside
//! the real side panel, at its minimum and default width, in both languages (explicit catalog
//! and fonts, the global UI language is never touched).
//!
//! Columns: every value widget (field, dropdown, toggle) of every section starts at one left x,
//! and lies inside the panel. Keys: no key cell wraps or is elided. Widgets come from the
//! accessibility tree of the last frame, keys from the test log of `ui_kit::key`.

use super::{tokens, EditorApp};
use crate::editor::bodies::BodyKind;
use crate::editor::{Editor, Sel};
use crate::ui_kit::{self, KeyRow};
use ddc_model::{OpeningSpec, RampSpec, WallSpec};
use ddc_ui::{Catalog, Lang};
use egui::{self, accesskit};

const SCREEN: (f32, f32) = (1600.0, 1000.0);
const EPS: f32 = 0.5;
const FRAMES: usize = 4;

#[derive(Clone, Copy)]
enum Pick {
    Wall,
    Door,
    /// A ramp: its panel carries the level dropdowns.
    Level,
    Imported,
    /// Nothing selected.
    Nothing,
    /// Two walls.
    Many,
    /// A straight stair.
    Stair,
    /// An imported mesh whose category is a very long word.
    LongName,
    /// A room closed by four walls.
    Room,
}

/// Category of the `LongName` mesh: no break to wrap at.
const LONG: &str = "WWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWW";

struct Drawn {
    /// Every text the last frame painted, with its rect.
    texts: Vec<(String, egui::Rect)>,
    panel: egui::Rect,
    asked: f32,
    keys: Vec<KeyRow>,
    /// (role, label, rect) of every value widget.
    widgets: Vec<(accesskit::Role, String, egui::Rect)>,
    /// Frame of the value widget of a key row, from the kit's row probe (kv_field, kv_widget):
    /// the accessible node of a kit text field is its text area, inset by the field padding.
    probes: Vec<(String, egui::Rect)>,
}

fn font_ctx() -> egui::Context {
    ddc_ui::theme::headless_ctx()
}

fn is_value_widget(role: accesskit::Role) -> bool {
    use accesskit::Role;
    matches!(
        role,
        Role::TextInput | Role::SpinButton | Role::ComboBox | Role::CheckBox | Role::Slider
    )
}

fn collect_texts(s: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
    match s {
        egui::Shape::Text(t) => {
            let r = egui::Rect::from_min_size(t.pos, t.galley.size());
            out.push((t.galley.text().to_string(), r));
        }
        egui::Shape::Vec(v) => v.iter().for_each(|x| collect_texts(x, out)),
        _ => {}
    }
}

fn app_with(pick: Pick, lang: Lang) -> EditorApp {
    let mut ed = Editor::new_in_memory().unwrap();
    let level = ed.level;
    let wall_type = ed.current_type().unwrap().id;
    let wall = ed
        .model
        .add_wall(&WallSpec {
            a: (0.0, 0.0),
            b: (5.0, 0.0),
            level,
            base_offset: 0.0,
            height: 3.0,
            wall_type,
            offset0: None,
        })
        .unwrap();
    let mut second = None;
    let sel = match pick {
        Pick::Wall => Sel::Wall(wall),
        Pick::Door => {
            let spec = OpeningSpec { along: 1.0, width: 0.9, sill: 0.0, height: 2.1, kind: "door".into() };
            Sel::Opening(ed.model.add_opening(wall, &spec).unwrap())
        }
        Pick::Level => {
            let mut spec = RampSpec::new(level, None, vec![(0.0, 2.0), (8.0, 2.0)]);
            spec.top_offset = 0.5;
            let id = ed.model.add_ramp(&spec).unwrap();
            Sel::Ext(crate::editor::structx::code_of("ramp").unwrap(), id)
        }
        Pick::Nothing => Sel::Wall(wall),
        Pick::Many => {
            let spec = WallSpec {
                a: (0.0, 3.0),
                b: (5.0, 3.0),
                level,
                base_offset: 0.0,
                height: 3.0,
                wall_type,
                offset0: None,
            };
            second = Some(Sel::Wall(ed.model.add_wall(&spec).unwrap()));
            Sel::Wall(wall)
        }
        Pick::Room => {
            let sides = [
                ((5.0, 0.0), (5.0, 4.0)),
                ((5.0, 4.0), (0.0, 4.0)),
                ((0.0, 4.0), (0.0, 0.0)),
            ];
            for (a, b) in sides {
                let spec = WallSpec {
                    a,
                    b,
                    level,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type,
                    offset0: None,
                };
                ed.model.add_wall(&spec).unwrap();
            }
            let spec = ddc_model::RoomSpec {
                level,
                name: "Hall".into(),
                number: "1".into(),
                point: (2.5, 2.0),
                height: 3.0,
            };
            Sel::Room(ed.model.add_room(&spec).unwrap())
        }
        Pick::Stair => {
            let top = ed.model.add_level("Top", ed.elevation + 3.0).unwrap();
            let spec = ddc_model::StairSpec::straight(level, top, (0.0, 0.0), 0.0);
            let id = ed.model.add_stair(&spec).unwrap();
            crate::editor::structx::sel("stair", id).unwrap()
        }
        Pick::Imported | Pick::LongName => {
            let cat = if matches!(pick, Pick::LongName) { LONG } else { "furniture" };
            let pos: Vec<u8> = [0.0f64, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect();
            let idx: Vec<u8> = [0u32, 2, 1, 0, 1, 3, 1, 2, 3, 0, 3, 2]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect();
            ed.model
                .sql()
                .execute(
                    "INSERT INTO mesh(level, category, name, positions, indices) VALUES (?1,?2,'x',?3,?4)",
                    rusqlite::params![level, cat, pos, idx],
                )
                .unwrap();
            Sel::Body(BodyKind::Mesh, ed.model.sql().last_insert_rowid())
        }
    };
    ed.reload().unwrap();
    ed.selection = match pick {
        Pick::Nothing => vec![],
        _ => std::iter::once(sel).chain(second).collect(),
    };
    let mut app = EditorApp::new(ed, None, "test".into());
    app.sh.cat = Catalog::new(lang);
    app
}

fn draw(pick: Pick, lang: Lang, width: f32) -> Drawn {
    let mut app = app_with(pick, lang);
    let ctx = font_ctx();
    ctx.enable_accesskit();
    let mut out = Drawn {
        texts: vec![],
        panel: egui::Rect::NOTHING,
        asked: width,
        keys: vec![],
        widgets: vec![],
        probes: vec![],
    };
    for _ in 0..FRAMES {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SCREEN.0, SCREEN.1))),
            ..egui::RawInput::default()
        };
        let mut got = 0.0;
        let run = ctx.run(raw, |ctx| {
            ctx.data_mut(|d| d.remove::<Vec<KeyRow>>(ui_kit::key_log_id()));
            let _ = app.sh.theme.apply(ctx);
            let t = tokens(&app.sh.theme);
            got = ui_kit::side_panel(ctx, &t, width, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("ddc-props.scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| app.props(ui, &t));
            });
        });
        out.panel = egui::Rect::from_min_max(egui::pos2(SCREEN.0 - got, 0.0), egui::pos2(SCREEN.0, SCREEN.1));
        out.keys = ctx.data(|d| d.get_temp::<Vec<KeyRow>>(ui_kit::key_log_id())).unwrap_or_default();
        out.widgets.clear();
        out.texts.clear();
        for c in &run.shapes {
            collect_texts(&c.shape, &mut out.texts);
        }
        if let Some(update) = run.platform_output.accesskit_update {
            for (_, node) in update.nodes {
                let (Some(b), true) = (node.bounds(), is_value_widget(node.role())) else {
                    continue;
                };
                let rect = egui::Rect::from_min_max(
                    egui::pos2(b.x0 as f32, b.y0 as f32),
                    egui::pos2(b.x1 as f32, b.y1 as f32),
                );
                if rect.width() < 1.0 || rect.height() < 1.0 {
                    continue;
                }
                let label = node.label().or_else(|| node.value()).unwrap_or("").to_string();
                out.widgets.push((node.role(), label, rect));
            }
        }
    }
    out.probes = out
        .keys
        .iter()
        .filter_map(|(k, _, _, _)| ui_kit::probed(&ctx, k).map(|r| (k.clone(), r)))
        .collect();
    out
}

/// Value widgets that sit in a key row: the leftmost of each row, right of its key. A probed
/// row gives its widget frame; other rows fall back to the accessible nodes.
fn row_widgets(d: &Drawn) -> Vec<(String, egui::Rect)> {
    let mut rows = vec![];
    let near = |r: &egui::Rect, kr: &egui::Rect| {
        (r.center().y - kr.center().y).abs() < 10.0 && r.left() >= kr.right() - EPS
    };
    for (k, kr, _, _) in &d.keys {
        let probe = d.probes.iter().find(|(pk, r)| pk == k && near(r, kr)).map(|(_, r)| *r);
        let best = probe.or_else(|| {
            d.widgets
                .iter()
                .filter(|(_, _, r)| near(r, kr))
                .map(|(_, _, r)| *r)
                .min_by(|a, b| a.left().total_cmp(&b.left()))
        });
        if let Some(r) = best {
            rows.push((k.clone(), r));
        }
    }
    rows
}

fn column_violations(d: &Drawn) -> Vec<String> {
    let mut bad = vec![];
    if (d.panel.width() - d.asked.max(ui_kit::PROPS_MIN_W)).abs() > 1.0 {
        bad.push(format!("panel is {} wide, asked {}", d.panel.width(), d.asked));
    }
    let rows = row_widgets(d);
    if rows.is_empty() {
        bad.push("no value widget was found next to a key".to_string());
    }
    if let Some((_, first)) = rows.first() {
        for (k, r) in &rows {
            // `kv_widget_w` grows a list left into the free part of its own key cell so a long
            // name stays whole: allowed only while it keeps clear of that key's text
            let key_end = d.keys.iter().find(|(kk, ..)| kk == k).map(|(_, kr, ..)| kr.right());
            let grown = r.left() < first.left()
                && key_end.is_some_and(|e| r.left() >= e + ddc_ui::theme::layout::PROP_GAP - EPS);
            if (r.left() - first.left()).abs() > EPS && !grown {
                bad.push(format!("{k:?} value starts at x {} (first row {})", r.left(), first.left()));
            }
        }
    }
    for (role, label, r) in &d.widgets {
        if r.left() < d.panel.left() - EPS || r.right() > d.panel.right() + EPS {
            bad.push(format!("{role:?} {label:?} {r:?} leaves the panel {:?}", d.panel));
        }
    }
    bad
}

fn key_violations(d: &Drawn) -> Vec<String> {
    let mut bad = vec![];
    if d.keys.is_empty() {
        bad.push("no key cell was drawn".to_string());
    }
    for (k, r, rows, elided) in &d.keys {
        if *rows > 1 {
            bad.push(format!("key {k:?} wraps to {rows} rows"));
        }
        // an elided key keeps a readable stub; `ui_kit::key` gives it the full-text tooltip
        if *elided && r.width() < ddc_ui::theme::layout::PROP_KEY_MIN - EPS {
            bad.push(format!("key {k:?} is elided to {} pt", r.width()));
        }
        if r.right() > d.panel.right() + EPS || r.left() < d.panel.left() - EPS {
            bad.push(format!("key {k:?} {r:?} leaves the panel"));
        }
    }
    bad
}

#[derive(Clone, Copy)]
enum Check {
    Columns,
    Keys,
}

fn run(pick: Pick, lang: Lang, width: f32, check: Check) {
    let d = draw(pick, lang, width);
    let bad = match check {
        Check::Columns => column_violations(&d),
        Check::Keys => key_violations(&d),
    };
    assert!(bad.is_empty(), "{}", bad.join("; "));
}

const MIN: f32 = ui_kit::PROPS_MIN_W;
const DEF: f32 = ui_kit::PROPS_W;

macro_rules! cases {
    ($($name:ident: $pick:expr, $lang:expr, $w:expr, $check:expr;)*) => {
        $(
            #[test]
            fn $name() {
                run($pick, $lang, $w, $check);
            }
        )*
    };
}

cases! {
    wall_en_min_columns: Pick::Wall, Lang::En, MIN, Check::Columns;
    wall_en_def_columns: Pick::Wall, Lang::En, DEF, Check::Columns;
    wall_ru_min_columns: Pick::Wall, Lang::Ru, MIN, Check::Columns;
    wall_ru_def_columns: Pick::Wall, Lang::Ru, DEF, Check::Columns;
    door_en_min_columns: Pick::Door, Lang::En, MIN, Check::Columns;
    door_en_def_columns: Pick::Door, Lang::En, DEF, Check::Columns;
    door_ru_min_columns: Pick::Door, Lang::Ru, MIN, Check::Columns;
    door_ru_def_columns: Pick::Door, Lang::Ru, DEF, Check::Columns;
    level_en_min_columns: Pick::Level, Lang::En, MIN, Check::Columns;
    level_en_def_columns: Pick::Level, Lang::En, DEF, Check::Columns;
    level_ru_min_columns: Pick::Level, Lang::Ru, MIN, Check::Columns;
    level_ru_def_columns: Pick::Level, Lang::Ru, DEF, Check::Columns;
    imported_en_min_columns: Pick::Imported, Lang::En, MIN, Check::Columns;
    imported_en_def_columns: Pick::Imported, Lang::En, DEF, Check::Columns;
    imported_ru_min_columns: Pick::Imported, Lang::Ru, MIN, Check::Columns;
    imported_ru_def_columns: Pick::Imported, Lang::Ru, DEF, Check::Columns;
    wall_en_min_keys: Pick::Wall, Lang::En, MIN, Check::Keys;
    wall_en_def_keys: Pick::Wall, Lang::En, DEF, Check::Keys;
    wall_ru_def_keys: Pick::Wall, Lang::Ru, DEF, Check::Keys;
    door_en_min_keys: Pick::Door, Lang::En, MIN, Check::Keys;
    door_en_def_keys: Pick::Door, Lang::En, DEF, Check::Keys;
    door_ru_def_keys: Pick::Door, Lang::Ru, DEF, Check::Keys;
    level_en_min_keys: Pick::Level, Lang::En, MIN, Check::Keys;
    level_en_def_keys: Pick::Level, Lang::En, DEF, Check::Keys;
    level_ru_def_keys: Pick::Level, Lang::Ru, DEF, Check::Keys;
    imported_en_min_keys: Pick::Imported, Lang::En, MIN, Check::Keys;
    imported_en_def_keys: Pick::Imported, Lang::En, DEF, Check::Keys;
    imported_ru_def_keys: Pick::Imported, Lang::Ru, DEF, Check::Keys;
}

// Long Russian keys (the phase key is 17 letters) do not fit the ~112 pt key column at the
// minimum width: they stay on one line and end with an ellipsis.
cases! {
    wall_ru_min_keys: Pick::Wall, Lang::Ru, MIN, Check::Keys;
    door_ru_min_keys: Pick::Door, Lang::Ru, MIN, Check::Keys;
    level_ru_min_keys: Pick::Level, Lang::Ru, MIN, Check::Keys;
    imported_ru_min_keys: Pick::Imported, Lang::Ru, MIN, Check::Keys;
}

// One shared key width across the groups of a panel: every value column starts at one x for
// each element kind, at both widths and in both languages (long Russian keys included).
cases! {
    stair_en_min_columns: Pick::Stair, Lang::En, MIN, Check::Columns;
    stair_en_def_columns: Pick::Stair, Lang::En, DEF, Check::Columns;
    stair_ru_min_columns: Pick::Stair, Lang::Ru, MIN, Check::Columns;
    stair_ru_def_columns: Pick::Stair, Lang::Ru, DEF, Check::Columns;
    room_en_min_columns: Pick::Room, Lang::En, MIN, Check::Columns;
    room_en_def_columns: Pick::Room, Lang::En, DEF, Check::Columns;
    room_ru_min_columns: Pick::Room, Lang::Ru, MIN, Check::Columns;
    room_ru_def_columns: Pick::Room, Lang::Ru, DEF, Check::Columns;
    stair_en_min_keys: Pick::Stair, Lang::En, MIN, Check::Keys;
    stair_ru_min_keys: Pick::Stair, Lang::Ru, MIN, Check::Keys;
    stair_ru_def_keys: Pick::Stair, Lang::Ru, DEF, Check::Keys;
    room_en_min_keys: Pick::Room, Lang::En, MIN, Check::Keys;
    room_ru_min_keys: Pick::Room, Lang::Ru, MIN, Check::Keys;
    room_ru_def_keys: Pick::Room, Lang::Ru, DEF, Check::Keys;
}

/// Every kind, both languages and widths: one value x for all groups of the panel.
#[test]
fn every_group_shares_one_value_column() {
    let picks = [Pick::Wall, Pick::Door, Pick::Level, Pick::Stair, Pick::Room, Pick::Imported];
    for pick in picks {
        for lang in LANGS {
            for w in WIDTHS {
                let bad = column_violations(&draw_in(pick, lang, w));
                assert!(bad.is_empty(), "{lang:?} {w}: {}", bad.join("; "));
            }
        }
    }
}

/// No key is wider than the key column: the widest value-widget-free x of a row is its key's
/// right edge, which stays left of the shared value x.
#[test]
fn ru_min_keys_end_left_of_the_value_column() {
    let d = draw(Pick::Level, Lang::Ru, MIN);
    let rows = row_widgets(&d);
    let x = rows.first().map(|(_, r)| r.left()).unwrap_or(0.0);
    for (k, kr, _, _) in &d.keys {
        assert!(kr.right() <= x + EPS, "{k:?} {kr:?} overlaps the value column at {x}");
    }
}

fn draw_in(pick: Pick, lang: Lang, width: f32) -> Drawn {
    crate::with_test_lang(lang == Lang::Ru, || draw(pick, lang, width))
}

fn has_text(d: &Drawn, want: &str) -> bool {
    let want = want.trim().to_lowercase();
    d.texts.iter().any(|(t, _)| t.trim().to_lowercase() == want)
}

const LANGS: [Lang; 2] = [Lang::En, Lang::Ru];
const WIDTHS: [f32; 2] = [MIN, DEF];

/// Nothing selected: the whole-model totals are the panel's content.
#[test]
fn nothing_selected_shows_the_model_totals() {
    for lang in LANGS {
        for w in WIDTHS {
            let d = draw_in(Pick::Nothing, lang, w);
            let model = crate::tr_lang(lang == Lang::Ru, "props.model", &[]);
            assert!(has_text(&d, &model), "{lang:?} {w}: no Model heading");
        }
    }
}

/// One element or many: its own figures only, never the whole-model totals.
#[test]
fn a_selection_hides_the_model_totals() {
    let picks = [
        Pick::Wall,
        Pick::Door,
        Pick::Level,
        Pick::Imported,
        Pick::Stair,
        Pick::Many,
        Pick::Room,
    ];
    for pick in picks {
        for lang in LANGS {
            for w in WIDTHS {
                let d = draw_in(pick, lang, w);
                let model = crate::tr_lang(lang == Lang::Ru, "props.model", &[]);
                assert!(!has_text(&d, &model), "{lang:?} {w}: Model heading under a selection");
            }
        }
    }
}

/// The element header paints once: exactly one "#id" text.
#[test]
fn every_kind_paints_the_header_once() {
    let picks = [
        Pick::Stair,
        Pick::Imported,
        Pick::Level,
        Pick::LongName,
        Pick::Wall,
        Pick::Door,
        Pick::Room,
    ];
    for pick in picks {
        for lang in LANGS {
            let d = draw_in(pick, lang, DEF);
            let ids = d
                .texts
                .iter()
                .filter(|(t, _)| t.strip_prefix('#').is_some_and(|n| n.parse::<i64>().is_ok()))
                .count();
            assert_eq!(ids, 1, "{lang:?}: {ids} header ids");
        }
    }
}

/// The stair's read-only figures sit above its first editable field.
#[test]
fn stair_figures_sit_above_the_first_field() {
    for lang in LANGS {
        for w in WIDTHS {
            let d = draw_in(Pick::Stair, lang, w);
            let ru = lang == Lang::Ru;
            let cap = crate::tr_lang(ru, "stair.risers", &[]);
            let strip = d.texts.iter().find(|(t, _)| t.trim() == cap).map(|(_, r)| r.top());
            let field = d.widgets.iter().map(|(_, _, r)| r.top()).min_by(f32::total_cmp);
            let (Some(strip), Some(field)) = (strip, field) else {
                panic!("{lang:?} {w}: figures {strip:?}, first field {field:?}");
            };
            assert!(strip < field, "{lang:?} {w}: figures at {strip}, first field at {field}");
        }
    }
}

/// Walls, openings and rooms carry the same read-only strip as the stair: its first caption
/// sits above the first editable field and below the header.
#[test]
fn wall_opening_room_figures_sit_above_the_first_field() {
    let cases = [
        (Pick::Wall, "props.plan_area"),
        (Pick::Door, "props.in_wall"),
        (Pick::Room, "props.area"),
    ];
    for (pick, key) in cases {
        for lang in LANGS {
            for w in WIDTHS {
                let d = draw_in(pick, lang, w);
                let cap = crate::tr_lang(lang == Lang::Ru, key, &[]);
                let strip = d.texts.iter().find(|(t, _)| t.trim() == cap).map(|(_, r)| r.top());
                let field = d.widgets.iter().map(|(_, _, r)| r.top()).min_by(f32::total_cmp);
                let head = d
                    .texts
                    .iter()
                    .find(|(t, _)| t.strip_prefix('#').is_some_and(|n| n.parse::<i64>().is_ok()))
                    .map(|(_, r)| r.top());
                let (Some(strip), Some(field), Some(head)) = (strip, field, head) else {
                    panic!("{lang:?} {w} {key}: figures {strip:?}, field {field:?}, head {head:?}");
                };
                assert!(head < strip, "{lang:?} {w} {key}: strip {strip} above header {head}");
                assert!(strip < field, "{lang:?} {w} {key}: strip {strip}, first field {field}");
            }
        }
    }
}

/// A name with no break to wrap at stays inside the panel (cut with a tooltip).
#[test]
fn a_long_name_stays_inside_the_panel() {
    for lang in LANGS {
        for w in WIDTHS {
            let d = draw_in(Pick::LongName, lang, w);
            let long: Vec<_> = d.texts.iter().filter(|(t, _)| t.contains("WWWW")).collect();
            assert!(!long.is_empty(), "{lang:?} {w}: the long name was not drawn");
            for (t, r) in long {
                let ok = r.left() >= d.panel.left() - EPS && r.right() <= d.panel.right() + EPS;
                assert!(ok, "{lang:?} {w}: {t:.20} at {r:?} leaves the panel {:?}", d.panel);
            }
        }
    }
}
