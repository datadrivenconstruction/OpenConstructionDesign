// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Command registry: `data/commands.json` plus fragments `data/commands.d/*.json`
//! (same schema, merged in file-name order), shortcuts, two-letter mnemonic input by key.
use serde::Deserialize;
use std::collections::BTreeMap;

pub const COMMANDS_JSON: &str = include_str!("../data/commands.json");
/// `command id -> icon` for commands whose fragment leaves `icon` empty: icons are bound
/// here without editing the command fragment itself. An explicit `icon` in a fragment wins.
pub const COMMAND_ICONS_JSON: &str = include_str!("../data/command_icons.json");

pub fn command_icons() -> &'static BTreeMap<String, String> {
    static M: std::sync::OnceLock<BTreeMap<String, String>> = std::sync::OnceLock::new();
    M.get_or_init(|| serde_json::from_str(COMMAND_ICONS_JSON).expect("data/command_icons.json"))
}
use crate::fragments::COMMAND_FRAGMENTS;
/// Mnemonic sequence timeout, seconds (SHELL_AND_COMMANDS §4).
pub const MNEMONIC_TIMEOUT: f64 = 1.5;

/// Group order of the Modify tab. The contextual tab repeats it, so a group never moves when a
/// selection swaps the tabs; groups not listed follow in registry order.
pub const MODIFY_GROUPS: [&str; 5] = ["modify", "visib", "clip", "geometry", "measure"];

/// Position of a command's ribbon group in [`MODIFY_GROUPS`]; any other group ranks after them.
pub fn modify_rank(c: &Command) -> usize {
    let g = crate::widgets::ribbon::group_name(c);
    MODIFY_GROUPS
        .iter()
        .position(|m| *m == g)
        .unwrap_or(MODIFY_GROUPS.len())
}

#[derive(Debug, Clone, Deserialize)]
pub struct Command {
    pub id: String,
    /// Icon file name in docs/design/icons_v4, embedded and unique per tool (tests in `icons`);
    /// `""` = a panel or palette command with no glyph (docs/design/ICONS_V4.md).
    pub icon: String,
    #[serde(default)]
    pub mnemonic: Option<String>,
    #[serde(default)]
    pub shortcuts: Vec<String>,
    /// Ribbon tab id (`arch`, `struct`, …); `None` for universal commands.
    #[serde(default)]
    pub tab: Option<String>,
    /// R ribbon, C context bar, M radial, A app bar, D browser, … (P palette is implicit).
    #[serde(default)]
    pub surfaces: Vec<String>,
    #[serde(default)]
    pub stage: String,
    /// Ribbon panel (caption `rgroup.<g>`); `None` — derived from the id.
    #[serde(default)]
    pub group: Option<String>,
    /// Runs as another command (same tool shown on two tabs).
    #[serde(default)]
    pub alias: Option<String>,
    /// Selection categories that show it on the contextual «Modify» tab (`*` — any).
    #[serde(default)]
    pub context: Vec<String>,
    /// Enter / «Repeat last» may run it again: drawing and modify tools only, never delete,
    /// file, dialogs or toggles.
    #[serde(default)]
    pub repeatable: bool,
    /// Ribbon button size; `None` is medium (see `widgets::ribbon_layout`).
    #[serde(default)]
    pub size: Option<crate::widgets::ribbon_layout::Size>,
    /// Ribbon collapse priority, 1 keeps its size longest, 9 shrinks first; `None` by size.
    #[serde(default)]
    pub priority: Option<u8>,
    /// Split button: commands offered under its caret; they get no button of their own.
    #[serde(default)]
    pub variants: Vec<String>,
    /// Removes something, resets, or acts on everything at once: picked from a split caret it
    /// runs once and the main part goes back to the parent, so a click never repeats it.
    #[serde(default)]
    pub once: bool,
    /// Drawn as its group's settings launcher next to the group title, not as a button.
    #[serde(default)]
    pub launcher: bool,
    /// Keys a context menu shows for this command; the owning panel handles them, so they are
    /// not global shortcuts (no conflicts, no dispatch).
    #[serde(default)]
    pub context_keys: Vec<String>,
    /// Its shortcut is held, not pressed: listed and conflict-checked like any binding, but a
    /// key press never runs the command (the canvas reads the held key itself).
    #[serde(default)]
    pub hold: bool,
    /// Opens a window or file dialog that still needs input before anything happens: its label
    /// ends with «…» (checked by `tests/wording_lint.rs`). Tools, toggles, panels and direct
    /// actions are not dialogs.
    #[serde(default)]
    pub dialog: bool,
}

impl Command {
    pub fn label_key(&self) -> String {
        format!("cmd.{}", self.id)
    }
    pub fn on(&self, surface: &str) -> bool {
        self.surfaces.iter().any(|s| s == surface)
    }
    pub fn parsed_shortcuts(&self) -> Vec<Shortcut> {
        self.shortcuts
            .iter()
            .filter_map(|s| Shortcut::parse(s))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Shortcut {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: egui::Key,
}

impl Shortcut {
    /// `Ctrl+Shift+A`, `Del`, `F11`, `Ctrl+,`, `Ctrl+Tab`.
    pub fn parse(s: &str) -> Option<Self> {
        let mut sc = Shortcut {
            ctrl: false,
            shift: false,
            alt: false,
            key: egui::Key::Escape,
        };
        let parts: Vec<&str> = s.split('+').collect();
        let (last, mods) = parts.split_last()?;
        for m in mods {
            match *m {
                "Ctrl" => sc.ctrl = true,
                "Shift" => sc.shift = true,
                "Alt" => sc.alt = true,
                _ => return None,
            }
        }
        let name = match *last {
            "Del" => "Delete",
            "," => "Comma",
            k => k,
        };
        sc.key = egui::Key::from_name(name)?;
        Some(sc)
    }
    pub fn matches(&self, key: egui::Key, m: egui::Modifiers) -> bool {
        self.key == key && self.ctrl == m.command && self.shift == m.shift && self.alt == m.alt
    }
    /// Combos the OS or window manager owns; the app must never bind them.
    pub fn os_reserved(&self) -> bool {
        use egui::Key::*;
        matches!(
            (self.ctrl, self.shift, self.alt, self.key),
            (false, false, true, F4) | (false, _, true, Tab) | (true, false, true, Delete)
        ) || (self.ctrl && self.shift && self.key == Escape)
    }
}

impl std::fmt::Display for Shortcut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.ctrl {
            f.write_str("Ctrl+")?;
        }
        if self.shift {
            f.write_str("Shift+")?;
        }
        if self.alt {
            f.write_str("Alt+")?;
        }
        f.write_str(self.key.symbol_or_name())
    }
}

#[derive(Debug, PartialEq)]
pub enum Conflict {
    DuplicateId(String),
    DuplicateMnemonic(String, String, String),
    BadMnemonic(String, String),
    DuplicateShortcut(String, String, String),
    BadShortcut(String, String),
    OsReserved(String, String),
}

pub struct Registry {
    cmds: Vec<Command>,
    by_id: BTreeMap<String, usize>,
}

impl Registry {
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        Self::from_sources(&[s])
    }
    /// Concatenates command arrays; a duplicate id is reported by [`Registry::conflicts`].
    pub fn from_sources(srcs: &[&str]) -> Result<Self, serde_json::Error> {
        let mut cmds: Vec<Command> = Vec::new();
        for s in srcs {
            cmds.extend(serde_json::from_str::<Vec<Command>>(s)?);
        }
        let by_id = cmds
            .iter()
            .enumerate()
            .rev()
            .map(|(i, c)| (c.id.clone(), i))
            .collect();
        Ok(Self { cmds, by_id })
    }
    pub fn builtin() -> Self {
        let mut srcs = vec![COMMANDS_JSON];
        srcs.extend(COMMAND_FRAGMENTS.iter().map(|f| f.1));
        let mut r =
            Self::from_sources(&srcs).expect("commands.json and fragments are validated by tests");
        for c in &mut r.cmds {
            if c.icon.is_empty() {
                if let Some(i) = command_icons().get(c.id.as_str()) {
                    c.icon = i.clone();
                }
            }
        }
        r
    }
    /// Command a click on `id` actually runs (follows `alias`).
    pub fn resolve<'a>(&'a self, id: &'a str) -> &'a str {
        match self.get(id).and_then(|c| c.alias.as_deref()) {
            Some(a) if self.get(a).is_some() => a,
            _ => id,
        }
    }
    /// Contextual-tab commands for a selection category: the groups of the Modify tab in the
    /// same order ([`MODIFY_GROUPS`]), then the category's own groups on the right.
    pub fn contextual(&self, category: &str) -> Vec<&Command> {
        let mut v: Vec<&Command> = self
            .cmds
            .iter()
            .filter(|c| c.on("R") && c.context.iter().any(|k| k == "*" || k == category))
            .collect();
        v.sort_by_key(|c| modify_rank(c));
        v
    }
    pub fn all(&self) -> &[Command] {
        &self.cmds
    }
    pub fn get(&self, id: &str) -> Option<&Command> {
        self.by_id.get(id).map(|&i| &self.cmds[i])
    }
    /// Key hint of a menu row: the first global shortcut, else the first context key, else "".
    pub fn menu_keys(&self, id: &str) -> String {
        self.get(id)
            .and_then(|c| c.shortcuts.first().or(c.context_keys.first()))
            .cloned()
            .unwrap_or_default()
    }
    pub fn by_mnemonic(&self, m: &str) -> Option<&Command> {
        self.cmds.iter().find(|c| c.mnemonic.as_deref() == Some(m))
    }
    pub fn by_shortcut(&self, key: egui::Key, m: egui::Modifiers) -> Option<&Command> {
        self.cmds
            .iter()
            .find(|c| !c.hold && c.parsed_shortcuts().iter().any(|s| s.matches(key, m)))
    }
    /// Ribbon tabs in document order.
    pub fn tabs(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for t in self.cmds.iter().filter_map(|c| c.tab.clone()) {
            if !v.contains(&t) {
                v.push(t);
            }
        }
        v
    }

    pub fn conflicts(&self) -> Vec<Conflict> {
        let mut out = Vec::new();
        let mut ids = BTreeMap::new();
        let mut mn: BTreeMap<String, String> = BTreeMap::new();
        let mut sc: BTreeMap<Shortcut, String> = BTreeMap::new();
        for c in &self.cmds {
            if ids.insert(c.id.clone(), ()).is_some() {
                out.push(Conflict::DuplicateId(c.id.clone()));
            }
            if let Some(m) = &c.mnemonic {
                if m.len() != 2 || !m.bytes().all(|b| b.is_ascii_uppercase()) {
                    out.push(Conflict::BadMnemonic(c.id.clone(), m.clone()));
                }
                if let Some(prev) = mn.insert(m.clone(), c.id.clone()) {
                    out.push(Conflict::DuplicateMnemonic(m.clone(), prev, c.id.clone()));
                }
            }
            for s in &c.shortcuts {
                let Some(p) = Shortcut::parse(s) else {
                    out.push(Conflict::BadShortcut(c.id.clone(), s.clone()));
                    continue;
                };
                if p.os_reserved() {
                    out.push(Conflict::OsReserved(c.id.clone(), s.clone()));
                }
                if let Some(prev) = sc.insert(p, c.id.clone()) {
                    out.push(Conflict::DuplicateShortcut(s.clone(), prev, c.id.clone()));
                }
            }
        }
        out
    }
}

pub const RENAMES_JSON: &str = include_str!("../data/command_renames.json");

/// Old command id → current id (`data/command_renames.json`; rows are never removed).
pub fn renames() -> &'static BTreeMap<String, String> {
    #[derive(Deserialize)]
    struct File {
        renames: BTreeMap<String, String>,
    }
    static R: std::sync::OnceLock<BTreeMap<String, String>> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        serde_json::from_str::<File>(RENAMES_JSON)
            .expect("data/command_renames.json")
            .renames
    })
}

/// Current id of a command saved under an older id (follows chains); other ids unchanged.
pub fn current_id(id: &str) -> &str {
    let mut id = id;
    for _ in 0..=renames().len() {
        match renames().get(id) {
            Some(n) => id = n.as_str(),
            None => break,
        }
    }
    id
}

/// Moves entries saved under old command ids to the current ids; an entry already saved
/// under the current id wins (it is newer).
pub fn migrate_ids<V>(m: &mut BTreeMap<String, V>) {
    let old: Vec<String> = m
        .keys()
        .filter(|k| current_id(k) != k.as_str())
        .cloned()
        .collect();
    for k in old {
        if let Some(v) = m.remove(&k) {
            m.entry(current_id(&k).to_string()).or_insert(v);
        }
    }
}

/// QWERTY keys in the order of every `data/lang/<code>/layout_keys.txt`.
const QWERTY: &str = "qwertyuiop[]asdfghjkl;'zxcvbnm,.`";

/// Letters of the non-Latin keyboard layout for the UI language, else of the first shipped one.
fn layout_keys() -> &'static str {
    let lang = crate::i18n::current();
    let layouts = crate::fragments::KEY_LAYOUTS;
    lang.chain()
        .find_map(|l| layouts.iter().find(|(c, _)| *c == l.code()))
        .or(layouts.first())
        .map_or("", |(_, k)| k.trim())
}

/// Letter of any shipped non-Latin layout → QWERTY by key position, for text typed in the
/// command line / palette.
pub fn cyr_to_latin(c: char) -> Option<char> {
    let lc = c.to_lowercase().next()?;
    crate::fragments::KEY_LAYOUTS
        .iter()
        .find_map(|(_, k)| k.trim().chars().position(|r| r == lc))
        .and_then(|i| QWERTY.chars().nth(i))
}
/// QWERTY → the same key in the layout of [`layout_keys`].
pub fn latin_to_cyr(c: char) -> Option<char> {
    let lc = c.to_ascii_lowercase();
    let i = QWERTY.chars().position(|r| r == lc)?;
    layout_keys().chars().nth(i)
}

/// Letter of a physical key (A–Z), independent of the active keyboard layout.
pub fn physical_letter(k: egui::Key) -> Option<char> {
    let n = k.name();
    let mut it = n.chars();
    match (it.next(), it.next()) {
        (Some(c), None) if c.is_ascii_uppercase() => Some(c),
        _ => None,
    }
}

#[derive(Debug, PartialEq)]
pub enum MnemonicEvent {
    /// First letter typed; echo it.
    Pending(String),
    Fired(String),
    NoMatch(String),
}

/// Two-letter mnemonic state machine; feed it physical keys while the view has focus.
#[derive(Default)]
pub struct MnemonicInput {
    first: Option<(char, f64)>,
}

impl MnemonicInput {
    pub fn pending(&self) -> Option<char> {
        self.first.map(|f| f.0)
    }
    pub fn reset(&mut self) {
        self.first = None;
    }
    pub fn key(&mut self, reg: &Registry, k: egui::Key, now: f64) -> Option<MnemonicEvent> {
        self.letter(reg, physical_letter(k)?, now)
    }
    pub fn letter(&mut self, reg: &Registry, c: char, now: f64) -> Option<MnemonicEvent> {
        let c = c.to_ascii_uppercase();
        if !c.is_ascii_uppercase() {
            return None;
        }
        match self.first.take() {
            Some((a, t)) if now - t <= MNEMONIC_TIMEOUT => {
                let m: String = [a, c].iter().collect();
                Some(match reg.by_mnemonic(&m) {
                    Some(cmd) => MnemonicEvent::Fired(cmd.id.clone()),
                    None => MnemonicEvent::NoMatch(m),
                })
            }
            _ => {
                self.first = Some((c, now));
                Some(MnemonicEvent::Pending(c.to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renamed_ids_lead_to_live_commands() {
        let r = Registry::builtin();
        assert!(!renames().is_empty());
        for (old, new) in renames() {
            assert!(r.get(old).is_none(), "{old}: an old id must not be reused");
            assert!(
                r.get(current_id(old)).is_some(),
                "{old} -> {new}: no such command"
            );
        }
        assert_eq!(current_id("xs50.undo_apply"), "xlsx.undo_apply");
        assert_eq!(current_id("edit.undo"), "edit.undo");
        let mut m: BTreeMap<String, u8> = BTreeMap::new();
        m.insert("xs57.undo_import".into(), 1);
        m.insert("edit.undo".into(), 2);
        migrate_ids(&mut m);
        assert_eq!(m.get("keys.undo_import"), Some(&1));
        assert!(!m.contains_key("xs57.undo_import"));
        m.insert("xs57.undo_import".into(), 3);
        migrate_ids(&mut m);
        assert_eq!(
            m.get("keys.undo_import"),
            Some(&1),
            "the entry under the new id wins"
        );
    }

    #[test]
    fn menu_keys_prefer_shortcut_then_context_key() {
        let r = Registry::from_json(
            r#"[{"id":"a","icon":"","shortcuts":["Ctrl+K"],"context_keys":["F2"]},
                {"id":"b","icon":"","context_keys":["F2"]},
                {"id":"c","icon":""}]"#,
        )
        .unwrap();
        assert_eq!(r.menu_keys("a"), "Ctrl+K");
        assert_eq!(r.menu_keys("b"), "F2");
        assert_eq!(r.menu_keys("c"), "");
        assert_eq!(r.menu_keys("none"), "");
    }

    #[test]
    fn keymap_unique() {
        let r = Registry::builtin();
        assert!(r.all().len() > 140);
        assert_eq!(r.conflicts(), vec![]);
        assert_eq!(r.get("app.palette").unwrap().shortcuts[0], "Ctrl+K");
    }

    /// The round trip starts with the export: it comes first on the Data tab and owns Ctrl+E.
    #[test]
    fn data_tab_export_precedes_import() {
        let r = Registry::builtin();
        let pos = |id: &str| {
            r.all()
                .iter()
                .filter(|c| c.tab.as_deref() == Some("data") && c.on("R"))
                .position(|c| c.id == id)
                .unwrap_or_else(|| panic!("{id} not on the Data tab"))
        };
        assert!(pos("data.export_xlsx") < pos("data.import_xlsx"));
        let e = r.get("data.export_xlsx").unwrap();
        assert_eq!(e.shortcuts, vec!["Ctrl+E".to_string()]);
        let owners: Vec<&str> = r
            .all()
            .iter()
            .filter(|c| c.shortcuts.iter().any(|s| s == "Ctrl+E"))
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(owners, vec!["data.export_xlsx"]);
    }

    #[test]
    fn conflicts_detected() {
        let j = r#"[{"id":"a","icon":"","mnemonic":"WA","shortcuts":["Ctrl+K"],"tab":null,"surfaces":[],"stage":"M1"},
                   {"id":"b","icon":"","mnemonic":"WA","shortcuts":["Ctrl+K","Alt+F4","Ctrl+Nope"],"tab":null,"surfaces":[],"stage":"M1"},
                   {"id":"b","icon":"","mnemonic":"W","shortcuts":[],"tab":null,"surfaces":[],"stage":"M1"}]"#;
        let c = Registry::from_json(j).unwrap().conflicts();
        assert_eq!(c.len(), 6, "{c:?}");
    }

    #[test]
    fn mnemonic_by_physical_key() {
        let r = Registry::builtin();
        let mut m = MnemonicInput::default();
        use egui::Key;
        assert_eq!(
            m.key(&r, Key::W, 0.0),
            Some(MnemonicEvent::Pending("W".into()))
        );
        assert_eq!(
            m.key(&r, Key::A, 1.0),
            Some(MnemonicEvent::Fired("arch.wall".into()))
        );
        // Timeout restarts the sequence.
        m.key(&r, Key::W, 0.0);
        assert_eq!(
            m.key(&r, Key::A, 2.0),
            Some(MnemonicEvent::Pending("A".into()))
        );
        m.reset();
        // Russian layout text: the letters on the W and A keys (upper and lower case).
        let ru: Vec<char> = crate::test_fx::fx("w_a_keys").chars().collect();
        let (a, b) = (cyr_to_latin(ru[0]).unwrap(), cyr_to_latin(ru[1]).unwrap());
        m.letter(&r, a, 0.0);
        assert_eq!(
            m.letter(&r, b, 0.5),
            Some(MnemonicEvent::Fired("arch.wall".into()))
        );
        assert_eq!(m.key(&r, Key::Num1, 0.0), None);
        assert_eq!(latin_to_cyr('w'), ru[0].to_lowercase().next());
    }

    #[test]
    fn shortcut_lookup() {
        let r = Registry::builtin();
        let ctrl = egui::Modifiers::COMMAND;
        assert_eq!(r.by_shortcut(egui::Key::K, ctrl).unwrap().id, "app.palette");
        let cs = egui::Modifiers {
            shift: true,
            ..ctrl
        };
        assert_eq!(r.by_shortcut(egui::Key::Z, cs).unwrap().id, "edit.redo");
        assert_eq!(Shortcut::parse("Ctrl+,").unwrap().key, egui::Key::Comma);
        assert_eq!(r.tabs().len(), 9);
    }

    #[test]
    fn default_bindings_are_unique_and_f9_is_the_held_axes_key() {
        let r = Registry::builtin();
        assert_eq!(r.conflicts(), vec![]);
        let mut seen = std::collections::BTreeMap::new();
        for c in r.all() {
            for s in &c.shortcuts {
                let p = Shortcut::parse(s).unwrap_or_else(|| panic!("{} {s}", c.id));
                let prev = seen.insert(p, c.id.clone());
                assert!(prev.is_none(), "{s}: {prev:?} and {}", c.id);
            }
        }
        let b = crate::keymap::Bindings::build(&r, &crate::keymap::UserKeys::default());
        assert!(b.conflicts.is_empty(), "{:?}", b.conflicts);
        let f9 = Shortcut::parse("F9").unwrap();
        assert_eq!(seen.get(&f9).map(String::as_str), Some("view.axes_full"));
        let c = r.get("view.axes_full").unwrap();
        assert!(c.hold && c.on("P"));
        assert!(r.by_shortcut(egui::Key::F9, egui::Modifiers::NONE).is_none());
    }

    #[test]
    fn fragments_merge_and_aliases() {
        for (name, src) in COMMAND_FRAGMENTS {
            serde_json::from_str::<Vec<Command>>(src).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
        let r = Registry::builtin();
        assert_eq!(r.resolve("str.level"), "arch.level");
        assert_eq!(r.resolve("arch.wall"), "arch.wall");
        for c in r.all().iter().filter_map(|c| c.alias.as_deref()) {
            assert!(r.get(c).is_some(), "alias to unknown {c}");
        }
        let w: Vec<_> = r.contextual("wall").iter().map(|c| c.id.as_str()).collect();
        assert!(
            w.contains(&"mod.move") && w.contains(&"ctx.wall.profile"),
            "{w:?}"
        );
        assert_eq!(r.contextual("wall")[0].group.as_deref(), Some("modify"));
        // the contextual tab repeats the Modify tab's groups in its order, its own on the right
        let groups = |v: Vec<&Command>| {
            let mut g: Vec<String> = vec![];
            for c in v {
                let n = crate::widgets::ribbon::group_name(c).to_string();
                if !g.contains(&n) {
                    g.push(n);
                }
            }
            g
        };
        let mut modify: Vec<&Command> = r
            .all()
            .iter()
            .filter(|c| c.tab.as_deref() == Some("modify") && c.on("R"))
            .collect();
        modify.sort_by_key(|c| modify_rank(c));
        let (m, w) = (groups(modify), groups(r.contextual("wall")));
        let shared: Vec<&String> = w.iter().filter(|g| m.contains(g)).collect();
        let in_modify: Vec<&String> = m.iter().filter(|g| w.contains(g)).collect();
        assert_eq!(shared, in_modify, "modify {m:?} vs contextual {w:?}");
        let first_own = w.iter().position(|g| !m.contains(g)).unwrap_or(w.len());
        assert!(w[first_own..].iter().all(|g| !m.contains(g)), "{w:?}");
        // Demolish — on the contextual tab of building categories, not rooms
        assert!(r.contextual("door").iter().any(|c| c.id == "mod.demolish"));
        assert!(!r.contextual("room").iter().any(|c| c.id == "mod.demolish"));
        assert!(!r
            .contextual("duct")
            .iter()
            .any(|c| c.id == "ctx.wall.profile"));
        let two = Registry::from_sources(&[
            r#"[{"id":"x","icon":""}]"#,
            r#"[{"id":"y","icon":"","tab":"arch","surfaces":["R"]}]"#,
        ])
        .unwrap();
        assert_eq!(two.all().len(), 2);
        assert_eq!(two.get("y").unwrap().tab.as_deref(), Some("arch"));
    }

    /// TOOLS_UI §6: every tab carries its listed panels.
    #[test]
    fn ribbon_tabs_full() {
        let r = Registry::builtin();
        let groups = |tab: &str| {
            let mut v: Vec<String> = vec![];
            for c in r
                .all()
                .iter()
                .filter(|c| c.tab.as_deref() == Some(tab) && c.on("R"))
            {
                let g = crate::widgets::ribbon::group_name(c).to_string();
                if !v.contains(&g) {
                    v.push(g);
                }
            }
            v
        };
        for (tab, want) in [
            ("arch", &["build", "circ", "opening", "rooms", "datum"][..]),
            ("struct", &["struct", "grids", "datum", "str_space"]),
            ("systems", &["hvac", "plumb", "elec", "systems"]),
            ("annotate", &["dims", "text", "symbols"]),
            (
                "modify",
                &["clip", "geometry", "modify", "visib", "measure"],
            ),
            ("view", &["graphics", "create", "windows"]),
            ("manage", &["settings", "project", "components", "file"]),
            ("data", &["exchange", "check"]),
            ("mass", &["mass", "mass_edit", "mass_join", "mass_face", "mass_free"]),
        ] {
            let got = groups(tab);
            for g in want {
                assert!(got.iter().any(|x| x == g), "{tab}: {g} missing in {got:?}");
            }
        }
    }

    #[test]
    fn the_assistant_leads_the_first_tab_and_keeps_its_key() {
        let r = Registry::builtin();
        let first_tab = r.tabs().first().cloned();
        assert_eq!(first_tab.as_deref(), Some("arch"));
        let first = r
            .all()
            .iter()
            .find(|c| c.tab.as_deref() == Some("arch") && c.on("R"))
            .unwrap();
        assert_eq!((first.id.as_str(), first.icon.as_str()), ("ai.home", "spark"));
        assert_eq!(r.resolve("ai.home"), "ai.ask");
        assert_eq!(r.get("ai.ask").unwrap().shortcuts, ["Ctrl+J"]);
        assert_eq!(r.get("ai.panel").unwrap().icon, "spark");
    }
}
