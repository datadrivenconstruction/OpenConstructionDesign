// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Command usage («frecency»: frequency decayed by age) for palette ranking, the recent-commands
//! bar and «Repeat last». One store per UI thread; the host loads/saves it as JSON.
use crate::cmdargs::{self, Args};
use crate::i18n::{Catalog, Lang};
use crate::{fuzzy, Command, Registry};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::BTreeMap;

/// Weight halves every 3 days without use.
pub const HALF_LIFE: f64 = 3.0 * 86400.0;
/// Largest ranking bonus usage can add to a fuzzy score (one tier is ~150–200).
pub const BONUS_MAX: i32 = 160;
/// Most recent ids kept.
pub const ORDER_MAX: usize = 32;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub count: u32,
    /// Unix seconds of the last use.
    pub last: f64,
    /// Decayed use count at `last`.
    pub weight: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub cmds: BTreeMap<String, Entry>,
    /// Most recent first, unique.
    pub order: Vec<String>,
    /// Bumped on every use (hosts detect «a command ran» by comparing it).
    #[serde(skip)]
    pub seq: u64,
}

/// Commands worth recording as «the last command» (recent bar, history labels). Repeating
/// is stricter: see [`Usage::last_repeatable`].
pub fn recordable(id: &str) -> bool {
    !matches!(
        id,
        "app.palette"
            | "app.repeat"
            | "app.history"
            | "app.where_is"
            | "app.find"
            | "app.help"
            | "edit.undo"
            | "edit.redo"
            | "mgr.keymap"
            | "help.shortcuts"
            | "mgr.settings"
    ) && !id.starts_with("ui.")
        && !id.starts_with('@')
}

fn decay(dt: f64) -> f64 {
    0.5f64.powf(dt.max(0.0) / HALF_LIFE)
}

impl Usage {
    pub fn note(&mut self, id: &str, now: f64) {
        if id.is_empty() || id.starts_with('@') {
            return;
        }
        let e = self.cmds.entry(id.to_string()).or_default();
        e.weight = e.weight * decay(now - e.last) + 1.0;
        e.count += 1;
        e.last = now;
        self.order.retain(|x| x != id);
        self.order.insert(0, id.to_string());
        self.order.truncate(ORDER_MAX);
        self.seq += 1;
    }
    pub fn weight(&self, id: &str, now: f64) -> f64 {
        self.cmds
            .get(id)
            .map_or(0.0, |e| e.weight * decay(now - e.last))
    }
    /// Ranking bonus 0..BONUS_MAX, saturating with use.
    pub fn bonus(&self, id: &str, now: f64) -> i32 {
        let w = self.weight(id, now);
        (BONUS_MAX as f64 * w / (w + 2.0)).round() as i32
    }
    /// Ids by weight, heaviest first (ties: more recent first).
    pub fn top(&self, now: f64, n: usize) -> Vec<String> {
        let mut v: Vec<(&String, f64)> =
            self.cmds.keys().map(|k| (k, self.weight(k, now))).collect();
        v.sort_by(|a, b| {
            b.1.total_cmp(&a.1).then_with(|| {
                let pos = |k: &String| self.order.iter().position(|o| o == k).unwrap_or(usize::MAX);
                pos(a.0).cmp(&pos(b.0))
            })
        });
        v.into_iter().take(n).map(|x| x.0.clone()).collect()
    }
    /// Last `n` recorded commands, most recent first.
    pub fn recent(&self, n: usize) -> Vec<String> {
        self.order
            .iter()
            .filter(|i| recordable(i))
            .take(n)
            .cloned()
            .collect()
    }
    /// The last recorded command if it is flagged `repeatable` in the registry; after a
    /// delete, a file command or a toggle there is nothing to repeat.
    pub fn last_repeatable(&self, reg: &Registry) -> Option<&str> {
        self.order
            .iter()
            .find(|i| recordable(i))
            .filter(|i| reg.get(i).is_some_and(|c| c.repeatable))
            .map(String::as_str)
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
    /// Bad or empty text gives an empty store.
    pub fn from_json(s: &str) -> Usage {
        let mut u: Usage = serde_json::from_str(s).unwrap_or_default();
        u.migrate_ids();
        u
    }
    /// Usage saved under old command ids moves to the current ids.
    pub fn migrate_ids(&mut self) {
        crate::commands::migrate_ids(&mut self.cmds);
        let mut seen = std::collections::BTreeSet::new();
        self.order = std::mem::take(&mut self.order)
            .into_iter()
            .map(|id| crate::commands::current_id(&id).to_string())
            .filter(|id| seen.insert(id.clone()))
            .collect();
    }
}

thread_local! {
    static STORE: RefCell<Usage> = RefCell::new(Usage::default());
}

/// Unix time, seconds.
pub fn now() -> f64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// Record a use in this thread's store.
pub fn note(id: &str) {
    let t = now();
    STORE.with(|s| s.borrow_mut().note(id, t));
}

pub fn with<R>(f: impl FnOnce(&mut Usage) -> R) -> R {
    STORE.with(|s| f(&mut s.borrow_mut()))
}

/// Fuzzy score of one command name reading; exact mnemonic wins outright.
fn cmd_score(cmd: &Command, cat: &Catalog, head: &str) -> Option<i32> {
    let key = cmd.label_key();
    let (l, le) = (cat.t(&key), cat.t_in(Lang::En, &key));
    // the id is a fallback, as in `fuzzy::search`
    let by_id = fuzzy::best(head, &[&cmd.id]).map(|s| s - 50);
    let mut s = fuzzy::best(head, &[&l, &le]).max(by_id)?;
    if cmd
        .mnemonic
        .as_deref()
        .is_some_and(|m| m.eq_ignore_ascii_case(head.trim()))
    {
        s += 2000;
    }
    Some(s)
}

/// One palette match: command, total score, arguments left after its name.
#[derive(Debug, Clone)]
pub struct Hit<'a> {
    pub cmd: &'a Command,
    pub score: i32,
    pub args: Args,
}

/// Rank commands for a query with arguments: every reading of the query (`cmdargs::heads`) is
/// scored, the best per command kept, usage adds up to [`BONUS_MAX`]. Ties break by id.
/// Numbers in arguments use the catalog language's decimal mark ([`search_with_mark`]).
pub fn search_with<'a>(
    reg: &'a Registry,
    cat: &Catalog,
    query: &str,
    usage: &Usage,
    now: f64,
) -> Vec<Hit<'a>> {
    let mark = crate::numparse::DecimalMark::for_lang(cat.lang);
    search_with_mark(reg, cat, query, usage, now, mark)
}

/// [`search_with`] with argument numbers read in the UI decimal mark `mark`.
pub fn search_with_mark<'a>(
    reg: &'a Registry,
    cat: &Catalog,
    query: &str,
    usage: &Usage,
    now: f64,
    mark: crate::numparse::DecimalMark,
) -> Vec<Hit<'a>> {
    let heads = cmdargs::heads_in(query, mark);
    let mut v: Vec<Hit<'a>> = reg
        .all()
        .iter()
        .filter_map(|cmd| {
            let mut best: Option<(i32, Args)> = None;
            for (h, a, pen) in &heads {
                if h.is_empty() && !query.trim().is_empty() {
                    continue;
                }
                if let Some(s) = cmd_score(cmd, cat, h) {
                    let s = s - pen;
                    if best.as_ref().is_none_or(|b| s > b.0) {
                        best = Some((s, a.clone()));
                    }
                }
            }
            let (s, args) = best?;
            Some(Hit {
                cmd,
                score: s + usage.bonus(&cmd.id, now),
                args,
            })
        })
        .collect();
    v.sort_by(|a, b| b.score.cmp(&a.score).then(a.cmd.id.cmp(&b.cmd.id)));
    v
}

/// Palette ranking with this thread's usage store.
pub fn search<'a>(reg: &'a Registry, cat: &Catalog, query: &str) -> Vec<&'a Command> {
    let t = now();
    with(|u| search_with(reg, cat, query, u, t))
        .into_iter()
        .map(|h| h.cmd)
        .collect()
}

/// Arguments the query gives to command `id` (empty if it is not matched).
pub fn args_for(reg: &Registry, cat: &Catalog, query: &str, id: &str) -> Args {
    args_for_mark(reg, cat, query, id, crate::numparse::DecimalMark::for_lang(cat.lang))
}

/// [`args_for`] with numbers read in the UI decimal mark `mark`.
pub fn args_for_mark(
    reg: &Registry,
    cat: &Catalog,
    query: &str,
    id: &str,
    mark: crate::numparse::DecimalMark,
) -> Args {
    search_with_mark(reg, cat, query, &Usage::default(), 0.0, mark)
        .into_iter()
        .find(|h| h.cmd.id == id)
        .map(|h| h.args)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(h: &[Hit]) -> Vec<String> {
        h.iter().map(|h| h.cmd.id.clone()).collect()
    }

    #[test]
    fn frecency_weight_decays_and_saturates() {
        let mut u = Usage::default();
        u.note("arch.wall", 0.0);
        u.note("arch.wall", 0.0);
        assert!((u.weight("arch.wall", 0.0) - 2.0).abs() < 1e-9);
        assert!((u.weight("arch.wall", HALF_LIFE) - 1.0).abs() < 1e-9);
        assert_eq!(u.bonus("nope", 0.0), 0);
        for _ in 0..1000 {
            u.note("mod.move", 1.0);
        }
        assert!(u.bonus("mod.move", 1.0) <= BONUS_MAX);
        assert!(u.bonus("mod.move", 1.0) > u.bonus("arch.wall", 1.0));
        assert_eq!(u.top(1.0, 2), vec!["mod.move", "arch.wall"]);
        u.note("edit.undo", 2.0);
        assert_eq!(u.order[0], "edit.undo");
        let reg = Registry::builtin();
        assert_eq!(u.last_repeatable(&reg), Some("mod.move"));
        // delete, file commands and toggles are never repeated
        for id in ["mod.delete", "file.save", "mgr.level_delete", "view.3d"] {
            let mut v = u.clone();
            v.note(id, 3.0);
            assert_eq!(v.last_repeatable(&reg), None, "{id}");
        }
        assert_eq!(u.recent(5), vec!["mod.move", "arch.wall"]);
        let back = Usage::from_json(&u.to_json());
        assert_eq!(back.cmds, u.cmds);
        assert_eq!(back.order, u.order);
        assert!(Usage::from_json("{bad").cmds.is_empty());
        u.note("@q:x", 3.0);
        assert!(!u.cmds.contains_key("@q:x"));
    }

    #[test]
    fn ranking_by_usage_and_arguments() {
        use crate::test_fx::fx;
        let (r, c) = (Registry::builtin(), Catalog::new(Lang::Ru));
        let empty = Usage::default();
        // no usage: same leader as the plain fuzzy search
        for q in [fx("wall_pre"), "wa", "undo", "ifc"] {
            let base = fuzzy::search(&r, &c, q)[0].id.clone();
            assert_eq!(search_with(&r, &c, q, &empty, 0.0)[0].cmd.id, base, "{q}");
        }
        // a command used often overtakes a slightly better name match
        let plain = ids(&search_with(&r, &c, fx("wall"), &empty, 0.0));
        assert_eq!(plain[0], "arch.wall");
        let mut u = Usage::default();
        for _ in 0..20 {
            u.note("arch.wall_room", 100.0);
        }
        let ranked = ids(&search_with(&r, &c, fx("wall"), &u, 100.0));
        assert_eq!(ranked[0], "arch.wall_room", "{ranked:?}");
        // but not across a whole match class: "Bearing wall" (word prefix) stays below
        let mut u2 = Usage::default();
        u2.note("str.wall", 100.0);
        let ranked = ids(&search_with(&r, &c, fx("wall"), &u2, 100.0));
        assert_eq!(ranked[0], "arch.wall");
        // usage never lifts a non-match
        assert!(search_with(&r, &c, fx("nomatch"), &u, 100.0).is_empty());
        // arguments: the name is matched without them
        let q = format!("{} 300 {}", fx("wall"), fx("concrete"));
        let h = search_with(&r, &c, &q, &empty, 0.0);
        assert_eq!(h[0].cmd.id, "arch.wall");
        assert_eq!(h[0].args.length(0, true), Some(0.3));
        assert_eq!(h[0].args.words, vec![fx("concrete").to_string()]);
        let h = search_with(&r, &c, "door 900x2100", &empty, 0.0);
        assert_eq!(h[0].cmd.id, "arch.door");
        assert_eq!(h[0].args.nums.len(), 2);
        // a full-name match beats dropping a word into the arguments (label per 196e5019)
        let h = search_with(&r, &Catalog::new(Lang::En), "save copy as ifc", &empty, 0.0);
        // the name belongs to the File-menu row since the asking variant became «Export IFC…»
        assert_eq!(h[0].cmd.id, "file.save_ifc");
        assert!(h[0].args.is_empty());
        assert_eq!(
            args_for(&r, &c, &format!("{} 250", fx("wall")), "arch.wall").length(0, true),
            Some(0.25)
        );
    }

    #[test]
    fn thread_store() {
        let s0 = with(|u| u.seq);
        note("arch.wall");
        assert_eq!(with(|u| u.seq), s0 + 1);
        let reg = Registry::builtin();
        assert_eq!(
            with(|u| u.last_repeatable(&reg).map(String::from)).as_deref(),
            Some("arch.wall")
        );
    }

    #[test]
    fn repeat_flag_only_on_tools() {
        let reg = Registry::builtin();
        for id in [
            "arch.wall",
            "arch.door",
            "mod.move",
            "mod.copy",
            "str.beam",
            "mep.duct",
        ] {
            assert!(reg.get(id).is_some_and(|c| c.repeatable), "{id}");
        }
        for c in reg.all() {
            let bad = c.id.starts_with("file.")
                || c.id.contains("delete")
                || c.id.starts_with("view.")
                || c.id.starts_with("set.")
                || c.id.starts_with("app.");
            assert!(!(bad && c.repeatable), "{} must not repeat", c.id);
        }
    }
}
