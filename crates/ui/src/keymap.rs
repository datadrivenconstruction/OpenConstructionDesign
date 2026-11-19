// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! User key map over the registry defaults: chords (`Ctrl+Shift+X`, `F5`) and two-letter
//! sequences per command, conflict detection, import/export as one JSON file (`keymap.json`;
//! sequences are mirrored to `shortcuts.json` for [`crate::shortcuts2::Keymap::load`]).
use crate::commands::Shortcut;
use crate::shortcuts2::{Keymap, SeqConflict};
use crate::Registry;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const FILE: &str = "keymap.json";
pub const VERSION: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UserKeys {
    #[serde(default)]
    pub version: u32,
    /// Command → chords; an empty list unbinds the defaults.
    #[serde(default)]
    pub chords: BTreeMap<String, Vec<String>>,
    /// Command → sequence; `""` unbinds.
    #[serde(default)]
    pub seqs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyConflict {
    /// One chord on several commands; the first keeps it (user bindings come first).
    Chord {
        key: String,
        kept: String,
        dropped: String,
    },
    /// The OS or window manager owns this combination.
    Reserved {
        cmd: String,
        key: String,
    },
    /// Not a chord: needs Ctrl/Alt or an F-key (plain letters are sequences).
    BadChord {
        cmd: String,
        key: String,
    },
    UnknownCommand(String),
    Seq(SeqConflict),
}

impl KeyConflict {
    /// Command ids involved (UI highlights their rows).
    pub fn commands(&self) -> Vec<&str> {
        match self {
            KeyConflict::Chord { kept, dropped, .. } => vec![kept, dropped],
            KeyConflict::Reserved { cmd, .. } | KeyConflict::BadChord { cmd, .. } => vec![cmd],
            KeyConflict::UnknownCommand(c) => vec![c],
            KeyConflict::Seq(SeqConflict::Duplicate { kept, dropped, .. }) => vec![kept, dropped],
            KeyConflict::Seq(SeqConflict::Bad { cmd, .. }) => vec![cmd],
            KeyConflict::Seq(SeqConflict::UnknownCommand(c)) => vec![c],
            KeyConflict::Seq(SeqConflict::BadFile(_)) => vec![],
        }
    }
}

/// Text form that parses back (`key.name()`, not the display symbol).
pub fn chord_text(s: &Shortcut) -> String {
    let mut t = String::new();
    if s.ctrl {
        t += "Ctrl+";
    }
    if s.shift {
        t += "Shift+";
    }
    if s.alt {
        t += "Alt+";
    }
    t += s.key.name();
    t
}

/// A chord the app can route: Ctrl or Alt held, or an F-key.
pub fn chord_ok(s: &Shortcut) -> bool {
    let n = s.key.name();
    s.ctrl
        || s.alt
        || (n.starts_with('F') && n.len() > 1 && n[1..].chars().all(|c| c.is_ascii_digit()))
}

/// Effective bindings: chord lookup, sequence keymap and every conflict found.
#[derive(Debug, Default)]
pub struct Bindings {
    chords: BTreeMap<Shortcut, String>,
    by_cmd: BTreeMap<String, Vec<String>>,
    pub seqs: Keymap,
    pub conflicts: Vec<KeyConflict>,
}

impl Bindings {
    pub fn build(reg: &Registry, user: &UserKeys) -> Bindings {
        let mut b = Bindings::default();
        let mut want: Vec<(String, String)> = vec![];
        for (cmd, list) in &user.chords {
            if reg.get(cmd).is_none() {
                b.conflicts.push(KeyConflict::UnknownCommand(cmd.clone()));
                continue;
            }
            want.extend(list.iter().map(|k| (cmd.clone(), k.clone())));
        }
        for c in reg.all() {
            if !user.chords.contains_key(&c.id) {
                want.extend(c.shortcuts.iter().map(|k| (c.id.clone(), k.clone())));
            }
        }
        for (cmd, key) in want {
            let Some(sc) = Shortcut::parse(key.trim()) else {
                b.conflicts.push(KeyConflict::BadChord { cmd, key });
                continue;
            };
            let is_default = reg
                .get(&cmd)
                .is_some_and(|c| c.parsed_shortcuts().contains(&sc));
            if !chord_ok(&sc) && !is_default {
                b.conflicts.push(KeyConflict::BadChord { cmd, key });
                continue;
            }
            if sc.os_reserved() {
                b.conflicts.push(KeyConflict::Reserved { cmd, key });
                continue;
            }
            if let Some(kept) = b.chords.get(&sc) {
                if *kept != cmd {
                    b.conflicts.push(KeyConflict::Chord {
                        key: chord_text(&sc),
                        kept: kept.clone(),
                        dropped: cmd,
                    });
                }
                continue;
            }
            b.by_cmd
                .entry(cmd.clone())
                .or_default()
                .push(chord_text(&sc));
            b.chords.insert(sc, cmd);
        }
        b.seqs = Keymap::with_overrides(reg, &user.seqs_json());
        b.conflicts
            .extend(b.seqs.conflicts.iter().cloned().map(KeyConflict::Seq));
        b
    }

    pub fn command(&self, key: egui::Key, m: egui::Modifiers) -> Option<&str> {
        let sc = Shortcut {
            ctrl: m.command,
            shift: m.shift,
            alt: m.alt,
            key,
        };
        self.chords.get(&sc).map(String::as_str)
    }
    pub fn chords_of(&self, cmd: &str) -> &[String] {
        self.by_cmd.get(cmd).map_or(&[], Vec::as_slice)
    }
    /// Command that owns `chord` now (other than `cmd`): live check in the editor.
    pub fn owner_of(&self, chord: &str, cmd: &str) -> Option<&str> {
        let sc = Shortcut::parse(chord)?;
        self.chords
            .get(&sc)
            .map(String::as_str)
            .filter(|o| *o != cmd)
    }
}

impl UserKeys {
    pub fn seqs_json(&self) -> String {
        if self.seqs.is_empty() {
            return String::new();
        }
        serde_json::to_string(&self.seqs).unwrap_or_default()
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
    /// Parsed file; a file from a newer version is refused (its meaning may differ).
    pub fn from_json(s: &str) -> Result<UserKeys, String> {
        // serde also reads a struct from a JSON array, positionally
        let v: serde_json::Value = serde_json::from_str(s).map_err(|e| e.to_string())?;
        if !v.is_object() {
            return Err("not a key map object".into());
        }
        let mut k: UserKeys = serde_json::from_value(v).map_err(|e| e.to_string())?;
        if k.version > VERSION {
            return Err(format!("version {} > {VERSION}", k.version));
        }
        crate::commands::migrate_ids(&mut k.chords);
        crate::commands::migrate_ids(&mut k.seqs);
        Ok(k)
    }
    /// Only the user's own changes, not the defaults: an imported file keeps following future
    /// default changes for everything it does not touch. Unknown commands are left out.
    pub fn overrides(&self, reg: &Registry) -> UserKeys {
        let mut u = UserKeys {
            version: VERSION,
            ..Default::default()
        };
        for (cmd, list) in &self.chords {
            let Some(c) = reg.get(cmd) else { continue };
            let mine: Vec<Shortcut> = list
                .iter()
                .filter_map(|k| Shortcut::parse(k.trim()))
                .collect();
            if mine != c.parsed_shortcuts() || mine.len() != list.len() {
                u.chords.insert(cmd.clone(), list.clone());
            }
        }
        for (cmd, seq) in &self.seqs {
            let Some(c) = reg.get(cmd) else { continue };
            let def = c.mnemonic.as_deref().unwrap_or("").to_ascii_uppercase();
            if seq.trim().to_ascii_uppercase() != def {
                u.seqs.insert(cmd.clone(), seq.clone());
            }
        }
        u
    }
    /// Replace the chords of `cmd` (`None` — unbind).
    pub fn set_chord(&mut self, cmd: &str, chord: Option<&str>) {
        self.chords.insert(
            cmd.into(),
            chord.map(|c| vec![c.to_string()]).unwrap_or_default(),
        );
    }
    /// Two letters; Cyrillic is mapped by key position (the Russian letters on the W and A keys give "WA").
    pub fn set_seq(&mut self, cmd: &str, seq: &str) {
        let s: String = seq
            .trim()
            .chars()
            .map(|c| crate::commands::cyr_to_latin(c).unwrap_or(c))
            .collect();
        self.seqs.insert(cmd.into(), s.to_ascii_uppercase());
    }
    /// Back to the registry default for `cmd`.
    pub fn reset(&mut self, cmd: &str) {
        self.chords.remove(cmd);
        self.seqs.remove(cmd);
    }
    /// Imported entries override ours.
    pub fn merge(&mut self, other: UserKeys) {
        self.chords.extend(other.chords);
        self.seqs.extend(other.seqs);
    }
    /// `keymap.json`, or sequences from the older `shortcuts.json`.
    pub fn load(dir: Option<&Path>) -> UserKeys {
        Self::load_checked(dir).0
    }
    /// [`Self::load`]; an unreadable `keymap.json` is kept as `keymap.json.bad` (second value)
    /// so the next save does not wipe the user's chords silently.
    pub fn load_checked(dir: Option<&Path>) -> (UserKeys, Option<std::path::PathBuf>) {
        let Some(d) = dir else {
            return (UserKeys::default(), None);
        };
        let path = d.join(FILE);
        if let Ok(t) = std::fs::read_to_string(&path) {
            return match UserKeys::from_json(&t) {
                Ok(k) => (k, None),
                Err(_) => (UserKeys::default(), crate::uxstate::keep_bad(&path)),
            };
        }
        let mut seqs = std::fs::read_to_string(crate::shortcuts2::user_file(d))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        crate::commands::migrate_ids(&mut seqs);
        let k = UserKeys {
            version: VERSION,
            seqs,
            ..Default::default()
        };
        (k, None)
    }
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let mut me = self.clone();
        me.version = VERSION;
        crate::uxstate::write_atomic(&dir.join(FILE), &me.to_json())?;
        crate::uxstate::write_atomic(&crate::shortcuts2::user_file(dir), &self.seqs_json())
    }
}

/// Chords the app keeps for itself: they cannot be given to another command.
/// Returns the i18n suffix of the reason (`quick.keys.res_<name>`).
pub fn reserved_reason(s: &Shortcut) -> Option<&'static str> {
    use egui::Key::*;
    match (s.ctrl, s.shift, s.alt, s.key) {
        (true, false, false, C) => Some("copy"),
        (true, false, false, V) => Some("paste"),
        (true, false, false, X) => Some("cut"),
        (true, false, false, Z) | (true, true, false, Z) | (true, false, false, Y) => Some("undo"),
        (true, false, false, S) | (true, true, false, S) => Some("save"),
        (true, false, false, K) | (true, true, false, P) => Some("palette"),
        _ => None,
    }
}

/// Chords the OS or window manager takes before the app sees them (shown struck through).
pub const OS_CHORDS: [&str; 4] = ["Alt+F4", "Alt+Tab", "Ctrl+Alt+Delete", "Ctrl+Shift+Escape"];

/// What to do when a new chord is already bound to another command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// The two commands exchange chords.
    Swap,
    /// The new command takes the chord; the other loses just that chord.
    Take,
    /// Nothing changes.
    Keep,
}

impl UserKeys {
    /// Give `chord` to `cmd`, whose current owner is `other`, per `how`. One call changes both
    /// commands at once, so the pair is never left half-done.
    pub fn resolve(
        &mut self,
        b: &Bindings,
        cmd: &str,
        other: &str,
        chord: &str,
        how: Resolution,
    ) {
        if how == Resolution::Keep || cmd == other {
            return;
        }
        let same = |a: &str| match (Shortcut::parse(a), Shortcut::parse(chord)) {
            (Some(x), Some(y)) => x == y,
            _ => a == chord,
        };
        let mut theirs: Vec<String> = b
            .chords_of(other)
            .iter()
            .filter(|c| !same(c))
            .cloned()
            .collect();
        if how == Resolution::Swap {
            for c in b.chords_of(cmd) {
                if !same(c) && !theirs.contains(c) {
                    theirs.push(c.clone());
                }
            }
        }
        self.chords.insert(cmd.into(), vec![chord.to_string()]);
        self.chords.insert(other.into(), theirs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_clean_and_lookup() {
        let r = Registry::builtin();
        let b = Bindings::build(&r, &UserKeys::default());
        assert_eq!(b.conflicts, vec![]);
        assert_eq!(
            b.command(egui::Key::K, egui::Modifiers::COMMAND),
            Some("app.palette")
        );
        assert_eq!(b.seqs.command("WA"), Some("arch.wall"));
        assert_eq!(b.chords_of("app.palette"), ["Ctrl+K", "Ctrl+Shift+P"]);
    }

    #[test]
    fn shortcut_conflicts_detected() {
        let r = Registry::builtin();
        let mut u = UserKeys::default();
        // user takes Ctrl+K from the palette: user wins, the default owner is reported
        u.set_chord("mod.move", Some("Ctrl+K"));
        u.set_chord("mod.rotate", Some("Alt+F4"));
        u.set_chord("mod.mirror", Some("Q"));
        u.set_chord("mod.copy", Some("Ctrl+Nope"));
        u.set_chord("no.such", Some("Ctrl+F9"));
        u.set_seq("mod.trim", "WA");
        let b = Bindings::build(&r, &u);
        assert_eq!(
            b.command(egui::Key::K, egui::Modifiers::COMMAND),
            Some("mod.move")
        );
        let has = |c: &KeyConflict| b.conflicts.contains(c);
        assert!(has(&KeyConflict::Chord {
            key: "Ctrl+K".into(),
            kept: "mod.move".into(),
            dropped: "app.palette".into()
        }));
        assert!(has(&KeyConflict::Reserved {
            cmd: "mod.rotate".into(),
            key: "Alt+F4".into()
        }));
        assert!(has(&KeyConflict::BadChord {
            cmd: "mod.mirror".into(),
            key: "Q".into()
        }));
        assert!(has(&KeyConflict::BadChord {
            cmd: "mod.copy".into(),
            key: "Ctrl+Nope".into()
        }));
        assert!(has(&KeyConflict::UnknownCommand("no.such".into())));
        assert!(has(&KeyConflict::Seq(SeqConflict::Duplicate {
            seq: "WA".into(),
            kept: "mod.trim".into(),
            dropped: "arch.wall".into()
        })));
        // the palette keeps its second chord; live check names the new owner
        assert_eq!(b.chords_of("app.palette"), ["Ctrl+Shift+P"]);
        assert_eq!(b.owner_of("Ctrl+K", "app.palette"), Some("mod.move"));
        assert_eq!(b.owner_of("Ctrl+K", "mod.move"), None);
        let c = b
            .conflicts
            .iter()
            .find(|c| matches!(c, KeyConflict::Chord { .. }))
            .unwrap();
        assert_eq!(c.commands(), ["mod.move", "app.palette"]);
        // unbinding removes the defaults
        let mut u = UserKeys::default();
        u.set_chord("edit.undo", None);
        let b = Bindings::build(&r, &u);
        assert_eq!(b.command(egui::Key::Z, egui::Modifiers::COMMAND), None);
        assert!(b.conflicts.is_empty());
        u.reset("edit.undo");
        let b = Bindings::build(&r, &u);
        assert_eq!(
            b.command(egui::Key::Z, egui::Modifiers::COMMAND),
            Some("edit.undo")
        );
    }

    #[test]
    fn import_export_roundtrip() {
        let r = Registry::builtin();
        let mut u = UserKeys::default();
        u.set_chord("mod.move", Some("Ctrl+Alt+M"));
        u.set_seq("mod.move", "qq");
        // a user entry equal to the default is not exported
        u.set_chord("app.palette", Some("Ctrl+K"));
        u.chords.insert(
            "app.palette".into(),
            vec!["Ctrl+K".into(), "Ctrl+Shift+P".into()],
        );
        u.set_seq("arch.wall", "wa");
        let full = u.overrides(&r);
        assert_eq!(full.chords.len(), 1);
        assert_eq!(full.chords["mod.move"], ["Ctrl+Alt+M"]);
        assert_eq!(full.seqs.len(), 1);
        assert_eq!(full.seqs["mod.move"], "QQ");
        let back = UserKeys::from_json(&full.to_json()).unwrap();
        let b2 = Bindings::build(&r, &back);
        assert_eq!(b2.conflicts, vec![]);
        assert_eq!(back.overrides(&r), full);
        // unbinding a default is an override too
        let mut un = UserKeys::default();
        un.set_chord("edit.undo", None);
        assert_eq!(un.overrides(&r).chords["edit.undo"], Vec::<String>::new());
        assert!(UserKeys::from_json("[1]").is_err());
        assert!(UserKeys::from_json(r#"{"version": 99}"#).is_err());
        let mut mine = UserKeys::default();
        mine.set_seq("mod.copy", "CC");
        let mut imp = UserKeys::default();
        imp.set_seq("mod.move", "QQ");
        mine.merge(imp);
        assert_eq!(mine.seqs["mod.copy"], "CC");
        assert_eq!(mine.seqs["mod.move"], "QQ");
        let dir = std::env::temp_dir().join(format!("ddc-km-{}", std::process::id()));
        u.save(&dir).unwrap();
        assert_eq!(UserKeys::load(Some(&dir)).chords, u.chords);
        // the sequence mirror is what the startup keymap reads
        assert_eq!(Keymap::load(&r, Some(&dir)).command("QQ"), Some("mod.move"));
        // a corrupt file is kept aside
        std::fs::write(dir.join(FILE), "{oops").unwrap();
        let (k, bad) = UserKeys::load_checked(Some(&dir));
        assert!(k.chords.is_empty());
        let bad = bad.expect("backup");
        assert_eq!(std::fs::read_to_string(&bad).unwrap(), "{oops");
        let _ = std::fs::remove_file(&bad);
        assert_eq!(UserKeys::load(Some(&dir)).seqs["mod.move"], "QQ");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            chord_text(&Shortcut::parse("Ctrl+,").unwrap()),
            "Ctrl+Comma"
        );
        assert!(Shortcut::parse("Ctrl+Comma").is_some());
    }

    /// A key map saved before a command id was renamed (`data/command_renames.json`) still
    /// binds: from `keymap.json`, from the older `shortcuts.json` and in the usage store.
    #[test]
    fn old_saved_keymap_binds_renamed_commands() {
        let r = Registry::builtin();
        let (old, new) = ("xs57.undo_import", "keys.undo_import");
        assert_eq!(crate::commands::current_id(old), new);
        let b0 = Bindings::build(&r, &UserKeys::default());
        let m = egui::Modifiers::COMMAND | egui::Modifiers::ALT | egui::Modifiers::SHIFT;
        let key = [
            egui::Key::F9,
            egui::Key::F10,
            egui::Key::F11,
            egui::Key::F12,
        ]
        .into_iter()
        .find(|&k| b0.command(k, m).is_none())
        .expect("a free chord");
        let chord = chord_text(&Shortcut {
            ctrl: true,
            shift: true,
            alt: true,
            key,
        });
        assert!(b0.seqs.command("QQ").is_none());
        let saved =
            format!(r#"{{"version":1,"chords":{{"{old}":["{chord}"]}},"seqs":{{"{old}":"QQ"}}}}"#);
        let dir = std::env::temp_dir().join(format!("ddc-km-renamed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILE), &saved).unwrap();
        let u = UserKeys::load(Some(&dir));
        assert!(u.chords.get(old).is_none() && u.seqs.get(old).is_none());
        let b = Bindings::build(&r, &u);
        assert_eq!(b.conflicts, vec![]);
        assert_eq!(b.command(key, m), Some(new));
        assert_eq!(b.seqs.command("QQ"), Some(new));
        // the sequences-only file of older versions
        std::fs::remove_file(dir.join(FILE)).unwrap();
        std::fs::write(
            crate::shortcuts2::user_file(&dir),
            format!(r#"{{"{old}":"QQ"}}"#),
        )
        .unwrap();
        assert_eq!(Keymap::load(&r, Some(&dir)).command("QQ"), Some(new));
        let b = Bindings::build(&r, &UserKeys::load(Some(&dir)));
        assert_eq!(b.seqs.command("QQ"), Some(new));
        let _ = std::fs::remove_dir_all(&dir);
        // usage ranking and «Repeat last» follow the rename too
        let usage = crate::frecency::Usage::from_json(&format!(
            r#"{{"cmds":{{"{old}":{{"count":4,"last":1.0,"weight":4.0}}}},"order":["{old}","{new}"]}}"#
        ));
        assert_eq!(usage.cmds.get(new).map(|e| e.count), Some(4));
        assert_eq!(usage.order, vec![new.to_string()]);
    }

    #[test]
    fn swap_exchanges_bindings() {
        let r = Registry::builtin();
        let b = Bindings::build(&r, &UserKeys::default());
        let old: Vec<String> = b.chords_of("edit.undo").to_vec();
        assert!(!old.is_empty());
        let mut u = UserKeys::default();
        u.resolve(&b, "edit.undo", "app.palette", "Ctrl+K", Resolution::Swap);
        let b2 = Bindings::build(&r, &u);
        assert_eq!(b2.conflicts, vec![]);
        assert_eq!(b2.chords_of("edit.undo"), ["Ctrl+K"]);
        for c in &old {
            assert!(b2.chords_of("app.palette").contains(c), "{c}");
        }
        assert!(b2.chords_of("app.palette").contains(&"Ctrl+Shift+P".to_string()));
        assert!(!b2.chords_of("app.palette").contains(&"Ctrl+K".to_string()));
    }

    #[test]
    fn take_unbinds_other() {
        let r = Registry::builtin();
        let b = Bindings::build(&r, &UserKeys::default());
        let mut u = UserKeys::default();
        u.resolve(&b, "mod.move", "app.palette", "Ctrl+K", Resolution::Take);
        let b2 = Bindings::build(&r, &u);
        assert_eq!(b2.conflicts, vec![]);
        assert_eq!(b2.chords_of("mod.move"), ["Ctrl+K"]);
        assert_eq!(b2.chords_of("app.palette"), ["Ctrl+Shift+P"]);
        // taking the only chord leaves the other command unbound
        let mut u = UserKeys::default();
        u.resolve(&b, "mod.move", "edit.undo", "Ctrl+Z", Resolution::Take);
        let b3 = Bindings::build(&r, &u);
        assert!(b3.chords_of("edit.undo").iter().all(|c| c != "Ctrl+Z"));
    }

    #[test]
    fn keep_changes_nothing() {
        let r = Registry::builtin();
        let b = Bindings::build(&r, &UserKeys::default());
        let mut u = UserKeys::default();
        u.resolve(&b, "mod.move", "app.palette", "Ctrl+K", Resolution::Keep);
        assert_eq!(u, UserKeys::default());
    }

    #[test]
    fn reserved_chord_refused() {
        for (k, why) in [
            ("Ctrl+C", "copy"),
            ("Ctrl+V", "paste"),
            ("Ctrl+Z", "undo"),
            ("Ctrl+Shift+Z", "undo"),
            ("Ctrl+S", "save"),
            ("Ctrl+K", "palette"),
            ("Ctrl+Shift+P", "palette"),
        ] {
            assert_eq!(reserved_reason(&Shortcut::parse(k).unwrap()), Some(why), "{k}");
        }
        assert_eq!(reserved_reason(&Shortcut::parse("Ctrl+Alt+M").unwrap()), None);
        for k in OS_CHORDS {
            assert!(Shortcut::parse(k).is_some_and(|s| s.os_reserved()), "{k}");
        }
    }
}
