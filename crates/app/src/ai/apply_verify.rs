// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Apply writes exactly the draft the person saw. After the step is written, and before it is
//! announced, the live model is diffed with the draft's base copy the same way the plan card was
//! (`diff_from` + `group_diff`, compared through `plan::fingerprint`). Any difference takes the
//! step back whole (model and op journal as before, no Redo entry), writes an audit entry and
//! returns a message in the UI language naming the first difference.
//!
//! The base copy equals the live model at the plan's `base_head`: Apply refuses a stale plan
//! first (any edit after the draft was frozen), so both sides are measured from the same rows.
//! Type rows are units of their own: a type parameter set through an element is one changed type
//! row on the card (with the count of elements that take it) and one in the applied delta.

use super::exec;
use super::plan::{fingerprint, group_diff, CompiledPlan, ElemDiff};
use super::AiError;
use crate::editor::Editor;
use ddc_model::{AuditEntry, ElKey, Model, RowKind};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// What the plan card showed, and what it was measured against.
pub struct Shown<'a> {
    /// The base copy the card was diffed with.
    pub base: &'a Path,
    pub diff: &'a [ElemDiff],
    /// The card stopped at `limit` rows: the applied delta is compared up to the same limit.
    pub truncated: bool,
    pub limit: usize,
}

/// The first way the written step differs from the card.
#[derive(Clone, Debug, PartialEq)]
pub enum Divergence {
    /// On the card, not written.
    Missing { key: ElKey, kind: RowKind },
    /// Written, not on the card.
    Extra { key: ElKey, kind: RowKind },
    /// Created, changed or deleted other than the card said.
    Kind {
        key: ElKey,
        shown: RowKind,
        applied: RowKind,
    },
    /// Same element, another value (or another set of fields).
    Value { key: ElKey, col: String },
    /// One side stopped at the row limit, the other did not.
    Truncated,
    /// The written step could not be read back: nothing proves it right.
    Unreadable(String),
    /// The draft's base copy is gone: nothing to measure Apply against, so it is not written.
    NoBase,
}

fn kind_key(k: RowKind) -> &'static str {
    match k {
        RowKind::Created => "apply_verify.kind.created",
        RowKind::Changed => "apply_verify.kind.changed",
        RowKind::Deleted => "apply_verify.kind.deleted",
    }
}

impl Divergence {
    /// The message in `ru` (true) or English; element keys read as on the card (`wall 12`).
    pub fn text(&self, ru: bool) -> String {
        let t = |k: &str, a: &[(&str, &str)]| crate::tr_lang(ru, k, a);
        let what = |k: &ElKey| format!("{} {}", k.0, k.1);
        let body = match self {
            Divergence::Missing { key, kind } => t(
                "apply_verify.diff.missing",
                &[("what", &what(key)), ("kind", &t(kind_key(*kind), &[]))],
            ),
            Divergence::Extra { key, kind } => t(
                "apply_verify.diff.extra",
                &[("what", &what(key)), ("kind", &t(kind_key(*kind), &[]))],
            ),
            Divergence::Kind {
                key,
                shown,
                applied,
            } => t(
                "apply_verify.diff.kind",
                &[
                    ("what", &what(key)),
                    ("shown", &t(kind_key(*shown), &[])),
                    ("applied", &t(kind_key(*applied), &[])),
                ],
            ),
            Divergence::Value { key, col } => {
                t("apply_verify.diff.value", &[("what", &what(key)), ("col", col)])
            }
            Divergence::Truncated => t("apply_verify.diff.truncated", &[]),
            Divergence::Unreadable(e) => t("apply_verify.diff.unreadable", &[("e", e)]),
            Divergence::NoBase => return t("apply_verify.no_base", &[]),
        };
        t("apply_verify.rolled_back", &[("what", &body)])
    }
}

type Print = (RowKind, Vec<(String, Option<String>, Option<String>)>);

fn by_key(diff: &[ElemDiff]) -> BTreeMap<ElKey, Print> {
    fingerprint(diff)
        .into_iter()
        .map(|(k, kind, cells)| (k, (kind, cells)))
        .collect()
}

/// The first difference between the card and the applied delta, in key order; `None`: equal.
pub fn first_difference(shown: &[ElemDiff], applied: &[ElemDiff]) -> Option<Divergence> {
    let (a, b) = (by_key(shown), by_key(applied));
    let keys: BTreeSet<&ElKey> = a.keys().chain(b.keys()).collect();
    for k in keys {
        match (a.get(k), b.get(k)) {
            (Some((kind, _)), None) => {
                return Some(Divergence::Missing {
                    key: k.clone(),
                    kind: *kind,
                })
            }
            (None, Some((kind, _))) => {
                return Some(Divergence::Extra {
                    key: k.clone(),
                    kind: *kind,
                })
            }
            (Some((ka, ca)), Some((kb, cb))) => {
                if ka != kb {
                    return Some(Divergence::Kind {
                        key: k.clone(),
                        shown: *ka,
                        applied: *kb,
                    });
                }
                if ca != cb {
                    let col = ca
                        .iter()
                        .zip(cb)
                        .find(|(x, y)| x != y)
                        .map(|(x, _)| x.0.clone())
                        .or_else(|| {
                            let n = ca.len().min(cb.len());
                            ca.get(n).or(cb.get(n)).map(|c| c.0.clone())
                        })
                        .unwrap_or_default();
                    return Some(Divergence::Value {
                        key: k.clone(),
                        col,
                    });
                }
            }
            (None, None) => {}
        }
    }
    None
}

/// What the live model holds after `since` compared with `base`, grouped as the card groups it.
pub fn applied_delta(
    m: &Model,
    base: &Path,
    since: u64,
    limit: usize,
) -> Result<(Vec<ElemDiff>, bool), ddc_model::Error> {
    let touched = m.changes_since(since);
    let raw = m.diff_from(base, &touched, limit)?;
    Ok((group_diff(&raw), raw.truncated))
}

/// The written step against the card. `since`: the change head just before Apply.
pub fn check(m: &Model, shown: &Shown<'_>, since: u64) -> Result<(), Divergence> {
    let _s = ddc_model::perf::span("apply.check");
    let (got, truncated) = applied_delta(m, shown.base, since, shown.limit)
        .map_err(|e| Divergence::Unreadable(e.to_string()))?;
    if let Some(d) = first_difference(shown.diff, &got) {
        return Err(d);
    }
    if truncated != shown.truncated {
        return Err(Divergence::Truncated);
    }
    Ok(())
}

/// The person's Apply of the card `shown`: written in one undo step, checked, taken back on any
/// difference. A stale plan is refused before anything is written (`AiError::Stale`). `audit`
/// carries the session fields of the entry written for a taken-back Apply.
pub fn apply_verified(
    ed: &mut Editor,
    p: &CompiledPlan,
    merge: Option<(i64, Option<String>)>,
    shown: &Shown<'_>,
    audit: &AuditEntry,
    ru: bool,
) -> Result<Option<AiError>, AiError> {
    apply_verified_src(ed, p, merge, None, shown, audit, ru)
}

/// [`apply_verified`] that records `src` on the parameter values the Apply changed.
pub fn apply_verified_src(
    ed: &mut Editor,
    p: &CompiledPlan,
    merge: Option<(i64, Option<String>)>,
    src: Option<ddc_model::ValueSrc>,
    shown: &Shown<'_>,
    audit: &AuditEntry,
    ru: bool,
) -> Result<Option<AiError>, AiError> {
    let mut found: Option<Divergence> = None;
    // copies in memory are checked as files are
    let res = if shown.base.is_file() || ddc_model::is_memory_copy(shown.base) {
        let since = ed.model.change_head();
        exec::apply_merge_verified_src(ed, p, merge, src, &mut |ed: &mut Editor, top: i64| {
            let Err(d) = check(&ed.model, shown, since) else {
                return Ok(());
            };
            let mut msg = d.text(ru);
            if let Err(e) = ed.take_back_ai(top) {
                msg = format!("{msg} {}", crate::tr_lang(ru, "apply_verify.take_back_failed", &[("e", &e)]));
            }
            found = Some(d);
            Err(AiError::Diverged(msg))
        })
    } else {
        // not stale and no copy to measure against: refused, never applied unchecked
        if p.steps.is_empty() {
            return Err(AiError::EmptyPlan);
        }
        if ed.model.plan_stale(p.base_head) {
            return Err(AiError::Stale);
        }
        found = Some(Divergence::NoBase);
        Err(AiError::Diverged(Divergence::NoBase.text(ru)))
    };
    if let (Some(d), Err(AiError::Diverged(msg))) = (&found, &res) {
        let mut e = audit.clone();
        e.event = "apply_check".to_string();
        e.status = "diverged".to_string();
        // the journal reads in one language whatever the UI shows
        e.text = Some(d.text(false));
        e.plan = Some(format!("steps={}", p.steps.len()));
        e.op_seq = None;
        crate::ai::session::audit_write(&ed.model, &e);
        ed.say(crate::status::Severity::Warning, msg.clone());
    }
    res
}

/// Elements that take a change of the type row `key`; `None` for a row that is not a type.
/// The same counter as the workbook preview and the properties panel (`type_use`).
pub fn type_users(m: &Model, key: &ElKey) -> Option<usize> {
    crate::type_use::users(m, &key.0, key.1)
}

/// The card label of a changed type row: its name and how many elements take the change.
pub fn type_row_label(m: &Model, key: &ElKey) -> Option<String> {
    let n = type_users(m, key)?;
    let name: Option<String> = m
        .sql()
        .query_row(
            &format!("SELECT name FROM {} WHERE id=?1", key.0),
            [key.1],
            |r| r.get(0),
        )
        .ok();
    let ty = match name.filter(|s| !s.is_empty()) {
        Some(s) => super::fmt::text(&s),
        None => format!("{} {}", key.0, key.1),
    };
    Some(crate::tr(
        "apply_verify.card.type_row",
        &[("type", &ty), ("n", &n.to_string())],
    ))
}

#[cfg(test)]
#[path = "apply_verify_tests.rs"]
mod tests;
