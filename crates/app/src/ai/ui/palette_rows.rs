// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Ctrl+K and the assistant: the «Ask AI: <text>» row and the draft rows.
//!
//! The assistant only ever makes drafts. «Apply draft» here is the panel's own Apply action
//! (`Action::Apply`), never a second apply path; the other draft rows move the view only.
use super::{act, with, Action, Facts};
use crate::ai::plan::PlanView;
use crate::shell::EditorApp;
use ddc_ai::Mode;
use ddc_model::ElKey;
use ddc_ui::fuzzy;
use ddc_ui::i18n::Catalog;
use ddc_ui::widgets::palette::Extra;
use ddc_ui::{Lang, Registry};

/// A command scoring this high (a prefix or a word prefix of its name) is what the person
/// is typing: the list stays with commands and no «Ask AI» row is offered. Word-prefix scores
/// are 800 minus the name length, so the bar sits below that.
pub const STRONG: i32 = 700;
/// What Ctrl+Enter shows on the «Ask AI» row.
pub const ASK_KEYS: &str = "Ctrl+Enter";
/// The typed text is cut to this many characters inside the row.
const SHOWN_CHARS: usize = 36;

/// The draft rows, in list order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Draft {
    Apply,
    Reject,
    Next,
    Show,
}

impl Draft {
    pub const ALL: [Draft; 4] = [Draft::Apply, Draft::Reject, Draft::Next, Draft::Show];

    /// The palette key is `@aid:<kind>`.
    pub const fn kind(self) -> &'static str {
        match self {
            Draft::Apply => "apply",
            Draft::Reject => "reject",
            Draft::Next => "next",
            Draft::Show => "show",
        }
    }

    pub fn from_kind(k: &str) -> Option<Draft> {
        Draft::ALL.into_iter().find(|d| d.kind() == k)
    }

    /// The registry command that carries the label's keys.
    pub const fn command(self) -> &'static str {
        match self {
            Draft::Apply => "ai.apply",
            Draft::Reject => "ai.reject",
            Draft::Next => "ai.next_change",
            Draft::Show => "ai.show_draft",
        }
    }

    const fn label_key(self) -> &'static str {
        match self {
            Draft::Apply => "ai.palette.draft.apply",
            Draft::Reject => "ai.palette.draft.reject",
            Draft::Next => "ai.palette.draft.next",
            Draft::Show => "ai.palette.draft.show",
        }
    }
}

/// What the rows need to know about the current draft.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DraftIn {
    /// Steps Apply would write: the «N» of «Apply draft (N)».
    pub steps: usize,
    pub can_apply: bool,
    /// Elements the draft touches: what «Next change» walks through.
    pub changes: usize,
}

/// The draft as the rows see it: a plan with something in it. `None`: no draft.
pub fn draft_in(plan: Option<&PlanView>, can_apply: bool) -> Option<DraftIn> {
    let p = plan.filter(|p| !p.items.is_empty())?;
    Some(DraftIn {
        steps: p.compiled.steps.len(),
        can_apply,
        changes: p.diff.len(),
    })
}

/// Which draft rows are offered. Apply only while the panel would let it through, so a pick
/// never ends in a refusal; the view rows only when there is something to look at.
pub fn draft_list(d: Option<DraftIn>) -> Vec<Draft> {
    let Some(d) = d else {
        return vec![];
    };
    let mut v = vec![];
    if d.can_apply {
        v.push(Draft::Apply);
    }
    v.push(Draft::Reject);
    if d.changes > 0 {
        v.push(Draft::Next);
        v.push(Draft::Show);
    }
    v
}

/// A typed query keeps the draft rows that match their name; an empty one keeps all.
pub fn draft_matches(query: &str, label: &str) -> bool {
    query.trim().is_empty() || fuzzy::best(query.trim(), &[label]).is_some()
}

/// The element after the one the view stopped on; the first when none is pinned.
pub fn next_index(keys: &[ElKey], pinned: &[ElKey]) -> Option<usize> {
    if keys.is_empty() {
        return None;
    }
    let at = match pinned {
        [one] => keys.iter().position(|k| k == one),
        _ => None,
    };
    Some(at.map_or(0, |i| (i + 1) % keys.len()))
}

/// What a draft row does. Apply and Reject are the panel's own actions.
pub fn draft_action(d: Draft, keys: Vec<ElKey>, pinned: &[ElKey]) -> Option<Action> {
    match d {
        Draft::Apply => Some(Action::Apply),
        Draft::Reject => Some(Action::Reject),
        Draft::Next => next_index(&keys, pinned).map(|i| Action::Zoom(vec![keys[i].clone()])),
        Draft::Show => (!keys.is_empty()).then_some(Action::Zoom(keys)),
    }
}

/// The mode of a question asked from the palette, as the starter prompts do: changes are
/// proposed only about a selection, otherwise the assistant answers and reads.
pub fn ask_mode(has_selection: bool, propose_on: bool) -> Mode {
    if has_selection && propose_on {
        Mode::Propose
    } else {
        Mode::ReadOnly
    }
}

/// Best score of the query over command names: a strong one means a command is meant.
pub fn top_score(reg: &Registry, cat: &Catalog, query: &str) -> Option<i32> {
    let q = query.trim();
    reg.all()
        .iter()
        .filter_map(|c| {
            let key = c.label_key();
            let (l, le) = (cat.t(&key), cat.t_in(Lang::En, &key));
            fuzzy::best(q, &[&l, &le, &c.id])
        })
        .max()
}

/// The text of the «Ask AI» row and where it goes. `!text` always asks, in the first place;
/// a phrase is offered last, unless a command matches it strongly.
pub fn ask_offer(query: &str, parses: bool, top: Option<i32>) -> Option<(String, bool)> {
    let (text, first) = super::logic::palette_offer(query, parses)?;
    if !first && top.is_some_and(|s| s >= STRONG) {
        return None;
    }
    Some((text, first))
}

fn shorten(t: &str) -> String {
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() <= SHOWN_CHARS {
        return t;
    }
    let cut: String = t.chars().take(SHOWN_CHARS - 1).collect();
    format!("{cut}\u{2026}")
}

fn spark(key: String, label: String, detail: String) -> Extra {
    Extra {
        key,
        label,
        detail,
        icon: ddc_ui::icons::ctl::AI.into(),
    }
}

/// What the palette adds for one query.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Offers {
    pub drafts: Vec<Extra>,
    /// The «Ask AI» (or «Connect AI») row and whether it goes first.
    pub ask: Option<(Extra, bool)>,
}

pub struct Inputs<'a> {
    pub query: &'a str,
    pub connected: bool,
    pub draft: Option<DraftIn>,
}

pub fn offers(reg: &Registry, cat: &Catalog, i: &Inputs<'_>) -> Offers {
    let mut out = Offers::default();
    for d in draft_list(i.draft) {
        let n = i.draft.map_or(0, |x| x.steps).to_string();
        let label = cat.tf(d.label_key(), &[("n", &n)]);
        if !draft_matches(i.query, &label) {
            continue;
        }
        let keys = reg.get(d.command()).map(|c| c.shortcuts.join(" / "));
        out.drafts.push(spark(
            format!("@aid:{}", d.kind()),
            label,
            keys.unwrap_or_default(),
        ));
    }
    let q = i.query.trim();
    if q.is_empty() {
        return out;
    }
    let parses = crate::query::parse(q).is_ok();
    let top = top_score(reg, cat, q);
    if let Some((text, first)) = ask_offer(q, parses, top) {
        let row = if i.connected {
            spark(
                format!("@ai:{text}"),
                cat.tf("ai.palette.ask_q", &[("q", &shorten(&text))]),
                ASK_KEYS.into(),
            )
        } else {
            spark(
                "@aic:".into(),
                cat.t("ai.palette.connect"),
                ASK_KEYS.into(),
            )
        };
        out.ask = Some((row, first));
    }
    out
}

impl Offers {
    /// Puts the rows into the host's list; returns the palette's catch-all index and whether
    /// it is shown last.
    pub fn merge(self, ex: &mut Vec<Extra>, first: &mut bool) -> (Option<usize>, bool) {
        if !self.drafts.is_empty() {
            for (i, d) in self.drafts.into_iter().enumerate() {
                ex.insert(i, d);
            }
            *first = true;
        }
        match self.ask {
            Some((row, true)) => {
                ex.insert(0, row);
                *first = true;
                (Some(0), false)
            }
            Some((row, false)) => {
                ex.push(row);
                (Some(ex.len() - 1), true)
            }
            None => (None, false),
        }
    }
}

/// The palette host's call: the rows for this query from the live state.
pub fn palette_ai(app: &mut EditorApp, query: &str) -> Offers {
    let connected = with(|st| {
        st.init(app);
        st.active_ready()
    });
    let f = Facts::read();
    let draft = draft_in(f.plan.as_deref(), f.can_apply);
    offers(
        &app.sh.reg,
        &app.sh.cat,
        &Inputs {
            query,
            connected,
            draft,
        },
    )
}

/// A picked `@aid:<kind>` row or a draft command.
pub fn run_draft(app: &mut EditorApp, d: Draft) {
    let Some(plan) = Facts::read().plan else {
        return;
    };
    let pinned = with(|st| st.pinned.clone());
    let keys = plan.diff.iter().map(|x| x.key.clone()).collect();
    if let Some(a) = draft_action(d, keys, &pinned) {
        act(app, a);
    }
}

pub fn draft_pick(app: &mut EditorApp, kind: &str) {
    if let Some(d) = Draft::from_kind(kind) {
        run_draft(app, d);
    }
}

/// A picked «Connect AI to ask» row: the connection settings.
pub fn connect_pick(app: &mut EditorApp) {
    act(app, Action::OpenSettings);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;

    fn en() -> (Registry, Catalog) {
        (Registry::builtin(), Catalog::new(Lang::En))
    }

    /// A context whose fonts and styles are those of the app: text can be laid out.
    fn ready(a: &mut crate::shell::EditorApp) -> egui::Context {
        let ctx = egui::Context::default();
        a.sh.theme.apply(&ctx);
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        ctx
    }

    fn draft(steps: usize, can_apply: bool, changes: usize) -> Option<DraftIn> {
        Some(DraftIn {
            steps,
            can_apply,
            changes,
        })
    }

    fn input(q: &str, connected: bool, d: Option<DraftIn>) -> Inputs<'_> {
        Inputs {
            query: q,
            connected,
            draft: d,
        }
    }

    #[test]
    fn a_strong_command_match_hides_the_ask_row() {
        let (r, c) = en();
        // the name of a command, said in three words: a command is meant
        let name = c.t("cmd.arch.wall");
        let top = top_score(&r, &c, &name);
        assert!(top.unwrap() >= STRONG, "{top:?}");
        assert!(ask_offer(&name, false, None).is_some());
        assert_eq!(ask_offer(&name, false, top), None);
        let o = offers(&r, &c, &input(&name, true, None));
        assert!(o.ask.is_none(), "{o:?}");
        // «!» asks even then
        let o = offers(&r, &c, &input(&format!("!{name}"), true, None));
        assert!(o.ask.is_some());
    }

    #[test]
    fn the_threshold_between_a_command_and_a_question() {
        assert!(ask_offer("zzz qqq www", false, None).is_some());
        assert!(ask_offer("zzz qqq www", false, Some(STRONG - 1)).is_some());
        assert_eq!(ask_offer("zzz qqq www", false, Some(STRONG)), None);
        // two words that read as a query of the query language are not a question
        assert_eq!(ask_offer("door", true, None), None);
        // a weak fuzzy hit does not hide it
        let (r, c) = en();
        let o = offers(&r, &c, &input("zzz qqq www", true, None));
        assert!(o.ask.is_some());
    }

    #[test]
    fn empty_and_blank_text_offer_nothing() {
        let (r, c) = en();
        for q in ["", " ", "   \t "] {
            assert_eq!(offers(&r, &c, &input(q, true, None)), Offers::default(), "{q:?}");
        }
        assert_eq!(ask_offer("!", false, None), None);
        assert_eq!(ask_offer("!   ", false, None), None);
    }

    #[test]
    fn the_ask_row_carries_the_text_and_the_key() {
        let (r, c) = en();
        let o = offers(&r, &c, &input("  zzz  qqq www ", true, None));
        let (row, first) = o.ask.unwrap();
        assert!(!first);
        assert_eq!(row.key, "@ai:zzz  qqq www");
        assert_eq!(row.detail, ASK_KEYS);
        assert!(row.label.contains("zzz qqq www"), "{}", row.label);
        // a long phrase is cut inside the label, never in the key
        let long = "word ".repeat(30);
        let (row, _) = offers(&r, &c, &input(&long, true, None)).ask.unwrap();
        assert!(row.label.chars().count() < 60, "{}", row.label);
        assert_eq!(row.key, format!("@ai:{}", long.trim()));
    }

    #[test]
    fn without_a_connection_the_row_opens_the_connection() {
        let (r, c) = en();
        let (row, _) = offers(&r, &c, &input("zzz qqq www", false, None)).ask.unwrap();
        assert_eq!(row.key, "@aic:");
        assert_eq!(row.label, c.t("ai.palette.connect"));
        assert_ne!(row.label, "ai.palette.connect", "the string exists");
    }

    #[test]
    fn draft_rows_only_with_a_draft_and_with_the_right_count() {
        let (r, c) = en();
        assert!(offers(&r, &c, &input("", true, None)).drafts.is_empty());
        let o = offers(&r, &c, &input("", true, draft(7, true, 3)));
        let keys: Vec<&str> = o.drafts.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, ["@aid:apply", "@aid:reject", "@aid:next", "@aid:show"]);
        assert!(o.drafts[0].label.contains("(7)"), "{}", o.drafts[0].label);
        let o = offers(&r, &c, &input("", true, draft(12, true, 3)));
        assert!(o.drafts[0].label.contains("(12)"));
        // every row shows the keys of its command
        for (e, d) in o.drafts.iter().zip(Draft::ALL) {
            let want = r.get(d.command()).unwrap().shortcuts.join(" / ");
            assert!(!want.is_empty(), "{}", d.command());
            assert_eq!(e.detail, want);
        }
        // a draft the panel would not apply: no Apply row, no dead end
        let o = offers(&r, &c, &input("", true, draft(7, false, 3)));
        assert_eq!(o.drafts[0].key, "@aid:reject");
        // nothing to look at: Apply and Reject remain
        assert_eq!(draft_list(draft(2, true, 0)), [Draft::Apply, Draft::Reject]);
        assert_eq!(draft_list(None), []);
    }

    #[test]
    fn a_typed_query_keeps_the_draft_rows_it_names() {
        let (r, c) = en();
        let o = offers(&r, &c, &input("wall", true, draft(7, true, 3)));
        assert!(o.drafts.is_empty());
        let o = offers(&r, &c, &input("reject", true, draft(7, true, 3)));
        assert_eq!(o.drafts.len(), 1);
        assert_eq!(o.drafts[0].key, "@aid:reject");
    }

    #[test]
    fn merge_puts_drafts_first_and_the_ask_row_last() {
        let (r, c) = en();
        let o = offers(&r, &c, &input("", true, draft(1, true, 1)));
        let mut ex = vec![spark("@q:a".into(), "a".into(), String::new())];
        let mut first = false;
        assert_eq!(o.merge(&mut ex, &mut first), (None, false));
        assert!(first);
        assert_eq!(ex.len(), 5);
        assert_eq!(ex[4].key, "@q:a");

        let o = offers(&r, &c, &input("zzz qqq www", true, draft(1, true, 1)));
        let mut ex = vec![];
        let mut first = false;
        let (at, last) = o.merge(&mut ex, &mut first);
        // no draft row matches this query: only the ask row, at the end
        assert!(last && !first);
        assert_eq!(at, Some(ex.len() - 1));
        assert!(ex[at.unwrap()].key.starts_with("@ai:"));

        let o = offers(&r, &c, &input("!zzz", true, None));
        let mut ex = vec![spark("@q:a".into(), "a".into(), String::new())];
        let mut first = false;
        assert_eq!(o.merge(&mut ex, &mut first), (Some(0), false));
        assert!(first && ex[0].key == "@ai:zzz");
    }

    #[test]
    fn the_ask_row_is_last_in_the_list_and_only_it_takes_ctrl_enter() {
        let (r, c) = en();
        let mut p = ddc_ui::widgets::palette::Palette::default();
        p.query = "zzz qqq www".into();
        let o = offers(&r, &c, &input(&p.query, true, draft(2, true, 1)));
        let (at, last) = o.merge(&mut p.extras, &mut p.extras_first);
        p.catch_all = at;
        p.catch_all_last = last;
        p.extras_for = Some(p.query.clone());
        let items = p.items(&r, &c);
        let ddc_ui::widgets::palette::Item::Extra(k) = items.last().unwrap() else {
            panic!("the last row is an extra");
        };
        assert!(p.extras[*k].key.starts_with("@ai:"));
        assert_eq!(p.catch_all_row(&r, &c), Some(*k));
        // the extras of another query do not count
        p.query = "zzz".into();
        assert_eq!(p.catch_all_row(&r, &c), None);
    }

    /// Ctrl+Enter through the real widget: the highlighted row does not matter.
    #[test]
    fn ctrl_enter_picks_the_ask_row_from_anywhere() {
        let mut a = crate::shell::EditorApp::new(Editor::new_in_memory().unwrap(), None, "t".into());
        let o = offers(
            &a.sh.reg,
            &a.sh.cat,
            &input("zzz qqq www", true, draft(2, true, 1)),
        );
        let p = &mut a.sh.palette;
        p.open = true;
        p.query = "zzz qqq www".into();
        // draft rows drop out for a query that matches none of them: a host row stands first
        p.extras = vec![spark("@q:a".into(), "a".into(), String::new())];
        let (at, last) = o.merge(&mut p.extras, &mut p.extras_first);
        p.catch_all = at;
        p.catch_all_last = last;
        // through the keyboard layout "zzz" subsequence-matches a Russian command label
        p.extras_first = true;
        p.extras_for = Some(p.query.clone());
        p.sel = 0;
        let ctx = ready(&mut a);
        let ev = |m| egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: Some(egui::Key::Enter),
            pressed: true,
            repeat: false,
            modifiers: m,
        };
        let frame = |a: &mut crate::shell::EditorApp, m| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 700.0),
                )),
                events: vec![ev(m)],
                ..Default::default()
            };
            let mut got = None;
            let _ = ctx.run(input, |c| {
                let s = &mut a.sh;
                got = s.palette.show(c, &s.theme, &s.cat, &s.reg, &mut s.icons);
            });
            got
        };
        // plain Enter takes the highlighted row, not the question
        let plain = frame(&mut a, egui::Modifiers::NONE).unwrap();
        assert_eq!(plain, "@q:a");
        a.sh.palette.open = true;
        a.sh.palette.sel = 0;
        let got = frame(&mut a, egui::Modifiers::COMMAND).unwrap();
        assert_eq!(got, "@ai:zzz qqq www");
    }

    #[test]
    fn ctrl_enter_with_no_ask_row_picks_nothing() {
        let mut a = crate::shell::EditorApp::new(Editor::new_in_memory().unwrap(), None, "t".into());
        a.sh.palette.open = true;
        let ctx = ready(&mut a);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 700.0),
            )),
            events: vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: Some(egui::Key::Enter),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::COMMAND,
            }],
            ..Default::default()
        };
        let mut got = Some(String::new());
        let _ = ctx.run(input, |c| {
            let s = &mut a.sh;
            got = s.palette.show(c, &s.theme, &s.cat, &s.reg, &mut s.icons);
        });
        assert_eq!(got, None);
    }

    #[test]
    fn the_mode_follows_the_selection_like_the_starter_prompts() {
        assert!(matches!(ask_mode(true, true), Mode::Propose));
        assert!(matches!(ask_mode(true, false), Mode::ReadOnly));
        assert!(matches!(ask_mode(false, true), Mode::ReadOnly));
        assert!(matches!(ask_mode(false, false), Mode::ReadOnly));
    }

    #[test]
    fn next_walks_the_changes_and_wraps() {
        let k = |i: i64| ("wall".to_string(), i);
        let keys = [k(1), k(2), k(3)];
        assert_eq!(next_index(&keys, &[]), Some(0));
        assert_eq!(next_index(&keys, &[k(1)]), Some(1));
        assert_eq!(next_index(&keys, &[k(3)]), Some(0));
        assert_eq!(next_index(&keys, &[k(9)]), Some(0));
        assert_eq!(next_index(&keys, &[k(1), k(2)]), Some(0));
        assert_eq!(next_index(&[], &[]), None);
    }

    #[test]
    fn apply_and_reject_are_the_panels_own_actions() {
        let k = |i: i64| ("wall".to_string(), i);
        let two = || vec![k(1), k(2)];
        assert!(matches!(
            draft_action(Draft::Apply, two(), &[]),
            Some(Action::Apply)
        ));
        assert!(matches!(
            draft_action(Draft::Reject, two(), &[]),
            Some(Action::Reject)
        ));
        match draft_action(Draft::Next, two(), &[k(1)]) {
            Some(Action::Zoom(v)) => assert_eq!(v, vec![k(2)]),
            _ => panic!("next zooms to one element"),
        }
        match draft_action(Draft::Show, two(), &[]) {
            Some(Action::Zoom(v)) => assert_eq!(v, two()),
            _ => panic!("show zooms to all"),
        }
        // no changes: nothing to walk to or show
        assert!(draft_action(Draft::Next, vec![], &[]).is_none());
        assert!(draft_action(Draft::Show, vec![], &[]).is_none());
    }

    #[test]
    fn draft_kinds_round_trip_and_commands_exist() {
        let r = Registry::builtin();
        for d in Draft::ALL {
            assert_eq!(Draft::from_kind(d.kind()), Some(d));
            assert!(r.get(d.command()).is_some(), "{}", d.command());
        }
        assert_eq!(Draft::from_kind("x"), None);
    }

    /// The keys the rows show are the registry's, and no other command has them.
    #[test]
    fn draft_keys_are_unique_in_the_global_keymap() {
        let r = Registry::builtin();
        assert_eq!(r.conflicts(), vec![]);
        for d in Draft::ALL {
            let sc = &r.get(d.command()).unwrap().shortcuts;
            assert_eq!(sc.len(), 1, "{}", d.command());
            let owners: Vec<&str> = r
                .all()
                .iter()
                .filter(|c| c.shortcuts.contains(&sc[0]))
                .map(|c| c.id.as_str())
                .collect();
            assert_eq!(owners, [d.command()]);
        }
        // Ctrl+Enter belongs to the palette and the panel, not to a global command
        assert!(r
            .all()
            .iter()
            .all(|c| !c.shortcuts.iter().any(|s| s == ASK_KEYS)));
    }
}
