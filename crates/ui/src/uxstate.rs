// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Productivity state kept between runs (`ux.json` in the settings folder): command usage,
//! tips done, HUD / recent bar switches and, per project, last-used values per category
//! («smart defaults») and saved selection sets.
use crate::frecency::Usage;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const FILE: &str = "ux.json";
/// Projects remembered; the least recently used go first.
pub const PROJECTS_MAX: usize = 50;

/// A saved selection: level and items as (kind, id, sub-index).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedSet {
    pub name: String,
    #[serde(default)]
    pub level: i64,
    #[serde(default)]
    pub items: Vec<(String, i64, u32)>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectUx {
    /// Category (`wall`, `door`, `tool.column`, …) → field → value as text.
    #[serde(default)]
    pub defaults: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default)]
    pub sets: Vec<SavedSet>,
    /// Unix seconds of the last use (pruning).
    #[serde(default)]
    pub used: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UxState {
    #[serde(default)]
    pub usage: Usage,
    #[serde(default)]
    pub tips_done: bool,
    #[serde(default)]
    pub hud_off: bool,
    #[serde(default)]
    pub bar_off: bool,
    /// Tips at the right moment: which were shown and the global switch.
    #[serde(default)]
    pub ctx_tips: crate::ctx_tips::CtxTips,
    /// Project key (model file path; `""` — unsaved) → its values.
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectUx>,
    /// Key of the open project: `get`/`set` work on its values.
    #[serde(skip)]
    pub project: String,
}

/// Write via a temporary file and rename: a crash never leaves a half-written file.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let tmp = path.with_file_name(name);
    let res = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// Keep an unreadable settings file as `<name>.bad` (it is not overwritten by the next save).
pub fn keep_bad(path: &Path) -> Option<PathBuf> {
    let mut name = path.file_name()?.to_os_string();
    name.push(".bad");
    let bad = path.with_file_name(name);
    std::fs::rename(path, &bad).ok().map(|_| bad)
}

impl UxState {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
    pub fn from_json(s: &str) -> Result<UxState, String> {
        let mut u: UxState = serde_json::from_str(s).map_err(|e| e.to_string())?;
        u.usage.migrate_ids();
        Ok(u)
    }
    /// Saved state; a corrupt file is kept as `ux.json.bad` (second value) and defaults are used.
    pub fn load(dir: Option<&Path>) -> (UxState, Option<PathBuf>) {
        let Some(path) = dir.map(|d| d.join(FILE)) else {
            return (UxState::default(), None);
        };
        let Ok(t) = std::fs::read_to_string(&path) else {
            return (UxState::default(), None);
        };
        match UxState::from_json(&t) {
            Ok(s) => (s, None),
            Err(_) => (UxState::default(), keep_bad(&path)),
        }
    }
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        write_atomic(&dir.join(FILE), &self.to_json())
    }
    /// Switch to project `key`; the oldest projects beyond [`PROJECTS_MAX`] are dropped.
    pub fn open_project(&mut self, key: &str, now: f64) {
        self.project = key.to_string();
        self.projects.entry(key.to_string()).or_default().used = now;
        while self.projects.len() > PROJECTS_MAX {
            let old = self
                .projects
                .iter()
                .filter(|(k, _)| *k != key)
                .min_by(|a, b| a.1.used.total_cmp(&b.1.used))
                .map(|(k, _)| k.clone());
            match old {
                Some(k) => self.projects.remove(&k),
                None => break,
            };
        }
    }
    pub fn here(&self) -> Option<&ProjectUx> {
        self.projects.get(&self.project)
    }
    pub fn here_mut(&mut self) -> &mut ProjectUx {
        self.projects.entry(self.project.clone()).or_default()
    }
    pub fn get(&self, cat: &str, field: &str) -> Option<&str> {
        self.here()?
            .defaults
            .get(cat)?
            .get(field)
            .map(String::as_str)
    }
    pub fn get_f64(&self, cat: &str, field: &str) -> Option<f64> {
        self.get(cat, field)?
            .parse()
            .ok()
            .filter(|v: &f64| v.is_finite())
    }
    /// True if the value changed.
    pub fn set(&mut self, cat: &str, field: &str, v: impl ToString) -> bool {
        let v = v.to_string();
        let m = self.here_mut().defaults.entry(cat.to_string()).or_default();
        if m.get(field) == Some(&v) {
            return false;
        }
        m.insert(field.to_string(), v);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_defaults() {
        let mut s = UxState::default();
        s.open_project("a.ddc", 1.0);
        assert!(s.set("wall", "height", 2.8));
        assert!(!s.set("wall", "height", 2.8));
        s.usage.note("arch.wall", 1.0);
        s.tips_done = true;
        let mut b = UxState::from_json(&s.to_json()).unwrap();
        assert_eq!(b.get_f64("wall", "height"), None, "no project open yet");
        b.open_project("a.ddc", 2.0);
        assert_eq!(b.get_f64("wall", "height"), Some(2.8));
        assert_eq!(b.get("door", "width"), None);
        assert!(b.tips_done && !b.hud_off);
        assert_eq!(b.usage.order, vec!["arch.wall".to_string()]);
        // another project has its own values
        b.open_project("b.ddc", 3.0);
        assert_eq!(b.get_f64("wall", "height"), None);
        assert!(UxState::from_json("nope").is_err());
        let dir = std::env::temp_dir().join(format!("ddc-ux-{}", std::process::id()));
        b.save(&dir).unwrap();
        assert!(!dir.join("ux.json.tmp").exists());
        let (l, bad) = UxState::load(Some(&dir));
        assert!(l.tips_done && bad.is_none());
        // a corrupt file is kept aside, not silently replaced
        std::fs::write(dir.join(FILE), "{broken").unwrap();
        let (l, bad) = UxState::load(Some(&dir));
        assert!(!l.tips_done);
        let bad = bad.expect("backup");
        assert_eq!(std::fs::read_to_string(&bad).unwrap(), "{broken");
        assert!(!dir.join(FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn projects_are_pruned_oldest_first() {
        let mut s = UxState::default();
        for i in 0..PROJECTS_MAX + 3 {
            s.open_project(&format!("p{i}"), i as f64);
        }
        assert_eq!(s.projects.len(), PROJECTS_MAX);
        assert!(!s.projects.contains_key("p0") && s.projects.contains_key("p52"));
        assert_eq!(s.project, format!("p{}", PROJECTS_MAX + 2));
    }
}
