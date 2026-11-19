//! The palette's draft rows end in the panel's own Apply: the model moves only after it.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_ai::Mode;
use ddc_ai::fake::ScriptedProvider;
use ddc_app::ai::ui::palette_rows::{draft_in, draft_pick, offers, Inputs};
use ddc_app::ai::with_session;
use ddc_app::editor::Editor;
use ddc_app::shell::EditorApp;
use e2e_ai_common::*;
use serde_json::json;

fn app_with_draft() -> (EditorApp, usize) {
    let mut ed = Editor::new_in_memory().unwrap();
    let level = ed.level;
    add_wall(&mut ed, level, (0.0, 0.0), (5.0, 0.0));
    let provider = ScriptedProvider::new(script(vec![
        tool(
            "add_param",
            json!({"title": "Param", "name": "Rating", "ptype": "text", "cats": ["wall"]}),
        ),
        tool(
            "update_elements",
            json!({"title": "Set", "target": {"filter": "wall"}, "set": {"Rating": "A"}}),
        ),
    ]));
    let mut s = start(&mut ed, cfg(provider));
    turn(&mut s, &mut ed, "go", Mode::Propose);
    let steps = plan_of(&s).compiled.steps.len();
    assert!(s.can_apply());
    let app = EditorApp::new(ed, None, "test".into());
    with_session(|slot| *slot = Some(s));
    (app, steps)
}

#[test]
fn apply_draft_is_the_panels_apply_and_nothing_else_moves_the_model() {
    let (mut app, steps) = app_with_draft();
    assert!(steps > 0);
    let before = live_hash(&app.ed.model);

    // the rows read the draft, they do not touch it; N is the plan's own step count
    let (f_plan, can) = with_session(|s| {
        let u = s.as_ref().unwrap();
        (u.plan().cloned(), u.can_apply())
    });
    let d = draft_in(f_plan.as_deref(), can);
    assert_eq!(d.map(|x| x.steps), Some(steps));
    let o = offers(
        &app.sh.reg,
        &app.sh.cat,
        &Inputs {
            query: "",
            connected: true,
            draft: d,
        },
    );
    assert_eq!(o.drafts[0].key, "@aid:apply");
    assert!(o.drafts[0].label.contains(&format!("({steps})")));
    assert_eq!(live_hash(&app.ed.model), before);

    // the view rows and a failed pick leave the model alone
    draft_pick(&mut app, "show");
    draft_pick(&mut app, "next");
    draft_pick(&mut app, "no-such-row");
    assert_eq!(live_hash(&app.ed.model), before);

    draft_pick(&mut app, "apply");
    assert_ne!(live_hash(&app.ed.model), before, "Apply draft wrote the draft");

    // one undo step takes it back, like the panel's Apply
    app.ed.undo();
    assert_eq!(live_hash(&app.ed.model), before);
}

#[test]
fn reject_draft_drops_the_draft_and_leaves_the_model() {
    let (mut app, _) = app_with_draft();
    let before = live_hash(&app.ed.model);
    draft_pick(&mut app, "reject");
    assert_eq!(live_hash(&app.ed.model), before);
    let has = with_session(|s| s.as_ref().unwrap().plan().is_some());
    assert!(!has, "the draft is gone");
    // no draft: the rows are gone too
    let d = draft_in(None, false);
    assert!(d.is_none());
}
