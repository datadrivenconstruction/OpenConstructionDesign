//! After a draft step the realism critic runs on the scratch copy. Its findings go back to the
//! model as tasks (at most `MAX_CYCLES` per turn), and, when the project allows snapshots,
//! small views of the draft go along so the model can look at what it built.

use super::render_off;
use super::render_req::{Render3d, Target};
use super::session::Core;
use ddc_ai::ImageData;
use ddc_model::checks::{realism, CheckScope, Finding};
use ddc_ui::{Catalog, Lang};
use std::sync::atomic::Ordering;

pub const MAX_CYCLES: u8 = 3;
/// Findings listed per cycle; the rest are counted.
const MAX_LISTED: usize = 12;
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

/// One more refinement cycle is allowed while fewer than `MAX_CYCLES` have run this turn.
pub fn may_cycle(done: u8) -> bool {
    done < MAX_CYCLES
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

/// The text appended to the tool reply, or `None` when the draft is clean or the cap is spent.
pub fn critic_note(c: &mut Core) -> Option<String> {
    if !flag(c, POLICY_CRITIC) {
        return None;
    }
    let list = realism::critique(&c.ed.model, &CheckScope::All).ok()?;
    if list.is_empty() {
        return None;
    }
    if !may_cycle(c.critic_cycles) {
        return Some(format!(
            "critic: {} findings remain after {MAX_CYCLES} refinement cycles; \
             stop correcting and tell the user what is left",
            list.len()
        ));
    }
    c.critic_cycles += 1;
    let cat = Catalog::new(Lang::En);
    let mut lines = task_lines(&list, &cat);
    if list.len() > lines.len() {
        lines.push(format!("- and {} more", list.len() - lines.len()));
    }
    Some(format!(
        "critic cycle {}/{MAX_CYCLES}: fix these before you answer\n{}",
        c.critic_cycles,
        lines.join("\n")
    ))
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
}
