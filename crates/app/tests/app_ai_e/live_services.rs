//! Live scenarios for generated electrical and water services: the real provider reads a brief,
//! drafts the project with `services`, Apply writes it, and the built model is checked as an
//! engineer would: clean networks, every wet fixture joined, every circuit sized.
//!
//! DDC_AI_KEY=... DDC_LIVE_BASE_URL=https://host/v1 DDC_LIVE_MODEL=id
//! cargo test -p ddc-app --test app_ai_e live_services:: -- --ignored --test-threads=1
//! [DDC_LIVE_HEADERS=name=value,...] [DDC_LIVE_TASK_USD=usd]
use crate::{e2e_ai_common, e2e_common};

use ddc_ai::{build_provider, AuthKind, EgressGuard, Limits, Mode, Profile, Protocol, Secret, ToolCalling};
use ddc_app::ai::session::UiEvent;
use ddc_app::ai::UiSession;
use ddc_app::editor::Editor;
use e2e_ai_common::*;
use e2e_common::{count, ids};
use std::time::Duration;

const TURN: Duration = Duration::from_secs(1600);

/// One live session on an empty model; `None` without a key, so the run is skipped.
struct Live {
    ed: Editor,
    s: UiSession,
    errors: Vec<String>,
}

impl Live {
    fn open() -> Option<Live> {
        let key = std::env::var("DDC_AI_KEY").ok()?;
        // provider endpoint, model and extra headers come from the environment, never the repo
        let base_url = std::env::var("DDC_LIVE_BASE_URL").ok()?;
        let model = std::env::var("DDC_LIVE_MODEL").ok()?;
        let host = base_url
            .split("://")
            .nth(1)
            .and_then(|r| r.split('/').next())
            .unwrap_or_default()
            .to_string();
        let extra_headers: Vec<(String, String)> = std::env::var("DDC_LIVE_HEADERS")
            .unwrap_or_default()
            .split(',')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect();
        let per_task: f64 =
            std::env::var("DDC_LIVE_TASK_USD").ok().and_then(|b| b.parse().ok()).unwrap_or(1.5);
        let profile = Profile {
            id: "live".into(),
            name: "live".into(),
            protocol: Protocol::ContentBlocks,
            base_url,
            model,
            auth: AuthKind::Header { name: "x-api-key".into() },
            extra_headers,
            tool_calling: ToolCalling::Native,
            images: true,
            context_tokens: 200_000,
            max_output_tokens: 8192,
            price_in_per_mtok: Some(2.0),
            price_out_per_mtok: Some(10.0),
            executor: None,
            reach: None,
        };
        let guard = EgressGuard { local_only: false, consented_host: Some(host) };
        let provider = build_provider(&profile, Some(Secret::new(key)), guard).expect("provider");
        let mut ed = Editor::new_in_memory().unwrap();
        ed.reload().unwrap();
        let mut c = cfg(ddc_ai::fake::ScriptedProvider::new(vec![]));
        c.provider = provider;
        c.profile = profile;
        c.limits = Limits {
            max_steps: 40,
            max_tool_calls: 120,
            max_turn_tokens: 8_000_000,
            max_cost: Some(per_task),
            max_wall_s: 1500,
            ..Limits::default()
        };
        let view = view_of(&ed);
        let mut s = UiSession::start(&mut ed, c, view).unwrap();
        assert!(s.wait_idle(&mut ed, WAIT), "the session did not open");
        Some(Live { ed, s, errors: vec![] })
    }

    /// One turn, then Apply of what it drafted; the plan must exist and apply cleanly.
    fn say(&mut self, brief: &str) {
        let view = view_of(&self.ed);
        self.s.send_turn(&mut self.ed, brief.to_string(), view, Mode::Propose).unwrap();
        assert!(self.s.wait_idle(&mut self.ed, TURN), "the turn did not finish");
        for e in self.s.drain_events() {
            match e {
                UiEvent::Tool { name, ok: Some(false), err, .. } => {
                    self.errors.push(format!("{name}: {}", err.unwrap_or_default()))
                }
                UiEvent::Failed(e) => self.errors.push(format!("failed: {e}")),
                _ => {}
            }
        }
        let plan = self.s.plan().expect("the assistant drafted no plan");
        assert!(!plan.items.is_empty(), "the plan is empty; errors: {:?}", self.errors);
        if !plan.unreviewed.is_empty() || self.s.redraft().is_some() {
            let rev = self.s.plan_rev();
            self.s.review(Some(rev));
            assert!(self.s.wait_idle(&mut self.ed, WAIT));
        }
        self.s.apply(&mut self.ed).expect("Apply");
        assert!(self.s.wait_idle(&mut self.ed, WAIT), "the worker did not take the Apply");
    }
}

fn skipped() {
    eprintln!("DDC_AI_KEY is not set: live scenario skipped");
}

#[test]
#[ignore = "spends API credit; needs DDC_AI_KEY"]
fn e01_house_with_electrical() {
    let Some(mut l) = Live::open() else { return skipped() };
    l.say(
        "A two-storey detached house, about 10 by 8 m: living room, kitchen, hall and a toilet \
         below; three bedrooms and a bathroom above. Design the electrical installation too: \
         sockets, lighting, a distribution board and circuits. No water services.",
    );
    let f = assert_services(&l.ed.model, false);
    assert!(f.circuits >= 4, "{f:?}");
    assert!(f.power_runs > 0, "{f:?}");
    assert_eq!(f.water_runs, 0, "{f:?}");
    assert!(count(&l.ed.model, "wall") > 0);
}

#[test]
#[ignore = "spends API credit; needs DDC_AI_KEY"]
fn e02_apartment_section_with_unit_boards() {
    let Some(mut l) = Live::open() else { return skipped() };
    l.say(
        "An apartment section of five floors, two apartments per floor, each with a living \
         room, kitchen, bedroom and bathroom. Electrical only, with a distribution board in \
         every apartment's hall fed from a board on each floor.",
    );
    let f = assert_services(&l.ed.model, false);
    let boards = comps_of(&l.ed.model, &["Distribution Board"]);
    // 10 apartments with their own board, plus the floor boards
    assert!(boards.len() >= 10 + 5, "{} boards; {f:?}", boards.len());
    assert!(f.circuits >= 10, "{f:?}");
}

#[test]
#[ignore = "spends API credit; needs DDC_AI_KEY"]
fn e03_office_with_floor_boards() {
    let Some(mut l) = Live::open() else { return skipped() };
    l.say(
        "A four-storey office building with open offices, meeting rooms, a kitchenette and \
         toilets on each floor. Power and lighting with one distribution board per floor, not \
         per room.",
    );
    let f = assert_services(&l.ed.model, false);
    let boards = comps_of(&l.ed.model, &["Distribution Board"]);
    assert!(boards.len() >= 4, "{} boards; {f:?}", boards.len());
    assert!(f.circuits >= 8, "{f:?}");
    let stories = count(&l.ed.model, "level");
    assert!(boards.len() <= (stories as usize) * 2, "boards {} for {stories} levels", boards.len());
}

#[test]
#[ignore = "spends API credit; needs DDC_AI_KEY"]
fn e04_electrical_switched_off_and_on_keeps_water_and_fixture_keys() {
    let Some(mut l) = Live::open() else { return skipped() };
    l.say(
        "A two-storey house with a living room, kitchen, hall and toilet below, two bedrooms \
         and a bathroom above, with electrical installation and water supply and drainage.",
    );
    let f = assert_services(&l.ed.model, true);
    assert!(f.circuits > 0 && f.water_runs > 0, "{f:?}");
    let water = water_run_ids(&l.ed.model);
    let fixtures = ids(&l.ed.model, "comp");
    l.say("Revise the project: switch the electrical installation off, keep the water services.");
    let off = assert_services(&l.ed.model, true);
    assert_eq!(off.circuits, 0, "{off:?}");
    assert_eq!(off.power_runs, 0, "{off:?}");
    assert_eq!(water_run_ids(&l.ed.model), water, "switching power off must keep the pipes");
    let mid = ids(&l.ed.model, "comp");
    assert!(fixtures.iter().filter(|c| mid.contains(c)).count() > 0);
    l.say("Revise the project again: switch the electrical installation back on.");
    let on = assert_services(&l.ed.model, true);
    assert!(on.circuits > 0 && on.power_runs > 0, "{on:?}");
    assert_eq!(water_run_ids(&l.ed.model), water, "switching power on must keep the pipes");
    let now = ids(&l.ed.model, "comp");
    let lost: Vec<&i64> = fixtures.iter().filter(|c| !now.contains(c)).collect();
    assert!(lost.is_empty(), "fixtures changed their keys: {lost:?}");
}

#[test]
#[ignore = "spends API credit; needs DDC_AI_KEY"]
fn w01_house_with_water() {
    let Some(mut l) = Live::open() else { return skipped() };
    l.say(
        "A two-storey detached house: living room, kitchen, hall and toilet below; two \
         bedrooms and a bathroom above. Add cold and hot water supply and drainage to every \
         wet fixture. No electrical design.",
    );
    let f = assert_services(&l.ed.model, true);
    assert!(f.wet >= 3, "{f:?}");
    assert!(f.water_runs > 0, "{f:?}");
    assert_eq!(f.circuits, 0, "{f:?}");
}

#[test]
#[ignore = "spends API credit; needs DDC_AI_KEY"]
fn w02_apartment_section_with_water_and_appliances() {
    let Some(mut l) = Live::open() else { return skipped() };
    l.say(
        "An apartment section of four floors, two apartments per floor, each with a living \
         room, kitchen, bedroom and bathroom. Water supply and drainage with a washing \
         machine, dishwasher and cooker in every kitchen where it fits, and the power for \
         them.",
    );
    let f = assert_services(&l.ed.model, true);
    assert!(f.wet >= 8, "{f:?}");
    assert!(f.appliances >= 8, "{f:?}");
    assert!(f.circuits >= 8, "{f:?}");
    assert!(comps_of(&l.ed.model, &["Cooker"]).len() >= 1);
}

#[test]
#[ignore = "spends API credit; needs DDC_AI_KEY"]
fn w03_school_with_public_toilets() {
    let Some(mut l) = Live::open() else { return skipped() };
    l.say(
        "A three-storey school: classrooms along a corridor on each floor, and public toilets \
         for pupils and staff on every floor. Design water supply and drainage for the toilets \
         and washbasins.",
    );
    let f = assert_services(&l.ed.model, true);
    assert!(f.wet >= 6, "{f:?}");
    assert!(f.water_runs > 0, "{f:?}");
}

#[test]
#[ignore = "spends API credit; needs DDC_AI_KEY"]
fn w04_hotel_with_water() {
    let Some(mut l) = Live::open() else { return skipped() };
    l.say(
        "A five-storey hotel with twelve rooms per floor, each with its own bathroom, and a \
         ground floor lobby with public toilets. Water supply and drainage for every bathroom.",
    );
    let f = assert_services(&l.ed.model, true);
    assert!(f.wet >= 20, "{f:?}");
    assert!(f.water_runs > 0, "{f:?}");
}
