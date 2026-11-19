//! Hint table: the keys of the current step and the values they use, from one table.
//! The card, the AI answer «what can I do now» and the tests read `hint_rows`/`value_rows`/
//! `describe`; a session lists its own `hints()`, the old tools are described by `TABLE`
//! through the scope.
use super::input::{Key, Mods};
use super::scope::{Scope, ScopeKind};
use super::session::Cx;
use super::Interaction;
use crate::editor::Editor;
use ddc_ui::hint_card::{HintRow, ValueRow};

/// An input the user can give at a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chord {
    Click,
    DblClick,
    Drag,
    Key(Key, Mods),
    Digits,
    Tab,
    Space,
    Esc,
    Enter,
}

impl Chord {
    /// i18n key of the words for chords that are not a letter.
    pub fn label_key(self) -> Option<&'static str> {
        Some(match self {
            Chord::Click => "ix.chord.click",
            Chord::DblClick => "ix.chord.dblclick",
            Chord::Drag => "ix.chord.drag",
            Chord::Digits => "ix.chord.digits",
            Chord::Tab => "ix.chord.tab",
            Chord::Space => "ix.chord.space",
            Chord::Esc => "ix.chord.esc",
            Chord::Enter => "ix.chord.enter",
            Chord::Key(..) => return None,
        })
    }

    /// The chord as the user reads it («Click», «Ctrl+A»).
    pub fn label(self) -> String {
        if let Some(k) = self.label_key() {
            return crate::tr(k, &[]);
        }
        let Chord::Key(k, m) = self else { return String::new() };
        let name = match k {
            Key::Enter => crate::tr("ix.chord.enter", &[]),
            Key::Escape => crate::tr("ix.chord.esc", &[]),
            Key::Backspace => "Backspace".to_string(),
            Key::Tab => crate::tr("ix.chord.tab", &[]),
            Key::Close => "C".to_string(),
            Key::Arc => "A".to_string(),
        };
        let mut s = String::new();
        for (on, w) in [(m.ctrl, "Ctrl+"), (m.shift, "Shift+"), (m.alt, "Alt+")] {
            if on {
                s.push_str(w);
            }
        }
        s + &name
    }
}

/// One line of the card: a chord, what it does (`ix.hint.*`) and when it applies.
#[derive(Clone, Copy, Debug)]
pub struct Hint {
    pub chord: Chord,
    pub action_key: &'static str,
    pub when: Option<fn(&Cx) -> bool>,
}

impl Hint {
    pub const fn new(chord: Chord, action_key: &'static str) -> Self {
        Self { chord, action_key, when: None }
    }

    pub const fn when(mut self, f: fn(&Cx) -> bool) -> Self {
        self.when = Some(f);
        self
    }

    fn applies(&self, cx: &Cx) -> bool {
        self.when.map_or(true, |f| f(cx))
    }
}

/// A value the step reads: «Snap: end».
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HintValue {
    pub label_key: &'static str,
    pub value: String,
}

const ESC_CANCEL: Hint = Hint::new(Chord::Esc, "ix.hint.cancel");
const ESC_EXIT: Hint = Hint::new(Chord::Esc, "ix.hint.exit");
const ESC_POINT: Hint = Hint::new(Chord::Esc, "ix.hint.cancel_point");

const WALL_FIRST: &[Hint] = &[
    Hint::new(Chord::Click, "ix.hint.first_point"),
    Hint::new(Chord::Key(Key::Arc, Mods::NONE), "ix.hint.arc"),
    ESC_EXIT,
];
const WALL_NEXT: &[Hint] = &[
    Hint::new(Chord::Click, "ix.hint.next_point"),
    Hint::new(Chord::Digits, "ix.hint.length"),
    Hint::new(Chord::Key(Key::Arc, Mods::NONE), "ix.hint.arc"),
    Hint::new(Chord::Key(Key::Close, Mods::NONE), "ix.hint.close"),
    ESC_POINT,
];
const DRAFT: &[Hint] = &[
    Hint::new(Chord::Click, "ix.hint.next_point"),
    Hint::new(Chord::Digits, "ix.hint.value"),
    Hint::new(Chord::Enter, "ix.hint.finish"),
    ESC_CANCEL,
];
const PLACE: &[Hint] = &[Hint::new(Chord::Click, "ix.hint.place"), Hint::new(Chord::Tab, "ix.hint.variant"), ESC_EXIT];
const IDLE: &[Hint] = &[
    Hint::new(Chord::Click, "ix.hint.select"),
    Hint::new(Chord::Drag, "ix.hint.box_select"),
    Hint::new(Chord::DblClick, "ix.hint.edit"),
];
const MOVING: &[Hint] = &[
    Hint::new(Chord::Click, "ix.hint.drop"),
    Hint::new(Chord::Digits, "ix.hint.distance"),
    ESC_CANCEL,
];
const GRIP: &[Hint] = &[
    Hint::new(Chord::Drag, "ix.hint.drag_grip"),
    Hint::new(Chord::Digits, "ix.hint.exact_value"),
    Hint::new(Chord::Enter, "ix.hint.apply"),
    ESC_CANCEL,
];
const RESHAPE: &[Hint] = &[
    Hint::new(Chord::Drag, "ix.hint.move_vertex"),
    Hint::new(Chord::DblClick, "ix.hint.add_vertex"),
    Hint::new(Chord::Enter, "ix.hint.finish"),
    ESC_CANCEL,
];
const BOX: &[Hint] = &[Hint::new(Chord::Drag, "ix.hint.box_select"), ESC_CANCEL];
const PAINT: &[Hint] = &[Hint::new(Chord::Click, "ix.hint.paint"), ESC_EXIT];
const MEASURE: &[Hint] = &[Hint::new(Chord::Click, "ix.hint.measure_point"), ESC_EXIT];
const POINT: &[Hint] = &[Hint::new(Chord::Click, "ix.hint.pick_point"), ESC_CANCEL];

struct Row {
    kind: ScopeKind,
    tool: Option<&'static str>,
    step: Option<&'static str>,
    hints: &'static [Hint],
}

const fn row(kind: ScopeKind, tool: Option<&'static str>, step: Option<&'static str>, hints: &'static [Hint]) -> Row {
    Row { kind, tool, step, hints }
}

/// The one table. Rows are ordered specific first: tool and step, then the kind's general row.
static TABLE: &[Row] = &[
    row(ScopeKind::Drafting, Some("arch.wall"), Some("first"), WALL_FIRST),
    row(ScopeKind::Drafting, Some("arch.wall"), Some("next"), WALL_NEXT),
    row(ScopeKind::Idle, None, None, IDLE),
    row(ScopeKind::Drafting, None, None, DRAFT),
    row(ScopeKind::Placing, None, None, PLACE),
    row(ScopeKind::Moving, None, None, MOVING),
    row(ScopeKind::GripDrag, None, None, GRIP),
    row(ScopeKind::Reshaping, None, None, RESHAPE),
    row(ScopeKind::BoxSelect, None, None, BOX),
    row(ScopeKind::Painting, None, None, PAINT),
    row(ScopeKind::Measuring, None, None, MEASURE),
    row(ScopeKind::Pointing, None, None, POINT),
];

/// Hints of a scope from the table; never empty (every kind has a general row).
pub fn for_scope(s: &Scope) -> &'static [Hint] {
    let (tool, step) = match s {
        Scope::Drafting { tool, step } => (Some(tool.0), Some(*step)),
        Scope::Placing { tool } => (Some(tool.0), None),
        _ => (None, None),
    };
    let k = s.kind();
    let hit = |r: &&Row| r.kind == k && (r.tool.is_none() || r.tool == tool) && (r.step.is_none() || r.step == step);
    TABLE.iter().find(hit).map_or(&[], |r| r.hints)
}

/// Every action, chord and value key the table can show (the translation test reads it).
pub fn all_keys() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = TABLE.iter().flat_map(|r| r.hints.iter().map(|h| h.action_key)).collect();
    v.extend(["ix.help.title", "ix.now", "ix.val.snap", "ix.val.units", "ix.units.metric", "ix.units.imperial"]);
    for c in [
        Chord::Click,
        Chord::DblClick,
        Chord::Drag,
        Chord::Digits,
        Chord::Tab,
        Chord::Space,
        Chord::Esc,
        Chord::Enter,
    ] {
        v.extend(c.label_key());
    }
    v.sort_unstable();
    v.dedup();
    v
}

/// Values the old tools show: the snap under the cursor and the units of typed numbers.
fn legacy_values(ed: &Editor, s: &Scope) -> Vec<HintValue> {
    let mut v = vec![];
    if matches!(s, Scope::Drafting { .. } | Scope::Placing { .. } | Scope::Moving { .. }) {
        if let Some(sn) = ed.tool.snaps.get(ed.tool.snap_idx) {
            let dbg = format!("{:?}", sn.kind);
            let base = dbg.split('(').next().unwrap_or("").to_lowercase();
            let key = format!("ix.snap.{base}");
            let t = crate::tr(&key, &[]);
            v.push(HintValue { label_key: "ix.val.snap", value: if t == key { base } else { t } });
        }
        let u = match ed.tool.units {
            crate::units::Units::Metric => "ix.units.metric",
            crate::units::Units::Imperial => "ix.units.imperial",
        };
        v.push(HintValue { label_key: "ix.val.units", value: crate::tr(u, &[]) });
    }
    v
}

impl Interaction {
    fn cx<'a>(&self, ed: &'a Editor) -> Cx<'a> {
        Cx { ed, now_ms: self.now_ms }
    }

    /// Hints of the current step: the session's own, else the table through the scope.
    pub fn hints(&self, ed: &Editor) -> Vec<Hint> {
        let cx = self.cx(ed);
        let all: Vec<Hint> = match &self.session {
            Some(s) => s.hints().to_vec(),
            None => for_scope(&self.scope).to_vec(),
        };
        all.into_iter().filter(|h| h.applies(&cx)).collect()
    }

    pub fn hint_values(&self, ed: &Editor) -> Vec<HintValue> {
        match &self.session {
            Some(s) => s.values(&self.cx(ed)),
            None => legacy_values(ed, &self.scope),
        }
    }

    /// Rows for the card in the current language.
    pub fn hint_rows(&self, ed: &Editor) -> Vec<HintRow> {
        self.hints(ed)
            .iter()
            .map(|h| HintRow { chord_label: h.chord.label(), text: crate::tr(h.action_key, &[]) })
            .collect()
    }

    pub fn value_rows(&self, ed: &Editor) -> Vec<ValueRow> {
        self.hint_values(ed)
            .into_iter()
            .map(|v| ValueRow { label: crate::tr(v.label_key, &[]), value: v.value })
            .collect()
    }

    /// The last refusal in words, in the current language.
    pub fn refusal_text(&self) -> Option<String> {
        let r = self.last_refusal.as_ref()?;
        let args: Vec<(&str, &str)> = r.args.iter().map(|(k, v)| (*k, v.as_str())).collect();
        Some(crate::tr(r.msg_key, &args))
    }

    /// What the AI reads for «what can I do now»: data only.
    pub fn describe(&self, ed: &Editor) -> serde_json::Value {
        let (tool, step) = match &self.scope {
            Scope::Drafting { tool, step } => (Some(tool.0), Some(*step)),
            Scope::Placing { tool } => (Some(tool.0), None),
            _ => (None, None),
        };
        let hints: Vec<_> = self
            .hints(ed)
            .iter()
            .map(|h| {
                serde_json::json!({
                    "chord": h.chord.label(),
                    "action": h.action_key,
                    "text": crate::tr(h.action_key, &[]),
                })
            })
            .collect();
        let values: Vec<_> = self
            .hint_values(ed)
            .iter()
            .map(|v| {
                serde_json::json!({
                    "label": v.label_key,
                    "text": crate::tr(v.label_key, &[]),
                    "value": v.value,
                })
            })
            .collect();
        serde_json::json!({
            "scope": format!("{:?}", self.scope.kind()),
            "tool": tool,
            "step": step,
            "hints": hints,
            "values": values,
        })
    }
}

/// F1 topic for the running tool, built from the same table (id `help.tool.<tool>`).
pub fn help_topic(ed: &Editor) -> Option<ddc_ui::help::Topic> {
    let tool = ed.ix.scope().tool()?.0;
    let rows = ed.ix.hint_rows(ed);
    let mut md = format!("# {}
@tags keys, tool

## keys
", crate::tr("ix.help.title", &[("tool", tool)]));
    for r in &rows {
        md.push_str(&format!("- `{}` - {}
", r.chord_label.replace('`', ""), r.text));
    }
    let vals = ed.ix.value_rows(ed);
    if !vals.is_empty() {
        md.push_str("
## tips
");
        for v in &vals {
            md.push_str(&format!("- {}: {}
", v.label, v.value));
        }
    }
    ddc_ui::help::parse(&format!("help.tool.{tool}"), &md).ok()
}

/// Rows of the Ctrl+K section «Now»: keys of the running step (key `@now:`, a no-op when picked).
pub fn palette_rows(ed: &Editor) -> Vec<ddc_ui::widgets::palette::Extra> {
    if *ed.ix.scope() == Scope::Idle {
        return vec![];
    }
    let now = crate::tr("ix.now", &[]);
    ed.ix
        .hint_rows(ed)
        .into_iter()
        .map(|r| ddc_ui::widgets::palette::Extra {
            key: "@now:".into(),
            label: format!("{} - {}", r.chord_label, r.text),
            detail: now.clone(),
            icon: String::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interact::script::{parse, run};

    fn ix_scripts() -> Vec<(String, String)> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ix");
        let mut v: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "ixs"))
            .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read_to_string(p).unwrap()))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn every_kind_has_a_general_row() {
        for k in ScopeKind::ALL {
            assert!(
                TABLE.iter().any(|r| r.kind == k && r.tool.is_none() && r.step.is_none()),
                "no general hints for {k:?}"
            );
        }
    }

    /// Every scope any script reaches (every tool and step of the scenarios) has
    /// hints, and a way out unless it is idle.
    #[test]
    fn every_step_of_every_session_has_hints() {
        let mut seen = std::collections::BTreeSet::new();
        for (name, src) in ix_scripts() {
            let script = parse(&src).unwrap_or_else(|e| panic!("{name}: {e}"));
            let mut ed = Editor::new_in_memory().unwrap();
            let rep = run(&mut ed, &script);
            assert!(!rep.scopes.is_empty(), "{name}: no scopes recorded");
            for s in rep.scopes {
                let h = for_scope(&s);
                assert!(!h.is_empty(), "{name}: {s:?} has no hints");
                if s != Scope::Idle {
                    assert!(
                        h.iter().any(|x| matches!(x.chord, Chord::Esc | Chord::Enter)),
                        "{name}: {s:?} has no way out"
                    );
                }
                seen.insert(format!("{:?}", s.kind()));
            }
        }
        assert!(seen.len() >= 3, "scripts reached only {seen:?}");
    }

    #[test]
    fn hint_keys_translated_ru_en() {
        for k in all_keys() {
            for ru in [true, false] {
                let t = crate::tr_lang(ru, k, &[]);
                assert!(t != k && !t.is_empty(), "{k} is not translated (ru={ru})");
            }
        }
    }

    #[test]
    fn wall_card_matches_the_scenario() {
        let s = Scope::Drafting { tool: super::super::ToolId("arch.wall"), step: "next" };
        let keys: Vec<_> = for_scope(&s).iter().map(|h| h.action_key).collect();
        assert_eq!(
            keys,
            ["ix.hint.next_point", "ix.hint.length", "ix.hint.arc", "ix.hint.close", "ix.hint.cancel_point"]
        );
        assert_eq!(Chord::Key(Key::Close, Mods::NONE).label(), "C");
        assert_eq!(Chord::Key(Key::Arc, Mods { ctrl: true, ..Mods::NONE }).label(), "Ctrl+A");
    }

    #[test]
    fn help_topic_and_palette_rows_follow_the_table() {
        let mut ed = Editor::new_in_memory().unwrap();
        assert!(help_topic(&ed).is_none() && palette_rows(&ed).is_empty());
        let rep = run(&mut ed, &parse("tool(WA) → scope=Drafting
").unwrap());
        assert!(rep.ok(), "{:?}", rep.failures);
        let t = help_topic(&ed).expect("topic");
        assert_eq!(t.id, "help.tool.arch.wall");
        assert!(!t.blocks.is_empty());
        assert_eq!(palette_rows(&ed).len(), ed.ix.hint_rows(&ed).len());
    }

    #[test]
    fn describe_is_data_for_the_ai() {
        let mut ed = Editor::new_in_memory().unwrap();
        let rep = run(&mut ed, &parse("tool(WA) → scope=Drafting\n").unwrap());
        assert!(rep.ok(), "{:?}", rep.failures);
        let d = ed.ix.describe(&ed);
        assert_eq!(d["scope"], "Drafting");
        assert_eq!(d["tool"], "arch.wall");
        assert_eq!(d["step"], "first");
        assert!(d["hints"].as_array().unwrap().len() >= 2);
    }

    #[test]
    fn building_the_rows_is_cheap() {
        let ed = Editor::new_in_memory().unwrap();
        let n = 400u32;
        let t = std::time::Instant::now();
        for _ in 0..n {
            std::hint::black_box((ed.ix.hint_rows(&ed), ed.ix.value_rows(&ed)));
        }
        let us = t.elapsed().as_secs_f64() * 1e6 / f64::from(n);
        // 50 us in release; a debug build runs this code about 20x slower
        let budget = if cfg!(debug_assertions) { 1000.0 } else { 50.0 };
        assert!(us <= budget, "{us:.1} us per card, budget {budget}");
    }
}
