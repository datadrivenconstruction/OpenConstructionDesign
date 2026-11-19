// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Command arguments from the palette: Russian-language queries such as "wall 300 concrete", "door 900x2100", "column 3000 @Level 2"
//! (wall 300 concrete, door 900x2100, column 3000 @Level 2).
//! Numbers and words apply to the tool that was just launched; `@level` switches the level first.
//! A bare number is in project units (mm / inches), like any length field; `preview`
//! shows the parse in the palette before launch.
use crate::editor::{Editor, OpFn};
use crate::shell::EditorApp;
use crate::tools::Kind;
use crate::tr_str;
use crate::units::{display_length_lang, fmt_num_lang, ui_decimal_mark, DecimalMark, Units};
use ddc_ui::i18n::Lang;
use crate::wall_tool::Mode;
use ddc_ui::cmdargs::{Args, Unit};
use ddc_ui::frecency;
use ddc_ui::notify::Level;
use std::sync::Arc;

/// Material classes (order of `ui_kit::MAT_CLASSES`) and the words that identify them.
const MATS: [(&str, &[&str]); 6] = [
    ("concrete", &["\u{431}\u{435}\u{442}\u{43e}\u{43d}", "\u{436}\u{431}", "\u{436}\u{435}\u{43b}\u{435}\u{437}\u{43e}\u{431}\u{435}\u{442}", "concrete"]),
    (
        "masonry",
        &[
            "\u{43a}\u{438}\u{440}\u{43f}\u{438}\u{447}",
            "\u{43a}\u{43b}\u{430}\u{434}\u{43a}",
            "\u{431}\u{43b}\u{43e}\u{43a}",
            "\u{433}\u{430}\u{437}\u{43e}\u{431}\u{435}\u{442}",
            "\u{43f}\u{435}\u{43d}\u{43e}\u{431}\u{435}\u{442}",
            "brick",
            "masonry",
            "block",
        ],
    ),
    ("insulation", &["\u{443}\u{442}\u{435}\u{43f}\u{43b}", "\u{43c}\u{438}\u{43d}\u{432}\u{430}\u{442}", "insul", "wool"]),
    ("glass", &["\u{441}\u{442}\u{435}\u{43a}\u{43b}", "glass"]),
    ("wood", &["\u{434}\u{435}\u{440}\u{435}\u{432}", "\u{431}\u{440}\u{443}\u{441}", "wood", "timber"]),
    ("other", &["\u{433}\u{43a}\u{43b}", "\u{433}\u{438}\u{43f}\u{441}", "gypsum", "drywall"]),
];

/// Check order: masonry before concrete (an aerated concrete block is masonry).
const PRIORITY: [usize; 6] = [1, 0, 2, 3, 4, 5];

/// Palette value limits, m: wall thickness, other lengths.
pub const THICK: (f64, f64) = (0.01, 1.5);
pub const LEN_MAX: f64 = 100.0;

fn word_is(class: usize, w: &str) -> bool {
    MATS[class]
        .1
        .iter()
        .any(|s| w.starts_with(s) || (s.chars().count() > 4 && w.contains(s)))
}

/// Material class from a query word or a material name (index into [`MATS`]); one
/// classifier for both the query and type layers, otherwise the type is not found and gets duplicated.
pub fn material_of(text: &str) -> Option<usize> {
    let t = text.to_lowercase();
    let words: Vec<&str> = t.split_whitespace().collect();
    PRIORITY
        .iter()
        .copied()
        .find(|&c| words.iter().any(|w| word_is(c, w)))
}

/// Wall type for a query: existing (index into `ed.types`) or new (class, thickness).
#[derive(Clone, Debug, PartialEq)]
pub enum WallChoice {
    Keep,
    Existing(usize),
    Create { class: usize, thickness: f64 },
}

/// Main (thickest) material of each wall type -> class.
fn type_classes(ed: &Editor) -> Vec<(i64, usize)> {
    let db = ed.model.sql();
    let Ok(mut st) =
        db.prepare("SELECT wtype, material, thickness FROM wall_type_layer ORDER BY wtype, pos")
    else {
        return vec![];
    };
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f64>(2)?,
            ))
        })
        .map(|it| it.filter_map(Result::ok).collect::<Vec<_>>())
        .unwrap_or_default();
    let mut best: Vec<(i64, String, f64)> = vec![];
    for (t, m, th) in rows {
        match best.iter_mut().find(|b| b.0 == t) {
            Some(b) if th > b.2 => *b = (t, m, th),
            Some(_) => {}
            None => best.push((t, m, th)),
        }
    }
    best.into_iter()
        .map(|(t, m, _)| (t, material_of(&m).unwrap_or(5)))
        .collect()
}

pub fn wall_choice(ed: &Editor, thickness: Option<f64>, words: &[String]) -> WallChoice {
    let class = words.iter().find_map(|w| material_of(w));
    let names: Vec<String> = words
        .iter()
        .filter(|w| material_of(w).is_none() && w.chars().count() >= 3)
        .map(|w| w.to_lowercase())
        .collect();
    if thickness.is_none() && class.is_none() && names.is_empty() {
        return WallChoice::Keep;
    }
    let classes = type_classes(ed);
    let class_of = |id: i64| classes.iter().find(|c| c.0 == id).map(|c| c.1);
    let by_name: Vec<usize> = (0..ed.types.len())
        .filter(|&i| {
            let n = ed.types[i].name.to_lowercase();
            names.iter().any(|w| n.contains(w.as_str()))
        })
        .collect();
    let pool: Vec<usize> = if by_name.is_empty() {
        (0..ed.types.len()).collect()
    } else {
        by_name
    };
    let fits = |i: usize| {
        let t = &ed.types[i];
        class.is_none_or(|c| class_of(t.id) == Some(c))
            && thickness.is_none_or(|th| (t.thickness() - th).abs() < 5e-4)
    };
    // keep the current type if it fits, no switching
    if pool.contains(&ed.current_type) && fits(ed.current_type) {
        return WallChoice::Existing(ed.current_type);
    }
    if let Some(i) = pool.iter().copied().find(|&i| fits(i)) {
        return WallChoice::Existing(i);
    }
    if thickness.is_none() && class.is_none() {
        return WallChoice::Keep;
    }
    let cur = ed.types.get(ed.current_type);
    let class = class
        .or_else(|| cur.and_then(|t| class_of(t.id)))
        .unwrap_or(0);
    let thickness = thickness
        .or_else(|| cur.map(|t| t.thickness()))
        .unwrap_or(0.2);
    WallChoice::Create { class, thickness }
}

/// New type name `<material> <thickness, mm>` and its material; a name already taken (another
/// thickness rounding to the same mm, a renamed type) gets the first free ` (2)`, ` (3)`...
fn new_type_name(app: &EditorApp, class: usize, thickness: f64) -> (String, String) {
    let mat = app.sh.cat.t(&format!("quick.mat.{}", MATS[class].0));
    let taken: Vec<&str> = app.ed.types.iter().map(|t| t.name.as_str()).collect();
    (free_type_name(&type_name(&mat, thickness), &taken), mat)
}

/// `base`, else `base (2)`, `base (3)`...: the first name not in `taken`.
fn free_type_name(base: &str, taken: &[&str]) -> String {
    let mut cand = base.to_string();
    let mut n = 2;
    while taken.contains(&cand.as_str()) {
        cand = format!("{base} ({n})");
        n += 1;
    }
    cand
}

/// Stored name: the thickness as a plain whole number of millimetres (`Concrete 1000`), never
/// display text with group marks; names already in a file are kept as they are.
fn type_name(mat: &str, thickness: f64) -> String {
    format!("{mat} {}", crate::units::fmt_fixed(thickness * 1000.0, 0))
}

/// Level from `@...`: (id, name), or the name that was not found.
fn take_level(
    ed: &Editor,
    a: &mut Args,
    mark: DecimalMark,
) -> Option<Result<(i64, String), String>> {
    let lv = ed.levels();
    let names: Vec<&str> = lv.iter().map(|l| l.1.as_str()).collect();
    a.take_level_in(&names, mark)
        .map(|r| r.map(|i| (lv[i].0, lv[i].1.clone())))
}

fn is_wall_cmd(id: &str) -> bool {
    matches!(id, "arch.wall" | "str.wall" | "arch.wall_room")
}

/// Wall thickness and height from the arguments; out-of-range values go to `bad` (input text).
fn wall_dims(a: &Args, metric: bool, bad: &mut Vec<usize>) -> (Option<f64>, Option<f64>) {
    let th = a.length(0, metric);
    let th = match th {
        Some(v) if !(THICK.0..=THICK.1).contains(&v) => {
            bad.push(0);
            None
        }
        v => v,
    };
    let h = a.length(1, metric);
    let h = match h {
        Some(v) if v > LEN_MAX => {
            bad.push(1);
            None
        }
        v => v,
    };
    (th, h)
}

/// Opening width, height, sill (the sill may be 0).
fn opening_dims(a: &Args, metric: bool, bad: &mut Vec<usize>) -> [Option<f64>; 3] {
    let mut out = [None; 3];
    for (i, o) in out.iter_mut().enumerate() {
        let v = if i == 2 {
            a.length_or_zero(i, metric)
        } else {
            a.length(i, metric)
        };
        match v {
            Some(v) if v > LEN_MAX / 5.0 => bad.push(i),
            v => *o = v,
        }
    }
    out
}

/// Number `i` as typed, in the number style and unit names of the language (`ru`); a bare
/// number carries the project unit (`metric`: mm, else inches).
fn num_text(a: &Args, i: usize, ru: bool, metric: bool) -> String {
    let Some((v, u)) = a.nums.get(i) else {
        return String::new();
    };
    let d = format!("{v}").split_once('.').map_or(0, |(_, f)| f.len().min(6));
    let n = fmt_num_lang(*v, d, ru);
    let name = |key: &'static str| format!("{n}\u{a0}{}", tr_str(ru, key));
    match u {
        None if metric => name("unit_fmt.unit.mm"),
        None | Some(Unit::In) => format!("{n}\""),
        Some(Unit::Mm) => name("unit_fmt.unit.mm"),
        Some(Unit::Cm) => name("topo_units.unit.cm"),
        Some(Unit::M) => name("unit.m"),
        Some(Unit::Ft) => format!("{n}'"),
        Some(Unit::Deg) => format!("{n}\u{b0}"),
        Some(Unit::Pct) => format!("{n}\u{a0}%"),
    }
}

/// Refused values (`bad` holds argument indices) as text, in the catalog language.
fn bad_texts(a: &Args, bad: &[usize], ru: bool, metric: bool) -> Vec<String> {
    bad.iter().map(|&i| num_text(a, i, ru, metric)).collect()
}

/// Parse of the query for command `id` before launch, e.g. "Thickness 300 mm · Height 3 000 mm · level
/// 'Level 2' · new type 'Concrete 300'" (thickness, height, level, new type), in the UI language. Empty means no arguments.
pub fn preview(app: &EditorApp, id: &str, q: &str) -> String {
    preview_in(app, id, q, ui_decimal_mark())
}

/// [`preview`] with numbers read in the decimal mark `mark`.
pub fn preview_in(app: &EditorApp, id: &str, q: &str, mark: DecimalMark) -> String {
    let mut a = frecency::args_for_mark(&app.sh.reg, &app.sh.cat, q, id, mark);
    if a.is_empty() {
        return String::new();
    }
    let c = &app.sh.cat;
    let level = take_level(&app.ed, &mut a, mark);
    if !a.bad.is_empty() {
        return a
            .bad
            .iter()
            .map(|v| c.tf("quick.args.bad_number", &[("v", v.as_str())]))
            .collect::<Vec<_>>()
            .join(" · ");
    }
    let u = app.ed.tool.units;
    let metric = u == Units::Metric;
    let ru = c.lang == Lang::Ru;
    let mut parts: Vec<String> = vec![];
    match level {
        Some(Ok((_, name))) => parts.push(c.tf("quick.args.p_level", &[("name", &name)])),
        Some(Err(n)) => parts.push(c.tf("quick.args.no_level", &[("name", &n)])),
        None => {}
    }
    let mut bad = vec![];
    let labelled = |parts: &mut Vec<String>, key: &str, v: Option<f64>| {
        if let Some(v) = v {
            parts.push(format!("{} {}", c.t(key), display_length_lang(v, u, ru)));
        }
    };
    if is_wall_cmd(id) {
        let (th, h) = wall_dims(&a, metric, &mut bad);
        labelled(&mut parts, "quick.hud.thickness", th);
        labelled(&mut parts, "quick.hud.height", h);
        match wall_choice(&app.ed, th, &a.words) {
            WallChoice::Keep => {}
            WallChoice::Existing(i) => {
                let n = app
                    .ed
                    .types
                    .get(i)
                    .map_or(String::new(), |t| t.name.clone());
                parts.push(c.tf("quick.args.p_type", &[("name", &n)]));
            }
            WallChoice::Create { class, thickness } => {
                let (n, _) = new_type_name(app, class, thickness);
                parts.push(c.tf("quick.args.p_type_new", &[("name", &n)]));
            }
        }
    } else if matches!(id, "arch.door" | "arch.window") {
        let d = opening_dims(&a, metric, &mut bad);
        for (k, v) in ["quick.hud.width", "quick.hud.height", "quick.hud.sill"]
            .iter()
            .zip(d)
        {
            labelled(&mut parts, *k, v);
        }
    } else {
        for (i, (v, unit)) in a.nums.iter().enumerate() {
            parts.push(match unit {
                Some(Unit::Deg | Unit::Pct) => num_text(&a, i, ru, metric),
                _ if id == "mep.pipe" && i == 0 => format!("DN {}", fmt_num_lang(*v, 0, ru)),
                _ => a
                    .length_or_zero(i, metric)
                    .map_or_else(
                        || num_text(&a, i, ru, metric),
                        |m| display_length_lang(m, u, ru),
                    ),
            });
        }
        parts.extend(a.words.iter().map(|w| format!("«{w}»")));
    }
    for b in bad_texts(&a, &bad, ru, metric) {
        parts.push(c.tf("quick.args.bad_value", &[("v", &b)]));
    }
    parts.join(" · ")
}

/// Apply the arguments of query `q` to the just-launched command `id`. true means something
/// was applied.
pub fn apply(app: &mut EditorApp, id: &str, q: &str) -> bool {
    apply_in(app, id, q, ui_decimal_mark())
}

/// [`apply`] with numbers read in the decimal mark `mark`. A number the mark refuses (English
/// `1,5`) applies nothing: no level switch, no new type, no tool value; a warning names it.
pub fn apply_in(app: &mut EditorApp, id: &str, q: &str, mark: DecimalMark) -> bool {
    let mut a = frecency::args_for_mark(&app.sh.reg, &app.sh.cat, q, id, mark);
    if a.is_empty() {
        return false;
    }
    // resolving the level reads the text after its name: refused numbers there count too
    let level = take_level(&app.ed, &mut a, mark);
    if !a.bad.is_empty() {
        for b in a.bad.clone() {
            super::toast(app, Level::Warning, "quick.args.bad_number", &[("v", &b)]);
        }
        return false;
    }
    let mut done = false;
    match level {
        Some(Ok((l, _))) if l != app.ed.level => {
            app.ed.set_level(l);
            // a level change resets the tool: relaunch the command
            app.quick.repeating = true;
            app.run(id);
            done = true;
        }
        Some(Ok(_)) | None => {}
        Some(Err(n)) => super::toast(app, Level::Warning, "quick.args.no_level", &[("name", &n)]),
    }
    super::defaults::restore_now(app);
    let metric = app.ed.tool.units == Units::Metric;
    let mut bad = vec![];
    let ed = &mut app.ed;
    if let Some(t) = ed.model_tool_mut() {
        done |= ext(t, &a, metric);
    } else if ed.opening_mode {
        let d = opening_dims(&a, metric, &mut bad);
        let ty = &mut ed.otool.ty;
        for (f, v) in [&mut ty.width, &mut ty.height, &mut ty.sill]
            .into_iter()
            .zip(d)
        {
            if let Some(v) = v {
                *f = v;
                done = true;
            }
        }
    } else if matches!(ed.wall_mode(), Mode::Polyline | Mode::Rect) {
        done |= walls(app, &a, metric, &mut bad);
    }
    let ru = app.sh.cat.lang == Lang::Ru;
    for b in bad_texts(&a, &bad, ru, metric) {
        super::toast(app, Level::Warning, "quick.args.bad_value", &[("v", &b)]);
    }
    super::defaults::remember(app);
    done
}

/// Type created by the arguments for walls not yet drawn: undo depth after creation
/// and the previous current type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fresh {
    pub depth: usize,
    /// `Editor::pushes` after creation: trimming old steps can bring `depth` back by itself.
    pub pushes: u64,
    pub prev: usize,
}

/// Remove the type created by the arguments if no walls were ever drawn with it (the step stays in
/// neither undo nor redo).
pub fn drop_fresh(app: &mut EditorApp) {
    let Some(f) = app.quick.fresh_type.take() else {
        return;
    };
    if app.ed.pushes == f.pushes && app.ed.undo_len() == f.depth && app.ed.drop_last_step() {
        app.ed.current_type = f.prev.min(app.ed.types.len().saturating_sub(1));
    }
}

fn walls(app: &mut EditorApp, a: &Args, metric: bool, bad: &mut Vec<usize>) -> bool {
    drop_fresh(app);
    let mut done = false;
    let (th, h) = wall_dims(a, metric, bad);
    if let Some(h) = h {
        app.ed.height = h;
        done = true;
    }
    match wall_choice(&app.ed, th, &a.words) {
        WallChoice::Keep => {}
        WallChoice::Existing(i) => {
            app.ed.current_type = i;
            done = true;
        }
        WallChoice::Create { class, thickness } => {
            let (name, mat) = new_type_name(app, class, thickness);
            let n = name.clone();
            let op = OpFn(Arc::new(move |md: &mut ddc_model::Model| {
                md.add_wall_type(
                    &n,
                    &[ddc_model::LayerSpec {
                        material: mat.clone(),
                        thickness,
                    }],
                )
                .map(|_| ())
            }));
            let prev = app.ed.current_type;
            match app.ed.run_op(op) {
                Ok(()) => {
                    if let Some(i) = app.ed.types.iter().rposition(|t| t.name == name) {
                        app.ed.current_type = i;
                    }
                    app.quick.fresh_type = Some(Fresh {
                        depth: app.ed.undo_len(),
                        pushes: app.ed.pushes,
                        prev,
                    });
                    super::toast(
                        app,
                        Level::Success,
                        "quick.args.type_new",
                        &[("name", &name)],
                    );
                    done = true;
                }
                Err(e) => {
                    let e = e.to_string();
                    super::toast(app, Level::Warning, "quick.args.type_err", &[("err", &e)]);
                }
            }
        }
    }
    done
}

/// Numbers and words for a model tool: fields by kind, a word picks a type from the list.
pub fn ext(t: &mut crate::tools::Active, a: &Args, metric: bool) -> bool {
    let len = |i: usize| a.length_or_zero(i, metric).filter(|v| *v <= LEN_MAX);
    let o = &mut t.opts;
    let set = |f: &mut f64, v: Option<f64>| -> bool { v.map(|v| *f = v).is_some() };
    let mut done = false;
    match t.kind {
        Kind::Duct | Kind::Tray => {
            done |= set(&mut o.width, len(0));
            done |= set(&mut o.depth, len(1));
            done |= set(&mut o.z, len(2));
        }
        Kind::Pipe => {
            if let Some(dn) = a.value(0).filter(|v| *v >= 6.0 && *v <= 2000.0) {
                o.dn = dn.round() as u32;
                done = true;
            }
            done |= set(&mut o.z, len(1));
        }
        Kind::Conduit => {
            if let Some(d) = a.value(0).filter(|v| *v >= 6.0 && *v <= 200.0) {
                o.dn = d.round() as u32;
                done = true;
            }
            done |= set(&mut o.z, len(1));
        }
        Kind::Terminal
        | Kind::Fixture
        | Kind::Light
        | Kind::Device
        | Kind::Equipment
        | Kind::Route => {
            done |= set(&mut o.z, len(0));
        }
        Kind::Railing | Kind::Ceiling => done |= set(&mut o.height, len(0)),
        Kind::Stair => done |= set(&mut o.width, len(0)),
        Kind::Column | Kind::StructColumn | Kind::Shaft => done |= set(&mut o.top_off, len(0)),
        Kind::Beam => done |= set(&mut o.z, len(0)),
        Kind::Roof => {
            done |= set(
                &mut o.slope_deg,
                a.value(0).filter(|v| *v > 0.0 && *v < 89.0),
            );
            done |= set(&mut o.overhang, len(1));
        }
        _ => {}
    }
    let words: Vec<String> = a.words.iter().map(|w| w.to_lowercase()).collect();
    if let Some(i) = t.types.iter().position(|n| {
        words
            .iter()
            .any(|w| w.chars().count() >= 2 && n.to_lowercase().contains(w.as_str()))
    }) {
        t.opts.ty = i;
        done = true;
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &[&str]) -> Vec<String> {
        s.iter().map(|w| w.to_string()).collect()
    }

    fn app() -> EditorApp {
        EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into())
    }

    /// Russian word from the data file.
    fn ru(key: &str) -> String {
        let m: serde_json::Value = serde_json::from_str(include_str!("../../tests/data/args_ru.json")).unwrap();
        m[key].as_str().unwrap().to_string()
    }

    /// Fills `{key}` placeholders of a query template with Russian words.
    fn q(t: &str) -> String {
        let m: serde_json::Value = serde_json::from_str(include_str!("../../tests/data/args_ru.json")).unwrap();
        let mut s = t.to_string();
        for (k, v) in m.as_object().unwrap() {
            s = s.replace(&format!("{{{k}}}"), v.as_str().unwrap());
        }
        s
    }

    #[test]
    fn materials_and_wall_types() {
        assert_eq!(material_of(&ru("Concrete")), Some(0));
        assert_eq!(material_of(&ru("brick")), Some(1));
        assert_eq!(material_of("gypsum"), Some(5));
        assert_eq!(material_of(&ru("wall")), None);
        // one rule for the query and for layer materials
        assert_eq!(material_of(&ru("block")), Some(1));
        assert_eq!(material_of(&ru("aerated")), Some(1));
        assert_eq!(material_of(&ru("rc")), Some(0));
        assert_eq!(material_of(&ru("blo")), None, "3 Cyrillic letters are not a substring");
        let ed = Editor::new_in_memory().unwrap();
        assert_eq!(wall_choice(&ed, None, &[]), WallChoice::Keep);
        let cur = ed.types[ed.current_type].thickness();
        assert_eq!(
            wall_choice(&ed, Some(cur), &[]),
            WallChoice::Existing(ed.current_type)
        );
        // thickness not among the types: new type; material as the current one if not named
        match wall_choice(&ed, Some(0.437), &words(&[ru("concrete").as_str()])) {
            WallChoice::Create { class, thickness } => {
                assert_eq!(class, 0);
                assert!((thickness - 0.437).abs() < 1e-9);
            }
            c => panic!("{c:?}"),
        }
        // a full type name (no numbers) selects it
        let last = ed.types.len() - 1;
        let name = ed.types[last].name.to_lowercase();
        let w: Vec<String> = name
            .split_whitespace()
            .filter(|w| w.chars().count() >= 3 && material_of(w).is_none())
            .map(String::from)
            .collect();
        if !w.is_empty() && ed.types.len() > 1 {
            assert!(matches!(
                wall_choice(&ed, None, &w),
                WallChoice::Existing(_)
            ));
        }
    }

    #[test]
    fn wall_args_create_type_one_undo_step() {
        let mut a = app();
        let n0 = a.ed.types.len();
        a.run("arch.wall");
        let d0 = a.ed.undo_len();
        assert!(apply(&mut a, "arch.wall", &q("{wall} 437 3000 {concrete}")));
        assert_eq!(a.ed.types.len(), n0 + 1);
        assert_eq!(a.ed.undo_len(), d0 + 1);
        let t = &a.ed.types[a.ed.current_type];
        assert!((t.thickness() - 0.437).abs() < 1e-9, "{}", t.name);
        assert!((a.ed.height - 3.0).abs() < 1e-9);
        // the same query again: the type is not duplicated
        assert!(apply(&mut a, "arch.wall", &q("{wall} 437 {concrete}")));
        assert_eq!(a.ed.types.len(), n0 + 1);
        // door: width x height, sill 0 allowed
        a.run("arch.door");
        assert!(apply(&mut a, "arch.door", &q("{door} 1000x2200 0")));
        assert!((a.ed.otool.ty.width - 1.0).abs() < 1e-9);
        assert!((a.ed.otool.ty.height - 2.2).abs() < 1e-9);
        assert_eq!(a.ed.otool.ty.sill, 0.0);
        assert!(!apply(&mut a, "arch.door", &ru("door")));
    }

    /// What the palette arguments may change: types, current type, height, level, undo depth.
    fn snapshot(a: &EditorApp) -> (usize, usize, f64, f64, i64, usize, usize) {
        (
            a.ed.types.len(),
            a.ed.current_type,
            a.ed.height,
            a.ed.types[a.ed.current_type].thickness(),
            a.ed.level,
            a.ed.undo_len(),
            a.ed.redo_len(),
        )
    }

    #[test]
    fn ambiguous_numbers_are_refused_with_no_change() {
        const EN: DecimalMark = DecimalMark::Point;
        const RU: DecimalMark = DecimalMark::Comma;
        let mut a = app();
        a.run("arch.wall");
        let lv = a.ed.levels();
        let name = lv
            .iter()
            .find(|l| l.0 == a.ed.level)
            .map(|l| l.1.clone())
            .unwrap();
        let s0 = snapshot(&a);
        for q in [
            "wall 1,5 concrete",
            "wall 437 1,5",
            "wall 1,5 m",
            "wall 8'6,5\"",
            "wall 437 1,23,4",
        ] {
            a.sh.toasts.live.clear();
            assert!(!apply_in(&mut a, "arch.wall", q, EN), "{q}");
            assert_eq!(snapshot(&a), s0, "{q}");
            assert!(!a.sh.toasts.live.is_empty(), "{q}: a warning is shown");
            let t = &a.sh.toasts.live.last().unwrap();
            assert_eq!(t.level, Level::Warning);
            assert!(t.text.contains(','), "{q}: {}", t.text);
        }
        // a refused number after the level name: no level switch, nothing applied either
        let refused = format!("wall 437 @{name} 1,5");
        assert!(!apply_in(&mut a, "arch.wall", &refused, EN));
        assert_eq!(snapshot(&a), s0);
        // Russian: an invalid number is refused the same way
        assert!(!apply_in(&mut a, "arch.wall", &q("{wall} 437 1,23,456"), RU));
        assert_eq!(snapshot(&a), s0);
        // openings too
        a.run("arch.door");
        let s1 = snapshot(&a);
        let (op0, ot0) = (a.ed.otool.ty.width, a.ed.otool.ty.height);
        assert!(!apply_in(&mut a, "arch.door", "door 1,5x2100", EN));
        assert_eq!(snapshot(&a), s1);
        assert_eq!((a.ed.otool.ty.width, a.ed.otool.ty.height), (op0, ot0));
        // the preview names the refused text and shows no value
        let p = preview_in(&a, "arch.wall", "wall 1,5 concrete", EN);
        assert!(p.contains("1,5"), "{p}");
        assert!(!p.contains(&a.sh.cat.t("quick.hud.thickness")), "{p}");
    }

    #[test]
    fn stored_type_names_hold_plain_numbers() {
        assert_eq!(type_name("Concrete", 1.0), "Concrete 1000");
        assert_eq!(type_name("Concrete", 12.5), "Concrete 12500");
        assert_eq!(type_name("Brick", 0.4374), "Brick 437");
        assert_eq!(type_name("Brick", -1e-9), "Brick 0", "never -0");
        assert_eq!(type_name("Brick", -1.2), "Brick -1200");
        for (lang, mark, q) in [
            (Lang::En, DecimalMark::Point, "wall 1,000 concrete"),
            (Lang::Ru, DecimalMark::Comma, "wall 1000 concrete"),
        ] {
            let mut a = app();
            a.sh.cat = ddc_ui::i18n::Catalog::new(lang);
            a.run("arch.wall");
            let ru = lang == Lang::Ru;
            // the thread speaks the other language: the catalog decides the preview style
            let p = crate::with_test_lang(!ru, || preview_in(&a, "arch.wall", q, mark));
            let shown = if ru { "1\u{a0}000\u{a0}" } else { "1,000\u{a0}" };
            assert!(p.contains(shown), "{lang:?} {p}");
            let n0 = a.ed.types.len();
            assert!(apply_in(&mut a, "arch.wall", q, mark), "{lang:?}");
            assert_eq!(a.ed.types.len(), n0 + 1);
            let name = a.ed.types[a.ed.current_type].name.clone();
            assert!(name.ends_with(" 1000"), "{lang:?} {name:?}");
            assert!(!name.contains([',', '\u{a0}', '\u{202f}']), "{lang:?} {name:?}");
            assert!(p.contains(&name), "the preview names the stored type: {p}");
        }
    }

    #[test]
    fn stored_type_names_never_clash() {
        assert_eq!(free_type_name("Brick 438", &[]), "Brick 438");
        assert_eq!(free_type_name("Brick 438", &["Brick 438"]), "Brick 438 (2)");
        let taken = ["Brick 438", "Brick 438 (2)", "Brick 438 (4)"];
        assert_eq!(free_type_name("Brick 438", &taken), "Brick 438 (3)");
        assert_eq!(free_type_name("Brick 438 (2)", &["Brick 438 (2)"]), "Brick 438 (2) (2)");
        for (lang, mark) in [(Lang::En, DecimalMark::Point), (Lang::Ru, DecimalMark::Comma)] {
            let mut a = app();
            a.sh.cat = ddc_ui::i18n::Catalog::new(lang);
            a.run("arch.wall");
            // the generated name and its first suffix are taken by types of other thicknesses
            let base = new_type_name(&a, 0, 1.0).0;
            for (n, th) in [(base.clone(), 0.3), (format!("{base} (2)"), 0.35)] {
                let op = OpFn(Arc::new(move |md: &mut ddc_model::Model| {
                    let l = ddc_model::LayerSpec { material: "X".into(), thickness: th };
                    md.add_wall_type(&n, &[l]).map(|_| ())
                }));
                a.ed.run_op(op).unwrap();
            }
            let want = format!("{base} (3)");
            assert_eq!(new_type_name(&a, 0, 1.0).0, want, "{lang:?}");
            let p = preview_in(&a, "arch.wall", "wall 1000 concrete", mark);
            assert!(p.contains(&want), "{lang:?}: the preview names the free name: {p}");
            let n0 = a.ed.types.len();
            assert!(apply_in(&mut a, "arch.wall", "wall 1000 concrete", mark), "{lang:?}");
            assert_eq!(a.ed.types.len(), n0 + 1, "{lang:?}: the type is made");
            let t = &a.ed.types[a.ed.current_type];
            assert_eq!(t.name, want, "{lang:?}");
            assert!((t.thickness() - 1.0).abs() < 1e-9, "{lang:?}");
        }
    }

    #[test]
    fn numbers_read_in_the_given_mark_one_undo_step() {
        const EN: DecimalMark = DecimalMark::Point;
        const RU: DecimalMark = DecimalMark::Comma;
        // English «1,000» is 1000 mm, not 1 mm
        let mut a = app();
        a.run("arch.wall");
        let (n0, d0) = (a.ed.types.len(), a.ed.undo_len());
        assert!(apply_in(&mut a, "arch.wall", "wall 1,000 3,000 concrete", EN));
        assert_eq!((a.ed.types.len(), a.ed.undo_len()), (n0 + 1, d0 + 1));
        let t = &a.ed.types[a.ed.current_type];
        assert!((t.thickness() - 1.0).abs() < 1e-9, "{}", t.name);
        assert!((a.ed.height - 3.0).abs() < 1e-9);
        // 437.5 mm read with a point; the preview shows whole millimetres (437.5 -> 438)
        let p = preview_in(&a, "arch.wall", "wall 437.5", EN);
        assert!(p.contains("438") && !p.contains("4375"), "{p}");
        // Russian "0,437 m" and "3000"
        let mut a = app();
        a.run("arch.wall");
        let (n0, d0) = (a.ed.types.len(), a.ed.undo_len());
        assert!(apply_in(&mut a, "arch.wall", &q("{wall} 0,437 {m} 3000 {concrete}"), RU));
        assert_eq!((a.ed.types.len(), a.ed.undo_len()), (n0 + 1, d0 + 1));
        let t = &a.ed.types[a.ed.current_type];
        assert!((t.thickness() - 0.437).abs() < 1e-9, "{}", t.name);
        assert!((a.ed.height - 3.0).abs() < 1e-9);
        // the same text in English: «0,437» is refused, nothing more is written
        let s = snapshot(&a);
        assert!(!apply_in(&mut a, "arch.wall", "wall 0,437 m 3,000 concrete", EN));
        assert_eq!(snapshot(&a), s);
        // feet-inches in both marks (8'6" = 2.5908 m: a height within limits)
        for m in [EN, RU] {
            let mut a = app();
            a.run("arch.wall");
            assert!(apply_in(&mut a, "arch.wall", "wall 200mm 8'6\"", m), "{m:?}");
            assert!((a.ed.height - 102.0 * 0.0254).abs() < 1e-9, "{m:?}");
        }
    }

    #[test]
    fn unused_new_type_is_dropped_when_tool_ends() {
        let mut a = app();
        let n0 = a.ed.types.len();
        let cur0 = a.ed.current_type;
        a.run("arch.wall");
        let d0 = a.ed.undo_len();
        assert!(apply(&mut a, "arch.wall", &q("{wall} 437 {concrete}")));
        assert_eq!(a.ed.types.len(), n0 + 1);
        // Esc without walls: type removed, in neither undo nor redo
        a.ed.set_mode(Mode::Select);
        drop_fresh(&mut a);
        assert_eq!(a.ed.types.len(), n0);
        assert_eq!((a.ed.undo_len(), a.ed.redo_len()), (d0, 0));
        assert_eq!(a.ed.current_type, cur0);
        // a drawn wall keeps the type
        a.run("arch.wall");
        assert!(apply(&mut a, "arch.wall", &q("{wall} 437 {concrete}")));
        a.ed.set_mode(Mode::Rect);
        a.ed.handle(crate::wall_tool::Event::Press(ddc_geom::v2(0.0, 0.0)));
        a.ed.handle(crate::wall_tool::Event::Release(ddc_geom::v2(3.0, 3.0)));
        a.ed.set_mode(Mode::Select);
        drop_fresh(&mut a);
        assert_eq!(a.ed.types.len(), n0 + 1);
    }

    #[test]
    fn fresh_type_is_kept_when_an_underlay_step_landed_at_its_depth() {
        let mut a = app();
        a.run("arch.wall");
        let mut ds = crate::drawing::Drawings::new();
        ds.add(
            &ddc_io::drawing_model::fixture::sample_plan(0),
            "d",
            None,
            a.ed.level,
            None,
            crate::drawing::Origin::AsIs,
            ddc_geom::v2(0.0, 0.0),
        );
        let mut edit = |a: &mut EditorApp, x: f64| {
            ds.xf = (x, 0.0, 0.0);
            ds.apply_xf(true, false);
            ds.sync_undo(&mut a.ed);
        };
        edit(&mut a, 1.0);
        assert!(apply(&mut a, "arch.wall", &q("{wall} 437 {concrete}")));
        let types = a.ed.types.len();
        let depth = a.ed.undo_len();
        edit(&mut a, 2.0);
        a.ed.trim_underlay(0);
        assert_eq!(a.ed.undo_len(), depth, "one step in, the oldest out");
        drop_fresh(&mut a);
        assert_eq!((a.ed.undo_len(), a.ed.redo_len()), (depth, 0));
        assert_eq!(a.ed.types.len(), types);
    }

    #[test]
    fn units_bounds_and_levels() {
        let mut a = app();
        a.run("arch.wall");
        let n0 = a.ed.types.len();
        // "wall 2000" is 2 m, "wall 1,2" is 1.2 mm: outside the thickness limits, no type is created
        apply(&mut a, "arch.wall", &q("{wall} 2000"));
        apply(&mut a, "arch.wall", &q("{wall} 1,2"));
        assert_eq!(a.ed.types.len(), n0);
        // "300 cm" is 3 m: also out of range; "30 cm" is 300 mm
        apply(&mut a, "arch.wall", &q("{wall} 300 {cm}"));
        assert_eq!(a.ed.types.len(), n0);
        let p = preview(&a, "arch.wall", &q("{wall} 30 {cm} 2800"));
        assert!(p.contains("300") && p.contains("2\u{a0}800"), "{p}");
        let p = preview(&a, "arch.wall", &q("{wall} 12000"));
        assert!(p.contains("12"), "{p}");
        assert!(preview(&a, "arch.wall", &ru("wall")).is_empty());
        // level with a space in its name: the exact match, not "first starting with the prefix"
        let lv = a.ed.levels();
        let name = lv
            .iter()
            .find(|l| l.0 == a.ed.level)
            .map(|l| l.1.clone())
            .unwrap();
        let p = preview(&a, "arch.wall", &format!("{} 200 @{name} 3000", ru("wall")));
        assert!(p.contains(&name) && p.contains("3\u{a0}000"), "{p}");
        let p = preview(&a, "arch.wall", &q("{wall} 200 @{missing}"));
        assert!(p.contains(&ru("missing")), "{p}");
    }

    fn has_cyrillic(s: &str) -> bool {
        s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c))
    }

    /// Preview of `q` with the catalog (messages, unit names, number style) in `lang`; the
    /// global UI language is never touched.
    fn preview_lang(a: &mut EditorApp, lang: Lang, mark: DecimalMark, id: &str, q: &str) -> String {
        a.sh.cat = ddc_ui::i18n::Catalog::new(lang);
        preview_in(a, id, q, mark)
    }

    #[test]
    fn refused_length_uses_language_number_style_and_unit_names() {
        const PT: DecimalMark = DecimalMark::Point;
        const CM: DecimalMark = DecimalMark::Comma;
        let mut a = app();
        a.ed.tool.units = Units::Metric;
        // too-large wall height with an explicit unit, decimals and thousands
        let p = preview_lang(&mut a, Lang::En, PT, "arch.wall", "wall 200 1234.5 m");
        assert!(p.contains("1,234.5\u{a0}m"), "{p}");
        // quoted spans are document names (the seed type follows the language it was made in)
        let unquoted: String = p.split('\u{201c}').map(|s| s.split_once('\u{201d}').map_or(s, |x| x.1)).collect();
        assert!(!has_cyrillic(&unquoted), "{p}");
        let p = preview_lang(&mut a, Lang::Ru, CM, "arch.wall", "wall 200 1234,5 \u{43c}");
        assert!(p.contains("1\u{a0}234,5\u{a0}\u{43c}"), "{p}");
        // integer value: no decimal mark at all, in both languages
        let p = preview_lang(&mut a, Lang::En, PT, "arch.wall", "wall 200 300 m");
        assert!(p.contains("300\u{a0}m") && !p.contains("300."), "{p}");
        let p = preview_lang(&mut a, Lang::Ru, CM, "arch.wall", "wall 200 300 \u{43c}");
        assert!(p.contains("300\u{a0}\u{43c}") && !p.contains("300,"), "{p}");
        // centimetres and millimetres
        let p = preview_lang(&mut a, Lang::En, PT, "arch.wall", "wall 200 20000 cm");
        assert!(p.contains("20,000\u{a0}cm"), "{p}");
        let p = preview_lang(&mut a, Lang::Ru, CM, "arch.wall", "wall 200 20000 \u{441}\u{43c}");
        assert!(p.contains("20\u{a0}000\u{a0}\u{441}\u{43c}"), "{p}");
        let p = preview_lang(&mut a, Lang::En, PT, "arch.door", "door 30000");
        assert!(p.contains("30,000\u{a0}mm"), "{p}");
        let p = preview_lang(&mut a, Lang::Ru, CM, "arch.door", "door 30000");
        assert!(p.contains("30\u{a0}000\u{a0}\u{43c}\u{43c}"), "{p}");
        // too-thin / too-thick wall: the same text rules
        let p = preview_lang(&mut a, Lang::En, PT, "arch.wall", "wall 2000.5 mm");
        assert!(p.contains("2,000.5\u{a0}mm"), "{p}");
        let p = preview_lang(&mut a, Lang::Ru, CM, "arch.wall", "wall 2000,5 \u{43c}\u{43c}");
        assert!(p.contains("2\u{a0}000,5\u{a0}\u{43c}\u{43c}"), "{p}");
    }

    #[test]
    fn refused_length_imperial_keeps_inch_and_foot_marks() {
        const PT: DecimalMark = DecimalMark::Point;
        const CM: DecimalMark = DecimalMark::Comma;
        let mut a = app();
        a.ed.tool.units = Units::Imperial;
        // a bare number is inches in an imperial project
        let p = preview_lang(&mut a, Lang::En, PT, "arch.door", "door 900");
        assert!(p.contains("900\"") && !has_cyrillic(&p.replace("\u{201c}", "")), "{p}");
        let p = preview_lang(&mut a, Lang::Ru, CM, "arch.door", "door 900,5");
        assert!(p.contains("900,5\""), "{p}");
        // feet with a thousands group
        let p = preview_lang(&mut a, Lang::En, PT, "arch.wall", "wall 8 1200 ft");
        assert!(p.contains("1,200'"), "{p}");
        let p = preview_lang(&mut a, Lang::Ru, CM, "arch.wall", "wall 8 1200 ft");
        assert!(p.contains("1\u{a0}200'"), "{p}");
    }

    #[test]
    fn num_text_cases_without_the_preview() {
        let mut a = ddc_ui::cmdargs::Args::default();
        a.nums = vec![
            (1234.5, Some(Unit::M)),
            (7.0, Some(Unit::Deg)),
            (12.25, Some(Unit::Pct)),
            (5.0, None),
        ];
        assert_eq!(num_text(&a, 0, false, true), "1,234.5\u{a0}m");
        assert_eq!(num_text(&a, 0, true, true), "1\u{a0}234,5\u{a0}\u{43c}");
        assert_eq!(num_text(&a, 1, false, true), "7\u{b0}");
        assert_eq!(num_text(&a, 2, true, true), "12,25\u{a0}%");
        assert_eq!(num_text(&a, 2, false, true), "12.25\u{a0}%");
        assert_eq!(num_text(&a, 3, false, true), "5\u{a0}mm");
        assert_eq!(num_text(&a, 3, false, false), "5\"");
        assert_eq!(num_text(&a, 9, false, true), "");
    }
}
