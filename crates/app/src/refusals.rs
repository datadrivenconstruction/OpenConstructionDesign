//! Explainable refusals: tool errors become `ddc_model::refusal::Refusal`
//! with a stable code, an i18n message and fixes that name a command; one click on a fix runs
//! that command with the fix arguments. The type is shared with the AI tool catalogue; this
//! module only builds refusals for the interactive tools and runs their fixes.
use crate::editor::Editor;
use crate::opening_tool::PlaceError;
use crate::shell::EditorApp;
use crate::wall_tool::Event;
use ddc_model::refusal::{Fix, Refusal, RefusalCode};
use ddc_ui::Catalog;
use std::cell::RefCell;

/// Fix commands this module registers; every `Fix.command` built here is one of them.
pub const FIX_COMMANDS: &[&str] = &["arch.door.set_width", "wall.extend"];

/// Step of the numbers a fix proposes, mm.
const STEP_MM: f64 = 50.0;
/// Clear gap each side of an opening that a wall extension leaves, mm.
const JAMB_MM: f64 = 100.0;

fn mm(m: f64) -> f64 {
    (m * 1000.0).round()
}

fn fix(label_key: &'static str, command: &'static str, args: &[(&str, String)]) -> Fix {
    Fix { label_key, command, args: args.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect() }
}

/// A door or window that cannot be placed. `wall` is the host under the cursor (for the fix that
/// extends it); the texts of the old kinds stay as they were, a too wide opening has its own.
pub fn place_refusal(e: &PlaceError, wall: Option<i64>) -> Refusal {
    let (ru, u) = (crate::ui_is_ru(), crate::units::active_units());
    let l = |m: f64| crate::unit_text::len(m, ru, u);
    match e {
        PlaceError::NoWall => Refusal::new(RefusalCode::OpeningNoWall, "sketch_edit.pe_nowall"),
        PlaceError::NonPositive => Refusal::new(RefusalCode::InvalidInput, "sketch_edit.pe_nonpos"),
        PlaceError::Outside { len, from, to } if to - from > len + 1e-9 => {
            let w = to - from;
            let fit = ((mm(*len) * 0.85 / STEP_MM).floor() * STEP_MM).max(100.0);
            let mut r = Refusal::new(RefusalCode::OpeningWiderThanWall, "ix.refuse.opening_wider_than_wall")
                .arg("w", l(w))
                .arg("len", l(*len))
                .fix(fix("ix.fix.opening_width", "arch.door.set_width", &[("w", format!("{fit}"))]));
            if let Some(id) = wall {
                let want = ((mm(w) + 2.0 * JAMB_MM) / STEP_MM).ceil() * STEP_MM;
                r = r.fix(fix("ix.fix.wall_extend", "wall.extend", &[("wall", id.to_string()), ("len", format!("{want}"))]));
            }
            r
        }
        PlaceError::Outside { len, from, to } => {
            Refusal::new(RefusalCode::OpeningOutside, "sketch_edit.pe_outside")
                .arg("wall", format!("{}…{}", l(0.0), l(*len)))
                .arg("need", format!("{}…{}", l(*from), l(*to)))
        }
        PlaceError::TooTall { top, height } => Refusal::new(RefusalCode::OpeningTooTall, "sketch_edit.pe_tall")
            .arg("top", l(*top))
            .arg("h", l(*height)),
        PlaceError::Overlap { other } => {
            Refusal::new(RefusalCode::OpeningOverlap, "sketch_edit.pe_overlap").arg("other", other.to_string())
        }
        PlaceError::Hosts { opening, err } => {
            let mut r = place_refusal(err, None);
            r.msg_key = "xlval.pe_hosts";
            r.args = vec![("id", opening.to_string()), ("e", err.to_string())];
            r
        }
    }
}

impl From<PlaceError> for Refusal {
    fn from(e: PlaceError) -> Self {
        place_refusal(&e, None)
    }
}

/// «Wall too short»: the next point is on the last one.
pub fn wall_too_short() -> Refusal {
    Refusal::new(RefusalCode::WallTooShort, "err.wall_short")
}

/// Nothing above to build to (stairs, ramps to the top level).
pub fn level_none_above() -> Refusal {
    Refusal::new(RefusalCode::LevelNoneAbove, "tool.err.no_level_above")
}

/// A component tool without a family library.
pub fn component_no_library() -> Refusal {
    Refusal::new(RefusalCode::ComponentNoLibrary, "tool.err.no_library")
}

/// A transform the selection cannot take: `kinds` is the selection in words, `why` an i18n key.
pub fn xform_not_allowed(why: &'static str, kinds: &str) -> Refusal {
    Refusal::new(RefusalCode::XformNotAllowed, "caps.refused")
        .arg("kinds", kinds)
        .arg("reason", crate::tr(why, &[]))
}

/// A model refusal because the edit breaks a constraint; other errors are not refusals.
pub fn constraint_conflict(e: &ddc_model::Error) -> Option<Refusal> {
    matches!(e, ddc_model::Error::Conflict(_)).then(|| {
        Refusal::new(RefusalCode::ConstraintConflict, "modify_ops.con.refused").arg("why", crate::constraints_ui::conflict_why(e))
    })
}

impl crate::opening_tool::OpeningTool {
    /// The refusal behind the preview under the cursor (none while it is placeable).
    pub fn refusal(&self) -> Option<Refusal> {
        match &self.preview {
            None => Some(place_refusal(&PlaceError::NoWall, None)),
            Some(pv) => pv.error.as_ref().map(|e| place_refusal(e, self.walls.get(pv.wall).map(|w| w.id))),
        }
    }
}

impl Editor {
    /// Show a refusal in the status line and keep it (with its fixes) for the card and the AI.
    pub fn refuse(&mut self, r: Refusal) {
        crate::interact::refuse(self, &r);
    }
}

/// Words of a refusal in the catalog's language (model carries keys and arguments only).
pub trait RefusalText {
    fn text(&self, cat: &Catalog) -> String;
}

impl RefusalText for Refusal {
    fn text(&self, cat: &Catalog) -> String {
        let args: Vec<(&str, &str)> = self.args.iter().map(|(k, v)| (*k, v.as_str())).collect();
        cat.tf(self.msg_key, &args)
    }
}

/// Label of a fix button.
pub fn fix_label(f: &Fix, cat: &Catalog) -> String {
    let args: Vec<(&str, &str)> = f.args.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    cat.tf(f.label_key, &args)
}

thread_local! {
    /// Arguments of the fix being run; commands take none, so they read them here.
    static ARGS: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
}

fn arg(k: &str) -> Option<f64> {
    ARGS.with(|a| a.borrow().iter().find(|(key, _)| key == k).and_then(|(_, v)| v.parse().ok()))
}

/// Run a fix: its command with the fix arguments. False if the command did nothing.
pub fn run_fix(app: &mut EditorApp, f: &Fix) -> bool {
    ARGS.with(|a| *a.borrow_mut() = f.args.clone());
    app.ed.ix.dismiss_offer();
    let ok = app.run(f.command);
    ARGS.with(|a| a.borrow_mut().clear());
    ok
}

/// Set the width of the opening being placed (mm) and place it where the cursor is: one operation.
pub fn set_opening_width(ed: &mut Editor, w_mm: f64) -> bool {
    if !ed.opening_mode || !(w_mm > 0.0) {
        return false;
    }
    ed.otool.ty.width = w_mm / 1000.0;
    let p = ed.otool.cursor;
    ed.handle_opening(Event::Click(p));
    true
}

/// Extend wall `id` to `len_mm` along its axis: one operation.
pub fn extend_wall(ed: &mut Editor, id: i64, len_mm: f64) -> bool {
    if !ed.walls.iter().any(|w| w.id == id) || !(len_mm > 0.0) {
        return false;
    }
    ed.edit_wall(id, Some(len_mm / 1000.0), None, None);
    true
}

pub fn register(app: &mut EditorApp) {
    app.register_command("arch.door.set_width", |app| {
        if let Some(w) = arg("w") {
            set_opening_width(&mut app.ed, w);
        }
    });
    app.register_command("wall.extend", |app| {
        if let (Some(id), Some(len)) = (arg("wall"), arg("len")) {
            extend_wall(&mut app.ed, id as i64, len);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    use crate::interact::script::{parse, run};

    /// Code → script that provokes it (`refusal=<code>` is checked by the script), or a unit
    /// producer for tools that need no pointer.
    enum Trigger {
        Script(&'static str),
        Unit(fn() -> Refusal),
    }

    fn table() -> Vec<(RefusalCode, Trigger)> {
        vec![
            (RefusalCode::WallTooShort, Trigger::Script("refusal_wall_short.ixs")),
            (RefusalCode::OpeningNoWall, Trigger::Script("refusal_opening_no_wall.ixs")),
            (RefusalCode::OpeningOutside, Trigger::Script("refusal_opening_outside.ixs")),
            (RefusalCode::OpeningWiderThanWall, Trigger::Script("refusal_opening_wider.ixs")),
            (RefusalCode::OpeningTooTall, Trigger::Unit(|| {
                place_refusal(&PlaceError::TooTall { top: 2.7, height: 2.5 }, None)
            })),
            (RefusalCode::OpeningOverlap, Trigger::Unit(|| place_refusal(&PlaceError::Overlap { other: 4 }, None))),
            (RefusalCode::LevelNoneAbove, Trigger::Unit(level_none_above)),
            (RefusalCode::ComponentNoLibrary, Trigger::Unit(component_no_library)),
            (RefusalCode::XformNotAllowed, Trigger::Unit(|| xform_not_allowed("caps.refused", "walls"))),
            (RefusalCode::ConstraintConflict, Trigger::Unit(|| {
                constraint_conflict(&ddc_model::Error::Conflict("edit breaks constraints".into())).unwrap()
            })),
        ]
    }

    /// One refusal per code, as the tools build it.
    fn sample(c: RefusalCode) -> Refusal {
        match c {
            RefusalCode::WallTooShort => wall_too_short(),
            RefusalCode::OpeningNoWall => place_refusal(&PlaceError::NoWall, None),
            RefusalCode::OpeningOutside => place_refusal(&PlaceError::Outside { len: 6.0, from: 5.5, to: 6.4 }, None),
            RefusalCode::OpeningWiderThanWall => {
                place_refusal(&PlaceError::Outside { len: 0.6, from: 0.0, to: 0.9 }, Some(1))
            }
            RefusalCode::OpeningTooTall => place_refusal(&PlaceError::TooTall { top: 2.7, height: 2.5 }, None),
            RefusalCode::OpeningOverlap => place_refusal(&PlaceError::Overlap { other: 4 }, None),
            RefusalCode::LevelNoneAbove => level_none_above(),
            RefusalCode::ComponentNoLibrary => component_no_library(),
            RefusalCode::XformNotAllowed => xform_not_allowed("caps.refused", "walls"),
            RefusalCode::ConstraintConflict => {
                constraint_conflict(&ddc_model::Error::Conflict("edit breaks constraints".into())).unwrap()
            }
            c => panic!("{c:?} is not an interactive tool code"),
        }
    }

    #[test]
    fn every_code_has_a_triggering_test() {
        let t = table();
        for c in RefusalCode::CODES {
            assert!(t.iter().any(|(k, _)| k == c), "{c:?} has no trigger");
        }
        assert_eq!(t.len(), RefusalCode::CODES.len());
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ix");
        for (code, trig) in t {
            match trig {
                Trigger::Unit(f) => assert_eq!(f().code, code),
                Trigger::Script(file) => {
                    let src = std::fs::read_to_string(dir.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
                    assert!(src.contains(&format!("refusal={}", code.as_str())), "{file} does not check {code:?}");
                    let mut ed = Editor::new_in_memory().unwrap();
                    let rep = run(&mut ed, &parse(&src).unwrap_or_else(|e| panic!("{file}: {e}")));
                    assert!(rep.ok(), "{file}: {:?}", rep.failures);
                }
            }
        }
    }

    #[test]
    fn fix_commands_are_registered() {
        let app = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
        for c in FIX_COMMANDS {
            assert!(app.handlers.contains_key(*c), "{c} is not registered");
        }
        let r = place_refusal(&PlaceError::Outside { len: 0.6, from: 0.0, to: 0.9 }, Some(3));
        assert_eq!(r.code, RefusalCode::OpeningWiderThanWall);
        assert_eq!(r.fixes.len(), 2);
        for f in &r.fixes {
            assert!(FIX_COMMANDS.contains(&f.command), "{}", f.command);
        }
        // the scenario of the card: wall 600, door 900 -> width 500 or wall 1100
        assert_eq!(r.fixes[0].args, vec![("w".to_string(), "500".to_string())]);
        assert_eq!(r.fixes[1].args[1], ("len".to_string(), "1100".to_string()));
    }

    #[test]
    fn codes_translated_ru_en() {
        for ru in [true, false] {
            let cat = Catalog::new(if ru { ddc_ui::Lang::Ru } else { ddc_ui::Lang::En });
            for &code in RefusalCode::CODES {
                let r = sample(code);
                assert_eq!(r.code, code);
                let t = cat.t(r.msg_key);
                assert!(t != r.msg_key && !t.is_empty(), "{} (ru={ru})", r.msg_key);
                for f in &r.fixes {
                    assert!(cat.t(f.label_key) != f.label_key, "{} (ru={ru})", f.label_key);
                }
            }
        }
    }

    #[test]
    fn wider_than_wall_text_names_both_sizes() {
        let r = place_refusal(&PlaceError::Outside { len: 0.6, from: 0.0, to: 0.9 }, Some(3));
        let cat = Catalog::new(ddc_ui::Lang::En);
        let t = r.text(&cat);
        assert!(t.contains("0.9") || t.contains("900"), "{t}");
        assert!(t.contains("0.6") || t.contains("600"), "{t}");
        assert_eq!(fix_label(&r.fixes[0], &cat), cat.tf("ix.fix.opening_width", &[("w", "500")]));
    }
}
