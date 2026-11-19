//! What the scratch model looks like with the plan on it, against the base: new problems only,
//! so an old model full of warnings still gets an honest verdict for this change.

use super::plan::{ElemDiff, PlanItem, QtyDelta};
use crate::editor::Editor;
use ddc_ai::catalog::Tier;
use ddc_model::checks::{CheckRule, CheckScope, Finding};
use ddc_model::{
    ElKey, ElementWarning, Error, Issue, MaterialQty, Model, OpeningSpec, RowKind, Totals,
};
use std::collections::{HashMap, HashSet};

/// More touched elements than this needs an explicit "Allow large": a slip here is expensive.
/// The default of a project rule the person can change (`PlanCaps`).
pub const LARGE_PLAN: usize = 500;
/// More deleted elements than this needs "Allow large" too.
pub const LARGE_DELETE: usize = 50;
/// More touched elements than this is refused: the change list of the card stops there
/// (`DIFF_LIMIT`), so a larger plan could not be shown or verified.
pub const REFUSED_PLAN: usize = 150_000;
/// The highest the refusal limit can be set: just below the change list's size.
pub const REFUSED_PLAN_MAX: usize = 199_999;
const MAX_RULE_FINDINGS: usize = 200;
/// Below this many deleted elements the shares do not apply: a 9-wall house loses a wall at 11 %,
/// and a person's ordinary edits would all need "Allow large". The card names 50 for categories.
pub const MASS_MIN: usize = 50;
/// More than this share of one category deleted is a mass deletion.
pub const MASS_CATEGORY_PERCENT: usize = 25;
/// More than this share of all built elements deleted is a mass deletion.
pub const MASS_MODEL_PERCENT: usize = 10;
/// A model of at least this many built elements is never left empty by one plan: smaller scratch
/// models are cleared by hand all the time (the card's literal rule would block those too).
pub const WIPE_FLOOR: usize = 10;

/// The size rule of a project: how many touched or deleted elements need the person's
/// "Allow large", and where a plan is refused. Stored in the project's `ai_policy`, so the
/// person steers it (`set`); 0 switches a limit off. The defaults are the constants above.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanCaps {
    pub confirm_above: usize,
    pub delete_above: usize,
    pub refuse_above: usize,
}

impl Default for PlanCaps {
    fn default() -> Self {
        PlanCaps {
            confirm_above: LARGE_PLAN,
            delete_above: LARGE_DELETE,
            refuse_above: REFUSED_PLAN,
        }
    }
}

impl PlanCaps {
    pub const KEYS: [&'static str; 3] = ["plan.confirm_above", "plan.delete_above", "plan.refuse_above"];

    /// The project's rule; a missing or unreadable value keeps the default.
    pub fn read(m: &Model) -> PlanCaps {
        let d = PlanCaps::default();
        let get = |k: &str, dflt: usize| match m.ai_policy(k) {
            Ok(Some(s)) => match s.trim().parse::<u64>() {
                Ok(0) => usize::MAX,
                Ok(n) => usize::try_from(n).unwrap_or(usize::MAX),
                Err(_) => dflt,
            },
            _ => dflt,
        };
        PlanCaps {
            confirm_above: get(Self::KEYS[0], d.confirm_above),
            delete_above: get(Self::KEYS[1], d.delete_above),
            // the wall can be moved, never removed: a plan past the card's change list
            // (`DIFF_LIMIT`) cannot be shown or verified
            refuse_above: get(Self::KEYS[2], d.refuse_above).min(REFUSED_PLAN_MAX),
        }
    }

    /// Sets one limit of the project (`KEYS[i]`): a number of elements, 0 = never.
    pub fn set(m: &Model, key: &str, n: u64) -> Result<(), Error> {
        if !Self::KEYS.contains(&key) {
            return Err(Error::Invalid(format!("no plan limit {key}")));
        }
        if key == Self::KEYS[2] && n == 0 {
            return Err(Error::Invalid("plan.refuse_above cannot be switched off".into()));
        }
        m.set_ai_policy(key, &n.to_string())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueLevel {
    Block,
    Warn,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlanIssue {
    pub level: IssueLevel,
    pub item: Option<u32>,
    pub el: Option<ElKey>,
    pub text_key: &'static str,
    pub args: Vec<String>,
}

fn issue(
    level: IssueLevel,
    item: Option<u32>,
    el: Option<ElKey>,
    text_key: &'static str,
    args: Vec<String>,
) -> PlanIssue {
    PlanIssue {
        level,
        item,
        el,
        text_key,
        args,
    }
}

/// The base state, captured before the first change.
#[derive(Clone, Debug)]
pub struct Baseline {
    pub issues: Vec<Issue>,
    pub wall_warn: Vec<ElementWarning>,
    pub cut_warn: Vec<ElementWarning>,
    pub totals: Totals,
    pub materials: Vec<MaterialQty>,
    /// (room, room it duplicates) before the plan: only new pairs are told.
    pub room_dups: Vec<(i64, i64)>,
    /// Material names the project had before the plan.
    pub material_names: Vec<String>,
    /// Overlapping opening pairs (lower id first) that already exist in the base model, with the
    /// geometry of both: a pair is tolerated only while neither opening has changed.
    pub overlaps: HashMap<(i64, i64), [Box4; 2]>,
}

impl Baseline {
    pub fn capture(m: &Model) -> Result<Baseline, Error> {
        let mut overlaps = HashMap::new();
        let walls: Vec<i64> = {
            let mut st = m.sql().prepare("SELECT DISTINCT wall FROM opening")?;
            let rows = st.query_map([], |r| r.get::<_, i64>(0))?;
            rows.collect::<Result<_, _>>()?
        };
        for w in walls {
            let ops = read_openings(m, w)?;
            for (a, b) in overlap_pairs(&ops) {
                if let (Some(x), Some(y)) = (box4_of(&ops, a), box4_of(&ops, b)) {
                    let (lo, hi) = pair_key(a, b);
                    overlaps.insert((lo, hi), if lo == a { [x, y] } else { [y, x] });
                }
            }
        }
        Ok(Baseline {
            overlaps,
            issues: m.check_integrity()?,
            wall_warn: m.wall_warnings()?,
            cut_warn: m.cut_warnings()?,
            totals: m.model_totals()?,
            materials: m.material_totals(None)?,
            room_dups: m.room_duplicates()?,
            material_names: super::materials::names(m),
        })
    }
}

fn new_warnings(
    now: Vec<ElementWarning>,
    was: &[ElementWarning],
    text_key: &'static str,
    out: &mut Vec<PlanIssue>,
) {
    let known: HashSet<(&str, i64, &str)> = was
        .iter()
        .map(|w| (w.table, w.id, w.message.as_str()))
        .collect();
    for w in now {
        if !known.contains(&(w.table, w.id, w.message.as_str())) {
            out.push(issue(
                IssueLevel::Warn,
                None,
                Some((w.table.to_string(), w.id)),
                text_key,
                vec![w.message],
            ));
        }
    }
}

/// Openings of changed walls can end up outside a shorter host, so those are checked too.
fn opening_issues(
    m: &Model,
    diff: &[ElemDiff],
    base_overlaps: &HashMap<(i64, i64), [Box4; 2]>,
    out: &mut Vec<PlanIssue>,
) -> Result<(), Error> {
    let mut walls: HashSet<i64> = HashSet::new();
    for e in diff.iter().filter(|e| e.kind != RowKind::Deleted) {
        match e.key.0.as_str() {
            "wall" => {
                walls.insert(e.key.1);
            }
            "opening" => {
                let host =
                    m.sql()
                        .query_row("SELECT wall FROM opening WHERE id=?1", [e.key.1], |r| {
                            r.get::<_, i64>(0)
                        });
                if let Ok(w) = host {
                    walls.insert(w);
                }
            }
            _ => {}
        }
    }
    let item_of: HashMap<&ElKey, Option<u32>> = diff.iter().map(|e| (&e.key, e.item)).collect();
    let mut sorted: Vec<i64> = walls.into_iter().collect();
    sorted.sort_unstable();
    for w in sorted {
        let ops = read_openings(m, w)?;
        for (id, o) in ops.iter() {
            let key = ("opening".to_string(), *id);
            let item = item_of.get(&key).copied().flatten();
            if let Err(e) = m.opening_fits(w, o) {
                out.push(issue(
                    IssueLevel::Block,
                    item,
                    Some(key.clone()),
                    "ai.issue.opening_outside",
                    vec![e.to_string()],
                ));
            }
        }
        for (a, b) in overlap_pairs(&ops) {
            if unchanged_baseline_pair(base_overlaps, &ops, a, b) {
                continue;
            }
            // Blame the opening the plan touched (the other one is named in the args).
            let (blamed, other) = if item_of.contains_key(&("opening".to_string(), b))
                && !item_of.contains_key(&("opening".to_string(), a))
            {
                (b, a)
            } else {
                (a, b)
            };
            let key = ("opening".to_string(), blamed);
            let item = item_of.get(&key).copied().flatten();
            out.push(issue(
                IssueLevel::Block,
                item,
                Some(key),
                "ai.issue.opening_overlap",
                vec![other.to_string()],
            ));
        }
    }
    Ok(())
}

/// along, width, sill, height of an opening.
pub type Box4 = [f64; 4];

fn box4_of(ops: &[(i64, OpeningSpec)], id: i64) -> Option<Box4> {
    ops.iter()
        .find(|(i, _)| *i == id)
        .map(|(_, o)| [o.along, o.width, o.sill, o.height])
}

/// A pair that overlapped before the plan is old news only while both openings still have their
/// base geometry; moving or widening either one makes it the plan's doing.
fn unchanged_baseline_pair(
    base: &HashMap<(i64, i64), [Box4; 2]>,
    ops: &[(i64, OpeningSpec)],
    a: i64,
    b: i64,
) -> bool {
    let (lo, hi) = pair_key(a, b);
    let Some(was) = base.get(&(lo, hi)) else {
        return false;
    };
    let (Some(x), Some(y)) = (box4_of(ops, lo), box4_of(ops, hi)) else {
        return false;
    };
    let same = |p: &Box4, q: &Box4| p.iter().zip(q).all(|(u, v)| (u - v).abs() < 1e-6);
    same(&x, &was[0]) && same(&y, &was[1])
}

fn pair_key(a: i64, b: i64) -> (i64, i64) {
    (a.min(b), a.max(b))
}

/// A row that cannot be read is an error, never silently skipped.
fn read_openings(m: &Model, wall: i64) -> Result<Vec<(i64, OpeningSpec)>, Error> {
    let mut st = m.sql().prepare(
        "SELECT id, along, width, sill, height, kind FROM opening WHERE wall=?1 ORDER BY along, id",
    )?;
    let rows = st.query_map([wall], |r| {
        Ok((
            r.get(0)?,
            OpeningSpec {
                along: r.get(1)?,
                width: r.get(2)?,
                sill: r.get(3)?,
                height: r.get(4)?,
                kind: r.get(5)?,
            },
        ))
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Every overlapping pair, not just neighbours: the list is sorted by `along`, so the sweep
/// stops once a later opening starts past the end of this one. Openings stacked in height
/// (a transom above a door) do not overlap.
fn overlap_pairs(ops: &[(i64, OpeningSpec)]) -> Vec<(i64, i64)> {
    let mut v = vec![];
    for (i, (a, o)) in ops.iter().enumerate() {
        for (b, p) in &ops[i + 1..] {
            if p.along >= o.along + o.width - 1e-6 {
                break;
            }
            let apart_up = p.sill >= o.sill + o.height - 1e-6 || o.sill >= p.sill + p.height - 1e-6;
            if !apart_up {
                v.push((*a, *b));
            }
        }
    }
    v
}

/// A room created or moved by the plan with no measured area has no closed boundary.
/// A room in a region another room holds is told as such, not as open.
fn room_issues(m: &Model, base: &Baseline, diff: &[ElemDiff], out: &mut Vec<PlanIssue>) {
    let dups = m.room_duplicates().unwrap_or_default();
    for e in diff
        .iter()
        .filter(|e| e.kind != RowKind::Deleted && e.key.0 == "room")
    {
        // a duplicate the model already had (rename-only plan) is not the plan's doing
        if dups.iter().any(|d| d.0 == e.key.1 && base.room_dups.contains(d)) {
            continue;
        }
        if let Some((_, k)) = dups.iter().find(|d| d.0 == e.key.1) {
            out.push(issue(
                IssueLevel::Warn,
                e.item,
                Some(e.key.clone()),
                "ai.issue.room_dup",
                vec![k.to_string()],
            ));
        } else if !crate::room_watch::is_enclosed(m, e.key.1) {
            out.push(issue(
                IssueLevel::Warn,
                e.item,
                Some(e.key.clone()),
                "ai.issue.room_open",
                vec![],
            ));
        }
    }
}

/// The model must not get worse: new integrity problems and bad openings block Apply.
/// `integrity_regressions` matches by table, id and problem, so a new row on a reused id would
/// inherit the excuse of the row that had that id.
fn reused_id_issues(m: &Model, base: &Baseline, diff: &[ElemDiff]) -> Vec<PlanIssue> {
    let created: Vec<&ElKey> = diff
        .iter()
        .filter(|e| e.kind == RowKind::Created)
        .map(|e| &e.key)
        .collect();
    let is_new = |t: &str, id: i64| created.iter().any(|k| k.0 == t && k.1 == id);
    if !base.issues.iter().any(|i| is_new(&i.table, i.id)) {
        return vec![];
    }
    let now = m.check_integrity().unwrap_or_default();
    now.into_iter()
        .filter(|i| {
            is_new(&i.table, i.id)
                && base
                    .issues
                    .iter()
                    .any(|b| b.table == i.table && b.id == i.id && b.problem == i.problem)
        })
        .map(|i| {
            issue(
                IssueLevel::Block,
                None,
                Some((i.table.clone(), i.id)),
                "ai.issue.integrity",
                vec![i.table, i.id.to_string(), i.problem],
            )
        })
        .collect()
}

pub fn validate(ed: &Editor, base: &Baseline, diff: &[ElemDiff]) -> Vec<PlanIssue> {
    let m = &ed.model;
    let mut out = vec![];
    match m.integrity_regressions(&base.issues) {
        Ok(list) => {
            for i in list {
                out.push(issue(
                    IssueLevel::Block,
                    None,
                    Some((i.table.clone(), i.id)),
                    "ai.issue.integrity",
                    vec![i.table, i.id.to_string(), i.problem],
                ));
            }
        }
        Err(e) => out.push(issue(
            IssueLevel::Block,
            None,
            None,
            "ai.issue.integrity",
            vec![String::new(), String::new(), e.to_string()],
        )),
    }
    out.extend(reused_id_issues(m, base, diff));
    if let Err(e) = opening_issues(m, diff, &base.overlaps, &mut out) {
        out.push(issue(
            IssueLevel::Block,
            None,
            None,
            "ai.issue.integrity",
            vec![String::new(), String::new(), e.to_string()],
        ));
    }
    if let Ok(now) = m.wall_warnings() {
        new_warnings(now, &base.wall_warn, "ai.issue.wall_warning", &mut out);
    }
    if let Ok(now) = m.cut_warnings() {
        new_warnings(now, &base.cut_warn, "ai.issue.cut_warning", &mut out);
    }
    room_issues(m, base, diff, &mut out);
    out
}

/// Rule findings on what the plan touched: warnings, never a block; a company rule advises.
pub fn rule_issues(
    m: &Model,
    rules: &[&'static dyn CheckRule],
    diff: &[ElemDiff],
) -> Vec<PlanIssue> {
    let els: Vec<ElKey> = diff
        .iter()
        .filter(|e| e.kind != RowKind::Deleted)
        .map(|e| e.key.clone())
        .collect();
    if els.is_empty() {
        return vec![];
    }
    let scope = CheckScope::Elements(els);
    let mut found: Vec<Finding> = vec![];
    for r in rules.iter().filter(|r| !r.heavy()) {
        if r.run(m, &scope, &mut found).is_err() {
            continue;
        }
    }
    // reference-only findings say nothing about the plan
    found.retain(|f| f.severity != ddc_model::checks::Severity::Info);
    found.sort_by_key(|f| f.severity);
    found.truncate(MAX_RULE_FINDINGS);
    found
        .into_iter()
        .map(|f| {
            let args = f.args.iter().map(|a| super::fmt::text(&a.1)).collect();
            issue(
                IssueLevel::Warn,
                None,
                f.elements.first().cloned(),
                f.msg_key,
                args,
            )
        })
        .collect()
}

/// Conflicts and the size limit: facts about the plan itself, not the model.
/// The diff hit its limit: the card shows less than Apply would do.
/// A change list cut at the card's limit cannot be verified: it blocks whatever "Allow large" says.
pub fn truncation_issues(truncated: bool, rows: usize) -> Vec<PlanIssue> {
    if truncated {
        vec![truncated_issue(rows)]
    } else {
        vec![]
    }
}

pub fn truncated_issue(rows: usize) -> PlanIssue {
    issue(
        IssueLevel::Block,
        None,
        None,
        "ai.issue.truncated",
        vec![rows.to_string()],
    )
}

/// The scratch copy is not in the state the steps describe, so there is nothing safe to apply.
pub fn replan_issue() -> PlanIssue {
    issue(
        IssueLevel::Block,
        None,
        None,
        "ai.issue.replan_failed",
        vec![],
    )
}

/// Where a plan lands: one type with the session's catalog target, so a draft option never gets
/// the main model's caps and a main plan never skips them.
pub use ddc_ai::catalog::Target;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlanSize {
    pub touched: usize,
    pub deleted: usize,
    /// Elements the plan makes; a plan of nothing else changes no existing element.
    pub created: usize,
}

impl PlanSize {
    pub fn of(diff: &[ElemDiff]) -> PlanSize {
        PlanSize {
            touched: diff.len(),
            deleted: diff.iter().filter(|e| e.kind == RowKind::Deleted).count(),
            created: diff.iter().filter(|e| e.kind == RowKind::Created).count(),
        }
    }

    /// Only new elements: nothing the model already holds is changed or deleted, so a slip
    /// costs one undo and the size alone asks no "Allow large".
    pub fn create_only(self) -> bool {
        self.touched > 0 && self.created == self.touched
    }
}

/// How the person confirms a plan. Only a draft is `Free`: the main model always gets Apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirm {
    Free,
    /// One press, no extra question: T3 within the caps.
    QuickApply,
    Apply,
    /// Apply after "Allow large" with the counts shown.
    AllowLarge,
    Refused,
}

pub fn confirm(target: Target, tier: Tier, size: PlanSize) -> Confirm {
    confirm_with(&PlanCaps::default(), target, tier, size)
}

pub fn confirm_with(caps: &PlanCaps, target: Target, tier: Tier, size: PlanSize) -> Confirm {
    if target == Target::Draft {
        return Confirm::Free;
    }
    if size.touched > caps.refuse_above {
        Confirm::Refused
    } else if (size.touched > caps.confirm_above && !size.create_only()) || size.deleted > caps.delete_above {
        Confirm::AllowLarge
    } else if tier == Tier::T3Auto {
        Confirm::QuickApply
    } else {
        Confirm::Apply
    }
}

fn size_issues(
    caps: &PlanCaps,
    size: PlanSize,
    target: Target,
    allow_large: bool,
    out: &mut Vec<PlanIssue>,
) {
    match confirm_with(caps, target, Tier::T2Apply, size) {
        Confirm::Refused => out.push(issue(
            IssueLevel::Block,
            None,
            None,
            "ai.issue.refused_large",
            vec![size.touched.to_string(), caps.refuse_above.to_string()],
        )),
        Confirm::AllowLarge if !allow_large => {
            if size.touched > caps.confirm_above {
                out.push(issue(
                    IssueLevel::Block,
                    None,
                    None,
                    "ai.issue.too_large",
                    vec![size.touched.to_string(), caps.confirm_above.to_string()],
                ));
            }
            if size.deleted > caps.delete_above {
                out.push(issue(
                    IssueLevel::Block,
                    None,
                    None,
                    "ai.issue.delete_large",
                    vec![size.deleted.to_string(), caps.delete_above.to_string()],
                ));
            }
        }
        _ => {}
    }
}

pub fn plan_issues(
    items: &[PlanItem],
    size: PlanSize,
    target: Target,
    allow_large: bool,
) -> Vec<PlanIssue> {
    plan_issues_with(&PlanCaps::default(), items, size, target, allow_large)
}

pub fn plan_issues_with(
    caps: &PlanCaps,
    items: &[PlanItem],
    size: PlanSize,
    target: Target,
    allow_large: bool,
) -> Vec<PlanIssue> {
    let mut out = vec![];
    for it in items {
        if let (true, Some(c)) = (it.checked, &it.conflict) {
            out.push(issue(
                IssueLevel::Block,
                Some(it.id),
                None,
                "ai.issue.conflict",
                vec![super::fmt::text(c)],
            ));
        }
    }
    size_issues(caps, size, target, allow_large, &mut out);
    out
}

/// What a plan deletes beyond the person's own hand: `n` of `of` built elements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MassDelete {
    pub n: usize,
    pub of: usize,
    /// The model would be left without built elements.
    pub wipe: bool,
}

/// `(count before the plan, deleted)` per category of built elements.
pub fn mass_of(per_category: &[(usize, usize)]) -> Option<MassDelete> {
    let before: usize = per_category.iter().map(|c| c.0).sum();
    let gone: usize = per_category.iter().map(|c| c.1).sum();
    if gone == 0 {
        return None;
    }
    if before >= WIPE_FLOOR && gone >= before {
        return Some(MassDelete { n: gone, of: before, wipe: true });
    }
    let worst = per_category
        .iter()
        .filter(|c| c.1 > MASS_MIN.max(c.0 * MASS_CATEGORY_PERCENT / 100))
        .max_by_key(|c| c.1);
    if let Some(c) = worst {
        return Some(MassDelete { n: c.1, of: c.0, wipe: false });
    }
    (gone > MASS_MIN.max(before * MASS_MODEL_PERCENT / 100))
        .then_some(MassDelete { n: gone, of: before, wipe: false })
}

/// The deletion of `diff` against the scratch model it was drawn on: the count before the plan
/// is the count now, less what the plan created, plus what it deleted.
pub fn mass_delete(m: &Model, diff: &[ElemDiff]) -> Option<MassDelete> {
    if !diff.iter().any(|e| e.kind == RowKind::Deleted) {
        return None;
    }
    let per: Vec<(usize, usize)> = super::rows::ELEMENT_TABLES
        .iter()
        .map(|t| {
            let of = |k: RowKind| diff.iter().filter(|e| e.key.0 == *t && e.kind == k).count();
            let (gone, made) = (of(RowKind::Deleted), of(RowKind::Created));
            let now: i64 = m
                .sql()
                .query_row(&format!("SELECT count(*) FROM \"{t}\""), [], |r| r.get(0))
                .unwrap_or(0);
            ((now.max(0) as usize + gone).saturating_sub(made), gone)
        })
        .collect();
    mass_of(&per)
}

/// The blocks of a mass deletion: "Allow large" by the person lifts the first, never the wipe.
pub fn mass_issues(m: &Model, diff: &[ElemDiff], allow_large: bool) -> Vec<PlanIssue> {
    match mass_delete(m, diff) {
        Some(d) if d.wipe => vec![issue(
            IssueLevel::Block,
            None,
            None,
            "ai.issue.wipe",
            vec![d.n.to_string(), d.of.to_string()],
        )],
        Some(d) if !allow_large => vec![issue(
            IssueLevel::Block,
            None,
            None,
            "ai.issue.mass_delete",
            vec![d.n.to_string(), d.of.to_string()],
        )],
        _ => vec![],
    }
}

fn push_delta(out: &mut Vec<QtyDelta>, key: String, level: Option<i64>, before: f64, after: f64) {
    if (before - after).abs() > 1e-6 {
        out.push(QtyDelta {
            key,
            level,
            before,
            after,
        });
    }
}

/// Totals that moved: counts and areas first, then each material by level.
pub fn qty_deltas(m: &Model, base: &Baseline) -> Result<Vec<QtyDelta>, Error> {
    let mut out = vec![];
    let (a, b) = (&base.totals, m.model_totals()?);
    let pairs: [(&str, f64, f64); 11] = [
        ("walls", a.walls as f64, b.walls as f64),
        ("doors", a.doors as f64, b.doors as f64),
        ("windows", a.windows as f64, b.windows as f64),
        ("wall_volume", a.wall_volume, b.wall_volume),
        ("slabs", a.slabs as f64, b.slabs as f64),
        ("slab_area", a.slab_area, b.slab_area),
        ("slab_volume", a.slab_volume, b.slab_volume),
        ("rooms", a.rooms as f64, b.rooms as f64),
        ("room_area", a.room_area, b.room_area),
        ("room_volume", a.room_volume, b.room_volume),
        ("curtains", a.curtains as f64, b.curtains as f64),
    ];
    for (k, x, y) in pairs {
        push_delta(&mut out, k.to_string(), None, x, y);
    }
    let mut was: HashMap<(String, i64), (f64, f64)> = base
        .materials
        .iter()
        .map(|q| ((q.material.clone(), q.level), (q.volume, q.area)))
        .collect();
    let mut rows: Vec<(String, i64, f64, f64, f64, f64)> = vec![];
    for q in m.material_totals(None)? {
        let (v0, a0) = was
            .remove(&(q.material.clone(), q.level))
            .unwrap_or((0.0, 0.0));
        rows.push((q.material, q.level, v0, q.volume, a0, q.area));
    }
    rows.extend(
        was.into_iter()
            .map(|((mat, l), (v0, a0))| (mat, l, v0, 0.0, a0, 0.0)),
    );
    rows.sort_by(|x, y| (x.1, &x.0).cmp(&(y.1, &y.0)));
    for (mat, l, v0, v1, a0, a1) in rows {
        push_delta(&mut out, format!("volume:{mat}"), Some(l), v0, v1);
        push_delta(&mut out, format!("area:{mat}"), Some(l), a0, a1);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::plan::PlanOp;
    use ddc_model::WallSpec;

    fn elem(table: &str, id: i64, kind: RowKind) -> ElemDiff {
        ElemDiff {
            key: (table.to_string(), id),
            kind,
            item: Some(1),
            level: None,
            label: String::new(),
            cells: vec![],
        }
    }

    fn item(conflict: Option<&str>, checked: bool) -> PlanItem {
        PlanItem {
            id: 4,
            title: "t".into(),
            why: String::new(),
            op: PlanOp::LoadFamily { def: String::new() },
            checked,
            depends_on: vec![],
            actor: "chat".into(),
            conflict: conflict.map(str::to_string),
            sets: Default::default(),
        }
    }

    #[test]
    fn a_shortened_wall_blocks_because_of_its_opening() {
        let mut ed = Editor::new_in_memory().unwrap();
        let t = ed.types[0].id;
        let spec = WallSpec {
            a: (0.0, 0.0),
            b: (6.0, 0.0),
            level: ed.level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        };
        let w = ed.model.add_walls(&[spec]).unwrap()[0];
        ed.model
            .add_opening_with(
                w,
                &OpeningSpec {
                    along: 4.0,
                    width: 1.0,
                    sill: 0.0,
                    height: 2.1,
                    kind: "door".into(),
                },
                Default::default(),
            )
            .unwrap();
        let base = Baseline::capture(&ed.model).unwrap();
        ed.model
            .sql()
            .execute("UPDATE wall SET bx=3.0 WHERE id=?1", [w])
            .unwrap();
        let got = validate(&ed, &base, &[elem("wall", w, RowKind::Changed)]);
        assert!(
            got.iter()
                .any(|i| i.level == IssueLevel::Block && i.text_key == "ai.issue.opening_outside"),
            "{got:?}"
        );
    }

    fn wall_ed() -> (Editor, i64) {
        let mut ed = Editor::new_in_memory().unwrap();
        let t = ed.types[0].id;
        let spec = WallSpec {
            a: (0.0, 0.0),
            b: (10.0, 0.0),
            level: ed.level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        };
        let w = ed.model.add_walls(&[spec]).unwrap()[0];
        (ed, w)
    }

    fn put(ed: &Editor, w: i64, along: f64, width: f64, sill: f64, height: f64) -> i64 {
        ed.model
            .sql()
            .execute(
                "INSERT INTO opening(wall, along, width, sill, height, kind) VALUES(?1,?2,?3,?4,?5,'door')",
                rusqlite::params![w, along, width, sill, height],
            )
            .unwrap();
        ed.model.sql().last_insert_rowid()
    }

    fn overlap_blocks(got: &[PlanIssue]) -> bool {
        got.iter()
            .any(|i| i.level == IssueLevel::Block && i.text_key == "ai.issue.opening_overlap")
    }

    #[test]
    fn non_adjacent_opening_overlap_blocks() {
        let (ed, w) = wall_ed();
        let base = Baseline::capture(&ed.model).unwrap();
        put(&ed, w, 1.0, 4.0, 0.0, 2.1);
        put(&ed, w, 2.0, 0.5, 0.0, 2.1);
        let c = put(&ed, w, 2.2, 0.5, 0.0, 2.1);
        let got = validate(&ed, &base, &[elem("opening", c, RowKind::Created)]);
        assert!(overlap_blocks(&got), "{got:?}");
    }

    #[test]
    fn overlap_with_untouched_old_opening_is_not_reported_if_in_baseline() {
        let (ed, w) = wall_ed();
        put(&ed, w, 1.0, 2.0, 0.0, 2.1);
        put(&ed, w, 2.0, 2.0, 0.0, 2.1);
        let far = put(&ed, w, 8.0, 1.0, 0.0, 2.1);
        let base = Baseline::capture(&ed.model).unwrap();
        let got = validate(&ed, &base, &[elem("opening", far, RowKind::Changed)]);
        assert!(!overlap_blocks(&got), "{got:?}");
    }

    #[test]
    fn moved_opening_in_baseline_overlap_pair_still_blocks() {
        let (ed, w) = wall_ed();
        let a = put(&ed, w, 1.0, 2.0, 0.0, 2.1);
        put(&ed, w, 2.0, 2.0, 0.0, 2.1);
        let base = Baseline::capture(&ed.model).unwrap();
        ed.model
            .sql()
            .execute("UPDATE opening SET along=1.5 WHERE id=?1", [a])
            .unwrap();
        let got = validate(&ed, &base, &[elem("opening", a, RowKind::Changed)]);
        assert!(overlap_blocks(&got), "{got:?}");
    }

    #[test]
    fn resized_opening_in_baseline_overlap_pair_still_blocks() {
        let (ed, w) = wall_ed();
        put(&ed, w, 1.0, 2.0, 0.0, 2.1);
        let b = put(&ed, w, 2.0, 2.0, 0.0, 2.1);
        let base = Baseline::capture(&ed.model).unwrap();
        ed.model
            .sql()
            .execute("UPDATE opening SET width=3.0 WHERE id=?1", [b])
            .unwrap();
        let got = validate(&ed, &base, &[elem("opening", b, RowKind::Changed)]);
        assert!(overlap_blocks(&got), "{got:?}");
    }

    #[test]
    fn untouched_baseline_overlap_pair_is_not_reported() {
        let (ed, w) = wall_ed();
        put(&ed, w, 1.0, 2.0, 0.0, 2.1);
        put(&ed, w, 2.0, 2.0, 0.0, 2.1);
        let base = Baseline::capture(&ed.model).unwrap();
        let other = put(&ed, w, 8.0, 1.0, 0.0, 2.1);
        let got = validate(&ed, &base, &[elem("opening", other, RowKind::Created)]);
        assert!(!overlap_blocks(&got), "{got:?}");
    }

    #[test]
    fn opening_row_read_error_blocks_instead_of_skipping() {
        let (ed, w) = wall_ed();
        let base = Baseline::capture(&ed.model).unwrap();
        let bad = put(&ed, w, 1.0, 1.0, 0.0, 2.1);
        ed.model
            .sql()
            .execute("UPDATE opening SET along='abc' WHERE id=?1", [bad])
            .unwrap();
        let got = validate(&ed, &base, &[elem("opening", bad, RowKind::Changed)]);
        assert!(
            got.iter()
                .any(|i| i.level == IssueLevel::Block && i.text_key == "ai.issue.integrity"),
            "{got:?}"
        );
    }

    #[test]
    fn transom_above_door_not_overlap() {
        let (ed, w) = wall_ed();
        let base = Baseline::capture(&ed.model).unwrap();
        put(&ed, w, 1.0, 1.0, 0.0, 2.1);
        let t = put(&ed, w, 1.0, 1.0, 2.1, 0.6);
        let got = validate(&ed, &base, &[elem("opening", t, RowKind::Created)]);
        assert!(!overlap_blocks(&got), "{got:?}");
    }

    #[test]
    fn room_dup_warns_only_on_pairs_the_plan_made() {
        let mut ed = Editor::new_in_memory().unwrap();
        let t = ed.types[0].id;
        let pts = [(0., 0.), (6., 0.), (6., 4.), (0., 4.), (0., 0.)];
        let specs: Vec<WallSpec> = (0..4)
            .map(|k| WallSpec {
                a: pts[k],
                b: pts[k + 1],
                level: ed.level,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .collect();
        ed.model.add_walls(&specs).unwrap();
        let room = |ed: &mut Editor, x: f64| {
            ed.model
                .add_room(&ddc_model::RoomSpec {
                    level: ed.level,
                    name: "R".into(),
                    number: String::new(),
                    point: (x, 2.0),
                    height: 3.0,
                })
                .unwrap()
        };
        room(&mut ed, 2.0);
        let old_dup = room(&mut ed, 3.0);
        assert!(ed.model.room_duplicates().unwrap().iter().any(|d| d.0 == old_dup));
        let base = Baseline::capture(&ed.model).unwrap();
        ed.model
            .sql()
            .execute("UPDATE room SET name='renamed' WHERE id=?1", [old_dup])
            .unwrap();
        let got = validate(&ed, &base, &[elem("room", old_dup, RowKind::Changed)]);
        assert!(!keys(&got).contains(&"ai.issue.room_dup"), "{got:?}");
        assert!(!keys(&got).contains(&"ai.issue.room_open"), "{got:?}");
        let new_dup = room(&mut ed, 4.0);
        let got = validate(&ed, &base, &[elem("room", new_dup, RowKind::Created)]);
        assert!(keys(&got).contains(&"ai.issue.room_dup"), "{got:?}");
    }

    #[test]
    fn ai_validate_still_reports_open_room() {
        let mut ed = Editor::new_in_memory().unwrap();
        let room = ed
            .model
            .add_room(&ddc_model::RoomSpec {
                level: ed.level,
                name: "Hall".into(),
                number: "101".into(),
                point: (3.0, 2.0),
                height: 2.7,
            })
            .unwrap();
        let base = Baseline::capture(&ed.model).unwrap();
        let got = validate(&ed, &base, &[elem("room", room, RowKind::Created)]);
        assert!(
            got.iter()
                .any(|i| i.level == IssueLevel::Warn && i.text_key == "ai.issue.room_open"),
            "{got:?}"
        );
    }

    #[test]
    fn known_warnings_are_not_repeated() {
        let w = |m: &str| ElementWarning {
            table: "wall",
            id: 1,
            message: m.to_string(),
        };
        let mut out = vec![];
        new_warnings(vec![w("old"), w("new")], &[w("old")], "k", &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].args, vec!["new".to_string()]);
    }

    fn size(touched: usize, deleted: usize) -> PlanSize {
        PlanSize { touched, deleted, created: 0 }
    }

    #[test]
    fn a_large_plan_that_only_creates_needs_no_allow() {
        let made = PlanSize { touched: 2424, deleted: 0, created: 2424 };
        assert!(made.create_only());
        assert!(plan_issues(&[], made, Target::Main, false).is_empty());
        assert_eq!(confirm(Target::Main, Tier::T2Apply, made), Confirm::Apply);
        // one changed element among the new ones: the size asks again
        let mixed = PlanSize { touched: 2424, deleted: 0, created: 2423 };
        assert_eq!(keys(&plan_issues(&[], mixed, Target::Main, false)), vec!["ai.issue.too_large"]);
        // past the hard cap a creating plan is still refused
        let huge = PlanSize { touched: REFUSED_PLAN + 1, deleted: 0, created: REFUSED_PLAN + 1 };
        assert_eq!(confirm(Target::Main, Tier::T2Apply, huge), Confirm::Refused);
    }

    fn keys(v: &[PlanIssue]) -> Vec<&'static str> {
        v.iter().map(|i| i.text_key).collect()
    }

    const TIERS: [Tier; 4] = [Tier::T0Read, Tier::T1Propose, Tier::T2Apply, Tier::T3Auto];

    #[test]
    fn only_a_ticked_conflict_blocks() {
        let one = size(1, 0);
        assert_eq!(
            plan_issues(&[item(Some("gone"), true)], one, Target::Main, false).len(),
            1
        );
        assert!(plan_issues(&[item(Some("gone"), false)], one, Target::Main, false).is_empty());
    }

    #[test]
    fn large_envelope_needs_allow() {
        let big = size(LARGE_PLAN + 1, 0);
        let got = plan_issues(&[], big, Target::Main, false);
        assert_eq!(keys(&got), vec!["ai.issue.too_large"]);
        assert_eq!(got[0].level, IssueLevel::Block);
        assert_eq!(
            got[0].args,
            vec![(LARGE_PLAN + 1).to_string(), LARGE_PLAN.to_string()]
        );
        assert!(plan_issues(&[], big, Target::Main, true).is_empty());
        assert_eq!(confirm(Target::Main, Tier::T2Apply, big), Confirm::AllowLarge);
        assert_eq!(confirm(Target::Main, Tier::T3Auto, big), Confirm::AllowLarge);
    }

    #[test]
    fn delete_cap_blocks() {
        let del = size(LARGE_DELETE + 1, LARGE_DELETE + 1);
        let got = plan_issues(&[], del, Target::Main, false);
        assert_eq!(keys(&got), vec!["ai.issue.delete_large"]);
        assert!(plan_issues(&[], del, Target::Main, true).is_empty());
        let both = size(LARGE_PLAN + 1, LARGE_DELETE + 1);
        assert_eq!(
            keys(&plan_issues(&[], both, Target::Main, false)),
            vec!["ai.issue.too_large", "ai.issue.delete_large"]
        );
    }

    #[test]
    fn cap_boundary_at_cap_passes_and_one_more_asks() {
        let at = size(LARGE_PLAN, LARGE_DELETE);
        assert!(plan_issues(&[], at, Target::Main, false).is_empty());
        assert_eq!(confirm(Target::Main, Tier::T2Apply, at), Confirm::Apply);
        assert_eq!(confirm(Target::Main, Tier::T3Auto, at), Confirm::QuickApply);
        assert_eq!(
            confirm(Target::Main, Tier::T2Apply, size(LARGE_PLAN + 1, 0)),
            Confirm::AllowLarge
        );
        assert_eq!(
            confirm(Target::Main, Tier::T2Apply, size(LARGE_DELETE, LARGE_DELETE + 1)),
            Confirm::AllowLarge
        );
        let top = size(REFUSED_PLAN, 0);
        assert_eq!(confirm(Target::Main, Tier::T2Apply, top), Confirm::AllowLarge);
        assert!(plan_issues(&[], top, Target::Main, true).is_empty());
        let over = size(REFUSED_PLAN + 1, 0);
        assert_eq!(confirm(Target::Main, Tier::T3Auto, over), Confirm::Refused);
        assert_eq!(
            keys(&plan_issues(&[], over, Target::Main, true)),
            vec!["ai.issue.refused_large"]
        );
    }

    #[test]
    fn main_model_never_skips_apply() {
        for t in TIERS {
            for s in [size(0, 0), size(1, 1), size(LARGE_PLAN + 1, LARGE_DELETE + 1)] {
                assert_ne!(confirm(Target::Main, t, s), Confirm::Free, "{t:?} {s:?}");
            }
        }
    }

    #[test]
    fn draft_edits_are_never_blocked_by_tier() {
        let sizes = [
            size(1, 0),
            size(LARGE_PLAN + 1, LARGE_DELETE + 1),
            size(REFUSED_PLAN + 1, REFUSED_PLAN + 1),
        ];
        for t in TIERS {
            for s in sizes {
                assert_eq!(confirm(Target::Draft, t, s), Confirm::Free, "{t:?} {s:?}");
                assert!(plan_issues(&[], s, Target::Draft, false).is_empty());
            }
        }
    }

    #[test]
    fn the_size_rule_is_the_projects_and_zero_switches_a_limit_off() {
        let ed = Editor::new_in_memory().unwrap();
        assert_eq!(PlanCaps::read(&ed.model), PlanCaps::default());
        // a plan of 2424 changes waits for the person under the defaults ...
        let big = size(2424, 0);
        let k = |c: &PlanCaps| keys(&plan_issues_with(c, &[], big, Target::Main, false));
        assert_eq!(k(&PlanCaps::default()), vec!["ai.issue.too_large"]);
        // ... and the person raises the limit for this project, or takes it away
        PlanCaps::set(&ed.model, "plan.confirm_above", 5000).unwrap();
        let caps = PlanCaps::read(&ed.model);
        assert_eq!(caps.confirm_above, 5000);
        assert!(k(&caps).is_empty());
        assert_eq!(confirm_with(&caps, Target::Main, Tier::T2Apply, big), Confirm::Apply);
        assert_eq!(
            keys(&plan_issues_with(&caps, &[], size(5001, 0), Target::Main, false)),
            vec!["ai.issue.too_large"]
        );
        PlanCaps::set(&ed.model, "plan.confirm_above", 0).unwrap();
        PlanCaps::set(&ed.model, "plan.delete_above", 0).unwrap();
        let off = PlanCaps::read(&ed.model);
        assert!(plan_issues_with(&off, &[], size(150_000, 150_000), Target::Main, false).is_empty());
        // the refusal is a limit of the same kind, not a fixed wall
        assert!(PlanCaps::set(&ed.model, "plan.refuse_above", 0).is_err());
        PlanCaps::set(&ed.model, "plan.refuse_above", 900_000).unwrap();
        let high = PlanCaps::read(&ed.model);
        assert_eq!(high.refuse_above, REFUSED_PLAN_MAX);
        assert!(!plan_issues_with(&high, &[], size(900_000, 0), Target::Main, true).is_empty());
        assert!(PlanCaps::set(&ed.model, "nope", 1).is_err());
        ed.model.set_ai_policy("plan.confirm_above", "many").unwrap();
        assert_eq!(PlanCaps::read(&ed.model).confirm_above, LARGE_PLAN);
    }

    // Pinned behaviour: a plan of REFUSED_PLAN (150 000) changes is refused even with "Allow large",
    // and a truncated change list blocks even with "Allow large".
    #[test]
    fn a_plan_of_150k_refuses_and_a_truncated_diff_blocks_with_allow_large() {
        assert_eq!(REFUSED_PLAN, 150_000);
        let over = size(REFUSED_PLAN + 1, 0);
        for allow in [false, true] {
            let k = keys(&plan_issues(&[], over, Target::Main, allow));
            assert_eq!(k, vec!["ai.issue.refused_large"], "allow_large={allow}");
        }
        assert!(plan_issues(&[], size(REFUSED_PLAN, 0), Target::Main, true).is_empty());
        let t = truncation_issues(true, 200_000);
        assert_eq!(keys(&t), vec!["ai.issue.truncated"]);
        assert_eq!(t[0].level, IssueLevel::Block);
        assert!(truncation_issues(false, 10).is_empty());
    }

    #[test]
    fn draft_target_skips_main_caps() {
        let over = size(REFUSED_PLAN + 1, REFUSED_PLAN + 1);
        assert!(plan_issues(&[], over, Target::Draft, false).is_empty());
        assert_eq!(confirm(Target::Draft, Tier::T2Apply, over), Confirm::Free);
        assert!(!plan_issues(&[], over, Target::Main, false).is_empty());
    }

    #[test]
    fn main_target_keeps_allow_large() {
        let big = size(LARGE_PLAN + 1, LARGE_DELETE + 1);
        assert_eq!(Target::default(), Target::Main);
        assert_eq!(confirm(Target::Main, Tier::T3Auto, big), Confirm::AllowLarge);
        assert_eq!(plan_issues(&[], big, Target::Main, false).len(), 2);
        assert!(plan_issues(&[], big, Target::Main, true).is_empty());
    }

    #[test]
    fn plan_size_counts_deletes() {
        let d = [
            elem("wall", 1, RowKind::Deleted),
            elem("wall", 2, RowKind::Changed),
            elem("door", 3, RowKind::Deleted),
        ];
        assert_eq!(PlanSize::of(&d), size(3, 2));
    }
    #[test]
    fn mass_delete_thresholds() {
        // 900 of 1000 walls: the category rule names the counts
        assert_eq!(
            mass_of(&[(1000, 900), (20, 0)]),
            Some(MassDelete { n: 900, of: 1000, wipe: false })
        );
        // a few doors of a small model are an ordinary edit
        assert_eq!(mass_of(&[(12, 3), (30, 0)]), None);
        // the floor: 50 of 60 walls is at the limit, 51 is over it
        assert_eq!(mass_of(&[(300, 50), (0, 0)]), None);
        assert!(mass_of(&[(300, 51)]).is_some());
        // many categories each below the category share, together over the model share
        let many: Vec<(usize, usize)> = (0..12).map(|_| (400, 50)).collect();
        assert_eq!(mass_of(&many).map(|d| d.n), Some(600));
        assert_eq!(mass_of(&[(100, 0)]), None);
    }

    #[test]
    fn wipe_is_a_block_allow_large_does_not_lift() {
        let d = mass_of(&[(20, 20), (5, 5)]).unwrap();
        assert!(d.wipe);
        assert_eq!(mass_of(&[(3, 3)]), None, "a tiny model can be cleared");
    }

}
