// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Saved workspaces: user-named copies of the current layout (`custom:<slug>` in
//! `workspaces.json`), rename, remove and quick switch. Pure list edits; the shell persists
//! the result with [`crate::workspace::save_custom`].
use crate::workspace::{Workspace, PRESET_IDS};

/// Longest name kept; longer input is refused rather than cut.
pub const MAX_NAME: usize = 40;
/// Saved layouts kept at most, presets not counted.
pub const MAX_SAVED: usize = 24;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameError {
    Empty,
    TooLong,
    /// The name of a built-in layout.
    Preset,
    Full,
}

impl NameError {
    /// i18n key of the refusal.
    pub fn key(&self) -> &'static str {
        match self {
            NameError::Empty => "create_input.ws.err.empty",
            NameError::TooLong => "create_input.ws.err.long",
            NameError::Preset => "create_input.ws.err.preset",
            NameError::Full => "create_input.ws.err.full",
        }
    }
}

pub fn is_preset(id: &str) -> bool {
    PRESET_IDS.contains(&id)
}

fn clean(name: &str) -> Result<String, NameError> {
    let n = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if n.is_empty() {
        return Err(NameError::Empty);
    }
    if n.chars().count() > MAX_NAME {
        return Err(NameError::TooLong);
    }
    if is_preset(&n.to_lowercase()) {
        return Err(NameError::Preset);
    }
    Ok(n)
}

/// `custom:<slug>`: lower-case letters and digits of any script, other runs become `-`;
/// a taken slug gets `-2`, `-3`…
pub fn custom_id(name: &str, all: &[Workspace]) -> String {
    let mut slug = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    let base = format!("custom:{}", if slug.is_empty() { "layout" } else { slug });
    let taken = |id: &str| all.iter().any(|w| w.id == id);
    if !taken(&base) {
        return base;
    }
    (2..).map(|k| format!("{base}-{k}")).find(|id| !taken(id)).expect("free id")
}

/// Saves `current` under `name`: a saved layout of the same name (any case) is overwritten
/// in place, otherwise a new one is added. Returns the saved id.
pub fn save_as(all: &mut Vec<Workspace>, current: &Workspace, name: &str) -> Result<String, NameError> {
    let name = clean(name)?;
    let same = |w: &Workspace| !is_preset(&w.id) && w.name.to_lowercase() == name.to_lowercase();
    let base = if is_preset(&current.id) { Some(current.id.clone()) } else { current.base.clone() };
    if let Some(w) = all.iter_mut().find(|w| same(w)) {
        let id = w.id.clone();
        *w = Workspace { id: id.clone(), name, base, ..current.clone() };
        return Ok(id);
    }
    if saved(all).count() >= MAX_SAVED {
        return Err(NameError::Full);
    }
    let id = custom_id(&name, all);
    all.push(Workspace { id: id.clone(), name, base, ..current.clone() });
    Ok(id)
}

/// Renames a saved layout; its id stays so `layout=` in prefs keeps pointing at it.
pub fn rename(all: &mut [Workspace], id: &str, name: &str) -> Result<(), NameError> {
    if is_preset(id) {
        return Err(NameError::Preset);
    }
    let name = clean(name)?;
    if let Some(w) = all.iter_mut().find(|w| w.id == id) {
        w.name = name;
    }
    Ok(())
}

/// Removes a saved layout; presets stay. True when one was removed.
pub fn remove(all: &mut Vec<Workspace>, id: &str) -> bool {
    let n = all.len();
    all.retain(|w| is_preset(&w.id) || w.id != id);
    all.len() != n
}

/// Saved layouts in list order (what `save_custom` writes).
pub fn saved(all: &[Workspace]) -> impl Iterator<Item = &Workspace> {
    all.iter().filter(|w| !is_preset(&w.id))
}

/// Quick switch: the next layout after `current`, wrapping; `back` goes the other way.
pub fn next(all: &[Workspace], current: &str, back: bool) -> Option<String> {
    if all.is_empty() {
        return None;
    }
    let n = all.len();
    let i = all.iter().position(|w| w.id == current).unwrap_or(0);
    let j = if back { (i + n - 1) % n } else { (i + 1) % n };
    Some(all[j].id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fx::fx;
    use crate::workspace::{load, preset, presets, save_custom};

    #[test]
    fn save_as_adds_then_overwrites_by_name_and_keeps_presets() {
        let mut all = presets();
        let mut cur = preset("focus").unwrap();
        cur.status = !cur.status;
        let (my, my_lc, lay) = (fx("ws_my"), fx("ws_my_lc"), fx("ws_layout"));
        let id = save_as(&mut all, &cur, &format!("  {my}   {lay} ")).unwrap();
        assert_eq!(id, format!("custom:{my_lc}-{lay}"));
        let w = all.iter().find(|w| w.id == id).unwrap();
        assert_eq!((w.name.as_str(), w.base.as_deref()), (format!("{my} {lay}").as_str(), Some("focus")));
        assert_eq!(w.status, cur.status);
        cur.status = !cur.status;
        assert_eq!(save_as(&mut all, &cur, &format!("{my_lc} {lay}")).unwrap(), id);
        assert_eq!(saved(&all).count(), 1);
        assert_eq!(all.iter().find(|w| w.id == id).unwrap().status, cur.status);
        assert_eq!(all.len(), PRESET_IDS.len() + 1);
    }

    #[test]
    fn names_are_checked_and_ids_stay_unique() {
        let mut all = presets();
        let cur = preset("classic").unwrap();
        assert_eq!(save_as(&mut all, &cur, "   "), Err(NameError::Empty));
        assert_eq!(save_as(&mut all, &cur, "Focus"), Err(NameError::Preset));
        assert_eq!(save_as(&mut all, &cur, &"x".repeat(MAX_NAME + 1)), Err(NameError::TooLong));
        let a = save_as(&mut all, &cur, "A/B").unwrap();
        let b = save_as(&mut all, &cur, "a b").unwrap();
        assert_eq!((a.as_str(), b.as_str()), ("custom:a-b", "custom:a-b-2"));
        assert_eq!(custom_id("!!!", &all), "custom:layout");
        for k in 0..MAX_SAVED - 2 {
            save_as(&mut all, &cur, &format!("n{k}")).unwrap();
        }
        assert_eq!(save_as(&mut all, &cur, "one more"), Err(NameError::Full));
        assert!(save_as(&mut all, &cur, "n0").is_ok(), "overwrite is allowed when full");
    }

    #[test]
    fn rename_remove_and_switch() {
        let mut all = presets();
        let est = fx("ws_est");
        let id = save_as(&mut all, &preset("data").unwrap(), est).unwrap();
        rename(&mut all, &id, &format!("{est} 2")).unwrap();
        assert_eq!(all.last().unwrap().name, format!("{est} 2"));
        assert_eq!(rename(&mut all, "classic", "x"), Err(NameError::Preset));
        assert_eq!(next(&all, &id, false).as_deref(), Some("classic"));
        assert_eq!(next(&all, "classic", true).as_deref(), Some(id.as_str()));
        assert!(!remove(&mut all, "classic"));
        assert!(remove(&mut all, &id));
        assert_eq!(all, presets());
    }

    #[test]
    fn saved_layouts_survive_the_file_round_trip() {
        let dir = std::env::temp_dir().join(format!("ddc_saved_ws_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut all = presets();
        let id = save_as(&mut all, &preset("split").unwrap(), fx("ws_two_tabs")).unwrap();
        let mine: Vec<Workspace> = saved(&all).cloned().collect();
        save_custom(&dir, &mine).unwrap();
        let back = load(&dir);
        assert_eq!(back.all, all);
        assert!(back.all.iter().any(|w| w.id == id));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
