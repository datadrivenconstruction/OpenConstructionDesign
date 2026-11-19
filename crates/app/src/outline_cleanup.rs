// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! «Clean up outline» for a floor, ceiling or shaft saved with a spike (a near-collinear back
//! turn, refused in new outlines): a preview shows the outline before and after, Apply writes
//! it as one undo step. Opened only by the command, never on its own and never on project open.

use std::cell::RefCell;
use std::sync::Arc;

use ddc_model::Model;
use egui::{self, Pos2, Stroke};

use crate::editor::{Editor, OpFn, Sel};
use crate::shell::EditorApp;
use crate::ui_kit as kit;

pub const COMMAND: &str = "ctx.floor.clean_outline";
pub const CEILING_COMMAND: &str = "ctx.ceiling.clean_outline";
pub const SHAFT_COMMAND: &str = "ctx.shaft.clean_outline";
const KEY: &str = "spike1.clean";

/// An element whose saved outline can be cleaned up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Host {
    Floor(i64),
    Ceiling(i64),
    Shaft(i64),
}

type Rings = Vec<Vec<(f64, f64)>>;

impl Host {
    /// The host of a selected element; None for every other kind.
    pub fn of(s: Sel) -> Option<Host> {
        let id = match s {
            Sel::Slab(i) | Sel::Body(_, i) | Sel::Ext(_, i) => i,
            _ => return None,
        };
        match crate::elem::of_sel(s)?.table {
            "slab" => Some(Host::Floor(id)),
            "ceiling" => Some(Host::Ceiling(id)),
            "shaft" => Some(Host::Shaft(id)),
            _ => None,
        }
    }

    fn rings(self, m: &Model) -> Result<Rings, ddc_model::Error> {
        match self {
            Host::Floor(id) => m.slab_rings(id),
            Host::Ceiling(id) => m.ceiling_rings(id),
            Host::Shaft(id) => Ok(vec![m.shaft_ring(id)?]),
        }
    }

    fn cleanup(self, m: &Model) -> Result<Option<(Rings, usize)>, ddc_model::Error> {
        match self {
            Host::Floor(id) => m.slab_outline_cleanup(id),
            Host::Ceiling(id) => m.ceiling_outline_cleanup(id),
            Host::Shaft(id) => m.shaft_outline_cleanup(id),
        }
    }

    /// Writes exactly the previewed `after`; the model refuses a stale `before`.
    fn apply(
        self,
        m: &mut Model,
        before: &[Vec<(f64, f64)>],
        after: &[Vec<(f64, f64)>],
    ) -> Result<(), ddc_model::Error> {
        match self {
            Host::Floor(id) => m.apply_slab_outline_cleanup(id, before, after),
            Host::Ceiling(id) => m.apply_ceiling_outline_cleanup(id, before, after),
            Host::Shaft(id) => m.apply_shaft_outline_cleanup(id, before, after),
        }
    }

    fn intro_key(self) -> &'static str {
        match self {
            Host::Floor(_) => "outline_cleanup.intro",
            Host::Ceiling(_) => "outline_cleanup.intro_ceiling",
            Host::Shaft(_) => "outline_cleanup.intro_shaft",
        }
    }

    fn none_key(self) -> &'static str {
        match self {
            Host::Floor(_) => "outline_cleanup.none",
            Host::Ceiling(_) => "outline_cleanup.none_ceiling",
            Host::Shaft(_) => "outline_cleanup.none_shaft",
        }
    }
}

/// A clean-up waiting for the answer: rings before and after, spikes found.
#[derive(Clone, Debug, PartialEq)]
pub struct Pending {
    pub host: Host,
    pub before: Vec<Vec<(f64, f64)>>,
    pub after: Vec<Vec<(f64, f64)>>,
    pub spikes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ask {
    Apply,
    Cancel,
}

thread_local! {
    static ASK: RefCell<Option<Pending>> = const { RefCell::new(None) };
}

pub fn register(app: &mut EditorApp) {
    for id in [COMMAND, CEILING_COMMAND, SHAFT_COMMAND] {
        app.register_command(id, open);
    }
}

fn open(a: &mut EditorApp) {
    let ru = a.sh.cat.lang == ddc_ui::Lang::Ru;
    let host = a.ed.selection.iter().find_map(|s| Host::of(*s));
    let key = match host.map(|h| (h, prepare(&a.ed, h))) {
        None => "outline_cleanup.need_host",
        Some((_, Ok(Some(p)))) => {
            set_pending(Some(p));
            return;
        }
        Some((h, Ok(None))) => h.none_key(),
        Some((_, Err(_))) => "outline_cleanup.failed",
    };
    a.ed.status = Some(crate::tr_lang(ru, key, &[]));
}

/// The preview of `host`: None when its outline has no spike. Writes nothing.
pub fn prepare(ed: &Editor, host: Host) -> Result<Option<Pending>, ddc_model::Error> {
    let before = host.rings(&ed.model)?;
    Ok(host
        .cleanup(&ed.model)?
        .map(|(after, spikes)| Pending {
            host,
            before,
            after,
            spikes,
        }))
}

pub fn set_pending(p: Option<Pending>) {
    ASK.with(|a| *a.borrow_mut() = p);
}

pub fn pending() -> Option<Pending> {
    ASK.with(|a| a.borrow().clone())
}

/// What Apply did with a preview.
#[derive(Clone, Debug, PartialEq)]
pub enum Applied {
    /// The previewed outline is written: one model op, so one undo step.
    Done,
    /// The outline changed since the preview: nothing written; the fresh preview, if any.
    Changed(Option<Pending>),
}

/// Apply writes exactly what `p` showed; if the outline moved since, it writes nothing and
/// previews again. Every outline kind applies through here.
pub fn apply(ed: &mut Editor, p: &Pending) -> Result<Applied, ddc_model::Error> {
    if p.host.rings(&ed.model)? != p.before {
        return prepare(ed, p.host).map(Applied::Changed);
    }
    let (host, before, after) = (p.host, p.before.clone(), p.after.clone());
    let f = move |m: &mut Model| host.apply(m, &before, &after);
    ed.run_op(OpFn(Arc::new(f)))?;
    Ok(Applied::Done)
}

/// The modal; the answer of this frame, if any. Esc is Cancel.
pub fn modal(ctx: &egui::Context, p: &Pending, ru: bool) -> Option<Ask> {
    let tr = |k: &'static str| crate::tr_str(ru, k);
    let th = ddc_ui::theme::of(ctx);
    let mut ev = None;
    let out = kit::modal_dialog(KEY, ctx, |ui| {
        let row = th.d().row;
        ui.set_width(row * 16.0);
        kit::modal_title(ui, tr("outline_cleanup.title"));
        let n = p.spikes.to_string();
        ui.label(crate::tr_lang(ru, p.host.intro_key(), &[("n", &n)]));
        preview(ui, p, egui::vec2(row * 16.0, row * 8.0));
        let sem = th.sem();
        ui.label(egui::RichText::new(tr("outline_cleanup.legend_before")).color(sem.warning));
        ui.label(egui::RichText::new(tr("outline_cleanup.legend_after")).color(sem.accent));
        ui.label(egui::RichText::new(tr("outline_cleanup.undo")).color(sem.text_secondary));
        kit::dialog_footer(ui, |ui| {
            if kit::primary_button(ui, tr("outline_cleanup.apply"), true).clicked() {
                ev = Some(Ask::Apply);
            }
            if kit::cancel_button(ui, tr("outline_cleanup.cancel")).clicked() {
                ev = Some(Ask::Cancel);
            }
        });
    });
    if out.should_close() {
        ev = Some(Ask::Cancel);
    }
    ev
}

/// Both outlines in one box: before in the warning colour, after over it in the accent.
fn preview(ui: &mut egui::Ui, p: &Pending, size: egui::Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let sem = *ddc_ui::theme::of(ui.ctx()).sem();
    let painter = ui.painter_at(rect);
    painter.rect_stroke(
        rect,
        2.0,
        Stroke::new(1.0, sem.border_subtle),
        egui::StrokeKind::Inside,
    );
    let pts = p.before.iter().chain(&p.after).flatten();
    let (mut lo, mut hi) = ((f64::MAX, f64::MAX), (f64::MIN, f64::MIN));
    for &(x, y) in pts {
        lo = (lo.0.min(x), lo.1.min(y));
        hi = (hi.0.max(x), hi.1.max(y));
    }
    let span = (hi.0 - lo.0).max(hi.1 - lo.1);
    if !(span.is_finite() && span > 0.0) {
        return;
    }
    let inner = rect.shrink(8.0);
    let k = (inner.width().min(inner.height()) as f64) / span;
    // plan y up, screen y down
    let to = |&(x, y): &(f64, f64)| {
        Pos2::new(
            inner.center().x + ((x - (lo.0 + hi.0) * 0.5) * k) as f32,
            inner.center().y - ((y - (lo.1 + hi.1) * 0.5) * k) as f32,
        )
    };
    let draw = |rs: &[Vec<(f64, f64)>], s: Stroke| {
        for r in rs {
            painter.add(egui::Shape::closed_line(r.iter().map(to).collect(), s));
        }
    };
    draw(&p.before, Stroke::new(3.0, sem.warning));
    draw(&p.after, Stroke::new(1.5, sem.accent));
}

/// Draws the pending clean-up and applies its answer.
pub fn frame(app: &mut EditorApp, ctx: &egui::Context) {
    let Some(p) = pending() else {
        return;
    };
    let ru = app.sh.cat.lang == ddc_ui::Lang::Ru;
    let Some(ask) = modal(ctx, &p, ru) else {
        return;
    };
    set_pending(None);
    if ask == Ask::Apply {
        app.ed.status = Some(match apply(&mut app.ed, &p) {
            Ok(Applied::Done) => {
                crate::tr_lang(ru, "outline_cleanup.done", &[("n", &p.spikes.to_string())])
            }
            Ok(Applied::Changed(fresh)) => {
                let key = if fresh.is_some() { "outline_cleanup.changed" } else { p.host.none_key() };
                set_pending(fresh);
                crate::tr_lang(ru, key, &[])
            }
            Err(e) => format!("{}: {e}", crate::tr_lang(ru, "outline_cleanup.failed", &[])),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::{CeilingSpec, LayerSpec, ShaftSpec, SlabSpec};

    /// A floor saved with the 45-degree snap spike (tip at index 1), written past the check.
    fn legacy(ed: &mut Editor) -> i64 {
        let m = &mut ed.model;
        let lv = m.add_level("L1", 0.0).unwrap();
        let layer = LayerSpec {
            material: "concrete".into(),
            thickness: 0.2,
        };
        let t = m.add_slab_type("floor", &[layer]).unwrap();
        let sq = vec![(0., 0.), (4., 0.), (4., 4.), (0., 4.)];
        let spec = SlabSpec {
            level: lv,
            slab_type: t,
            offset: 0.0,
            outer: sq,
            holes: vec![],
        };
        let s = m.add_slab(&spec).unwrap();
        let db = m.sql();
        db.execute("DELETE FROM slab_vertex WHERE slab=?1", [s]).unwrap();
        for (i, p) in spiked().iter().enumerate() {
            db.execute(
                "INSERT INTO slab_vertex VALUES (?1,0,?2,?3,?4)",
                rusqlite::params![s, i as i64, p.0, p.1],
            )
            .unwrap();
        }
        s
    }

    fn spiked() -> Vec<(f64, f64)> {
        let s = std::f64::consts::FRAC_1_SQRT_2;
        let c = 4.0 - s * 1.5;
        vec![(1., 1.), (4., 4.), (c, c + 1e-15), (0., 3.)]
    }

    fn font_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        ctx
    }

    #[test]
    fn preview_writes_nothing_and_apply_is_one_undo() {
        let mut ed = Editor::new_in_memory().unwrap();
        let s = legacy(&mut ed);
        let p = prepare(&ed, Host::Floor(s)).unwrap().expect("a spike");
        assert_eq!((p.spikes, p.before[0].clone()), (1, spiked()));
        assert_eq!(ed.model.slab_rings(s).unwrap()[0], spiked(), "preview is read-only");
        let undo = ed.can_undo();
        assert_eq!(apply(&mut ed, &p).unwrap(), Applied::Done);
        assert_eq!(ed.model.slab_rings(s).unwrap(), p.after);
        assert!(ed.can_undo());
        ed.undo();
        assert_eq!(ed.model.slab_rings(s).unwrap()[0], spiked(), "one undo restores it");
        assert_eq!(ed.can_undo(), undo, "the clean-up was a single step");
        ed.redo();
        assert_eq!(ed.model.slab_rings(s).unwrap(), p.after, "redo writes the preview again");
        // a clean outline has nothing to offer
        assert!(prepare(&ed, Host::Floor(s)).unwrap().is_none());
    }

    #[test]
    fn apply_after_the_floor_changed_writes_nothing_and_previews_again() {
        let mut ed = Editor::new_in_memory().unwrap();
        let s = legacy(&mut ed);
        let p = prepare(&ed, Host::Floor(s)).unwrap().unwrap();
        let mut moved = spiked();
        moved[3] = (0., 3.5);
        let o = moved.clone();
        let f = move |m: &mut Model| m.set_slab_boundary(s, &o, &[]);
        ed.run_op(OpFn(Arc::new(f))).unwrap();
        let fresh = match apply(&mut ed, &p).unwrap() {
            Applied::Changed(Some(f)) => f,
            other => panic!("{other:?}"),
        };
        assert_eq!(ed.model.slab_rings(s).unwrap()[0], moved, "the stale preview wrote nothing");
        assert_eq!(fresh.before[0], moved);
        assert_ne!(fresh.after, p.after, "the new preview shows the moved floor");
        // undo takes back the move, not a clean-up
        ed.undo();
        assert_eq!(ed.model.slab_rings(s).unwrap()[0], spiked());
        // the fresh preview is now stale too; the first one applies again
        assert!(matches!(apply(&mut ed, &fresh).unwrap(), Applied::Changed(Some(_))));
        assert_eq!(apply(&mut ed, &p).unwrap(), Applied::Done);
        assert_eq!(ed.model.slab_rings(s).unwrap(), p.after);
    }

    #[test]
    fn moving_another_vertex_of_a_legacy_floor_is_allowed() {
        let mut ed = Editor::new_in_memory().unwrap();
        let s = legacy(&mut ed);
        let mut moved = spiked();
        moved[3] = (0., 3.5);
        let o = moved.clone();
        let f = move |m: &mut Model| m.set_slab_boundary(s, &o, &[]);
        ed.run_op(OpFn(Arc::new(f))).unwrap();
        assert_eq!(ed.model.slab_rings(s).unwrap()[0], moved);
        assert!(pending().is_none(), "nothing is offered on its own");
    }

    #[test]
    fn drawing_a_new_spike_is_refused() {
        let mut ed = Editor::new_in_memory().unwrap();
        let s = legacy(&mut ed);
        let sq = vec![(0., 0.), (4., 0.), (4., 4.), (0., 4.)];
        let f = move |m: &mut Model| m.set_slab_boundary(s, &sq, &[]);
        ed.run_op(OpFn(Arc::new(f))).unwrap();
        // drawn over a clean outline: the spike is new, so it is refused
        let f = move |m: &mut Model| m.set_slab_boundary(s, &spiked(), &[]);
        let r = ed.run_op(OpFn(Arc::new(f)));
        assert!(r.is_err(), "{r:?}");
    }

    /// Texts in both languages, explicit catalog; the global language is never touched.
    #[test]
    fn texts_en_ru() {
        for ru in [false, true] {
            for k in [
                "outline_cleanup.title",
                "outline_cleanup.intro",
                "outline_cleanup.legend_before",
                "outline_cleanup.legend_after",
                "outline_cleanup.undo",
                "outline_cleanup.apply",
                "outline_cleanup.cancel",
                "outline_cleanup.done",
                "outline_cleanup.none",
                "outline_cleanup.failed",
                "outline_cleanup.changed",
                "outline_cleanup.intro_ceiling",
                "outline_cleanup.intro_shaft",
                "outline_cleanup.none_ceiling",
                "outline_cleanup.none_shaft",
                "outline_cleanup.need_host",
            ] {
                let t = crate::tr_lang(ru, k, &[("n", "2")]);
                assert_ne!(t, k, "missing {k}");
                let cyr = t.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c));
                assert_eq!(cyr, ru, "{k}: {t}");
            }
            for c in [COMMAND, CEILING_COMMAND, SHAFT_COMMAND] {
                let cmd = crate::tr_lang(ru, &format!("cmd.{c}"), &[]);
                assert!(cmd.starts_with(if ru { "\u{41e}" } else { "Clean" }), "{cmd}");
                let desc = format!("cmd.{c}.desc");
                assert_ne!(crate::tr_lang(ru, &desc, &[]), desc, "missing {desc}");
            }
        }
    }

    /// Second level at 3 m for shafts.
    fn top_level(ed: &mut Editor) -> i64 {
        ed.model.add_level("Shaft top", 3.0).unwrap()
    }

    fn floor_type(ed: &mut Editor) -> i64 {
        let layer = LayerSpec {
            material: "concrete".into(),
            thickness: 0.2,
        };
        ed.model.add_slab_type("ceiling", &[layer]).unwrap()
    }

    /// A ceiling saved with the spike, written past the check.
    fn legacy_ceiling(ed: &mut Editor) -> i64 {
        let t = floor_type(ed);
        let spec = CeilingSpec {
            level: ed.level,
            slab_type: t,
            height: 2.6,
            outer: vec![(0., 0.), (4., 0.), (4., 4.), (0., 4.)],
            holes: vec![],
        };
        let c = ed.model.add_ceiling(&spec).unwrap();
        let db = ed.model.sql();
        db.execute("DELETE FROM ceiling_vertex WHERE ceiling=?1", [c]).unwrap();
        for (i, p) in spiked().iter().enumerate() {
            db.execute(
                "INSERT INTO ceiling_vertex VALUES (?1,0,?2,?3,?4)",
                rusqlite::params![c, i as i64, p.0, p.1],
            )
            .unwrap();
        }
        c
    }

    /// The spike away from the floor and the ceiling, so the shaft cuts neither.
    fn shaft_ring() -> Vec<(f64, f64)> {
        spiked().iter().map(|p| (p.0 + 20.0, p.1)).collect()
    }

    /// A shaft saved with the spike, written past the check.
    fn legacy_shaft(ed: &mut Editor) -> i64 {
        let top = top_level(ed);
        let spec = ShaftSpec {
            level: ed.level,
            top_level: top,
            base_offset: 0.0,
            top_offset: 0.0,
            ring: vec![(20., 0.), (22., 0.), (22., 2.), (20., 2.)],
        };
        let s = ed.model.add_shaft(&spec).unwrap();
        let db = ed.model.sql();
        db.execute("DELETE FROM shaft_vertex WHERE shaft=?1", [s]).unwrap();
        for (i, p) in shaft_ring().iter().enumerate() {
            db.execute(
                "INSERT INTO shaft_vertex VALUES (?1,?2,?3,?4)",
                rusqlite::params![s, i as i64, p.0, p.1],
            )
            .unwrap();
        }
        s
    }

    fn shaft_sel(id: i64) -> Sel {
        crate::elem::by_table("shaft").and_then(|f| f.sel(id)).expect("shafts are selectable")
    }

    #[test]
    fn ceiling_and_shaft_clean_up_is_previewed_and_one_undo() {
        let mut ed = Editor::new_in_memory().unwrap();
        let c = legacy_ceiling(&mut ed);
        let s = legacy_shaft(&mut ed);
        for (h, before) in [(Host::Ceiling(c), spiked()), (Host::Shaft(s), shaft_ring())] {
            let p = prepare(&ed, h).unwrap().expect("a spike");
            assert_eq!((p.spikes, p.before[0].clone()), (1, before.clone()), "{h:?}");
            assert_eq!(h.rings(&ed.model).unwrap()[0], before, "{h:?}: preview is read-only");
            // a stale preview: the app previews again and the model refuses it on its own
            let mut stale = p.clone();
            stale.before[0].reverse();
            let r = apply(&mut ed, &stale).unwrap();
            assert!(matches!(r, Applied::Changed(Some(_))), "{h:?}: {r:?}");
            assert!(h.apply(&mut ed.model, &stale.before, &p.after).is_err(), "{h:?}");
            let mut other = p.after.clone();
            other[0].reverse();
            assert!(h.apply(&mut ed.model, &p.before, &other).is_err(), "{h:?}: not the preview");
            assert_eq!(h.rings(&ed.model).unwrap()[0], before, "{h:?}: nothing written");
            assert_eq!(apply(&mut ed, &p).unwrap(), Applied::Done, "{h:?}");
            assert_eq!(h.rings(&ed.model).unwrap(), p.after, "{h:?}: exactly the preview");
            assert!(prepare(&ed, h).unwrap().is_none(), "{h:?}: nothing left");
            ed.undo();
            assert_eq!(h.rings(&ed.model).unwrap()[0], before, "{h:?}: one undo restores it");
        }
    }

    #[test]
    fn the_command_takes_a_selected_ceiling_or_shaft() {
        let mut ed = Editor::new_in_memory().unwrap();
        let c = legacy_ceiling(&mut ed);
        let s = legacy_shaft(&mut ed);
        let csel = Sel::Body(crate::editor::bodies::BodyKind::Ceiling, c);
        assert_eq!(Host::of(csel), Some(Host::Ceiling(c)));
        assert_eq!(Host::of(shaft_sel(s)), Some(Host::Shaft(s)));
        assert_eq!(Host::of(Sel::Slab(7)), Some(Host::Floor(7)));
        assert_eq!(Host::of(Sel::Wall(7)), None);
        let mut a = crate::shell::EditorApp::new(ed, None, "test".into());
        for (cmd, sel, h) in [
            (CEILING_COMMAND, csel, Host::Ceiling(c)),
            (SHAFT_COMMAND, shaft_sel(s), Host::Shaft(s)),
        ] {
            set_pending(None);
            a.ed.selection = vec![sel];
            assert!(a.run(cmd), "{cmd} is registered");
            assert_eq!(pending().map(|p| p.host), Some(h), "{cmd}");
        }
        // nothing to clean: no modal, a status line instead
        set_pending(None);
        a.ed.selection = vec![Sel::Wall(1)];
        assert!(a.run(SHAFT_COMMAND));
        assert!(pending().is_none());
        assert!(a.ed.status.is_some());
    }

    /// A project from before spikes were refused, with one in a floor, a ceiling and a shaft:
    /// after a reload every element is still there and drawn, and nothing is cleaned.
    #[test]
    fn mixed_legacy_project_draws_every_element() {
        let mut ed = Editor::new_in_memory().unwrap();
        let f = legacy(&mut ed);
        // all three on the floor's level, the one the editor shows
        let sql = "SELECT level FROM slab WHERE id=?1";
        ed.level = ed.model.sql().query_row(sql, [f], |r| r.get(0)).unwrap();
        let c = legacy_ceiling(&mut ed);
        let s = legacy_shaft(&mut ed);
        ed.reload().unwrap();
        assert!(ed.slabs.iter().any(|x| x.id == f), "the floor is listed");
        let (_, issues) = ed.scene_report();
        assert!(issues.is_empty(), "{issues:?}");
        assert!(ed.model.slab_mesh(f).is_ok(), "the floor draws");
        let (sc, ts) = crate::edit3d::scene3d::compose(&mut ed);
        let shaft = crate::elem::by_table("shaft").and_then(|x| x.code).expect("shaft code");
        use crate::edit3d::pick::{Cat, Target};
        for t in [Target::new(Cat::Ceiling, c), Target::new(Cat::Ext(shaft), s)] {
            let k = ts.iter().position(|x| *x == t).unwrap_or_else(|| panic!("{t:?} drawn"));
            let e = crate::edit3d::scene3d::EXT_BASE + k as u32;
            let drawn = sc.meshes.iter().any(|m| m.element == e && !m.triangles.is_empty());
            assert!(drawn, "{t:?} has a body");
        }
        assert_eq!(ed.model.slab_rings(f).unwrap()[0], spiked(), "never cleaned on its own");
        assert_eq!(ed.model.ceiling_rings(c).unwrap()[0], spiked());
        assert_eq!(ed.model.shaft_ring(s).unwrap(), shaft_ring());
        assert!(pending().is_none(), "nothing is offered on its own");
    }

    #[test]
    fn modal_draws_and_never_answers_by_itself() {
        let mut ed = Editor::new_in_memory().unwrap();
        let s = legacy(&mut ed);
        let p = prepare(&ed, Host::Floor(s)).unwrap().unwrap();
        let ctx = font_ctx();
        for ru in [false, true] {
            let mut got = None;
            for _ in 0..3 {
                let _ = ctx.run(Default::default(), |ctx| got = got.or(modal(ctx, &p, ru)));
            }
            assert_eq!(got, None);
        }
        assert_eq!(ed.model.slab_rings(s).unwrap()[0], spiked());
    }
}
