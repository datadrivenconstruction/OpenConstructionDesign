// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Heavy edits: a cancel at any point leaves the model byte-identical, a finished
//! result is one undo step applied without kernel work, the frame loop runs meanwhile and Esc
//! cancels. Each test has its own geometry: the form caches are process-wide.

use super::*;
use crate::mass_tools::{commit_heavy, warm_edit};
use crate::tools::Rec;
use ddc_geom::cancel;
use ddc_model::{BoolOp, Form, MassSpec};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use web_time::Instant;

#[derive(Debug, PartialEq)]
struct Snap {
    bytes: Vec<u8>,
    head: u64,
    undo: usize,
}

fn snap(ed: &Editor) -> Snap {
    Snap {
        bytes: ed.model.to_bytes().unwrap(),
        head: ed.model.change_head(),
        undo: ed.undo_len(),
    }
}

fn spec(ed: &Editor, form: Form) -> MassSpec {
    MassSpec {
        level: ed.level,
        name: "m".into(),
        void: false,
        inplace: false,
        form,
    }
}

fn square(x: f64, h: f64) -> Vec<(f64, f64)> {
    vec![(x - h, -h), (x + h, -h), (x + h, h), (x - h, h)]
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    Boolean,
    Sweep,
    Loft,
}

type Edit = (Rec, Vec<(Form, Option<Form>)>);

/// Writes what the edit needs first (two forms for the boolean); `o` keeps tests apart.
fn prepare(ed: &mut Editor, k: Kind, o: f64) -> Edit {
    match k {
        Kind::Boolean => {
            let cub = |lo: f64, hi: f64| Form::Cuboid {
                lo: (lo + o, lo, 0.0),
                hi: (hi + o, hi, hi - lo),
            };
            let a = ed.model.add_mass(&spec(ed, cub(0.0, 2.0))).unwrap();
            let b = ed.model.add_mass(&spec(ed, cub(1.0, 3.5))).unwrap();
            let fb = ed.model.mass(b).unwrap().form;
            let warm = warm_edit(ed, a, |fa| Form::Bool {
                op: BoolOp::Union,
                a: fa,
                b: Box::new(fb),
            });
            let r = Rec::new("undo.mass.combine", move |m| {
                m.combine_masses(a, b, BoolOp::Union).map(|_| 1)
            });
            (r, warm)
        }
        Kind::Sweep | Kind::Loft => {
            let form = match k {
                Kind::Sweep => Form::Sweep {
                    profile: square(o, 0.5),
                    path: vec![(0.0, 0.0, 0.0), (0.0, 0.0, 3.0)],
                    ids: vec![],
                },
                _ => Form::Blend {
                    sections: vec![(square(o, 1.0), 0.0), (square(o, 0.5), 3.0)],
                    ids: vec![],
                },
            };
            let s = spec(ed, form);
            let warm = vec![(s.form.clone(), None)];
            let r = Rec::new("undo.mass.add", move |m| m.add_mass(&s).map(|_| 1));
            (r, warm)
        }
    }
}

fn wait_end(ed: &mut Editor) -> HeavyEnd {
    let t = Instant::now();
    loop {
        if let Some(e) = ed.pump_heavy() {
            return e;
        }
        assert!(t.elapsed().as_secs_f64() < 60.0, "heavy edit hung");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn bg_ops_cancel_at_any_point_leaves_the_model_identical() {
    // in place, so the trip hook of this thread stops the kernel at check n; the worker path
    // runs the same compute and apply code
    for (i, k) in [Kind::Boolean, Kind::Sweep, Kind::Loft].into_iter().enumerate() {
        let mut ed = Editor::new_in_memory().unwrap();
        let (r, warm) = prepare(&mut ed, k, 101.0 + 10.0 * i as f64);
        let before = snap(&ed);
        let (mut n, mut cancels) = (1u64, 0);
        loop {
            cancel::trip_at(Some(cancel::checks() + n));
            assert!(commit_heavy(&mut ed, r.clone(), warm.clone(), |_| {}));
            cancel::trip_at(None);
            if ed.model.change_head() != before.head {
                break;
            }
            assert_eq!(snap(&ed), before, "{k:?}: cancelled at check {n}");
            assert_eq!(ed.status, Some(crate::tr("job.cancelled", &[])), "{k:?} at {n}");
            cancels += 1;
            n += n.div_ceil(3);
            assert!(n < 50_000_000, "{k:?}: never finished");
        }
        assert!(cancels >= 2, "{k:?}: only {cancels} cancel points");
        assert_eq!(ed.undo_len(), before.undo + 1, "{k:?}: the finished run is one undo step");
    }
}

#[test]
fn bg_ops_worker_result_is_one_undo_step_applied_without_kernel_work() {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.bg = true;
    let (r, warm) = prepare(&mut ed, Kind::Boolean, 211.0);
    let before = snap(&ed);
    let masses = ed.model.masses().unwrap().len();
    assert!(commit_heavy(&mut ed, r, warm, |_| {}));
    assert!(ed.heavy.is_some(), "runs on a worker");
    assert_eq!(snap(&ed), before, "nothing written while the worker runs");
    // a second heavy edit is refused with a message, the first one goes on
    let (r2, w2) = prepare_refused();
    assert!(!commit_heavy(&mut ed, r2, w2, |_| {}));
    assert_eq!(ed.status, Some(crate::tr("job.busy_heavy", &[])));
    assert!(ed.heavy.is_some());
    let t = Instant::now();
    let end = loop {
        let c0 = cancel::checks();
        if let Some(e) = ed.pump_heavy() {
            assert_eq!(cancel::checks(), c0, "applying on the UI thread ran the kernel");
            break e;
        }
        assert!(t.elapsed().as_secs_f64() < 60.0, "heavy edit hung");
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(end, HeavyEnd::Applied);
    assert_eq!(ed.undo_len(), before.undo + 1);
    assert_eq!(ed.model.masses().unwrap().len(), masses - 1, "b merged into a");
    ed.undo();
    assert_eq!(ed.model.masses().unwrap().len(), masses, "one undo restores both");
}

fn prepare_refused() -> Edit {
    (Rec::new("undo.mass.add", |_| Ok(0)), vec![])
}

#[test]
fn bg_ops_cancel_after_the_worker_finished_drops_the_result() {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.bg = true;
    let (r, warm) = prepare(&mut ed, Kind::Loft, 311.0);
    let before = snap(&ed);
    let done = std::sync::Arc::new(AtomicBool::new(false));
    let d2 = done.clone();
    let started = ed.start_heavy(
        "job.title.form",
        false,
        move |_| {
            for (f, old) in &warm {
                f.prepared(old.as_ref()).map_err(err_text)?;
            }
            d2.store(true, Ordering::SeqCst);
            Ok(())
        },
        move |ed, ()| ed.apply_ext(r).map_err(|e| e.to_string()),
    );
    assert!(started);
    let t = Instant::now();
    while !done.load(Ordering::SeqCst) {
        assert!(t.elapsed().as_secs_f64() < 60.0, "worker hung");
        std::thread::sleep(Duration::from_millis(1));
    }
    // the result is (or is about to be) in the channel: cancel still wins
    std::thread::sleep(Duration::from_millis(30));
    ed.cancel_heavy();
    assert_eq!(wait_end(&mut ed), HeavyEnd::Cancelled);
    assert_eq!(snap(&ed), before);
}

#[test]
fn bg_ops_stale_base_drops_a_data_result() {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.bg = true;
    let gate = std::sync::Arc::new(AtomicBool::new(false));
    let g2 = gate.clone();
    assert!(ed.start_heavy(
        "job.title.form",
        true,
        move |_| {
            while !g2.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(7)
        },
        |_, _: i32| panic!("a stale result must not apply"),
    ));
    let (r, _) = prepare(&mut ed, Kind::Sweep, 411.0);
    ed.apply_ext(r).unwrap();
    gate.store(true, Ordering::SeqCst);
    assert_eq!(wait_end(&mut ed), HeavyEnd::Stale);
    assert_eq!(ed.status, Some(crate::tr("job.stale", &[])));
}

/// A boolean of two finely revolved tubes: seconds of kernel work in a test build.
fn big_boolean(ed: &mut Editor) -> Edit {
    let tube = |ax: f64| {
        let mut profile = vec![(1.0, 0.0)];
        for i in 0..24 {
            profile.push((3.0 + 0.1 * (i % 2) as f64, 4.0 * i as f64 / 23.0));
        }
        profile.push((1.0, 4.0));
        Form::Revolve {
            profile,
            axis: (ax + 511.0, 0.3),
            angle: std::f64::consts::TAU,
            seg: 256,
            ids: vec![],
        }
    };
    let a = ed.model.add_mass(&spec(ed, tube(0.0))).unwrap();
    let b = ed.model.add_mass(&spec(ed, tube(1.7))).unwrap();
    let fb = ed.model.mass(b).unwrap().form;
    let warm = warm_edit(ed, a, |fa| Form::Bool {
        op: BoolOp::Union,
        a: fa,
        b: Box::new(fb),
    });
    let r = Rec::new("undo.mass.combine", move |m| {
        m.combine_masses(a, b, BoolOp::Union).map(|_| 1)
    });
    (r, warm)
}

fn frame(a: &mut crate::shell::EditorApp, ctx: &egui::Context, t: f64, ev: Vec<egui::Event>) {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1366.0, 768.0))),
        time: Some(t),
        events: ev,
        ..Default::default()
    };
    let _ = ctx.run(input, |c| a.ui(c, None));
}

fn esc() -> egui::Event {
    egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: Some(egui::Key::Escape),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }
}

#[test]
fn bg_ops_frames_run_while_a_boolean_computes_and_esc_cancels_it() {
    let mut a = crate::shell::EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
    a.chrome.start.open = false;
    a.ed.bg = true;
    let (r, warm) = big_boolean(&mut a.ed);
    let ctx = egui::Context::default();
    frame(&mut a, &ctx, 0.0, vec![]);
    let before = snap(&a.ed);
    assert!(commit_heavy(&mut a.ed, r, warm, |_| {}));
    let mut t = 0.0;
    for _ in 0..3 {
        t += 0.016;
        frame(&mut a, &ctx, t, vec![]);
        assert!(a.ed.heavy.is_some(), "the kernel is still running: frames go on");
    }
    let v = crate::shell::tasks_ui::current(&a).expect("the status bar shows the job");
    assert_eq!(v.title, "job.title.form");
    assert!(v.cancellable);
    t += 0.016;
    frame(&mut a, &ctx, t, vec![esc()]);
    let h = a.ed.heavy.as_ref();
    assert!(h.is_none_or(|h| h.job.ctl.is_cancelled()), "Esc reached the job");
    let t0 = Instant::now();
    while a.ed.heavy.is_some() {
        t += 0.016;
        frame(&mut a, &ctx, t, vec![]);
        assert!(t0.elapsed().as_secs_f64() < 30.0, "cancel did not stop the kernel");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(snap(&a.ed), before, "cancelled: model byte-identical");
}
