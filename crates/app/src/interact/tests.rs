// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Rules of the interaction spine, one test each.
use super::*;
use super::sessions::select;
use crate::editor::Editor;
use crate::tools::Rec;
use crate::wall_tool::{Event, Mode};
use ddc_geom::{v2, V2};
use ddc_model::refusal::{Refusal, RefusalCode};
use ddc_ui::i18n::Lang;

/// The editor as the app opens it: in Select; the bare editor starts in the wall tool.
fn editor() -> Editor {
    Editor::new_as_app().unwrap()
}

/// The editor with the wall session running (the wall tool armed).
fn wall_editor() -> Editor {
    let mut ed = editor();
    ed.set_mode(Mode::Polyline);
    ed
}

fn esc() -> Input {
    Input::Key(Key::Escape, Mods::NONE)
}

fn click(x: f64, y: f64) -> Input {
    Input::Click(v2(x, y).into(), Btn::Left)
}

fn drafting(tool: &'static str, step: &'static str) -> Scope {
    Scope::Drafting { tool: ToolId(tool), step }
}

/// Deterministic generator (xorshift64): the property test replays the same sequences.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn coord(&mut self) -> f64 {
        (self.below(4001) as f64) / 200.0 - 2.0
    }
}

/// Tools a ribbon command arms without the app shell.
fn arm(ed: &mut Editor, k: u64) {
    match k {
        0 => {}
        1 => ed.set_mode(Mode::Polyline),
        2 => ed.set_mode(Mode::Rect),
        3 => {
            ed.set_mode(Mode::Polyline);
            ed.tool.curtain = true;
            // the app refreshes the interaction every frame
            Interaction::refresh(ed);
        }
        4 => ed.start_opening(crate::opening_tool::OpeningType::door()),
        5 => ed.start_opening(crate::opening_tool::OpeningType::window()),
        6 => ed.start_section(),
        7 => crate::topo::arm(ed, crate::topo::Tool::Points, Lang::En),
        8 => crate::tools::arm(ed, crate::tools::Kind::Column, Lang::En),
        9 => crate::tools::arm(ed, crate::tools::Kind::Beam, Lang::En),
        10 => crate::mass_tools::arm(ed, crate::mass_tools::Cmd::Extrude, Lang::En),
        11 => crate::annot_text::arm(ed, crate::annot_text::Kind::Text, Lang::En),
        12 => crate::annot_text::arm(ed, crate::annot_text::Kind::Leader, Lang::En),
        13 => crate::solid_tools::arm(ed, crate::solid_tools::Kind::Extrusion, false, Lang::En),
        14 => crate::sketch_mode::begin(ed, ddc_model::SketchHost::Slab(1_000_000)),
        _ => ed.set_mode(Mode::PickLine),
    }
}

fn random_input(r: &mut Rng) -> Input {
    let p: P = v2(r.coord(), r.coord()).into();
    match r.below(12) {
        0..=2 => Input::Move(p),
        3..=5 => Input::Click(p, Btn::Left),
        6 => Input::Press(p, Btn::Left),
        7 => Input::Release(p, Btn::Left),
        8 => {
            let k = [Key::Enter, Key::Backspace, Key::Tab, Key::Close, Key::Arc, Key::Escape];
            Input::Key(k[r.below(k.len() as u64) as usize], Mods::NONE)
        }
        9 => Input::Char(['1', '5', '.', ' ', 'x', '-'][r.below(6) as usize]),
        10 => Input::Tick(r.next() % 100_000),
        _ => Input::DblClick(p),
    }
}

/// Session that holds a given scope and keeps itself on every input, Esc included: only the
/// forced end of the second Esc removes it.
struct Hold(Scope);

impl ToolSession for Hold {
    fn id(&self) -> ToolId {
        ToolId("test.hold")
    }
    fn step(&self) -> &'static str {
        "hold"
    }
    fn hints(&self) -> &[Hint] {
        &[]
    }
    fn on_input(&mut self, _i: &Input, _cx: &Cx) -> Step {
        Step::Continue
    }
    fn preview(&self, _cx: &Cx, _out: &mut Preview) {}
    fn commit(&mut self, _cx: &Cx) -> Result<Rec, Refusal> {
        Err(Refusal::new(RefusalCode::Blocked, "ix.refuse.hold"))
    }
    fn scope(&self) -> Scope {
        self.0
    }
}

/// A slab drawn far from the random area, for `shape_edit` (Reshaping).
fn slab(ed: &mut Editor) -> i64 {
    crate::tools::arm(ed, crate::tools::Kind::Floor, Lang::En);
    for (x, y) in [(40.0, 40.0), (44.0, 40.0), (44.0, 44.0), (40.0, 44.0)] {
        ed.handle(Event::Click(v2(x, y)));
    }
    ed.handle(Event::Key(Key::Enter));
    ed.set_mode(Mode::Select);
    ed.model.sql().query_row("SELECT max(id) FROM slab", [], |r| r.get(0)).expect("a slab")
}

/// Start of a sequence: a legacy tool, a grip drag, a slab reshape or a held session.
fn start(ed: &mut Editor, r: &mut Rng, slab_id: i64) {
    match r.below(22) {
        k @ 0..=15 => arm(ed, k),
        16 | 17 => {
            // grab the first end of a wall: the grip drag of the legacy chain
            ed.set_mode(Mode::Select);
            let Some(w) = ed.walls.first().map(|w| (w.id, w.a)) else { return };
            ed.selection = vec![crate::editor::Sel::Wall(w.0)];
            Interaction::update(ed, Input::Move(w.1.into()));
            Interaction::update(ed, Input::Press(w.1.into(), Btn::Left));
            Interaction::update(ed, Input::Move((w.1 + v2(0.4, 0.3)).into()));
        }
        18 => {
            ed.set_mode(Mode::Select);
            crate::shape_edit::begin(ed, slab_id).expect("shape edit of the test slab");
            Interaction::refresh(ed);
        }
        _ => {
            let held = [
                Scope::Moving { op: ModOp::Move },
                Scope::BoxSelect,
                Scope::Painting,
                Scope::Measuring,
                Scope::Pointing,
            ];
            ed.ix.begin(Box::new(Hold(held[r.below(held.len() as u64) as usize])));
        }
    }
}

#[test]
fn two_esc_reach_idle_from_any_scope() {
    let mut ed = editor();
    let slab_id = slab(&mut ed);
    // one wall so the grip start has something to grab
    ed.commit_walls(vec![(v2(30.0, 0.0), v2(36.0, 0.0))]);
    let mut r = Rng(0x0DDC_1A70_5EED_0001);
    let mut seen = std::collections::HashSet::new();
    for n in 0..2000 {
        start(&mut ed, &mut r, slab_id);
        seen.insert(ed.ix.scope().kind());
        for _ in 0..r.below(9) {
            Interaction::update(&mut ed, random_input(&mut r));
            seen.insert(ed.ix.scope().kind());
        }
        let depth = ed.undo_entries().1.len();
        Interaction::update(&mut ed, esc());
        Interaction::update(&mut ed, esc());
        assert_eq!(*ed.ix.scope(), Scope::Idle, "sequence {n}");
        assert_eq!(Scope::from_legacy(&ed), Scope::Idle, "sequence {n}: flags left on");
        assert!(!ed.is_busy(), "sequence {n}: editor still busy");
        assert_eq!(ed.ix.session_id(), None, "sequence {n}: session left on");
        // Esc never takes back what is committed; at most it finishes a chain
        assert!(ed.undo_entries().1.len() >= depth, "sequence {n}: Esc undid a record");
    }
    let missing: Vec<_> = ScopeKind::ALL.iter().filter(|k| !seen.contains(k)).collect();
    assert!(missing.is_empty(), "never reached: {missing:?}");
}

#[test]
fn legacy_reset_leaves_no_flag_and_writes_nothing() {
    let mut r = Rng(0x0DDC_1A70_5EED_0002);
    // 11 and 12 arm text tools, whose cancel keeps typed text by committing it (by design)
    for k in (0..16).filter(|k| ![11, 12].contains(k)) {
        for n in 0..20 {
            let mut ed = editor();
            arm(&mut ed, k);
            for _ in 0..r.below(6) {
                Interaction::update(&mut ed, random_input(&mut r));
            }
            let before = ed.pushes;
            select::reset(&mut ed);
            assert_eq!(ed.pushes, before, "arm {k} run {n}");
            assert_eq!(Scope::from_legacy(&ed), Scope::Idle, "arm {k} run {n}");
            assert!(!ed.is_busy(), "arm {k} run {n}");
        }
    }
}

#[test]
fn pointer_motion_keeps_the_esc_run_and_a_click_breaks_it() {
    let mut ed = editor();
    ed.set_mode(Mode::Polyline);
    for i in [click(0.0, 0.0), click(4.0, 0.0), esc(), Input::Move(v2(1.0, 1.0).into()), Input::Tick(5)] {
        Interaction::update(&mut ed, i);
    }
    assert_eq!(*ed.ix.scope(), drafting("arch.wall", "first"));
    Interaction::update(&mut ed, esc());
    assert_eq!(*ed.ix.scope(), Scope::Idle);
    // Esc, click, Esc is no run: the chain started by the click ends, the tool stays (the first
    // Esc ends a chain: in an idle tool it would leave the tool, 6e94564ba)
    ed.set_mode(Mode::Polyline);
    for i in [click(0.0, 3.0), esc(), click(0.0, 5.0), esc()] {
        Interaction::update(&mut ed, i);
    }
    assert_eq!(ed.wall_mode(), Mode::Polyline);
}

#[test]
fn a_ribbon_command_starts_a_new_esc_run() {
    let mut ed = editor();
    // Esc in Idle, then the wall tool from the ribbon: the run starts over
    Interaction::update(&mut ed, esc());
    assert_eq!(ed.ix.esc_run, 1);
    ed.set_mode(Mode::Polyline);
    assert_eq!(ed.ix.esc_run, 0);
    // the next Esc is the first of a run: an idle tool is left by the old chain itself
    Interaction::update(&mut ed, Input::Move(v2(3.0, 0.0).into()));
    Interaction::update(&mut ed, esc());
    assert_eq!(ed.ix.esc_run, 1);
    assert_eq!(*ed.ix.scope(), Scope::Idle);
    // a refresh that keeps the scope keeps the run
    Interaction::update(&mut ed, esc());
    Interaction::refresh(&mut ed);
    assert_eq!(ed.ix.esc_run, 2);
}

#[test]
fn scope_table_snapshot() {
    let rows: Vec<String> = SCOPE_TABLE
        .iter()
        .map(|(k, h, o, _)| format!("{k:?}: hot={:04x} {}", h.0, o.names().join(",")))
        .collect();
    let want = "\
Idle: hot=ffff grips,selection,hover
Drafting: hot=0000 temp_dims,snap,preview,hint
Placing: hot=0075 snap,preview,hover,hint
Moving: hot=0000 temp_dims,snap,preview,selection,hint
GripDrag: hot=0000 grips,temp_dims,snap,preview,hint
Reshaping: hot=0000 grips,snap,preview,hint
BoxSelect: hot=ffff selection,box
Painting: hot=efff hover,hint
Measuring: hot=ffff temp_dims,snap,measure,hint
Pointing: hot=ffff snap,hover,hint";
    assert_eq!(rows.join("\n"), want);
    for (i, k) in ScopeKind::ALL.iter().enumerate() {
        assert_eq!(SCOPE_TABLE[i].0, *k);
    }
}

#[test]
fn hot_set_and_overlays_are_functions_of_the_scope() {
    use crate::edit3d::pick::Cat;
    let door = Scope::Placing { tool: ToolId("arch.door") };
    assert!(HotSet::of(&door).has(Cat::Wall));
    assert!(!HotSet::of(&door).has(Cat::Room));
    assert!(HotSet::of(&Scope::Idle).has(Cat::Ext(0x1B10)));
    assert!(!HotSet::of(&drafting("arch.wall", "next")).has(Cat::Wall));
    assert!(!HotSet::of(&Scope::Painting).has(Cat::Room));
    assert!(OverlaySet::of(&Scope::Idle).has(OverlaySet::GRIPS));
    assert!(OverlaySet::of(&drafting("arch.wall", "first")).has(OverlaySet::PREVIEW));
    assert!(!OverlaySet::of(&drafting("arch.wall", "first")).has(OverlaySet::GRIPS));
}

#[test]
fn legacy_scope_matches_flags() {
    let cases: Vec<(u64, Scope)> = vec![
        (0, Scope::Idle),
        (1, drafting("arch.wall", "first")),
        (2, drafting("arch.wall_room", "first")),
        (3, drafting("arch.curtain_wall", "first")),
        (4, Scope::Placing { tool: ToolId("arch.door") }),
        (5, Scope::Placing { tool: ToolId("arch.window") }),
        (6, drafting("arch.section", "first")),
        (7, drafting("site.toposolid", "first")),
        (8, Scope::Placing { tool: ToolId("arch.column") }),
        (9, drafting("str.beam", "first")),
        (10, drafting(crate::mass_tools::Cmd::Extrude.id(), "first")),
        (11, Scope::Placing { tool: ToolId("ann.text") }),
        (12, Scope::Placing { tool: ToolId("ann.leader") }),
        (13, drafting("arch.solid_extrusion", "first")),
        (14, drafting("sketch", "line")),
    ];
    for (k, want) in cases {
        let mut ed = editor();
        arm(&mut ed, k);
        // a tool that owns its state (section) has no flag left: its session names the scope
        let flags = if ed.ix.session_id().is_some() { *ed.ix.scope() } else { Scope::from_legacy(&ed) };
        assert_eq!(flags, want, "arm {k}");
        // a busy editor is never idle, an idle scope is never busy
        assert_eq!(ed.is_busy(), want != Scope::Idle, "arm {k}");
        Interaction::refresh(&mut ed);
        assert_eq!(*ed.ix.scope(), want, "arm {k}");
    }
    // the first click of a chain moves the step on
    let mut ed = editor();
    ed.set_mode(Mode::Polyline);
    ed.handle(Event::Click(v2(0.0, 0.0)));
    assert_eq!(*ed.ix.scope(), drafting("arch.wall", "next"));
    // a grip drag names the element and the role
    let mut ed = editor();
    let role = crate::grips::Role::End(1);
    ed.grip.active = Some(crate::grips::Active {
        grip: crate::grips::Grip {
            kind: crate::grips::Kind::Wall,
            id: 7,
            role: role.clone(),
            pos: v2(1.0, 0.0),
            view: String::new(),
        },
        now: v2(1.0, 0.0),
        preview: vec![],
        dims: vec![],
        error: None,
    });
    assert_eq!(
        Scope::from_legacy(&ed),
        Scope::GripDrag {
            target: Target::Elem(crate::grips::Kind::Wall, 7),
            grip: scope::grip_code(&role)
        }
    );
}

#[test]
fn grip_codes_are_distinct() {
    use crate::grips::Role;
    let rs = [
        Role::End(0),
        Role::End(1),
        Role::Flip,
        Role::Vertex(3),
        Role::Edge(3),
        Role::Move,
        Role::HoleVertex(1, 2),
        Role::HoleEdge(1, 2),
        Role::Insert(3),
        Role::Param("w".into(), 0),
        Role::Param("w".into(), 1),
    ];
    let codes: std::collections::HashSet<u32> = rs.iter().map(scope::grip_code).collect();
    assert_eq!(codes.len(), rs.len());
}

/// Every source of the module, as compiled.
const SOURCES: [(&str, &str); 23] = [
    ("host.rs", include_str!("host.rs")),
    ("adapter.rs", include_str!("adapter.rs")),
    ("mod.rs", include_str!("mod.rs")),
    ("pointer.rs", include_str!("pointer.rs")),
    ("preview.rs", include_str!("preview.rs")),
    ("scope.rs", include_str!("scope.rs")),
    ("session.rs", include_str!("session.rs")),
    ("input.rs", include_str!("input.rs")),
    ("hotset.rs", include_str!("hotset.rs")),
    ("overlay.rs", include_str!("overlay.rs")),
    ("parity.rs", include_str!("parity.rs")),
    ("hints.rs", include_str!("hints.rs")),
    ("sessions/mod.rs", include_str!("sessions/mod.rs")),
    ("sessions/model_tool.rs", include_str!("sessions/model_tool.rs")),
    ("sessions/section.rs", include_str!("sessions/section.rs")),
    ("sessions/select.rs", include_str!("sessions/select.rs")),
    ("sessions/arch.rs", include_str!("sessions/arch.rs")),
    ("sessions/special.rs", include_str!("sessions/special.rs")),
    ("sessions/grip.rs", include_str!("sessions/grip.rs")),
    ("sessions/modify.rs", include_str!("sessions/modify.rs")),
    ("sessions/view3d.rs", include_str!("sessions/view3d.rs")),
    ("script.rs", include_str!("script.rs")),
    ("tests.rs", include_str!("tests.rs")),
];

#[test]
fn interact_has_no_egui() {
    let banned = [["eg", "ui"].concat(), ["efr", "ame"].concat()];
    // the test's own name is the one allowed mention
    let own = format!("interact_has_no_{}", banned[0]);
    for (f, src) in SOURCES {
        let src = src.replace(&own, "");
        for b in &banned {
            assert!(!src.contains(b.as_str()), "{f} names {b}");
        }
    }
    // every file of the directory is in the list
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/interact");
    let rs_in = |sub: &str| -> Vec<String> {
        std::fs::read_dir(dir.join(sub))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".rs"))
            .map(|n| if sub.is_empty() { n } else { format!("{sub}/{n}") })
            .collect()
    };
    // the sessions live one level down: they are listed with their folder
    let mut on_disk: Vec<String> = rs_in("");
    on_disk.extend(rs_in("sessions"));
    on_disk.sort();
    let mut listed: Vec<String> = SOURCES.iter().map(|s| s.0.to_string()).collect();
    listed.sort();
    assert_eq!(on_disk, listed);
}

/// Journal rows without the clock column: what "the same model" means for a replay.
fn journal(ed: &Editor) -> String {
    let db = ed.model.sql();
    let cols: Vec<String> = db
        .prepare("SELECT name FROM pragma_table_info('op') WHERE name<>'stamp' ORDER BY cid")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|c| format!("quote({})", c.unwrap()))
        .collect();
    let sql = format!("SELECT {} FROM op ORDER BY seq", cols.join("||'|'||"));
    let rows: Vec<String> = db
        .prepare(&sql)
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    rows.join("\n")
}

#[test]
fn wall_scenario_walks_the_scope_and_keeps_the_journal() {
    let steps = [click(0.0, 0.0), click(5.0, 0.0), esc(), esc()];
    let mut ed = editor();
    ed.set_mode(Mode::Polyline);
    let mut walk = vec![*ed.ix.scope()];
    for i in steps {
        Interaction::update(&mut ed, i);
        walk.push(*ed.ix.scope());
    }
    walk.dedup();
    assert_eq!(
        walk,
        vec![
            drafting("arch.wall", "first"),
            drafting("arch.wall", "next"),
            drafting("arch.wall", "first"),
            Scope::Idle
        ]
    );
    // the same events down the old chain directly: the journal is the same, byte for byte
    let mut old = editor();
    old.set_mode(Mode::Polyline);
    for i in steps {
        select::route(&mut old, i.event().unwrap());
    }
    assert_eq!(old.walls.len(), 1);
    assert_eq!(journal(&ed), journal(&old));
    assert!(!journal(&ed).is_empty());
}

#[test]
fn input_round_trips_the_legacy_event() {
    let p = v2(1.5, -2.0);
    for e in [
        Event::Move(p),
        Event::Click(p),
        Event::Press(p),
        Event::Release(p),
        Event::Key(Key::Tab),
        Event::Char('7'),
    ] {
        assert_eq!(Input::from(e).event(), Some(e));
    }
    for i in [
        Input::Click(p.into(), Btn::Right),
        Input::DblClick(p.into()),
        Input::Mods(Mods { shift: true, ..Mods::NONE }),
        Input::Tick(3),
    ] {
        assert_eq!(i.event(), None);
    }
}

#[test]
fn pointer_without_plan_point_is_skipped() {
    let mut ed = editor();
    ed.set_mode(Mode::Polyline);
    let mut p: P = v2(1.0, 1.0).into();
    // a ray along the work plane: the view gives no point
    p.ray.d = ddc_geom::v3(1.0, 0.0, 0.0);
    assert_eq!(Input::Click(p, Btn::Left).point(), None);
    assert_eq!(Input::Click(p, Btn::Left).event(), None);
    let before = *ed.ix.scope();
    for i in [Input::Move(p), Input::Click(p, Btn::Left), Input::Move(p), Input::Click(p, Btn::Left)] {
        Interaction::update(&mut ed, i);
    }
    assert_eq!(*ed.ix.scope(), before);
    assert!(ed.walls.is_empty());
}

#[test]
fn plan_adapter_pointer_drives_the_same_tool_as_a_plan_point() {
    let ad = PlanAdapter { center: v2(0.0, 0.0), scale: 100.0, canvas: [0.0, 0.0], level: 0, z: 0.0 };
    let (mut a, mut b) = (editor(), editor());
    a.set_mode(Mode::Polyline);
    b.set_mode(Mode::Polyline);
    for (sx, sy) in [(0.0, 0.0), (400.0, 0.0), (400.0, -300.0)] {
        let p = ad.pointer([sx, sy]);
        a.input(Input::Move(p));
        a.input(Input::Click(p, Btn::Left));
        let w = p.plan().unwrap();
        b.handle(Event::Move(w));
        b.handle(Event::Click(w));
    }
    a.input(esc());
    b.handle(Event::Key(Key::Escape));
    assert!(!a.walls.is_empty());
    assert_eq!(a.walls.len(), b.walls.len());
    assert_eq!(journal(&a), journal(&b));
}

/// Session that collects two points and commits one level row.
struct TwoPoints {
    pts: Vec<ddc_geom::V2>,
    seen_ms: u64,
}

impl ToolSession for TwoPoints {
    fn id(&self) -> ToolId {
        ToolId("test.two")
    }
    fn step(&self) -> &'static str {
        if self.pts.is_empty() {
            "first"
        } else {
            "next"
        }
    }
    fn hints(&self) -> &[Hint] {
        const H: [Hint; 1] = [Hint::new(Chord::Esc, "ix.hint.cancel")];
        &H
    }
    fn on_input(&mut self, i: &Input, cx: &Cx) -> Step {
        self.seen_ms = cx.now_ms;
        match (*i, i.point()) {
            (Input::Click(_, Btn::Left), Some(p)) if self.pts.iter().any(|q| (*q - p).len() < 1e-9) => {
                Step::Refuse(Refusal::new(RefusalCode::InvalidInput, "ix.refuse.same_point"))
            }
            (Input::Click(_, Btn::Left), Some(p)) => {
                self.pts.push(p);
                if self.pts.len() == 2 {
                    Step::Commit
                } else {
                    Step::Continue
                }
            }
            (Input::Key(Key::Enter, _), _) => Step::Done,
            (Input::Key(Key::Escape, _), _) if self.pts.is_empty() => Step::Cancel,
            (Input::Key(Key::Escape, _), _) => {
                self.pts.clear();
                Step::Continue
            }
            _ => Step::Continue,
        }
    }
    fn preview(&self, _cx: &Cx, out: &mut Preview) {
        let pts = self.pts.iter().map(|p| ddc_geom::v3(p.x, p.y, 0.0)).collect();
        out.0.push(Prim::Polyline { pts, closed: false, style: Style::Line });
    }
    fn commit(&mut self, _cx: &Cx) -> Result<Rec, Refusal> {
        self.pts.clear();
        Ok(Rec::new("test.two", |m| m.add_level("ix test", 9.0).map(|_| 1)))
    }
}

fn two() -> Box<TwoPoints> {
    Box::new(TwoPoints { pts: vec![], seen_ms: 0 })
}

#[test]
fn session_owns_input_and_commits_through_the_editor() {
    let mut ed = editor();
    let levels = ed.levels().len();
    ed.ix.begin(two());
    assert_eq!(*ed.ix.scope(), drafting("test.two", "first"));
    Interaction::update(&mut ed, Input::Tick(250));
    Interaction::update(&mut ed, click(0.0, 0.0));
    assert_eq!(*ed.ix.scope(), drafting("test.two", "next"));
    // the session took the click: the wall tool saw nothing
    assert!(ed.wall_idle() && ed.selection.is_empty());
    Interaction::update(&mut ed, click(0.0, 0.0));
    assert_eq!(ed.status_sev.as_ref().unwrap().0, crate::status::Severity::Refusal);
    Interaction::update(&mut ed, click(3.0, 0.0));
    assert_eq!(ed.levels().len(), levels + 1, "one record applied");
    assert!(ed.can_undo());
    assert_eq!(*ed.ix.scope(), drafting("test.two", "first"));
    Interaction::update(&mut ed, Input::Key(Key::Enter, Mods::NONE));
    assert_eq!(*ed.ix.scope(), Scope::Idle);
    assert_eq!(ed.ix.session_id(), None);
}

#[test]
fn two_esc_end_a_session_that_keeps_itself() {
    let mut ed = editor();
    ed.ix.begin(two());
    Interaction::update(&mut ed, click(1.0, 1.0));
    Interaction::update(&mut ed, esc());
    // the first Esc only cleared the points
    assert_eq!(ed.ix.session_id(), Some(ToolId("test.two")));
    Interaction::update(&mut ed, click(2.0, 2.0));
    Interaction::update(&mut ed, esc());
    Interaction::update(&mut ed, esc());
    assert_eq!(*ed.ix.scope(), Scope::Idle);
    assert_eq!(ed.ix.session_id(), None);
}

#[test]
fn begin_replaces_and_end_if_matches() {
    let mut ed = editor();
    ed.ix.begin(two());
    Interaction::update(&mut ed, click(1.0, 1.0));
    ed.ix.begin(two());
    assert_eq!(*ed.ix.scope(), drafting("test.two", "first"));
    ed.ix.end_if(|s| matches!(s, Scope::Placing { .. }));
    assert_eq!(ed.ix.session_id(), Some(ToolId("test.two")));
    ed.ix.end_if(|s| s.tool() == Some(ToolId("test.two")));
    assert_eq!(*ed.ix.scope(), Scope::Idle);
}

#[test]
fn preview_and_hints_come_from_the_session() {
    let ed = editor();
    let mut s = two();
    let cx = Cx { ed: &ed, now_ms: 0 };
    s.on_input(&click(1.0, 2.0), &cx);
    let mut out = Preview::default();
    s.preview(&cx, &mut out);
    let want = Prim::Polyline { pts: vec![ddc_geom::v3(1.0, 2.0, 0.0)], closed: false, style: Style::Line };
    assert_eq!(out.0, vec![want.clone()]);
    assert_eq!(s.hints()[0].chord, crate::interact::hints::Chord::Esc);
    // the running session's preview, read through the spine, is what every view draws
    let mut ed = editor();
    ed.ix.begin(two());
    Interaction::update(&mut ed, click(1.0, 2.0));
    assert_eq!(Interaction::preview(&ed).0, vec![want]);
}

/// Spine cost per event over the bare legacy chain: 2 us in a release build, a
/// generous 30 us in debug so the bound still catches a per-event model read or allocation.
#[test]
fn update_overhead_per_event_under_2us() {
    const N: usize = 4_000;
    let bound = if cfg!(debug_assertions) { 30e-6 } else { 2e-6 };
    let moves: Vec<Event> =
        (0..N).map(|i| Event::Move(v2((i % 200) as f64 * 0.05, (i / 200) as f64 * 0.05))).collect();
    let mut a = editor();
    a.set_mode(Mode::Polyline);
    let mut b = editor();
    b.set_mode(Mode::Polyline);
    let mut best = f64::MAX;
    for _ in 0..5 {
        let t = std::time::Instant::now();
        for e in &moves {
            select::route(&mut b, *e);
        }
        let bare = t.elapsed().as_secs_f64();
        let t = std::time::Instant::now();
        for e in &moves {
            a.handle(*e);
        }
        let spine = t.elapsed().as_secs_f64();
        best = best.min((spine - bare) / N as f64);
    }
    eprintln!("spine overhead {:.3} us/event", best * 1e6);
    assert!(best <= bound, "{:.3} us/event over {:.0} us", best * 1e6, bound * 1e6);
}

#[test]
fn two_views_one_session() {
    // first click in the plan, second in the 3D view: one wall, one record
    let mut ed = editor();
    assert!(script::arm(&mut ed, "WA"));
    let plan = PlanAdapter { center: v2(0.0, 0.0), scale: 100.0, canvas: [1600.0, 1200.0], level: ed.level, z: ed.elevation };
    let cam = ddc_render::OrbitCamera { distance: 40.0, ..Default::default() };
    let v3d = View3dAdapter::new(&cam, [0.0, 0.0], [1600.0, 1200.0], ed.elevation, WorkPlane::level(ed.level, ed.elevation));
    let a = plan.pointer_at(plan.project_at(ddc_geom::v3(0.0, 0.0, ed.elevation)).unwrap());
    ed.input(Input::Click(a, Btn::Left));
    let b = v3d.pointer_at(v3d.project_at(ddc_geom::v3(6.0, 0.0, ed.elevation)).unwrap());
    ed.input(Input::Move(b));
    // the draft is one preview every view draws
    assert!(!ed.preview_prims().0.is_empty());
    ed.input(Input::Click(b, Btn::Left));
    ed.input(Input::Key(Key::Escape, Mods::NONE));
    assert_eq!(ed.walls.len(), 1);
    let w = &ed.walls[0];
    assert!((w.a - v2(0.0, 0.0)).len() < 1e-9 && (w.b - v2(6.0, 0.0)).len() < 1e-9, "{:?} {:?}", w.a, w.b);
}

#[test]
fn two_esc_on_a_session_leave_the_old_flags_alone() {
    let mut ed = editor();
    ed.set_mode(Mode::Polyline);
    ed.ix.begin(Box::new(Hold(drafting("test.hold", "hold"))));
    Interaction::update(&mut ed, esc());
    Interaction::update(&mut ed, esc());
    assert_eq!(*ed.ix.scope(), Scope::Idle);
    // the legacy reset (sketch, shape, set_mode) is for the legacy chain only
    assert_eq!(ed.wall_mode(), Mode::Polyline);
}

/// A selected void in a wall (0,0)-(6,0) and the point that grabs it.
fn opening_selected(ed: &mut Editor) -> V2 {
    ed.commit_walls(vec![(v2(0.0, 0.0), v2(6.0, 0.0))]);
    ed.start_opening(crate::opening_tool::OpeningType::void());
    ed.handle(Event::Click(v2(3.0, 0.0)));
    ed.set_mode(Mode::Select);
    ed.reload().unwrap();
    let id = ed.openings[0].id;
    ed.selection = vec![crate::editor::Sel::Opening(id)];
    let p = v2(3.0, 0.0);
    assert_eq!(ed.opening_at(p), Some(id));
    p
}

#[test]
fn undo_mid_opening_slide_ends_the_grip_session() {
    let mut ed = editor();
    let p = opening_selected(&mut ed);
    ed.handle(Event::Press(p));
    assert!(ed.drag.is_some());
    assert_eq!(ed.ix.session_id(), Some(ToolId("grip")));
    ed.undo();
    assert!(ed.drag.is_none());
    assert_eq!(ed.ix.session_id(), None);
    assert_eq!(*ed.ix.scope(), Scope::Idle);
}

#[test]
fn alt_mid_grip_drag_resnaps_without_a_move() {
    let mut ed = editor();
    ed.commit_walls(vec![(v2(0.0, 0.0), v2(6.0, 0.0)), (v2(6.0, 3.0), v2(9.0, 3.0))]);
    let id = ed.walls[0].id;
    ed.selection = vec![crate::editor::Sel::Wall(id)];
    ed.handle(Event::Move(v2(6.0, 0.0)));
    ed.handle(Event::Press(v2(6.0, 0.0)));
    assert_eq!(ed.ix.session_id(), Some(ToolId("grip")));
    let near = v2(6.0 + ed.tool.radius * 0.5, 3.0 + ed.tool.radius * 0.3);
    ed.handle(Event::Move(near));
    let now = |ed: &Editor| ed.grip.active.as_ref().map(|a| a.now);
    let snapped = now(&ed);
    Interaction::update(&mut ed, Input::Mods(Mods { alt: true, ..Mods::NONE }));
    assert!(ed.tool.no_snap);
    let free = now(&ed);
    assert_ne!(free, snapped, "Alt down re-snaps at once");
    assert_eq!(free, Some(near), "free placement is the pointer itself");
    Interaction::update(&mut ed, Input::Mods(Mods::NONE));
    assert_eq!(now(&ed), snapped, "Alt up snaps back without a move");
}

#[test]
fn copy_is_one_record_per_copy() {
    let mut ed = editor();
    ed.commit_walls(vec![(v2(0.0, 0.0), v2(6.0, 0.0))]);
    ed.selection = vec![crate::editor::Sel::Wall(ed.walls[0].id)];
    assert!(script::arm(&mut ed, "CO"));
    let before = ed.undo_len();
    for p in [v2(0.0, 0.0), v2(0.0, 3.0), v2(0.0, 6.0)] {
        ed.handle(Event::Click(p));
    }
    assert_eq!(ed.walls.len(), 3);
    assert_eq!(ed.undo_len(), before + 2, "each copy its own record");
    assert_eq!(*ed.ix.scope(), Scope::Moving { op: ModOp::Copy });
    Interaction::update(&mut ed, esc());
    Interaction::update(&mut ed, esc());
    ed.undo();
    assert_eq!(ed.walls.len(), 2, "one undo takes back the last copy only");
    ed.undo();
    assert_eq!(ed.walls.len(), 1);
}

#[test]
fn arch_tools_have_no_legacy_branch() {
    for id in ["tools", "section", "opening"] {
        assert!(select::HOOKS.iter().all(|s| s.id != id), "{id} is still in the old chain");
    }
    let mut ed = editor();
    // wall: arming makes the session; the chain point and Esc stay its own
    assert!(script::arm(&mut ed, "WA"));
    ed.handle(Event::Click(v2(0.0, 0.0)));
    assert_eq!(ed.ix.session_id(), Some(ToolId("arch.wall")));
    assert_eq!(*ed.ix.scope(), drafting("arch.wall", "next"));
    ed.handle(Event::Click(v2(5.0, 0.0)));
    Interaction::update(&mut ed, esc());
    assert_eq!(ed.walls.len(), 1);
    assert_eq!(ed.ix.session_id(), Some(ToolId("arch.wall")));
    Interaction::update(&mut ed, esc());
    assert_eq!(ed.ix.session_id(), None);
    assert_eq!(*ed.ix.scope(), Scope::Idle);
    assert_eq!(ed.wall_mode(), Mode::Select);
    // a door, then a column armed from the ribbon mid-session: the session follows the command
    assert!(script::arm(&mut ed, "DR"));
    ed.handle(Event::Move(v2(2.5, 0.0)));
    assert_eq!(ed.ix.session_id(), Some(ToolId("arch.door")));
    assert!(script::arm(&mut ed, "CL"));
    ed.handle(Event::Move(v2(2.5, 3.0)));
    assert_eq!(ed.ix.session_id(), Some(ToolId("arch.column")), "{:?}", ed.ix.scope());
    // the second Esc leaves any of them with the old flags reset
    Interaction::update(&mut ed, esc());
    Interaction::update(&mut ed, esc());
    assert_eq!(ed.ix.session_id(), None);
    assert!(ed.model_tool().is_none() && !ed.opening_mode && ed.section_tool().is_none());
}

#[test]
fn nested_session_esc_returns_to_parent() {
    let mut ed = editor();
    let parent = drafting("test.hold", "hold");
    let child = Scope::Reshaping { target: Target::Elem(crate::grips::Kind::Slab, 1) };
    ed.ix.begin(Box::new(Hold(parent)));
    ed.ix.begin_child(Box::new(Hold(child)));
    assert_eq!(ed.ix.depth(), 2);
    assert_eq!(*ed.ix.scope(), child);
    // the second Esc ends the child only: the parent runs again with a fresh Esc run
    Interaction::update(&mut ed, esc());
    Interaction::update(&mut ed, esc());
    assert_eq!(ed.ix.depth(), 1);
    assert_eq!(*ed.ix.scope(), parent);
    Interaction::update(&mut ed, esc());
    assert_eq!(*ed.ix.scope(), parent, "one Esc of the parent's own run does not leave it");
    Interaction::update(&mut ed, esc());
    assert_eq!(*ed.ix.scope(), Scope::Idle);
    assert_eq!(ed.ix.depth(), 0);
    // depth stays ≤ 2: a second child replaces the first; a new top-level session drops both
    ed.ix.begin(Box::new(Hold(parent)));
    ed.ix.begin_child(Box::new(Hold(child)));
    ed.ix.begin_child(Box::new(Hold(child)));
    assert_eq!(ed.ix.depth(), 2);
    ed.ix.begin(Box::new(Hold(parent)));
    assert_eq!(ed.ix.depth(), 1);
}

#[test]
fn snap_notes_reach_the_wall_session() {
    let mut ed = editor();
    ed.commit_walls(vec![(v2(0.0, 0.0), v2(6.0, 0.0))]);
    assert!(script::arm(&mut ed, "WA"));
    // Shift+M comes as a character and reaches the wall tool through its session
    Interaction::update(&mut ed, Input::Char('M'));
    assert_eq!(ed.ix.session_id(), Some(ToolId("arch.wall")));
    assert!(ed.tool.snap_state.over.is_some(), "M sets the midpoint override");
    // the clock reaches the tool's snap dwell: a rest on the end captures it
    Interaction::update(&mut ed, Input::Move(v2(6.0, 0.0).into()));
    Interaction::update(&mut ed, Input::Char('M'));
    Interaction::update(&mut ed, Input::Tick(0));
    Interaction::update(&mut ed, Input::Tick(400));
    assert_eq!(ed.ix.now_ms, 400);
    assert!(ed.tool.snap_state.held.is_some(), "a 300 ms rest holds the snap");
}

#[test]
fn special_has_no_legacy_branch() {
    for id in ["sketch", "shape", "roof_edge", "topo", "mass"] {
        assert!(select::HOOKS.iter().all(|s| s.id != id), "{id} is still in the old chain");
    }
    let mut ed = editor();
    let id = slab(&mut ed);
    // slab shape edit: a session from the first input to the second Esc
    crate::shape_edit::begin(&mut ed, id).expect("shape edit");
    ed.handle(Event::Move(v2(42.0, 42.0)));
    assert_eq!(ed.ix.session_id(), Some(ToolId("shape")));
    assert!(matches!(ed.ix.scope(), Scope::Reshaping { .. }));
    Interaction::update(&mut ed, esc());
    Interaction::update(&mut ed, esc());
    assert_eq!(ed.ix.session_id(), None);
    assert!(ed.shape.is_none());
    // topography points: the tool's command names the session; Esc twice leaves it
    crate::topo::arm(&mut ed, crate::topo::Tool::Points, Lang::En);
    ed.handle(Event::Move(v2(1.0, 1.0)));
    assert_eq!(ed.ix.session_id(), Some(ToolId("site.toposolid")));
    Interaction::update(&mut ed, esc());
    Interaction::update(&mut ed, esc());
    assert_eq!(*ed.ix.scope(), Scope::Idle);
    assert_eq!(ed.ix.session_id(), None);
}

#[test]
fn scope_table_3d_snapshot() {
    use crate::edit3d::{Edit3d, Tool};
    let rows: Vec<String> = [Tool::Select, Tool::Box, Tool::Measure, Tool::Face, Tool::Pick3, Tool::Mirror, Tool::Array]
        .into_iter()
        .map(|t| {
            let mut e = Edit3d::default();
            e.set_tool(t);
            let s = e.scope();
            let (_, h, o, sk) = &SCOPE_TABLE[s.kind() as usize];
            format!("{t:?}: {:?} hot={:04x} {} skip={sk:?}", s.kind(), h.0, o.names().join(","))
        })
        .collect();
    let want = "\
Select: Idle hot=ffff grips,selection,hover skip=None
Box: BoxSelect hot=ffff selection,box skip=None
Measure: Measuring hot=ffff temp_dims,snap,measure,hint skip=None
Face: Pointing hot=ffff snap,hover,hint skip=None
Pick3: Pointing hot=ffff snap,hover,hint skip=None
Mirror: Moving hot=0000 temp_dims,snap,preview,selection,hint skip=Selection
Array: Moving hot=0000 temp_dims,snap,preview,selection,hint skip=Selection";
    assert_eq!(rows.join("\n"), want);
    // drags: a grip leaves its element out of snapping, a gizmo move the selection
    use super::hotset::SnapSkip;
    let grip = Scope::GripDrag { target: Target::Elem(crate::grips::Kind::Wall, 1), grip: 0 };
    assert_eq!(SnapSkip::of(&grip), SnapSkip::Target);
    assert_eq!(SnapSkip::of(&Scope::Moving { op: ModOp::Move }), SnapSkip::Selection);
    assert_eq!(SnapSkip::of(&Scope::Idle), SnapSkip::None);
}

#[test]
fn view_switch_keeps_measuring() {
    use crate::edit3d::{Edit3d, Tool};
    let mut ed = editor();
    let mut e = Edit3d::default();
    e.set_tool(Tool::Measure);
    e.sync_scope(&mut ed);
    assert_eq!(*ed.ix.scope(), Scope::Measuring);
    assert_eq!(ed.ix.session_id(), Some(ToolId("e3d.measure")));
    // the plan takes over the pointer: its input passes, the 3D chain stays
    ed.handle(Event::Move(v2(1.0, 1.0)));
    ed.handle(Event::Click(v2(1.0, 1.0)));
    assert_eq!(*ed.ix.scope(), Scope::Measuring);
    e.sync_scope(&mut ed);
    assert_eq!(e.tool, Tool::Measure);
    // the second Esc reaches Idle and the 3D view drops its tool at the next sync
    Interaction::update(&mut ed, esc());
    Interaction::update(&mut ed, esc());
    assert_eq!(*ed.ix.scope(), Scope::Idle);
    e.sync_scope(&mut ed);
    assert_eq!(e.tool, Tool::Select);
    assert_eq!(ed.ix.session_id(), None);
}

#[test]
fn editor_handle_has_no_tool_branches() {
    let src = include_str!("../editor.rs");
    let at = src.find("    pub fn handle(&mut self, e: Event) {").expect("Editor::handle");
    let body = &src[at..at + src[at..].find("\n    }\n").expect("end of handle")];
    assert!(body.lines().count() <= 10, "handle grew:\n{body}");
    for flag in ["mode", "opening", "stool", "sketch", "shape", "mass", "drag", "if ", "match "] {
        assert!(!body.contains(flag), "Editor::handle branches on `{flag}`:\n{body}");
    }
    // the old chain is gone: selection is the route with no session
    assert!(!include_str!("mod.rs").contains(&["pub mod ", "legacy"].concat()));
    assert_eq!(select::HOOKS.last().map(|h| h.id), Some("select"));
}

#[test]
fn modify_has_no_legacy_branch() {
    // MV is a session: the clicks never go down the legacy chain (no select, no wall tool)
    assert!(select::HOOKS.iter().all(|s| !s.id.starts_with("mod")));
    let mut ed = editor();
    ed.commit_walls(vec![(v2(0.0, 0.0), v2(6.0, 0.0))]);
    let id = ed.walls[0].id;
    ed.selection = vec![crate::editor::Sel::Wall(id)];
    assert!(script::arm(&mut ed, "MV"));
    assert_eq!(ed.ix.session_id(), Some(ToolId("mod.move")));
    assert_eq!(*ed.ix.scope(), Scope::Moving { op: ModOp::Move });
    let before = ed.undo_len();
    ed.handle(Event::Click(v2(0.0, 0.0)));
    assert_eq!(*ed.ix.scope(), Scope::Moving { op: ModOp::Move });
    ed.handle(Event::Click(v2(1.2, 0.0)));
    assert_eq!(*ed.ix.scope(), Scope::Idle);
    assert_eq!(ed.undo_len(), before + 1);
    let w = ed.walls.iter().find(|w| w.id == id).expect("moved wall");
    assert!((w.a - v2(1.2, 0.0)).len() < 1e-6, "{:?}", w.a);
    let end = w.b;
    // a grip drag is a session from the press that grabs to the release
    ed.selection = vec![crate::editor::Sel::Wall(id)];
    ed.handle(Event::Move(end));
    ed.handle(Event::Press(end));
    assert_eq!(ed.ix.session_id(), Some(ToolId("grip")));
    assert!(matches!(ed.ix.scope(), Scope::GripDrag { .. }));
    ed.handle(Event::Move(end + v2(0.0, 2.0)));
    ed.handle(Event::Release(end + v2(0.0, 2.0)));
    assert_eq!(ed.ix.session_id(), None);
    assert_eq!(*ed.ix.scope(), Scope::Idle);
    assert_eq!(ed.undo_len(), before + 2);
}

/// 76b group 2: the section tool keeps its points in its session, not in an editor flag.
#[test]
fn section_tool_owns_its_points() {
    assert!(!include_str!("../editor.rs").contains(&["pub st", "ool:"].concat()));
    let mut ed = editor();
    ed.start_section();
    assert_eq!(ed.ix.session_id(), Some(ToolId("arch.section")));
    assert_eq!(*ed.ix.scope(), drafting("arch.section", "first"));
    ed.handle(Event::Click(v2(0.0, -1.0)));
    assert_eq!(*ed.ix.scope(), drafting("arch.section", "next"));
    ed.handle(Event::Click(v2(0.0, 5.0)));
    assert!(ed.section_tool().is_some_and(|t| t.b.is_some()));
    // Esc steps back to the second point, the next one drops the line, the third ends the tool
    ed.handle(Event::Key(Key::Escape));
    assert!(ed.section_tool().is_some_and(|t| t.a.is_some() && t.b.is_none()));
    ed.handle(Event::Click(v2(0.0, 5.0)));
    let views = ed.views.len();
    ed.handle(Event::Move(v2(2.0, 2.0)));
    ed.handle(Event::Click(v2(2.0, 2.0)));
    assert_eq!(ed.views.len(), views + 1, "the third click adds the view");
    assert!(ed.section_tool().is_some_and(|t| t.a.is_none()), "the tool chains");
    ed.set_mode(Mode::Polyline);
    assert!(ed.section_tool().is_none(), "another tool ends it");
}

/// A ribbon command after the section tool: the mode change ends the section session and the
/// refreshed scope is the wall tool's.
#[test]
fn ribbon_wall_after_section_ends_the_section() {
    let mut ed = editor();
    ed.start_section();
    ed.handle(Event::Click(v2(0.0, 0.0)));
    assert!(super::sessions::section::running(&ed));
    assert!(script::arm(&mut ed, "WA"));
    Interaction::refresh(&mut ed);
    assert!(ed.section_tool().is_none(), "the wall command closes the section tool");
    assert_eq!(*ed.ix.scope(), drafting("arch.wall", "first"));
    ed.handle(Event::Click(v2(1.0, 0.0)));
    assert_eq!(ed.ix.session_id(), Some(ToolId("arch.wall")));
}

/// 76b group 4: a `tools::Kind` tool keeps its `Active` in its session, not in the wall tool.
#[test]
fn model_tool_owns_its_state() {
    assert!(!include_str!("../wall_tool.rs").contains(&["pub e", "xt:"].concat()));
    let mut ed = editor();
    crate::tools::arm(&mut ed, crate::tools::Kind::Column, Lang::En);
    assert!(super::sessions::model_tool::running(&ed));
    assert_eq!(*ed.ix.scope(), Scope::Placing { tool: ToolId("arch.column") });
    ed.handle(Event::Click(v2(2.0, 3.0)));
    assert!(ed.model_tool().is_some(), "a placing tool stays armed after a click");
    assert_eq!(ed.ix.session_id(), Some(ToolId("arch.column")));
    // the wall command replaces it; the second Esc leaves any tool with the mode reset
    ed.set_mode(Mode::Polyline);
    assert!(ed.model_tool().is_none());
    crate::tools::arm(&mut ed, crate::tools::Kind::Beam, Lang::En);
    Interaction::update(&mut ed, esc());
    Interaction::update(&mut ed, esc());
    assert!(ed.model_tool().is_none() && !ed.tool_armed());
    assert_eq!(ed.ix.session_id(), None);
}

/// 76b group 3 (first step): the curtain wall is the wall tool's draft state, not an editor flag.
#[test]
fn curtain_wall_is_wall_tool_state() {
    assert!(!include_str!("../editor.rs").contains(&["pub curtain", "_mode"].concat()));
    let mut ed = editor();
    assert!(script::arm(&mut ed, "arch.curtain_wall"));
    assert!(ed.tool.curtain);
    Interaction::refresh(&mut ed);
    assert_eq!(*ed.ix.scope(), drafting("arch.curtain_wall", "first"));
    ed.set_mode(Mode::Select);
    assert!(!ed.tool.curtain, "a mode change ends the curtain wall");
}

/// 76b group 3: the drawing tools' draft is one value apart from their settings; a model
/// tool snaps with its own chain and no wall refinements.
#[test]
fn wall_draft_is_apart_from_settings() {
    let mut ed = editor();
    ed.set_mode(Mode::Polyline);
    ed.handle(Event::Click(v2(0.0, 0.0)));
    assert_eq!(ed.wall_draft().points.len(), 1);
    let grid = ed.tool.grid;
    ed.set_mode(Mode::Select);
    assert!(ed.wall_draft().points.is_empty() && ed.wall_draft().input.is_empty());
    assert_eq!(ed.tool.grid, grid, "a mode change keeps the settings");
    crate::tools::arm(&mut ed, crate::tools::Kind::Beam, Lang::En);
    ed.handle(Event::Click(v2(0.0, 0.0)));
    ed.handle(Event::Move(v2(3.0, 0.1)));
    assert!(!ed.tool.last.refine && ed.tool.last.own == false);
    assert!(crate::snap_refine::tool_hints(&ed.tool, ed.wall_draft()).is_empty(), "no wall hints for a beam");
}

/// Each session owns its chain; the snap engine takes the start point as a query.
#[test]
fn sessions_own_their_chains() {
    use crate::wall_tool::Chain;
    let mut ed = editor();
    crate::tools::arm(&mut ed, crate::tools::Kind::Beam, Lang::En);
    ed.handle(Event::Click(v2(0.0, 0.0)));
    ed.handle(Event::Move(v2(4.0, 0.0)));
    // the beam's points are its own: the wall draft stays empty, the view reads the session's
    assert!(ed.wall_draft().points.is_empty());
    assert_eq!(ed.chain(), vec![v2(0.0, 0.0)]);
    assert_eq!(ed.tool.last.from_xy(), Some(v2(0.0, 0.0)));
    // the same query by hand: the start point is a parameter, z rides along (snap2)
    let mut t = crate::wall_tool::Rig::new();
    t.grid = 0.0;
    t.ortho = true;
    t.track_chain(v2(5.0, 0.3), Chain::of(&[v2(1.0, 0.0)], 2.5, true));
    assert_eq!(t.last.from.map(|f| f.z), Some(2.5));
    assert_eq!(t.snapped(), v2(5.0, 0.0), "ortho from the query's point");
    assert!(t.draft.points.is_empty(), "the query does not write the wall draft");
    ed.set_mode(Mode::Select);
    assert!(ed.chain().is_empty());
}

/// A solid's chain keeps the arc draft it had when it shared the wall's points.
#[test]
fn solid_chain_keeps_the_arc_preview() {
    let mut ed = editor();
    assert!(script::arm(&mut ed, "arch.solid_extrusion"));
    ed.handle(Event::Click(v2(1.0, 1.0)));
    assert_eq!(ed.chain().first().copied(), Some(v2(1.0, 1.0)));
    ed.tool.arc = true;
    ed.handle(Event::Move(v2(4.0, 2.0)));
    let arc = ed.draft_arc();
    assert_eq!(arc.first().copied(), Some(v2(1.0, 1.0)), "{arc:?}");
    assert!(ed.draft_preview().is_empty(), "the arc replaces the straight draft");
}

/// 76b group 3: readers ask `tool_armed()`, not the wall tool's mode (one body to change).
#[test]
fn readers_ask_tool_armed() {
    let banned = ["tool.mode != Mode::Sel", "tool.mode == Mode::Sel", "t.mode == Mode::Sel", "t.mode != Mode::Sel"]
        .map(|s| format!("{s}ect"));
    for (f, src) in [
        ("editor.rs", include_str!("../editor.rs")),
        ("sessions/select.rs", include_str!("sessions/select.rs")),
        ("sessions/model_tool.rs", include_str!("sessions/model_tool.rs")),
        ("sessions/grip.rs", include_str!("sessions/grip.rs")),
        ("sessions/arch.rs", include_str!("sessions/arch.rs")),
        ("script.rs", include_str!("script.rs")),
        ("scope.rs", include_str!("scope.rs")),
        ("snap_refine.rs", include_str!("../snap_refine.rs")),
    ] {
        for b in &banned {
            assert!(!src.contains(b.as_str()), "{f} reads the mode: {b}");
        }
    }
    let mut ed = editor();
    assert!(!ed.tool_armed());
    ed.set_mode(Mode::Rect);
    assert!(ed.tool_armed() && ed.is_busy());
}

/// 76b group 3: the views and the shell ask `tool_armed()` too.
#[test]
fn views_ask_tool_armed() {
    let banned = ["tool.mode != ", "tool.mode == "].map(|s| format!("{s}Mode::Select"));
    let long = ["tool.mode != ", "tool.mode == "].map(|s| format!("{s}crate::wall_tool::Mode::Select"));
    for (f, src) in [
        ("plan_view.rs", include_str!("../plan_view.rs")),
        ("drawing.rs", include_str!("../drawing.rs")),
        ("section_view.rs", include_str!("../section_view.rs")),
        ("drafting_view.rs", include_str!("../drafting_view.rs")),
        ("shell.rs", include_str!("../shell.rs")),
        ("shell/panels.rs", include_str!("../shell/panels.rs")),
        ("shell/status_bar.rs", include_str!("../shell/status_bar.rs")),
        ("ai/ui/chat.rs", include_str!("../ai/ui/chat.rs")),
    ] {
        for b in banned.iter().chain(&long) {
            assert!(!src.contains(b.as_str()), "{f} reads the mode: {b}");
        }
    }
}

/// 76b group 3: the tool modules ask `tool_armed()`; only its body reads the wall mode.
#[test]
fn tools_ask_tool_armed() {
    let banned = ["tool.mode != ", "tool.mode == "].map(|s| format!("{s}Mode::Select"));
    for (f, src) in [
        ("annot_text.rs", include_str!("../annot_text.rs")),
        ("solid_tools.rs", include_str!("../solid_tools.rs")),
        ("modify_tools.rs", include_str!("../modify_tools.rs")),
        ("roof_edge_tools.rs", include_str!("../roof_edge_tools.rs")),
        ("clip_tools.rs", include_str!("../clip_tools.rs")),
        ("grips.rs", include_str!("../grips.rs")),
        ("cmdline.rs", include_str!("../cmdline.rs")),
    ] {
        for b in &banned {
            assert!(!src.contains(b.as_str()), "{f} reads the mode: {b}");
        }
    }
    assert_eq!(include_str!("../editor/busy.rs").matches(banned[0].as_str()).count(), 0, "the helper asks the tool");
}

/// Behaviour of the migrated tools (c8d1bde87, 9f4e6c491): a wall to grab, the tool armed
/// with one point placed and the cursor moved off it.
fn behaviour_setup(ed: &mut Editor, arm_tool: impl FnOnce(&mut Editor)) -> (i64, V2) {
    ed.set_mode(Mode::Polyline);
    ed.handle(Event::Click(v2(0.0, 10.0)));
    ed.handle(Event::Click(v2(4.0, 10.0)));
    ed.handle(Event::Key(Key::Escape));
    ed.set_mode(Mode::Select);
    let w = ed.walls.last().map(|w| (w.id, w.a)).expect("the grab wall");
    arm_tool(ed);
    ed.handle(Event::Click(v2(0.0, 0.0)));
    ed.handle(Event::Move(v2(3.0, 0.0)));
    w
}

/// While armed a press on a grip of the selected wall does not grab it; once the tool is gone
/// the grips are there and the press grabs.
fn grips_wait_for_the_tool(ed: &mut Editor, w: (i64, V2)) {
    ed.selection = vec![crate::editor::Sel::Wall(w.0)];
    assert!(!crate::grips::visible(ed).is_empty(), "the selected wall has grips");
    if ed.tool_armed() {
        Interaction::update(ed, Input::Press(w.1.into(), Btn::Left));
        assert!(!super::sessions::grip::running(ed), "an armed tool keeps the press");
        Interaction::update(ed, Input::Release(w.1.into(), Btn::Left));
    }
    ed.set_mode(Mode::Select);
    ed.selection = vec![crate::editor::Sel::Wall(w.0)];
    Interaction::update(ed, Input::Move(w.1.into()));
    Interaction::update(ed, Input::Press(w.1.into(), Btn::Left));
    assert!(super::sessions::grip::running(ed), "unarmed, the press grabs the grip");
}

#[test]
fn wall_tool_behaviour() {
    let mut ed = editor();
    let w = behaviour_setup(&mut ed, |ed| ed.set_mode(Mode::Polyline));
    assert!(!ed.draft_preview().is_empty(), "the rubber segment while armed");
    ed.handle(Event::Key(Key::Escape));
    assert!(ed.chain().is_empty() && ed.draft_preview().is_empty(), "Esc ends the chain");
    grips_wait_for_the_tool(&mut ed, w);
}

#[test]
fn model_tool_behaviour() {
    let mut ed = editor();
    let w = behaviour_setup(&mut ed, |ed| crate::tools::arm(ed, crate::tools::Kind::Beam, Lang::En));
    let a = ed.model_tool().expect("the beam is armed");
    assert!(a.seen && a.pts.len() == 1 && (a.cur - v2(3.0, 0.0)).len() < 0.5, "its own preview");
    assert!(ed.draft_preview().is_empty(), "no wall rubber over the beam's own");
    ed.handle(Event::Key(Key::Escape));
    assert!(ed.chain().is_empty(), "Esc ends the chain");
    grips_wait_for_the_tool(&mut ed, w);
}

#[test]
fn form_tool_behaviour() {
    let mut ed = editor();
    let w = behaviour_setup(&mut ed, |ed| {
        crate::mass_tools::arm(ed, crate::mass_tools::Cmd::Extrude, Lang::En)
    });
    assert_eq!(ed.chain(), vec![v2(0.0, 0.0)]);
    assert!(!ed.draft_preview().is_empty(), "the form's rubber segment");
    ed.tool.arc = true;
    ed.handle(Event::Move(v2(3.0, 1.0)));
    assert!(!ed.draft_arc().is_empty(), "the form's arc draft");
    ed.tool.arc = false;
    ed.handle(Event::Key(Key::Escape));
    assert!(ed.chain().is_empty() && ed.draft_preview().is_empty(), "Esc ends the chain");
    grips_wait_for_the_tool(&mut ed, w);
}

#[test]
fn solid_tool_behaviour() {
    let mut ed = editor();
    let w = behaviour_setup(&mut ed, |ed| {
        crate::solid_tools::arm(ed, crate::solid_tools::Kind::Extrusion, false, Lang::En)
    });
    assert_eq!(ed.chain().first().copied(), Some(v2(0.0, 0.0)));
    assert!(!ed.draft_preview().is_empty(), "the solid's rubber segment");
    ed.handle(Event::Key(Key::Escape));
    assert!(ed.chain().is_empty(), "Esc ends the chain");
    grips_wait_for_the_tool(&mut ed, w);
}

#[test]
fn section_tool_behaviour() {
    let mut ed = editor();
    let w = behaviour_setup(&mut ed, |ed| ed.start_section());
    ed.handle(Event::Click(v2(0.0, 4.0)));
    ed.handle(Event::Move(v2(2.0, 2.0)));
    assert!(ed.section_preview().is_some(), "the mark's draft while armed");
    ed.handle(Event::Key(Key::Escape));
    ed.handle(Event::Key(Key::Escape));
    ed.handle(Event::Key(Key::Escape));
    assert!(ed.section_tool().is_none(), "Esc steps back, then ends the tool");
    grips_wait_for_the_tool(&mut ed, w);
}

/// RMB while drawing finishes the chain as Enter does; with no chain it never reaches the
/// tools (the view opens its menu).
#[test]
fn right_click_finishes_the_chain() {
    let mut ed = editor();
    ed.set_mode(Mode::Polyline);
    Interaction::update(&mut ed, Input::Click(v2(1.0, 1.0).into(), Btn::Right));
    assert!(ed.chain().is_empty() && ed.walls.is_empty(), "no chain: nothing placed");
    ed.handle(Event::Click(v2(0.0, 0.0)));
    ed.handle(Event::Click(v2(4.0, 0.0)));
    ed.handle(Event::Click(v2(4.0, 3.0)));
    Interaction::update(&mut ed, Input::Click(v2(9.0, 9.0).into(), Btn::Right));
    assert_eq!(ed.walls.len(), 2, "the right click adds no segment of its own");
    assert!(ed.chain().is_empty(), "RMB finishes the chain");
    assert!(ed.tool_armed(), "the tool stays for the next chain, as after Enter");
}

/// RMB after two points finishes the chain at two points; the click adds no point of its own.
#[test]
fn right_click_after_two_points_ends_the_chain() {
    let mut ed = editor();
    ed.set_mode(Mode::Polyline);
    ed.handle(Event::Click(v2(0.0, 0.0)));
    ed.handle(Event::Click(v2(4.0, 0.0)));
    Interaction::update(&mut ed, Input::Click(v2(9.0, 9.0).into(), Btn::Right));
    assert_eq!(ed.walls.len(), 1, "two points make one segment; the right click adds none");
    assert!(ed.chain().is_empty(), "RMB finishes the chain");
}

/// 76b group 3: the wall draft is read through `wall_draft()` (the drawing session's); the
/// tool has no draft of its own.
#[test]
fn readers_ask_wall_draft() {
    let banned = ["tool", ".draft."].concat();
    for (f, src) in [
        ("tools/mod_arch.rs", include_str!("../tools/mod_arch.rs")),
        ("tools/ui.rs", include_str!("../tools/ui.rs")),
        ("mass_tools.rs", include_str!("../mass_tools.rs")),
        ("solid_tools.rs", include_str!("../solid_tools.rs")),
        ("annot_text.rs", include_str!("../annot_text.rs")),
        ("cmdline.rs", include_str!("../cmdline.rs")),
        ("plan_view.rs", include_str!("../plan_view.rs")),
        ("shell.rs", include_str!("../shell.rs")),
        ("shell/status_bar.rs", include_str!("../shell/status_bar.rs")),
        ("grips.rs", include_str!("../grips.rs")),
        ("editor.rs", include_str!("../editor.rs")),
        ("ai/mcp_bridge.rs", include_str!("../ai/mcp_bridge.rs")),
        ("sessions/model_tool.rs", include_str!("sessions/model_tool.rs")),
        ("script.rs", include_str!("script.rs")),
    ] {
        assert!(!src.contains(banned.as_str()), "{f} reads tool.draft");
    }
    let mut ed = editor();
    ed.set_mode(Mode::Polyline);
    ed.handle(Event::Char('5'));
    assert_eq!(ed.wall_draft().input, "5");
    ed.wall_draft_mut().input.clear();
    assert!(ed.wall_draft().input.is_empty());
}

/// 76b: a bridge begins when its tool is armed, before any input; disarming ends it.
#[test]
fn bridges_begin_at_arm() {
    let mut ed = editor();
    ed.set_mode(Mode::Polyline);
    assert!(sessions::arch::running(&ed), "the wall tool is its session from the arm");
    assert_eq!(ed.ix.session_id(), ed.ix.scope().tool());
    ed.set_mode(Mode::Select);
    assert!(ed.ix.session_id().is_none(), "disarm ends the bridge");
    assert_eq!(*ed.ix.scope(), Scope::Idle);
}

/// 76b: the drawing mode is read through `wall_mode()`; only the engine and the accessor
/// touch `tool.mode` (tests may still write it to disarm).
#[test]
fn readers_ask_wall_mode() {
    let banned = ["tool", ".mode =="].concat();
    let banned2 = ["tool", ".mode,"].concat();
    let banned3 = ["match ed.tool", ".mode"].concat();
    for (f, src) in [
        ("cmdline.rs", include_str!("../cmdline.rs")),
        ("drawing.rs", include_str!("../drawing.rs")),
        ("editor.rs", include_str!("../editor.rs")),
        // (scope.rs reads the legacy flag: the wall's arm request that `arch::live` begins from)
        ("sessions/arch.rs", include_str!("sessions/arch.rs")),
        ("plan_view.rs", include_str!("../plan_view.rs")),
        ("quick/args.rs", include_str!("../quick/args.rs")),
        ("quick/defaults.rs", include_str!("../quick/defaults.rs")),
        ("shell/chrome.rs", include_str!("../shell/chrome.rs")),
        ("shell/panels.rs", include_str!("../shell/panels.rs")),
        ("shell/tool_opts.rs", include_str!("../shell/tool_opts.rs")),
        ("shell.rs", include_str!("../shell.rs")),
        ("wall_pick.rs", include_str!("../wall_pick.rs")),
    ] {
        for b in [&banned, &banned2, &banned3] {
            assert!(!src.contains(b.as_str()), "{f} reads tool.mode");
        }
    }
    let mut ed = editor();
    ed.set_mode(Mode::Rect);
    assert_eq!(ed.wall_mode(), Mode::Rect);
    ed.set_mode(Mode::Select);
    assert_eq!(ed.wall_mode(), Mode::Select);
}

/// 76b: the wall session carries the drawing kind from the arm and follows a re-arm.
#[test]
fn wall_session_carries_its_kind() {
    use sessions::arch::{self, Arch};
    let mut ed = editor();
    ed.set_mode(Mode::Rect);
    assert_eq!(arch::kind(&ed), Some(Arch::Wall(Mode::Rect)));
    assert_eq!(ed.wall_mode(), Mode::Rect);
    ed.set_mode(Mode::Polyline);
    assert_eq!(arch::wall_kind(&ed), Some(Mode::Polyline));
    ed.handle(Event::Click(v2(0.0, 0.0)));
    assert_eq!(arch::wall_kind(&ed), Some(Mode::Polyline), "input keeps the kind");
    ed.set_mode(Mode::Select);
    assert_eq!(arch::wall_kind(&ed), None);
    assert_eq!(ed.wall_mode(), Mode::Select);
}

/// 76b: a session's chain draws as a polyline with the wall tool unarmed; a reset forgets it.
#[test]
fn session_chain_draws_without_the_wall_mode() {
    use crate::wall_tool::{Chain, Rig};
    let mut t = Rig::new();
    t.set_mode(Mode::Select);
    assert_eq!(t.mode, Mode::Select);
    let a = v2(0.0, 0.0);
    t.track_chain(v2(4.0, 0.0), Chain::of(&[a], 0.0, false));
    assert_eq!(t.kind(), Mode::Polyline, "the chain says what is drawn");
    assert!(!t.chain_preview(&[a]).is_empty(), "rubber segment with the wall unarmed");
    t.toggle_arc();
    t.track_chain(v2(4.0, 0.0), Chain::of(&[a], 0.0, false));
    assert!(!t.arc_preview_of(&[a]).is_empty(), "arc preview with the wall unarmed");
    t.reset();
    assert_eq!(t.kind(), Mode::Select);
}

/// 76b: `tool_armed()` is a drawing session; text, solids, forms, topo and model tools arm
/// through their sessions and leave the wall's own mode at `Select`.
#[test]
fn tool_armed_is_the_session() {
    let mut ed = wall_editor();
    assert!(ed.tool_armed() && sessions::arch::running(&ed), "opens with the wall session");
    let arms: [(&str, fn(&mut Editor)); 3] = [
        ("text", |ed| crate::annot_text::arm(ed, crate::annot_text::Kind::Text, Lang::En)),
        ("solid", |ed| assert!(script::arm(ed, "arch.solid_extrusion"))),
        ("column", |ed| crate::tools::arm(ed, crate::tools::Kind::Column, Lang::En)),
    ];
    for (what, arm) in arms {
        arm(&mut ed);
        assert_eq!(ed.tool.mode, Mode::Select, "{what}: the wall stays unarmed");
        assert!(ed.ix.session_id().is_some(), "{what}: a session from the arm");
        assert!(ed.tool_armed(), "{what}: armed is the session");
        assert_eq!(ed.wall_mode(), Mode::Polyline, "{what}: draws a chain");
        ed.set_mode(Mode::Select);
        assert!(!ed.tool_armed() && ed.ix.session_id().is_none(), "{what}: disarm ends it");
    }
    // a session that does not draw is not an armed tool
    assert!(script::arm(&mut ed, "DR"));
    assert!(ed.ix.session_id().is_some() && !ed.tool_armed(), "placing an opening");
}

/// 76b: the wall engine draws on the draft it is given (`*_in`).
#[test]
fn engine_takes_the_draft() {
    use crate::wall_tool::{WallDraft, WallTool};
    let mut t = WallTool::new();
    let mut d = WallDraft::default();
    t.handle_in(&mut d, Event::Click(v2(0.0, 0.0)));
    t.handle_in(&mut d, Event::Click(v2(4.0, 0.0)));
    assert_eq!(d.points.len(), 2);
    assert!(!t.preview_in(&d).is_empty() && !t.is_idle_in(&d));
    t.handle_in(&mut d, Event::Key(crate::wall_tool::Key::Escape));
    assert!(t.is_idle_in(&d), "Esc ends the chain it was given");
}

/// 76b: the wall draft is the wall session's: input draws on it (parked while the session is
/// out for its input), a new mode clears it, the session's end drops it.
#[test]
fn wall_session_owns_the_draft() {
    let mut ed = wall_editor();
    ed.handle(Event::Click(v2(0.0, 0.0)));
    ed.handle(Event::Click(v2(4.0, 0.0)));
    let s = ed.ix.get::<sessions::arch::ArchSession>().expect("the wall session");
    assert_eq!(s.draft().map(|d| d.points.len()), Some(2), "the session holds the chain");
    assert_eq!(ed.wall_draft().points.len(), 2);
    ed.set_mode(Mode::Rect);
    assert!(ed.wall_idle(), "a new mode starts a new draft");
    ed.handle(Event::Click(v2(0.0, 0.0)));
    assert!(ed.wall_draft().rect_start.is_some(), "the corner is the session's");
    ed.set_mode(Mode::Select);
    assert!(ed.wall_idle() && ed.ix.session_id().is_none(), "the draft ends with the session");
}
