// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! After a draft step the realism critic runs on the scratch copy. Its findings go back to the
//! model as tasks (at most `max_cycles()` per turn), and, when the project allows snapshots,
//! small views of the draft go along so the model can look at what it built.

use super::render_off;
use super::render_req::{Render3d, Target};
use super::session::Core;
use super::validate::{IssueLevel, PlanIssue};
use ddc_ai::ImageData;
use ddc_model::checks::{realism, CheckScope, Finding};
use ddc_ui::{Catalog, Lang};
use std::sync::atomic::Ordering;

/// Review cycles per turn (`limits::CRITIC_CYCLES`, a setting).
pub fn max_cycles() -> u8 {
    ddc_ai::limits::CRITIC_CYCLES.get().min(u64::from(u8::MAX)) as u8
}
/// Findings listed per cycle; the rest are counted.
const MAX_LISTED: usize = ddc_ai::limits::CRITIC_LISTED.us();
/// Tools after which the draft has changed shape.
pub const DRAFT_TOOLS: [&str; 5] = [
    "create_geometry",
    "define_family",
    "update_elements",
    "generate_project",
    "revise_project",
];
/// Low resolution on purpose: three views per step must stay cheap.
const SNAP_PX: u32 = 384;

/// One more refinement cycle is allowed while fewer than `max_cycles()` have run this turn.
pub fn may_cycle(done: u8) -> bool {
    done < max_cycles()
}

pub fn is_draft_tool(name: &str) -> bool {
    DRAFT_TOOLS.contains(&name)
}

/// One English line per finding; the model reads English whatever the UI language is.
pub fn task_lines(list: &[Finding], cat: &Catalog) -> Vec<String> {
    list.iter()
        .take(MAX_LISTED)
        .map(|f| {
            let args: Vec<String> = f.args.iter().map(|(_, v)| v.clone()).collect();
            let els: Vec<String> = f.elements.iter().map(|e| format!("{}#{}", e.0, e.1)).collect();
            let text = super::ui::logic::fill(&cat.t(f.msg_key), &args);
            format!("- [{}] {} ({})", f.rule, text, els.join(", "))
        })
        .collect()
}

/// What one review of the draft concluded.
#[derive(Debug, PartialEq)]
pub enum Verdict {
    Clean,
    /// Findings go back to the model as cycle `cycle` of `cap`.
    Fix { cycle: u8, lines: Vec<String>, count: usize },
    /// The cap is spent and `count` findings remain.
    Spent { count: usize },
}

/// One English line per plan issue (validation or a rule), blockers first.
pub fn issue_lines(list: &[PlanIssue], cat: &Catalog) -> Vec<String> {
    let mut sorted: Vec<&PlanIssue> = list.iter().collect();
    sorted.sort_by_key(|i| i.level != IssueLevel::Block);
    sorted
        .into_iter()
        .map(|i| {
            let text = super::ui::logic::fill(&cat.t(i.text_key), &i.args);
            let tag = if i.level == IssueLevel::Block { "check, blocks Apply" } else { "check" };
            match &i.el {
                Some((t, id)) => format!("- [{tag}] {text} ({t}#{id})"),
                None => format!("- [{tag}] {text}"),
            }
        })
        .collect()
}

/// Realism findings and plan checks judged together; `done` cycles already ran this turn.
pub fn review(realism: &[Finding], checks: &[PlanIssue], done: u8, cap: u8, cat: &Catalog) -> Verdict {
    let count = realism.len() + checks.len();
    if count == 0 {
        return Verdict::Clean;
    }
    if done >= cap {
        return Verdict::Spent { count };
    }
    let mut lines = issue_lines(checks, cat);
    lines.extend(task_lines(realism, cat));
    lines.truncate(MAX_LISTED);
    if count > lines.len() {
        lines.push(format!("- and {} more", count - lines.len()));
    }
    Verdict::Fix { cycle: done + 1, lines, count }
}

/// The text appended to the tool reply, or `None` when the draft is clean or the critic is off.
pub fn critic_note(c: &mut Core) -> Option<String> {
    if !flag(c, POLICY_CRITIC) {
        return None;
    }
    let realism = realism::critique(&c.ed.model, &CheckScope::All).unwrap_or_default();
    let checks = c.check_issues();
    let cat = Catalog::new(Lang::En);
    let v = review(&realism, &checks, c.critic_cycles, max_cycles(), &cat);
    note(c, v)
}

/// The reply with the critic's note: a JSON object reply stays one valid object, the note in its
/// `critic` field (fields keep their order); any other reply gets the note on a line of its own.
pub fn with_note(reply: &str, note: &str) -> String {
    let body = reply.trim_end();
    let is_object = serde_json::from_str::<serde_json::Value>(body).is_ok_and(|v| v.is_object());
    match body.strip_suffix('}') {
        Some(head) if is_object => {
            let sep = if head.trim_end().ends_with('{') { "" } else { "," };
            format!("{head}{sep}\"critic\":{}}}", serde_json::Value::from(note))
        }
        _ => format!("{reply}\n{note}"),
    }
}

/// Keeps the card's self-check line in step with the verdict and words the reply.
fn note(c: &mut Core, v: Verdict) -> Option<String> {
    match v {
        Verdict::Clean => {
            c.critic_left = 0;
            None
        }
        Verdict::Spent { count } => {
            c.critic_left = count;
            Some(format!(
                "critic: {count} findings remain after {} refinement cycles (limit `critic_cycles`,                  adjustable in Settings > AI); stop correcting and tell the user what is left",
                c.critic_cycles
            ))
        }
        Verdict::Fix { cycle, lines, count } => {
            c.critic_cycles = cycle;
            c.critic_left = count;
            Some(format!(
                "critic cycle {cycle}/{}: fix these before you answer
{}",
                max_cycles(),
                lines.join("
")
            ))
        }
    }
}

/// The card line: rounds run this turn and what is still open; `None` before the first round.
pub fn self_check(c: &Core) -> Option<SelfCheck> {
    (c.critic_cycles > 0).then_some(SelfCheck {
        rounds: c.critic_cycles,
        left: c.critic_left,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelfCheck {
    pub rounds: u8,
    pub left: usize,
}

fn view(yaw: f64, pitch: f64, ortho: bool) -> Render3d {
    Render3d {
        target: Target::All,
        yaw: Some(yaw),
        pitch: Some(pitch),
        ortho,
        max_px: SNAP_PX,
    }
}

/// Two 3D angles of the draft; empty when snapshots are off or no GPU is at hand.
pub fn snapshots(c: &mut Core) -> Vec<ImageData> {
    if !flag(c, POLICY_SNAP) || !c.allow_snapshots.load(Ordering::Relaxed) {
        return vec![];
    }
    let mut out = vec![];
    for (yaw, pitch) in [(35.0, 30.0), (215.0, 30.0)] {
        if let Ok(r) = render_off::view3d(&c.ed, c.view.units, &view(yaw, pitch, false)) {
            out.push(r.out.image);
        }
    }
    out
}

pub const POLICY_CRITIC: &str = "ai.critic";
pub const POLICY_SNAP: &str = "ai.auto_snapshots";

/// Both are on unless the project turned them off (the cost setting).
fn flag(c: &Core, key: &str) -> bool {
    super::ui::connect::policy_flag(&c.ed.model, key, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_json_reply_stays_one_object_with_the_note() {
        let note = "critic cycle 1/3: fix these\n- [check] \"a\"";
        let r = with_note("{\"untrusted\":\"data\",\"item\":1}\n", note);
        let v: serde_json::Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["critic"], note);
        assert!(r.starts_with("{\"untrusted\":\"data\",\"item\":1,"), "{r}");
        let v: serde_json::Value = serde_json::from_str(&with_note("{}", note)).unwrap();
        assert_eq!(v["critic"], note);
        assert_eq!(with_note("done", note), format!("done\n{note}"));
        assert_eq!(with_note("[1]", note), format!("[1]\n{note}"));
    }

    #[test]
    fn refinement_stops_after_three_cycles() {
        let mut done = 0;
        while may_cycle(done) {
            done += 1;
            assert!(done <= 10);
        }
        assert_eq!(done, 3);
    }

    #[test]
    fn only_draft_tools_trigger_the_critic() {
        assert!(is_draft_tool("create_geometry"));
        assert!(is_draft_tool("generate_project") && is_draft_tool("revise_project"));
        assert!(!is_draft_tool("measure"));
        assert!(!is_draft_tool("render_3d"));
    }

    #[test]
    fn task_lines_are_english_and_numbered_by_position() {
        use ddc_model::checks::{Fix, Severity};
        let f = Finding {
            rule: "realism",
            severity: Severity::Warning,
            elements: vec![],
            at: None,
            measure: None,
            msg_key: "ai.realism.tiny_part",
            args: vec![("name", "solid:4".to_string()), ("size", "3".to_string())],
            fix: Fix::None,
        };
        let cat = Catalog::new(Lang::En);
        let lines = task_lines(&[f], &cat);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("solid:4") && lines[0].contains("3 mm"), "{}", lines[0]);
        assert!(!lines[0].contains('{'));
    }

    fn finding(name: &str) -> Finding {
        use ddc_model::checks::{Fix, Severity};
        Finding {
            rule: "realism",
            severity: Severity::Warning,
            elements: vec![("solid".to_string(), 4)],
            at: None,
            measure: None,
            msg_key: "ai.realism.tiny_part",
            args: vec![("name", name.to_string()), ("size", "3".to_string())],
            fix: Fix::None,
        }
    }

    fn check(level: IssueLevel, id: i64) -> PlanIssue {
        PlanIssue {
            level,
            item: None,
            el: Some(("walls".to_string(), id)),
            text_key: "ai.issue.integrity",
            args: vec!["walls".to_string(), id.to_string(), "dangling host".to_string()],
        }
    }

    fn en() -> Catalog {
        Catalog::new(Lang::En)
    }

    #[test]
    fn clean_draft_says_nothing() {
        assert_eq!(review(&[], &[], 0, 3, &en()), Verdict::Clean);
        assert_eq!(review(&[], &[], 3, 3, &en()), Verdict::Clean, "clean beats a spent cap");
    }

    #[test]
    fn a_seeded_check_becomes_a_task_with_its_element() {
        let v = review(&[], &[check(IssueLevel::Warn, 7)], 0, 3, &en());
        let Verdict::Fix { cycle, lines, count } = v else { panic!("{v:?}") };
        assert_eq!((cycle, count), (1, 1));
        assert!(lines[0].starts_with("- [check]") && lines[0].contains("walls#7"), "{}", lines[0]);
        assert!(lines[0].contains("dangling host") && !lines[0].contains('{'), "{}", lines[0]);
    }

    #[test]
    fn realism_and_checks_share_one_task_list_blockers_first() {
        let v = review(
            &[finding("solid:4")],
            &[check(IssueLevel::Warn, 1), check(IssueLevel::Block, 2)],
            1,
            3,
            &en(),
        );
        let Verdict::Fix { cycle, lines, count } = v else { panic!("{v:?}") };
        assert_eq!((cycle, count, lines.len()), (2, 3, 3));
        assert!(lines[0].contains("blocks Apply") && lines[0].contains("walls#2"), "{lines:?}");
        assert!(lines[1].contains("walls#1"));
        assert!(lines[2].starts_with("- [realism]"));
    }

    #[test]
    fn the_cap_stops_the_loop_and_counts_what_is_left() {
        let checks = [check(IssueLevel::Block, 1)];
        assert_eq!(
            review(&[finding("a")], &checks, 3, 3, &en()),
            Verdict::Spent { count: 2 }
        );
        assert_eq!(review(&[], &checks, 0, 0, &en()), Verdict::Spent { count: 1 }, "cap 0 never cycles");
    }

    #[test]
    fn long_lists_are_cut_and_the_rest_counted() {
        let checks: Vec<PlanIssue> = (0..MAX_LISTED as i64 + 4).map(|i| check(IssueLevel::Warn, i)).collect();
        let Verdict::Fix { lines, count, .. } = review(&[], &checks, 0, 3, &en()) else { panic!() };
        assert_eq!(count, MAX_LISTED + 4);
        assert_eq!(lines.len(), MAX_LISTED + 1);
        assert_eq!(lines.last().unwrap(), "- and 4 more");
    }

    #[test]
    fn two_steps_fix_then_clear() {
        // step one finds a problem, the model fixes it, step two is clean
        let first = review(&[], &[check(IssueLevel::Block, 3)], 0, 3, &en());
        let Verdict::Fix { cycle, .. } = first else { panic!("{first:?}") };
        assert_eq!(review(&[], &[], cycle, 3, &en()), Verdict::Clean);
    }

    #[test]
    fn self_check_line_reads_rounds_and_what_is_left() {
        let cat = en();
        let open = super::super::ui::logic::self_check_text(&cat, SelfCheck { rounds: 2, left: 1 });
        assert!(open.contains('2') && open.contains('1') && !open.contains('{'), "{open}");
        let clear = super::super::ui::logic::self_check_text(&cat, SelfCheck { rounds: 3, left: 0 });
        assert!(clear.contains('3') && clear.contains("fixed"), "{clear}");
        let ru = super::super::ui::logic::self_check_text(&Catalog::new(Lang::Ru), SelfCheck { rounds: 2, left: 4 });
        assert!(ru.contains("Самопроверка") && ru.contains('4'), "{ru}");
    }
}
