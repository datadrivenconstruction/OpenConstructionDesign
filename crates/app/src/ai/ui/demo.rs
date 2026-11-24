// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A scripted conversation for `--ui-shot` (`ai-demo`): the panel's real path from the person's
//! words to the reply, a tool run and a draft, with a scripted model instead of a network call.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use ddc_ai::fake::{self, Script, ScriptedProvider};

use super::{stream_status, with, Entry};
use crate::ai::{with_session, SessionCfg, UiSession, ViewCtx};
use crate::shell::EditorApp;

const WAIT: Duration = Duration::from_secs(120);

/// The turns of one demo: `chat` answers in words, `build` drafts a house and explains it.
fn turns(which: &str) -> Option<(&'static str, Vec<Vec<Script>>, ddc_ai::Mode)> {
    let text = |s: &str| vec![Script::Text(s.to_string())];
    match which {
        "chat" => Some((
            "What is the gross floor area of level 1, and which rooms are the largest?",
            vec![text(
                "Level 1 has a **gross floor area of 182.4 m²**.\n\nThe three largest rooms:\n\n\
                 1. Living room: 38.6 m²\n2. Kitchen: 21.2 m²\n3. Bedroom 1: 16.8 m²\n\n\
                 The corridor takes 9 % of the floor, inside the usual range.",
            )],
            ddc_ai::Mode::ReadOnly,
        )),
        "build" => Some((
            "A two-storey family house, 140 m², three bedrooms and a gable roof",
            vec![
                vec![Script::Tool {
                    name: "generate_project".to_string(),
                    args: serde_json::json!({"spec": {"typology": "house", "floors": 2}}),
                }],
                text(
                    "Drafted the house as one item: two storeys, three bedrooms, a gable roof.\n\n\
                     Check it in the preview, then **Apply** to put it in the model.",
                ),
            ],
            ddc_ai::Mode::Propose,
        )),
        _ => None,
    }
}

/// Sends the demo prompt through the panel and waits for the reply; `Err` names what failed.
pub fn seed(app: &mut EditorApp, which: &str) -> Result<(), String> {
    let (prompt, script, mode) = turns(which).ok_or("ai-demo chat|build")?;
    let cfg = SessionCfg {
        provider: ScriptedProvider::new(script),
        profile: fake::profile(),
        limits: ddc_ai::Limits::default(),
        session: format!("ui-shot-{which}"),
        allow_snapshots: Arc::new(AtomicBool::new(false)),
        extra_generators: vec![],
        extra_rules: vec![],
        keep_text: false,
        confirm_ttl: crate::ai::session::CONFIRM_TTL,
        target: ddc_ai::catalog::Target::Main,
        auto_replan: false,
    };
    let view = ViewCtx { view: "plan".to_string(), level: app.ed.level, units: app.ed.tool.units, ..ViewCtx::default() };
    let mut s = UiSession::start(&app.ed, cfg, view.clone()).map_err(|e| format!("{e:?}"))?;
    if !s.wait_idle(&mut app.ed, WAIT) {
        return Err("the demo session did not open".into());
    }
    with(|st| {
        st.open = true;
        st.profiles = vec![fake::profile()];
        st.session_profile = fake::profile().id;
        st.log.push(Entry::User(prompt.to_string()));
        st.chat_dirty = true;
        st.journal = ddc_ai::TaskJournal::new(prompt);
        st.last = Some((prompt.to_string(), mode));
        stream_status::push(&mut st.stream, stream_status::Ev::Start(web_time::Instant::now()));
    });
    s.send_turn(&app.ed, prompt.to_string(), view, mode).map_err(|e| format!("{e:?}"))?;
    if !s.wait_idle(&mut app.ed, WAIT) {
        return Err("the demo turn did not finish".into());
    }
    with_session(|x| *x = Some(s));
    Ok(())
}
