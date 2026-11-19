// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Scope card of the XLSX export: what goes into the book (whole model, active level,
//! selection) and which categories. The result is an `xlsx::Scope` named in the book.
use std::path::{Path, PathBuf};

use ddc_io::xlsx::{self, Cat, Scope, Selected};
use ddc_model::Model;

use crate::editor::{Editor, Sel};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Base {
    #[default]
    Whole,
    Level,
    Selection,
}

pub const CATS: [(Cat, &str); 5] = [
    (Cat::Walls, "xlsx_scope.cat_walls"),
    (Cat::Openings, "xlsx_scope.cat_openings"),
    (Cat::Rooms, "xlsx_scope.cat_rooms"),
    (Cat::Slabs, "xlsx_scope.cat_slabs"),
    (Cat::Curtains, "xlsx_scope.cat_curtains"),
];

/// State of the card: the base and the ticked categories (all ticked = no category filter).
#[derive(Clone, Debug, PartialEq)]
pub struct ScopeCard {
    pub base: Base,
    pub cats: [bool; 5],
    /// Why the last Export press was refused.
    pub error: Option<String>,
}

impl Default for ScopeCard {
    fn default() -> Self {
        Self { base: Base::Whole, cats: [true; 5], error: None }
    }
}

fn selected_of(ed: &Editor) -> Selected {
    let mut s = Selected::default();
    for sel in &ed.selection {
        match *sel {
            Sel::Wall(id) => s.walls.push(id),
            Sel::Opening(id) => s.openings.push(id),
            Sel::Room(id) => s.rooms.push(id),
            Sel::Slab(id) => s.slabs.push(id),
            Sel::Curtain(id) | Sel::Panel(id, _) => s.curtains.push(id),
            _ => {}
        }
    }
    for v in [&mut s.walls, &mut s.openings, &mut s.rooms, &mut s.slabs, &mut s.curtains] {
        v.sort_unstable();
        v.dedup();
    }
    s
}

fn is_empty(s: &Selected) -> bool {
    s.walls.is_empty() && s.openings.is_empty() && s.rooms.is_empty() && s.slabs.is_empty() && s.curtains.is_empty()
}

/// The scope the card asks for; None = the whole model. Err = text for the card (nothing to export).
pub fn build(card: &ScopeCard, ed: &Editor, ru: bool) -> Result<Option<Scope>, String> {
    let t = |k: &str| crate::tr_lang(ru, k, &[]);
    let mut sc = Scope::default();
    let mut parts: Vec<String> = vec![];
    match card.base {
        Base::Whole => {}
        Base::Level => {
            let name: String = ed
                .model
                .sql()
                .query_row("SELECT name FROM level WHERE id=?1", [ed.level], |r| r.get(0))
                .map_err(|_| t("xlsx_scope.no_level"))?;
            sc.level = Some(ed.level);
            parts.push(name);
        }
        Base::Selection => {
            let s = selected_of(ed);
            if is_empty(&s) {
                return Err(t("xlsx_scope.nothing_selected"));
            }
            sc.selected = Some(s);
            parts.push(t("xlsx_scope.base_selection"));
        }
    }
    let on: Vec<Cat> = CATS.iter().zip(card.cats).filter(|(_, on)| *on).map(|((c, _), _)| *c).collect();
    if on.is_empty() {
        return Err(t("xlsx_scope.no_categories"));
    }
    if on.len() < CATS.len() {
        let names: Vec<String> = CATS.iter().filter(|(c, _)| on.contains(c)).map(|(_, k)| t(k)).collect();
        parts.push(names.join("+"));
        sc.cats = Some(on);
    }
    if sc.is_whole() {
        return Ok(None);
    }
    sc.label = parts.join(", ");
    Ok(Some(sc))
}

/// An export target that exists: what a rewrite would drop, and the ways out.
#[derive(Clone, Debug, PartialEq)]
pub struct OverCard {
    pub path: PathBuf,
    pub scope: Option<Scope>,
    /// Edits in the book not applied to the model yet.
    pub pending: usize,
    /// Rows of the book that the new export does not carry.
    pub outside: usize,
    /// A book of this project: it can be applied first or replaced. Any other book only
    /// gets a new name.
    pub ours: bool,
    /// Free name beside the book.
    pub name: PathBuf,
}

pub enum Guard {
    Write,
    Ask(OverCard),
}

/// Free file name beside `path`: `book-2.xlsx`, `book-3.xlsx`...
pub fn free_name(path: &Path) -> PathBuf {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "xlsx".into());
    (2u32..)
        .map(|n| path.with_file_name(format!("{stem}-{n}.{ext}")))
        .find(|p| !p.exists())
        .unwrap_or_else(|| path.to_path_buf())
}

/// Moves the old book into `_archive/` beside it (never deletes); returns its new place.
pub fn archive_book(path: &Path) -> Result<PathBuf, String> {
    let dir = path.parent().ok_or("no folder")?.join("_archive");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = path.file_name().ok_or("no file name")?.to_string_lossy().into_owned();
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut dest = dir.join(&name);
    let mut n = 2u32;
    while dest.exists() {
        dest = dir.join(format!("{stem}-{n}.{ext}"));
        n += 1;
    }
    std::fs::rename(path, &dest).map_err(|e| e.to_string())?;
    Ok(dest)
}

/// Looks at the book an export is about to overwrite. `read` is the dry preview of the book
/// against the model (nothing is written). A missing file, or a book of this project with
/// nothing to lose, is written over without a card.
pub fn guard(
    model: &mut Model,
    path: &Path,
    scope: Option<&Scope>,
    read: impl FnOnce(&mut Model, &Path) -> Result<xlsx::Report, String>,
) -> Result<Guard, String> {
    if !path.exists() {
        return Ok(Guard::Write);
    }
    let card = |pending, outside, ours| {
        Guard::Ask(OverCard { path: path.to_path_buf(), scope: scope.cloned(), pending, outside, ours, name: free_name(path) })
    };
    let uid: Option<i64> =
        model.sql().query_row("SELECT value FROM meta WHERE key='project_uid'", [], |r| r.get(0)).ok();
    let info = match xlsx::book_info(path) {
        Ok(Some(i)) if uid.is_some() && i.project == uid => i,
        _ => return Ok(card(0, 0, false)),
    };
    let Ok(rep) = read(model, path) else {
        return Ok(card(0, 0, false));
    };
    // anything the user typed into the book counts: new columns, rows the model does not take,
    // removed rows; a rewrite would drop each of them
    let pending = rep.changes.len()
        + rep.props.len()
        + rep.conflicts.len()
        + rep.rejected.len()
        + rep.proposals.len()
        + rep.ignored.len()
        + rep.removed.len()
        + rep.matches.len();
    let outside = xlsx::rows_outside(model, scope, &info.ids)?;
    if pending == 0 && outside == 0 {
        return Ok(Guard::Write);
    }
    Ok(card(pending, outside, true))
}

/// Preview header line naming the scope of the book; None for a whole-model book.
pub fn preview_line(ru: bool, r: &xlsx::Report) -> Option<String> {
    (!r.scope.is_empty()).then(|| crate::tr_lang(ru, "xlsx_scope.preview_scope", &[("scope", &r.scope)]))
}

/// Draws the card body; true when a base or category was changed.
pub fn show(ui: &mut egui::Ui, card: &mut ScopeCard, ru: bool) {
    let t = |k: &'static str| crate::tr_str(ru, k);
    for (b, k) in [
        (Base::Whole, "xlsx_scope.base_whole"),
        (Base::Level, "xlsx_scope.base_level"),
        (Base::Selection, "xlsx_scope.base_selection"),
    ] {
        ui.selectable_value(&mut card.base, b, t(k));
    }
    ui.add_space(6.0);
    ui.label(t("xlsx_scope.categories"));
    for ((_, k), on) in CATS.iter().zip(card.cats.iter_mut()) {
        ui.checkbox(on, t(k));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed() -> Editor {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.level = ed.model.add_level("Level 2", 3.0).unwrap();
        ed
    }

    #[test]
    fn whole_model_with_every_category_is_no_scope() {
        assert_eq!(build(&ScopeCard::default(), &ed(), false).unwrap(), None);
    }

    #[test]
    fn level_and_categories_are_named() {
        let mut c = ScopeCard { base: Base::Level, cats: [false; 5], error: None };
        c.cats[0] = true;
        c.cats[1] = true;
        let sc = build(&c, &ed(), false).unwrap().unwrap();
        assert_eq!(sc.level, Some(ed().level));
        assert_eq!(sc.cats, Some(vec![Cat::Walls, Cat::Openings]));
        assert_eq!(sc.label, "Level 2, walls+doors");
    }

    #[test]
    fn selection_scope_takes_selected_ids_and_refuses_empty() {
        let mut e = ed();
        let c = ScopeCard { base: Base::Selection, ..ScopeCard::default() };
        assert!(build(&c, &e, false).is_err());
        e.selection = vec![Sel::Wall(3), Sel::Wall(1), Sel::Opening(2)];
        let sc = build(&c, &e, false).unwrap().unwrap();
        let s = sc.selected.unwrap();
        assert_eq!((s.walls, s.openings), (vec![1, 3], vec![2]));
    }

    #[test]
    fn no_category_ticked_is_refused() {
        let c = ScopeCard { cats: [false; 5], ..ScopeCard::default() };
        assert!(build(&c, &ed(), false).is_err());
    }

    #[test]
    fn preview_line_names_scope() {
        let r = xlsx::Report { scope: "Level 2, walls+doors".into(), ..Default::default() };
        assert!(preview_line(false, &r).unwrap().contains("Level 2, walls+doors"));
        assert_eq!(preview_line(false, &xlsx::Report::default()), None);
    }
}
