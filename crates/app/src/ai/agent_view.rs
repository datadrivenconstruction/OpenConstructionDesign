// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! What the person sees of an external agent: the changes waiting for them and the live journal.
//! Everything here is decided without a window, so the tests pin it.

use super::plan::PlanItem;
use super::session::ACTOR_PREFIX;
use ddc_model::AuditEntry;
use std::time::Duration;

/// Agent changes that wait on the card for the person's decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Waiting {
    pub n: usize,
    /// Time until they count as rejected; `None` while a group is open and its own limit rules.
    pub left: Option<Duration>,
}

/// `expires_in` was read when the card was published, `age` ago.
pub fn waiting_of(
    items: &[PlanItem],
    expires_in: Option<Duration>,
    age: Duration,
) -> Option<Waiting> {
    let n = items
        .iter()
        .filter(|i| i.actor.starts_with(ACTOR_PREFIX))
        .count();
    (n > 0).then(|| Waiting {
        n,
        left: expires_in.map(|d| d.saturating_sub(age)),
    })
}

/// `m:ss`, rounded up so the last second still reads 0:01.
pub fn countdown(d: Duration) -> String {
    let s = d.as_millis().div_ceil(1000) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Whether a journal row belongs to the agent feed: an agent's own action, or the person's
/// decision about agent access (policy switch, revoke), which has the person as actor.
pub fn is_feed_row(e: &AuditEntry) -> bool {
    e.actor.starts_with(ACTOR_PREFIX) || e.event == "mcp_policy"
}

/// One line of the feed.
#[derive(Clone, Debug, PartialEq)]
pub struct FeedRow {
    pub seq: i64,
    pub ts: i64,
    pub actor: String,
    pub event: String,
    pub status: String,
    pub detail: String,
}

/// Newest first.
pub fn feed_rows(rows: Vec<(i64, i64, AuditEntry)>) -> Vec<FeedRow> {
    rows.into_iter()
        .filter(|r| is_feed_row(&r.2))
        .map(|(seq, ts, e)| FeedRow {
            seq,
            ts,
            detail: e.plan.or(e.text).unwrap_or_default(),
            actor: e.actor,
            event: e.event,
            status: e.status,
        })
        .rev()
        .collect()
}

/// The agent's own name, empty for the person's rows.
pub fn agent_name(actor: &str) -> &str {
    actor.strip_prefix(ACTOR_PREFIX).unwrap_or("")
}

/// Names of the agents seen in the feed, each once, in order of appearance.
pub fn agents_in(rows: &[FeedRow]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for r in rows {
        let n = agent_name(&r.actor);
        if !n.is_empty() && !out.iter().any(|o| o == n) {
            out.push(n.to_string());
        }
    }
    out
}

/// The rows of one agent plus the person's decisions about access, which concern every agent;
/// all rows when no agent is picked.
pub fn visible<'a>(rows: &'a [FeedRow], agent: Option<&str>) -> Vec<&'a FeedRow> {
    rows.iter()
        .filter(|r| match agent {
            None => true,
            Some(a) => agent_name(&r.actor).is_empty() || agent_name(&r.actor) == a,
        })
        .collect()
}

/// The catalog key of a row's event, so no raw name reaches the screen.
pub fn event_key(r: &FeedRow) -> &'static str {
    match (r.event.as_str(), r.detail.as_str()) {
        ("mcp_connect", _) => "mcp.ev.connect",
        ("mcp_policy", "revoke") => "mcp.ev.revoke",
        ("mcp_policy", "confirm") => "mcp.ev.policy_confirm",
        ("mcp_policy", _) => "mcp.ev.policy_auto",
        ("apply", _) => "mcp.ev.apply",
        ("reject", _) => "mcp.ev.reject",
        ("tool_write", _) => "mcp.ev.write",
        _ => "mcp.ev.other",
    }
}

/// Status words that have a catalog line; anything else is not shown raw.
pub fn status_key(status: &str) -> Option<&'static str> {
    Some(match status {
        "aborted" => "ai.audit.status.aborted",
        "blocked" => "ai.audit.status.blocked",
        "busy" => "ai.audit.status.busy",
        "commit" => "ai.audit.status.commit",
        "failed" => "ai.audit.status.failed",
        "idle" => "ai.audit.status.idle",
        "model_changed" => "ai.audit.status.model_changed",
        "revoked" => "ai.audit.status.revoked",
        "stale" => "ai.audit.status.stale",
        "timeout" => "ai.audit.status.timeout",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::plan::{PlanOp, Targets};

    fn item(id: u32, actor: &str) -> PlanItem {
        PlanItem {
            id,
            title: String::new(),
            why: String::new(),
            op: PlanOp::Delete {
                target: Targets::Selection,
            },
            checked: true,
            depends_on: vec![],
            actor: actor.to_string(),
            conflict: None,
            sets: Default::default(),
        }
    }

    fn entry(actor: &str, event: &str, plan: Option<&str>) -> (i64, i64, AuditEntry) {
        (
            1,
            10,
            AuditEntry {
                actor: actor.to_string(),
                event: event.to_string(),
                plan: plan.map(str::to_string),
                status: "ok".to_string(),
                ..AuditEntry::default()
            },
        )
    }

    #[test]
    fn waiting_agent_plan_surfaces_review() {
        let items = [item(1, "assistant"), item(2, "mcp:a"), item(3, "mcp:b")];
        let w = waiting_of(&items, Some(Duration::from_secs(600)), Duration::from_secs(90)).unwrap();
        assert_eq!(w.n, 2, "only the agents' items count");
        assert_eq!(w.left, Some(Duration::from_secs(510)));
        assert!(waiting_of(&items[..1], None, Duration::ZERO).is_none());
        let late = waiting_of(&items, Some(Duration::from_secs(5)), Duration::from_secs(9)).unwrap();
        assert_eq!(late.left, Some(Duration::ZERO));
        assert_eq!(waiting_of(&items, None, Duration::ZERO).unwrap().left, None);
    }

    #[test]
    fn the_countdown_reads_minutes_and_seconds() {
        assert_eq!(countdown(Duration::from_secs(510)), "8:30");
        assert_eq!(countdown(Duration::from_millis(200)), "0:01");
        assert_eq!(countdown(Duration::ZERO), "0:00");
    }

    #[test]
    fn the_feed_has_connect_revoke_and_policy_events() {
        let rows = feed_rows(vec![
            entry("mcp:a", "mcp_connect", None),
            entry("user", "mcp_policy", Some("revoke")),
            entry("user", "mcp_policy", Some("confirm")),
            entry("user", "apply", None),
            entry("assistant", "turn", None),
        ]);
        assert_eq!(rows.len(), 3, "chat and the person's own applies stay out");
        assert_eq!(rows[0].detail, "confirm", "newest first");
        assert_eq!(event_key(&rows[0]), "mcp.ev.policy_confirm");
        assert_eq!(event_key(&rows[1]), "mcp.ev.revoke");
        assert_eq!(event_key(&rows[2]), "mcp.ev.connect");
    }

    #[test]
    fn the_agent_filter_keeps_the_persons_decisions() {
        let rows = feed_rows(vec![
            entry("mcp:a", "mcp_connect", None),
            entry("mcp:b", "mcp_connect", None),
            entry("user", "mcp_policy", Some("revoke")),
        ]);
        assert_eq!(agents_in(&rows), vec!["b".to_string(), "a".to_string()]);
        assert_eq!(visible(&rows, None).len(), 3);
        let only_a = visible(&rows, Some("a"));
        assert_eq!(only_a.len(), 2);
        assert!(only_a.iter().all(|r| agent_name(&r.actor) != "b"));
    }

    #[test]
    fn a_status_without_a_line_is_never_shown_raw() {
        assert_eq!(status_key("busy"), Some("ai.audit.status.busy"));
        assert_eq!(status_key("something_new"), None);
    }
}
