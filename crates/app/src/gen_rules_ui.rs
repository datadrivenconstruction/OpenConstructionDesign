//! `gen.rules` ("Generation rules"): the rules of the generated project in one window. Rules by
//! group with search, each with its value in range, unit, tier and weight, lock and the layer it
//! came from; why the selected element is as it is; a conflict with one-click fixes; variants side
//! by side. The window never writes the model: changes go to the rule tools as one draft, which
//! the person applies in the AI panel as one undo step.

use crate::ai::gen_rules::{self, RuleRow};
use crate::editor::{Editor, Sel};
use crate::shell::EditorApp;
use crate::ui_kit as kit;
use ddc_ai::mcp::McpCall;
use ddc_ai::ToolOutcome;
use ddc_gen::project::rules::{self, RuleDef, Tier};
use ddc_gen::project::why::{RuleFix, Why};
use ddc_ui::theme::space;
use ddc_ui::widgets::{self, NumberFormat, TextField, Tone};
use ddc_ui::Catalog;
use eframe::egui;
use serde_json::{json, Map, Value as J};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::mpsc;

const KEY: &str = "gen_rules";
const CLIENT: &str = "rules";
const TABS: [&str; 4] = ["gen.tab.rules", "gen.tab.conflicts", "gen.tab.variants", "gen.tab.explain"];
const WEIGHT_MAX: f64 = 10.0;
const SKETCH_H: f32 = 96.0;
const SEARCH_GLYPH_X: f32 = space::SM;
const SEARCH_TEXT_X: f32 = space::SM + ddc_ui::theme::icon_size::SM + space::XS;

/// A pending change of one rule: what the person moved, nothing else.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Edit {
    pub value: Option<f64>,
    pub tier: Option<Tier>,
    pub weight: Option<f64>,
}

impl Edit {
    /// The `set_rule` entry: a bare value, or the object when the tier or weight moved.
    pub fn to_json(&self, d: &RuleDef) -> J {
        let v = self.value.map(|x| d.value_json(x));
        if self.tier.is_none() && self.weight.is_none() {
            return v.unwrap_or(J::Null);
        }
        let mut o = Map::new();
        if let Some(v) = v {
            o.insert("value".into(), v);
        }
        if let Some(t) = self.tier {
            o.insert("tier".into(), json!(t.name()));
        }
        if let Some(w) = self.weight {
            o.insert("weight".into(), json!(w));
        }
        J::Object(o)
    }
}

/// A conflict as the window shows it, computed off the UI thread.
#[derive(Clone, Debug, Default)]
pub struct ConflictView {
    pub error: String,
    pub labels: Vec<String>,
    /// Rule, value, the fix (its reason renders in the UI language).
    pub fixes: Vec<(String, J, RuleFix)>,
}

/// One variant of the gallery.
#[derive(Clone, Debug)]
pub struct VarView {
    pub set: Vec<(String, J)>,
    pub penalty: f64,
    pub tiers: [f64; 4],
    /// Outlines to sketch, in metres: footprint first, then courtyard and podium.
    pub outlines: Vec<Vec<(f64, f64)>>,
}

/// The project the window works on.
struct Cur {
    pid: String,
    brief: J,
    rows: Vec<RuleRow>,
    error: Option<String>,
}

type Job<T> = mpsc::Receiver<Result<T, String>>;

#[derive(Default)]
pub struct Win {
    pub open: bool,
    tab: usize,
    /// 0: every group; else `rules::groups()[group - 1]`.
    group: usize,
    q: String,
    changed: bool,
    pub edits: BTreeMap<String, Edit>,
    /// Lock state the person wants, where it differs from the project.
    pub locks: BTreeMap<String, bool>,
    /// A project given by the layout probes instead of the stored one.
    fixed: Option<(String, J)>,
    at: Option<(String, u64)>,
    cur: Option<Cur>,
    conflict: Option<Result<Option<ConflictView>, String>>,
    conflict_job: Option<Job<Option<ConflictView>>>,
    vars: Option<Result<Vec<VarView>, String>>,
    vars_job: Option<Job<Vec<VarView>>>,
    seed: u64,
    why_at: Option<(Sel, u64)>,
    why: Option<Result<(String, Vec<Why>, Vec<String>), String>>,
    queue: Vec<(String, J)>,
    reply: Option<mpsc::Receiver<ToolOutcome>>,
    note: Option<(bool, String)>,
}

thread_local! {
    static WIN: RefCell<Win> = RefCell::new(Win::default());
}

/// The last applied project of the model: id and brief.
pub fn source(ed: &Editor) -> Option<(String, J)> {
    let s = crate::ai::project_plan::stored(&ed.model).ok()?;
    s.last().map(|(pid, v)| (pid.clone(), v["input"].clone()))
}

fn load(pid: String, brief: J) -> Cur {
    match gen_rules::build(&brief) {
        Ok(b) => Cur { rows: gen_rules::rows(&brief, &b.spec, Some(&b.score)), pid, brief, error: None },
        Err(e) => Cur { rows: vec![], pid, brief, error: Some(e) },
    }
}

/// A step that shows `span` in about a hundred moves, and the decimals it needs.
pub fn step_of(d: &RuleDef) -> (f64, usize) {
    if d.int || d.flag || !d.options.is_empty() {
        return (1.0, 0);
    }
    let span = (d.max - d.min).max(1e-6);
    let e = (span / 100.0).log10().floor();
    let step = 10f64.powf(e);
    (step, (-e).max(0.0) as usize)
}

/// The value the row shows: the pending one, the one in force, the default, the minimum.
pub fn shown(r: &RuleRow, e: Option<&Edit>) -> f64 {
    e.and_then(|e| e.value).or(r.value).or(r.def.default).unwrap_or(r.def.min)
}

/// The `set_rule`, `lock` and `unlock` calls of the pending changes, releases first so a rule
/// unlocked here can change, locks last so a rule locked here keeps its new value.
pub fn calls(pid: &str, edits: &BTreeMap<String, Edit>, locks: &BTreeMap<String, bool>) -> Vec<(String, J)> {
    let mut out = vec![];
    let targets = |on: bool| -> Vec<J> {
        locks.iter().filter(|l| *l.1 == on).map(|l| json!({"rule": l.0})).collect()
    };
    let off = targets(false);
    if !off.is_empty() {
        out.push(("unlock".to_string(), json!({"targets": off, "project": pid})));
    }
    let patch: Map<String, J> = edits
        .iter()
        .filter_map(|(id, e)| Some((id.clone(), e.to_json(rules::def(id)?))))
        .collect();
    if !patch.is_empty() {
        out.push((
            "set_rule".to_string(),
            json!({"rules": patch, "project": pid, "why": "Rules window"}),
        ));
    }
    let on = targets(true);
    if !on.is_empty() {
        out.push(("lock".to_string(), json!({"targets": on, "project": pid})));
    }
    out
}

/// The pending rule changes as a spec merge patch, for the conflict check.
fn what_if(edits: &BTreeMap<String, Edit>) -> Option<J> {
    let rules: Map<String, J> = edits
        .iter()
        .filter_map(|(id, e)| Some((id.clone(), e.to_json(rules::def(id)?))))
        .collect();
    (!rules.is_empty()).then(|| json!({ "rules": rules }))
}

pub fn conflict_of(brief: &J, changes: Option<&J>) -> Option<ConflictView> {
    gen_rules::conflicts(brief, changes).map(|c| ConflictView {
        error: c.error.clone(),
        labels: c.labels.clone(),
        fixes: c.fixes.iter().map(|f| (f.rule.clone(), f.value.clone(), f.clone())).collect(),
    })
}

/// A fix taken: its value becomes the pending value of its rule.
pub fn take_fix(edits: &mut BTreeMap<String, Edit>, rule: &str, value: &J) -> Result<(), String> {
    let d = rules::def(rule).ok_or(format!("no rule {rule}"))?;
    let x = d.parse_value(value).ok_or(format!("{rule}: must be {}", d.takes()))?;
    edits.entry(rule.to_string()).or_default().value = Some(x);
    Ok(())
}

pub fn variants_of(brief: &J, seed: u64) -> Result<Vec<VarView>, String> {
    let cfg = gen_rules::vary_cfg(brief, gen_rules::MAX_VARIANTS, seed, &[])?;
    let v = gen_rules::variants(brief, &cfg)?;
    Ok(v.chosen
        .iter()
        .map(|x| {
            let (w, d) = x.project.footprint;
            let mut outlines = vec![vec![(0.0, 0.0), (w, 0.0), (w, d), (0.0, d)]];
            outlines.extend(x.project.court.clone());
            outlines.extend(x.project.podium.clone());
            VarView { set: x.set.clone(), penalty: x.score.total(), tiers: x.score.tiers(), outlines }
        })
        .collect())
}

fn spawn<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Job<T> {
    let (tx, rx) = mpsc::channel();
    let tx_panic = tx.clone();
    let _ = crate::jobs::spawn_guarded(
        "ddc-gen-rules",
        move || {
            let _ = tx.send(f());
        },
        move |msg| {
            let _ = tx_panic.send(Err(msg));
        },
    );
    rx
}

fn poll<T>(job: &mut Option<Job<T>>, into: &mut Option<Result<T, String>>) {
    let Some(rx) = job else {
        return;
    };
    match rx.try_recv() {
        Ok(r) => {
            *into = Some(r);
            *job = None;
        }
        Err(mpsc::TryRecvError::Disconnected) => {
            *into = Some(Err("the check stopped".into()));
            *job = None;
        }
        Err(mpsc::TryRecvError::Empty) => {}
    }
}

/// What the person sees of a tool answer: its reason, or its error.
pub fn note_of(o: &ToolOutcome) -> String {
    let v: J = serde_json::from_str(&o.text).unwrap_or(J::Null);
    let pick = if o.is_error { ["error", "why"] } else { ["why", "error"] };
    pick.iter()
        .find_map(|k| v[*k].as_str().map(str::to_string))
        .unwrap_or_else(|| o.text.chars().take(240).collect())
}

/// A reason's effect in the UI language: `gen.why.e.<key>` with its slots, the English text only
/// when no catalog in the fallback chain has the key.
pub fn effect_text(cat: &Catalog, y: &Why) -> String {
    keyed_text(cat, y.key, &y.args, &y.effect)
}

/// The reason of a proposed fix in the UI language; free text (a brief's words) as written.
pub fn fix_text(cat: &Catalog, f: &RuleFix) -> String {
    keyed_text(cat, f.key, &f.args, &f.why)
}

fn keyed_text(cat: &Catalog, key: &str, args: &[(&'static str, String)], english: &str) -> String {
    let k = format!("gen.why.e.{key}");
    if key.is_empty() || cat.t(&k) == k {
        return english.to_string();
    }
    let args: Vec<(&str, &str)> = args.iter().map(|(a, v)| (*a, v.as_str())).collect();
    cat.tf(&k, &args)
}

fn rule_title(cat: &Catalog, id: &str) -> String {
    let k = format!("gen.rule.{id}.title");
    if cat.table(cat.lang).contains_key(&k) {
        cat.t(&k)
    } else {
        id.to_string()
    }
}

fn value_text(d: &RuleDef, v: &J, cat: &Catalog) -> String {
    let nf = NumberFormat::for_lang(cat.lang);
    match d.parse_value(v) {
        Some(x) if !d.options.is_empty() => d.options.get(x as usize).cloned().unwrap_or_default(),
        Some(x) if d.flag => cat.t(if x > 0.5 { "gen.yes" } else { "gen.no" }),
        Some(x) => {
            let n = widgets::field::format_number(x, step_of(d).1, nf);
            if d.unit.is_empty() { n } else { format!("{n} {}", d.unit) }
        }
        None => v.to_string(),
    }
}

fn sync(w: &mut Win, ed: &Editor) {
    let src = w.fixed.clone().or_else(|| source(ed));
    let key = src.as_ref().map(|s| (s.0.clone(), ed.model.change_head()));
    if key == w.at {
        return;
    }
    w.at = key;
    // the window builds and compares only: it reads the terrain the model has now
    w.cur = src.map(|(pid, brief)| load(pid, crate::ai::project_plan::with_model_terrain(&ed.model, &brief)));
    w.conflict = None;
    w.vars = None;
    if let Some(c) = &w.cur {
        if c.error.is_some() {
            let brief = c.brief.clone();
            w.conflict_job = Some(spawn(move || Ok(conflict_of(&brief, None))));
        }
    }
}

fn rules_tab(ui: &mut egui::Ui, w: &mut Win, cur: &Cur, cat: &Catalog) {
    let th = ddc_ui::theme::of(ui.ctx());
    let groups = rules::groups();
    let names: Vec<String> = std::iter::once(cat.t("gen.group.all"))
        .chain(groups.iter().map(|g| cat.t(&format!("gen.group.{g}"))))
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    ui.horizontal_wrapped(|ui| {
        widgets::dropdown(ui, &th, "gen_rules.group", &mut w.group, &refs, Some(200.0));
        ui.add_space(space::SM);
        widgets::toggle(ui, &th, &mut w.changed, &cat.t("gen.changed_only"));
    });
    ui.add_space(space::XS);
    let (hint, tip) = (cat.t("gen.search"), cat.t("gen.search.clear"));
    let wd = ui.available_width();
    let _ = TextField::new(&mut w.q)
        .id_salt("gen_rules.q")
        .labelled_by(egui::Id::new("gen_rules.q.lbl"), &hint)
        .hint(&hint)
        .width(wd)
        .search("find", SEARCH_GLYPH_X, SEARCH_TEXT_X, &tip)
        .show(ui, &th);
    ui.add_space(space::SM);
    let group = w.group.checked_sub(1).and_then(|i| groups.get(i).copied());
    let q = w.q.trim().to_lowercase();
    let shown_rows: Vec<&RuleRow> = gen_rules::filter(&cur.rows, group, "", w.changed)
        .into_iter()
        .filter(|r| {
            q.is_empty()
                || r.def.id.to_lowercase().contains(&q)
                || rule_title(cat, &r.def.id).to_lowercase().contains(&q)
        })
        .collect();
    if shown_rows.is_empty() {
        kit::empty_note(ui, cat.t("gen.none_found"), None);
    }
    let nf = NumberFormat::for_lang(cat.lang);
    for r in shown_rows {
        rule_row(ui, &th, w, r, cat, nf);
    }
}

fn rule_row(ui: &mut egui::Ui, th: &ddc_ui::Theme, w: &mut Win, r: &RuleRow, cat: &Catalog, nf: NumberFormat) {
    let d = r.def;
    let id = d.id.as_str();
    let title = rule_title(cat, id);
    ui.push_id(id, |ui| {
        ui.horizontal(|ui| {
            let was = w.locks.get(id).copied().unwrap_or(r.locked);
            let mut on = was;
            widgets::toggle_bare(ui, th, &mut on, &cat.tf("gen.lock", &[("rule", &title)]));
            if on != was {
                if on == r.locked {
                    w.locks.remove(id);
                } else {
                    w.locks.insert(id.to_string(), on);
                }
            }
            ui.add(egui::Label::new(egui::RichText::new(&title).strong()).truncate());
            widgets::badge(ui, th, &cat.t(&format!("gen.src.{}", r.source)), Tone::Neutral);
            if r.limiting() {
                let c = widgets::field::format_number(r.cost, 3, nf);
                widgets::badge(ui, th, &cat.tf("gen.cost", &[("cost", &c)]), Tone::Warning);
            }
        });
        let locked = w.locks.get(id).copied().unwrap_or(r.locked);
        let before = w.edits.get(id).copied();
        let mut e = before.unwrap_or_default();
        let mut x = shown(r, before.as_ref());
        ui.add_enabled_ui(!locked, |ui| {
            if !d.options.is_empty() {
                let opts: Vec<&str> = d.options.iter().map(String::as_str).collect();
                let was = (x.max(0.0) as usize).min(opts.len() - 1);
                let mut i = was;
                widgets::dropdown(ui, th, ("gen_rules.opt", id), &mut i, &opts, Some(200.0));
                if i != was {
                    e.value = Some(i as f64);
                }
            } else if d.flag {
                let was = x > 0.5;
                let mut on = was;
                widgets::toggle(ui, th, &mut on, &title);
                if on != was {
                    e.value = Some(if on { 1.0 } else { 0.0 });
                }
            } else {
                let (step, dec) = step_of(d);
                if widgets::field::slider(ui, th, ("gen_rules.v", id), &mut x, (d.min, d.max), step, dec, nf) {
                    e.value = Some(x);
                }
            }
            let range = format!(
                "{} … {} {}",
                widgets::field::format_number(d.min, step_of(d).1, nf),
                widgets::field::format_number(d.max, step_of(d).1, nf),
                d.unit
            );
            kit::hint(ui, cat.tf("gen.range", &[("range", range.trim())]));
            if let Some(t) = r.tier {
                let tiers: Vec<String> = Tier::ALL.iter().map(|t| cat.t(&format!("gen.tier.{}", t.name()))).collect();
                let refs: Vec<&str> = tiers.iter().map(String::as_str).collect();
                let was = Tier::ALL.iter().position(|x| *x == e.tier.unwrap_or(t)).unwrap_or(0);
                let mut i = was;
                ui.horizontal(|ui| {
                    widgets::dropdown(ui, th, ("gen_rules.tier", id), &mut i, &refs, Some(140.0));
                    if i != was {
                        e.tier = Some(Tier::ALL[i]);
                    }
                    ui.label(cat.t("gen.weight"));
                    let mut wt = e.weight.unwrap_or(r.weight);
                    if widgets::field::slider(ui, th, ("gen_rules.w", id), &mut wt, (0.0, WEIGHT_MAX), 0.5, 1, nf) {
                        e.weight = Some(wt);
                    }
                });
            }
        });
        let desc = format!("gen.rule.{id}.desc");
        if cat.table(cat.lang).contains_key(&desc) {
            kit::hint(ui, cat.t(&desc));
        }
        if Some(e) != before && e != Edit::default() {
            w.edits.insert(id.to_string(), e);
        }
        ui.separator();
    });
}

fn conflicts_tab(ui: &mut egui::Ui, w: &mut Win, cur: &Cur, cat: &Catalog) {
    kit::hint(ui, cat.t("gen.conflict.about"));
    let busy = w.conflict_job.is_some();
    if kit::secondary_button(ui, cat.t(if busy { "gen.checking" } else { "gen.conflict.check" })).clicked() && !busy {
        let (brief, changes) = (cur.brief.clone(), what_if(&w.edits));
        w.conflict_job = Some(spawn(move || Ok(conflict_of(&brief, changes.as_ref()))));
    }
    ui.add_space(space::SM);
    match &w.conflict {
        None => {}
        Some(Err(e)) => kit::empty_note(ui, cat.t("gen.failed"), Some(e.as_str())),
        Some(Ok(None)) => kit::empty_note(ui, cat.t("gen.conflict.none"), None),
        Some(Ok(Some(c))) => {
            let c = c.clone();
            kit::section_title(ui, cat.t("gen.conflict.title"));
            ui.add(egui::Label::new(c.error.as_str()).wrap());
            for l in &c.labels {
                ui.label(format!("• {l}"));
            }
            kit::section_title(ui, cat.t("gen.conflict.fixes"));
            for (i, (rule, value, fix)) in c.fixes.iter().enumerate() {
                let why = fix_text(cat, fix);
                let Some(d) = rules::def(rule) else {
                    continue;
                };
                ui.push_id(("gen_rules.fix", i), |ui| {
                    ui.horizontal_wrapped(|ui| {
                        let what = format!("{} → {}", rule_title(cat, rule), value_text(d, value, cat));
                        ui.label(egui::RichText::new(&what).strong());
                        if kit::secondary_button(ui, cat.t("gen.fix.use")).clicked() && take_fix(&mut w.edits, rule, value).is_ok() {
                            w.conflict = None;
                            w.tab = 0;
                        }
                    });
                    kit::hint(ui, why.as_str());
                });
            }
        }
    }
}

fn sketch(ui: &mut egui::Ui, v: &VarView) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), SKETCH_H), egui::Sense::hover());
    let pts: Vec<(f64, f64)> = v.outlines.iter().flatten().copied().collect();
    if pts.is_empty() {
        return;
    }
    let (x0, x1) = pts.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
    let (y0, y1) = pts.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
    let s = ((rect.width() - 8.0) as f64 / (x1 - x0).max(1e-6)).min((rect.height() - 8.0) as f64 / (y1 - y0).max(1e-6));
    let c = rect.center();
    let at = |p: &(f64, f64)| {
        egui::pos2(c.x + ((p.0 - (x0 + x1) / 2.0) * s) as f32, c.y - ((p.1 - (y0 + y1) / 2.0) * s) as f32)
    };
    let ink = ui.visuals().text_color();
    let p = ui.painter_at(rect);
    for (i, o) in v.outlines.iter().enumerate() {
        let line: Vec<egui::Pos2> = o.iter().map(at).collect();
        let width = if i == 0 { 1.5 } else { 1.0 };
        p.add(egui::Shape::closed_line(line, egui::Stroke::new(width, ink)));
    }
}

fn variants_tab(ui: &mut egui::Ui, w: &mut Win, cur: &Cur, cat: &Catalog) {
    kit::hint(ui, cat.t("gen.variants.about"));
    let busy = w.vars_job.is_some();
    if kit::secondary_button(ui, cat.t(if busy { "gen.checking" } else { "gen.variants.find" })).clicked() && !busy {
        let (brief, seed) = (cur.brief.clone(), w.seed);
        w.vars_job = Some(spawn(move || variants_of(&brief, seed)));
    }
    ui.add_space(space::SM);
    let vars = match &w.vars {
        None => return,
        Some(Err(e)) => return kit::empty_note(ui, cat.t("gen.failed"), Some(e.as_str())),
        Some(Ok(v)) if v.is_empty() => return kit::empty_note(ui, cat.t("gen.variants.none"), None),
        Some(Ok(v)) => v.clone(),
    };
    let nf = NumberFormat::for_lang(cat.lang);
    let mut pick = None;
    ui.columns(vars.len(), |cols| {
        for (i, (ui, v)) in cols.iter_mut().zip(&vars).enumerate() {
            ui.push_id(("gen_rules.var", i), |ui| {
                kit::section_title(ui, cat.tf("gen.variant", &[("n", &(i + 1).to_string())]));
                sketch(ui, v);
                let pen = widgets::field::format_number(v.penalty, 3, nf);
                ui.label(cat.tf("gen.penalty", &[("value", &pen)]));
                for (rule, val) in &v.set {
                    let Some(d) = rules::def(rule) else {
                        continue;
                    };
                    let differs = vars[0].set.iter().find(|b| b.0 == *rule).map(|b| &b.1) != Some(val);
                    let t = format!("{}: {}", rule_title(cat, rule), value_text(d, val, cat));
                    ui.add(egui::Label::new(if differs { egui::RichText::new(t).strong() } else { t.into() }).wrap());
                }
                if v.tiers[0] > 0.0 {
                    let th = ddc_ui::theme::of(ui.ctx());
                    widgets::badge(ui, &th, &cat.t("gen.variant.hard_miss"), Tone::Danger);
                }
                if kit::secondary_button(ui, cat.t("gen.variant.use")).clicked() {
                    pick = Some(i);
                }
            });
        }
    });
    if let Some(i) = pick {
        w.queue.push((
            "regenerate".into(),
            json!({"variant": i + 1, "seed": w.seed, "project": cur.pid, "why": "Rules window"}),
        ));
    }
}

fn explain_tab(ui: &mut egui::Ui, w: &mut Win, ed: &Editor, cat: &Catalog) {
    let one = match ed.selection[..] {
        [s] => Some(s),
        _ => None,
    };
    let Some(s) = one else {
        return kit::empty_note(ui, cat.t("gen.why.none"), Some(&cat.t("gen.why.none_hint")));
    };
    let key = (s, ed.model.change_head());
    if w.why_at != Some(key) {
        w.why_at = Some(key);
        w.why = Some(why_of(ed, s));
    }
    let (pid, reasons, refs) = match &w.why {
        Some(Err(e)) => return kit::empty_note(ui, cat.t("gen.why.not_generated"), Some(e.as_str())),
        Some(Ok(x)) => x.clone(),
        None => return,
    };
    let th = ddc_ui::theme::of(ui.ctx());
    for y in &reasons {
        let title = rule_title(cat, &y.rule);
        let v = rules::def(&y.rule).map_or_else(|| y.value.to_string(), |d| value_text(d, &y.value, cat));
        kit::section_title(ui, format!("{title}: {v}"));
        ui.horizontal(|ui| {
            widgets::badge(ui, &th, &cat.t(&format!("gen.src.{}", y.source)), Tone::Neutral);
        });
        ui.add(egui::Label::new(effect_text(cat, y)).wrap());
        ui.add_space(space::XS);
    }
    let storey = refs.iter().find_map(|r| r.strip_prefix("storey:")?.parse::<u32>().ok());
    if let Some(k) = storey {
        if kit::secondary_button(ui, cat.tf("gen.lock_storey", &[("n", &k.to_string())])).clicked() {
            w.queue.push((
                "lock".into(),
                json!({"targets": [{"storey": k, "keep": "geometry"}], "project": pid}),
            ));
        }
    }
}

/// The project that built a selected element, the reasons behind it and the refs they came from.
pub fn why_of(ed: &Editor, s: Sel) -> Result<(String, Vec<Why>, Vec<String>), String> {
    let door = |id: i64| match ed.model.opening_kind(id).as_deref() {
        Ok("window") => "window",
        _ => "door",
    };
    let k = crate::ai::rows::key_of_sel(s, door).ok_or("this element is not generated")?;
    let (pid, brief, refs) = gen_rules::element_refs(&ed.model, k.cat, k.id)?;
    let b = gen_rules::build(&crate::ai::project_plan::with_model_terrain(&ed.model, &brief))?;
    Ok((pid, gen_rules::explain(&b.spec, &b.project, &refs), refs))
}

enum Act {
    None,
    Close,
}

/// Window body (apart from the frame, for the layout probes).
fn body(ui: &mut egui::Ui, w: &mut Win, ed: &Editor, cat: &Catalog) -> Act {
    sync(w, ed);
    let Some(cur) = w.cur.take() else {
        kit::empty_note(ui, cat.t("gen.none"), Some(&cat.t("gen.none_hint")));
        let mut act = Act::None;
        kit::dialog_footer(ui, |ui| {
            if kit::cancel_button(ui, cat.t("dlg.close")).clicked() {
                act = Act::Close;
            }
        });
        return act;
    };
    let th = ddc_ui::theme::of(ui.ctx());
    let tabs: Vec<String> = TABS.iter().map(|k| cat.t(k)).collect();
    let refs: Vec<&str> = tabs.iter().map(String::as_str).collect();
    widgets::tab_bar(ui, &th, &refs, &mut w.tab);
    ui.add_space(space::SM);
    if let Some(e) = &cur.error {
        kit::empty_note(ui, cat.t("gen.build_failed"), Some(e.as_str()));
    }
    match w.tab {
        1 => conflicts_tab(ui, w, &cur, cat),
        2 => variants_tab(ui, w, &cur, cat),
        3 => explain_tab(ui, w, ed, cat),
        _ => rules_tab(ui, w, &cur, cat),
    }
    if let Some((bad, text)) = &w.note {
        ui.add_space(space::SM);
        let tone = if *bad { Tone::Danger } else { Tone::Success };
        widgets::badge(ui, &th, &cat.t(if *bad { "gen.refused" } else { "gen.drafted" }), tone);
        ui.add(egui::Label::new(text.as_str()).wrap());
    }
    let pending = w.edits.len() + w.locks.len();
    let mut act = Act::None;
    kit::dialog_footer(ui, |ui| {
        let busy = w.reply.is_some() || !w.queue.is_empty();
        if kit::primary_button(ui, cat.tf("gen.preview", &[("n", &pending.to_string())]), pending > 0 && !busy).clicked() {
            w.queue.extend(calls(&cur.pid, &w.edits, &w.locks));
            w.note = None;
        }
        if kit::secondary_button(ui, cat.t("gen.reset")).clicked() {
            w.edits.clear();
            w.locks.clear();
        }
        if kit::cancel_button(ui, cat.t("dlg.close")).clicked() {
            act = Act::Close;
        }
    });
    w.cur = Some(cur);
    act
}

/// Sends the next queued call; its draft waits in the AI panel.
fn send_next(a: &mut EditorApp, w: &mut Win) {
    if w.reply.is_some() || w.queue.is_empty() {
        return;
    }
    if let Err(e) = crate::ai::mcp_bridge::ensure_tool_session(a) {
        w.queue.clear();
        w.note = Some((true, e));
        return;
    }
    let (name, args) = w.queue.remove(0);
    let (tx, rx) = mpsc::channel();
    let call = McpCall::new(CLIENT, name, args, tx);
    crate::ai::session::with_session(|s| {
        if let Some(u) = s.as_mut() {
            u.mcp_call(&a.ed, call, None);
        }
    });
    w.reply = Some(rx);
}

fn take_reply(a: &mut EditorApp, w: &mut Win) {
    let Some(rx) = &w.reply else {
        return;
    };
    let out = match rx.try_recv() {
        Ok(o) => o,
        Err(mpsc::TryRecvError::Empty) => return,
        Err(mpsc::TryRecvError::Disconnected) => ToolOutcome::error("the AI session ended".to_string()),
    };
    w.reply = None;
    w.note = Some((out.is_error, note_of(&out)));
    if out.is_error {
        w.queue.clear();
    } else {
        w.edits.clear();
        w.locks.clear();
        if w.queue.is_empty() {
            crate::ai::ui::open_panel(a);
        }
    }
}

fn run(_: &mut EditorApp) {
    WIN.with(|w| {
        let mut w = w.borrow_mut();
        w.open = true;
        w.at = None;
    });
}

/// Opens on `brief` as project `P1` (none: on the model's own) at `tab`.
pub fn open_for_layout(brief: Option<J>, tab: usize) {
    WIN.with(|w| {
        *w.borrow_mut() = Win { open: true, tab, fixed: brief.map(|b| ("P1".to_string(), b)), ..Win::default() }
    });
}

/// Called every frame.
pub fn frame(a: &mut EditorApp, ctx: &egui::Context) {
    let mut w = WIN.with(|w| std::mem::take(&mut *w.borrow_mut()));
    if !w.open {
        return;
    }
    take_reply(a, &mut w);
    poll(&mut w.conflict_job, &mut w.conflict);
    poll(&mut w.vars_job, &mut w.vars);
    let (mut open, mut act) = (true, Act::None);
    kit::Dialog::new(KEY, a.sh.cat.t("cmd.gen.rules"), kit::DialogSize::Wide)
        .open(&mut open)
        .scroll_body()
        .show(ctx, |ui| act = body(ui, &mut w, &a.ed, &a.sh.cat));
    w.open = open && !matches!(act, Act::Close);
    send_next(a, &mut w);
    if w.reply.is_some() || w.conflict_job.is_some() || w.vars_job.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
    WIN.with(|s| *s.borrow_mut() = w);
}

pub fn register(a: &mut EditorApp) {
    a.register_command("gen.rules", run);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn office() -> J {
        json!({"typology": "office", "floors": 2})
    }

    #[test]
    fn steps_follow_the_range() {
        let d = rules::def("grid.span").unwrap();
        let (step, dec) = step_of(d);
        assert!(step > 0.0 && step <= (d.max - d.min) / 50.0, "{step}");
        assert!(dec >= 1, "{dec}");
        for d in rules::defs().iter().filter(|d| d.int || d.flag || !d.options.is_empty()) {
            assert_eq!(step_of(d), (1.0, 0), "{}", d.id);
        }
    }

    #[test]
    fn pending_changes_become_unlock_set_lock_in_that_order() {
        let mut edits = BTreeMap::new();
        edits.insert("grid.span".to_string(), Edit { value: Some(6.0), ..Edit::default() });
        edits.insert("goal.compact".to_string(), Edit { weight: Some(4.0), tier: Some(Tier::Strong), value: None });
        let mut locks = BTreeMap::new();
        locks.insert("depth.band".to_string(), false);
        locks.insert("grid.span".to_string(), true);
        let c = calls("P1", &edits, &locks);
        let names: Vec<&str> = c.iter().map(|c| c.0.as_str()).collect();
        assert_eq!(names, ["unlock", "set_rule", "lock"]);
        assert_eq!(c[1].1["rules"]["grid.span"], json!(6.0));
        assert_eq!(c[1].1["rules"]["goal.compact"], json!({"tier": "strong", "weight": 4.0}));
        assert_eq!(c[2].1["targets"], json!([{"rule": "grid.span"}]));
        assert!(c.iter().all(|c| c.1["project"] == "P1"));
        // each call passes the argument check of its tool
        for (n, args) in &c {
            let spec = ddc_ai::catalog::find(n).unwrap();
            assert_eq!(ddc_ai::catalog::validate(spec, args), Ok(()), "{n} {args}");
        }
        assert!(calls("P1", &BTreeMap::new(), &BTreeMap::new()).is_empty());
    }

    #[test]
    fn a_fix_taken_in_the_window_resolves_its_conflict() {
        let mut brief = office();
        brief["rules"] = json!({"goal.efficiency": {"value": 0.98, "hard": true}});
        let c = conflict_of(&brief, None).expect("an efficiency nobody can build conflicts");
        assert!(!c.fixes.is_empty(), "{c:?}");
        let (rule, value, _) = &c.fixes[0];
        let mut edits = BTreeMap::new();
        take_fix(&mut edits, rule, value).unwrap();
        let mut patched = brief.clone();
        ddc_gen::project::merge_patch(&mut patched, &what_if(&edits).unwrap());
        assert!(conflict_of(&patched, None).is_none(), "{rule} = {value} leaves a conflict");
        assert!(take_fix(&mut edits, "no.such", &json!(1)).is_err());
    }

    #[test]
    fn variants_sketch_their_footprint() {
        let v = variants_of(&office(), 1).unwrap();
        assert!(!v.is_empty() && v.len() <= gen_rules::MAX_VARIANTS);
        for x in &v {
            assert!(x.outlines[0].len() == 4 && x.penalty.is_finite());
        }
    }

    #[test]
    fn tool_answers_show_their_reason_or_error() {
        let ok = ToolOutcome { text: json!({"why": "grid set"}).to_string(), images: vec![], is_error: false, flagged: 0 };
        assert_eq!(note_of(&ok), "grid set");
        let bad = ToolOutcome { text: json!({"error": "locked"}).to_string(), images: vec![], is_error: true, flagged: 0 };
        assert_eq!(note_of(&bad), "locked");
    }
}
