// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Two-letter shortcuts (TOOLS_UI §0): two letters typed in a row without modifiers within
//! [`SEQ_TIMEOUT`], by physical key (layout-independent). Sequences come from the registry
//! mnemonics, then the user file `shortcuts.json` (`{"cmd.id": "XY"}`, `""` unbinds).
use crate::Registry;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Gap allowed between the two letters, seconds.
pub const SEQ_TIMEOUT: f64 = 0.8;
/// User override file name in the settings folder.
pub const USER_FILE: &str = "shortcuts.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeqConflict {
    /// Same sequence on two commands (the first keeps it).
    Duplicate {
        seq: String,
        kept: String,
        dropped: String,
    },
    /// Not two latin letters.
    Bad { cmd: String, seq: String },
    /// Override for a command the registry does not know.
    UnknownCommand(String),
    /// User file is not a JSON object of strings.
    BadFile(String),
}

#[derive(Debug, Default, Clone)]
pub struct Keymap {
    by_seq: BTreeMap<String, String>,
    by_cmd: BTreeMap<String, String>,
    pub conflicts: Vec<SeqConflict>,
}

fn valid(seq: &str) -> bool {
    seq.len() == 2 && seq.bytes().all(|b| b.is_ascii_uppercase())
}

impl Keymap {
    /// Registry mnemonics only.
    pub fn from_registry(reg: &Registry) -> Self {
        Self::with_overrides(reg, "")
    }

    /// Registry mnemonics with the user overrides applied; every problem is in `conflicts`.
    pub fn with_overrides(reg: &Registry, user: &str) -> Self {
        let mut want: Vec<(String, String)> = reg
            .all()
            .iter()
            .filter_map(|c| Some((c.id.clone(), c.mnemonic.clone()?)))
            .collect();
        let mut km = Keymap::default();
        if !user.trim().is_empty() {
            match serde_json::from_str::<BTreeMap<String, String>>(user) {
                Ok(mut ov) => {
                    crate::commands::migrate_ids(&mut ov);
                    for (cmd, seq) in ov {
                        if reg.get(&cmd).is_none() {
                            km.conflicts.push(SeqConflict::UnknownCommand(cmd));
                            continue;
                        }
                        let seq = seq.trim().to_ascii_uppercase();
                        want.retain(|(c, _)| *c != cmd);
                        if !seq.is_empty() {
                            // user bindings win over defaults: put them first
                            want.insert(0, (cmd, seq));
                        }
                    }
                }
                Err(e) => km.conflicts.push(SeqConflict::BadFile(e.to_string())),
            }
        }
        for (cmd, seq) in want {
            if !valid(&seq) {
                km.conflicts.push(SeqConflict::Bad { cmd, seq });
                continue;
            }
            if let Some(kept) = km.by_seq.get(&seq) {
                km.conflicts.push(SeqConflict::Duplicate {
                    seq,
                    kept: kept.clone(),
                    dropped: cmd,
                });
                continue;
            }
            km.by_seq.insert(seq.clone(), cmd.clone());
            km.by_cmd.insert(cmd, seq);
        }
        km
    }

    /// Reads `<dir>/shortcuts.json` if present.
    pub fn load(reg: &Registry, dir: Option<&Path>) -> Self {
        let text = dir
            .map(user_file)
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        Self::with_overrides(reg, &text)
    }

    pub fn command(&self, seq: &str) -> Option<&str> {
        self.by_seq.get(seq).map(String::as_str)
    }
    /// Sequence bound to a command (palette and tooltips show it).
    pub fn seq_of(&self, cmd: &str) -> Option<&str> {
        self.by_cmd.get(cmd).map(String::as_str)
    }
    /// Some sequence starts with this letter.
    pub fn is_prefix(&self, c: char) -> bool {
        self.by_seq.keys().any(|s| s.starts_with(c))
    }
    /// Bound sequences that start with `c`, as (sequence, command), in alphabetical order.
    pub fn starting_with(&self, c: char) -> Vec<(&str, &str)> {
        let c = c.to_ascii_uppercase();
        self.by_seq
            .iter()
            .filter(|(s, _)| s.starts_with(c))
            .map(|(s, id)| (s.as_str(), id.as_str()))
            .collect()
    }
    pub fn len(&self) -> usize {
        self.by_seq.len()
    }
    pub fn is_empty(&self) -> bool {
        self.by_seq.is_empty()
    }
}

pub fn user_file(dir: &Path) -> PathBuf {
    dir.join(USER_FILE)
}

/// Codes listed in the unknown-code message.
const UNKNOWN_LIST: usize = 4;

/// Plain-words message for two letters that are no shortcut: what was typed, the codes that
/// start with the first letter and where to look the rest up.
pub fn unknown_message(cat: &crate::Catalog, km: &Keymap, code: &str) -> String {
    let code = code.to_ascii_uppercase();
    let first = code.chars().next().unwrap_or(' ');
    let near: Vec<String> = km
        .starting_with(first)
        .into_iter()
        .take(UNKNOWN_LIST)
        .map(|(s, id)| format!("{s} {}", cat.t(&format!("cmd.{id}"))))
        .collect();
    let args = [("code", code.as_str())];
    if near.is_empty() {
        return cat.tf("seq.unknown", &args);
    }
    let list = near.join(", ");
    let first = first.to_string();
    let args = [
        ("code", code.as_str()),
        ("first", first.as_str()),
        ("list", list.as_str()),
    ];
    cat.tf("seq.unknown_near", &args)
}

#[derive(Debug, PartialEq, Eq)]
pub enum SeqEvent {
    /// First letter accepted; echo it in the status bar.
    Pending(char),
    Fired(String),
    NoMatch(String),
    /// Pending letter expired.
    Timeout,
}

/// Sequence state; feed letters while the view has focus and no text field is active.
#[derive(Debug, Default)]
pub struct Seq {
    first: Option<(char, f64)>,
}

impl Seq {
    pub fn pending(&self) -> Option<char> {
        self.first.map(|f| f.0)
    }
    pub fn reset(&mut self) {
        self.first = None;
    }
    /// Call every frame: drops a first letter older than the timeout.
    pub fn tick(&mut self, now: f64) -> Option<SeqEvent> {
        match self.first {
            Some((_, t)) if now - t > SEQ_TIMEOUT => {
                self.first = None;
                Some(SeqEvent::Timeout)
            }
            _ => None,
        }
    }
    /// A letter that starts no sequence returns `None` and stays with the active tool.
    pub fn feed(&mut self, km: &Keymap, c: char, now: f64) -> Option<SeqEvent> {
        let c = c.to_ascii_uppercase();
        if !c.is_ascii_uppercase() {
            self.first = None;
            return None;
        }
        self.tick(now);
        if let Some((a, _)) = self.first.take() {
            let s: String = [a, c].iter().collect();
            return Some(match km.command(&s) {
                Some(id) => SeqEvent::Fired(id.to_string()),
                None => SeqEvent::NoMatch(s),
            });
        }
        if !km.is_prefix(c) {
            return None;
        }
        self.first = Some((c, now));
        Some(SeqEvent::Pending(c))
    }
    pub fn key(&mut self, km: &Keymap, k: egui::Key, now: f64) -> Option<SeqEvent> {
        self.feed(km, crate::commands::physical_letter(k)?, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_keymap_clean_and_spec_sequences() {
        let r = Registry::builtin();
        let km = Keymap::from_registry(&r);
        assert_eq!(km.conflicts, vec![]);
        for (seq, cmd) in [
            ("WA", "arch.wall"),
            ("CL", "arch.column"),
            ("RF", "arch.roof"),
            ("CG", "arch.ceiling"),
            ("ST", "arch.stair"),
            ("BM", "str.beam"),
            ("GR", "arch.grid"),
            ("DT", "mep.duct"),
            ("PI", "mep.pipe"),
            ("MV", "mod.move"),
            ("RO", "mod.rotate"),
            ("MM", "mod.mirror"),
            ("TR", "mod.trim"),
            ("DE", "mod.delete"),
            ("HI", "mod.isolate"),
            ("VV", "view.filters"),
            ("UN", "mgr.units"),
            ("WT", "view.tile"),
        ] {
            assert_eq!(km.command(seq), Some(cmd), "{seq}");
        }
    }

    #[test]
    fn unknown_code_message_names_the_code_and_close_ones() {
        let r = Registry::builtin();
        let km = Keymap::from_registry(&r);
        assert!(km.starting_with('w').iter().any(|(s, _)| *s == "WA"));
        for lang in [crate::Lang::En, crate::Lang::Ru] {
            let c = crate::Catalog::new(lang);
            let m = unknown_message(&c, &km, "wq");
            assert!(m.contains("WQ") && m.contains("WA"), "{m}");
            assert!(!m.contains('{'), "{m}");
            // no sequence starts with Y: the short form
            let m = unknown_message(&c, &km, "YY");
            assert!(m.contains("YY") && !m.contains('{'), "{m}");
        }
    }

    #[test]
    fn sequence_timeout_and_passthrough() {
        let r = Registry::builtin();
        let km = Keymap::from_registry(&r);
        let mut s = Seq::default();
        assert_eq!(s.feed(&km, 'm', 0.0), Some(SeqEvent::Pending('M')));
        assert_eq!(
            s.feed(&km, 'v', 0.5),
            Some(SeqEvent::Fired("mod.move".into()))
        );
        s.feed(&km, 'M', 0.0);
        assert_eq!(s.tick(0.79), None);
        assert_eq!(s.tick(0.81), Some(SeqEvent::Timeout));
        assert_eq!(s.pending(), None);
        // a late second letter starts over
        s.feed(&km, 'M', 0.0);
        assert_eq!(s.feed(&km, 'M', 1.0), Some(SeqEvent::Pending('M')));
        assert_eq!(s.feed(&km, 'Q', 1.1), Some(SeqEvent::NoMatch("MQ".into())));
        // no sequence starts with Y: the key stays with the tool
        assert_eq!(s.feed(&km, 'Y', 2.0), None);
        assert_eq!(s.key(&km, egui::Key::Num1, 2.0), None);
    }

    #[test]
    fn user_overrides_and_conflicts() {
        let r = Registry::builtin();
        let km = Keymap::with_overrides(
            &r,
            r#"{"mod.move":"QQ","mod.rotate":"","nope.cmd":"ZZ","mod.copy":"WA","arch.door":"1x"}"#,
        );
        assert_eq!(km.command("QQ"), Some("mod.move"));
        assert_eq!(km.command("MV"), None);
        assert_eq!(km.seq_of("mod.rotate"), None);
        // the user binding takes WA; the default owner loses it and is reported
        assert_eq!(km.command("WA"), Some("mod.copy"));
        assert!(km.conflicts.contains(&SeqConflict::Duplicate {
            seq: "WA".into(),
            kept: "mod.copy".into(),
            dropped: "arch.wall".into()
        }));
        assert!(km
            .conflicts
            .contains(&SeqConflict::UnknownCommand("nope.cmd".into())));
        assert!(km
            .conflicts
            .iter()
            .any(|c| matches!(c, SeqConflict::Bad { cmd, .. } if cmd == "arch.door")));
        let bad = Keymap::with_overrides(&r, "[1,2]");
        assert!(matches!(bad.conflicts[0], SeqConflict::BadFile(_)));
        assert!(bad.len() > 100);
        let dir = std::env::temp_dir().join(format!("ddc-sc2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(user_file(&dir), r#"{"mod.move":"QQ"}"#).unwrap();
        assert_eq!(Keymap::load(&r, Some(&dir)).command("QQ"), Some("mod.move"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
