// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Command contracts as data (`tests/contracts/commands.txt`, one row per registry command).
//! The gate enumerates every palette command, ribbon button and menu path and names the ones
//! without a row. A generator turns rows into named checks run headless on a small fixture;
//! per-step oracles: model integrity, no raw catalogue key on screen, a refused or unavailable
//! command changes nothing and pushes no undo step, one undo step restores exactly and redo
//! reproduces it. Seeded op sequences check that undo/redo and save→reopen give the same model.
//! `DDC_E2E_SEQS` sets the sequence count (default 6).

use crate::e2e_common;

use std::collections::{BTreeMap, BTreeSet};
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, OnceLock};

use ddc_app::editor::{Editor, OpFn, Sel};
use ddc_app::shell::{EditorApp, WIRED};
use ddc_app::wall_tool::Mode;
use ddc_model::{CurtainSpec, GridLines, Model, MullionSection, OpeningSpec, WallSpec};
use ddc_ui::i18n::Lang;
use e2e_common::{self as ec, Rng, TableRows};

const CONTRACTS: &str = include_str!("../contracts/commands.txt");
/// Outside the undo contract: the journal itself and clash statuses.
const SKIP: &[&str] = &["op", "clash_item"];
/// Ceiling on `todo` rows: only goes down as rows become concrete.
const TODO_MAX: usize = 444;

// ---------------------------------------------------------------- rows

#[derive(Debug, Clone, Copy, PartialEq)]
enum Undo {
    Zero,
    One,
    AtMostOne,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Active {
    Tool,
    Once,
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Exec {
    Run,
    Todo,
    Skip,
}

#[derive(Debug, Clone)]
struct Row {
    line: usize,
    id: String,
    routes: BTreeSet<String>,
    /// `-` none, `*` any, else categories (the first one is the scripted selection).
    sel: Vec<String>,
    undo: Undo,
    active: Active,
    exec: Exec,
    note: String,
}

fn parse(src: &str) -> Result<Vec<Row>, String> {
    let mut out = vec![];
    for (k, raw) in src.lines().enumerate() {
        let line = k + 1;
        let t = raw.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = t.split('|').map(str::trim).collect();
        if f.len() < 6 || f.len() > 7 {
            return Err(format!("line {line}: {} fields, want 6 or 7", f.len()));
        }
        let undo = match f[3] {
            "0" => Undo::Zero,
            "1" => Undo::One,
            "?" => Undo::AtMostOne,
            x => return Err(format!("line {line}: undo '{x}'")),
        };
        let active = match f[4] {
            "tool" => Active::Tool,
            "once" => Active::Once,
            "?" => Active::Any,
            x => return Err(format!("line {line}: active '{x}'")),
        };
        let exec = match f[5] {
            "run" => Exec::Run,
            "todo" => Exec::Todo,
            "skip" => Exec::Skip,
            x => return Err(format!("line {line}: exec '{x}'")),
        };
        let note = f.get(6).copied().unwrap_or("").to_string();
        if exec == Exec::Skip && note.is_empty() {
            return Err(format!("line {line}: skip without a reason"));
        }
        let sel = if f[2] == "-" {
            vec![]
        } else {
            f[2].split(',').map(|s| s.trim().to_string()).collect()
        };
        out.push(Row {
            line,
            id: f[0].to_string(),
            routes: f[1].split_whitespace().map(str::to_string).collect(),
            sel,
            undo,
            active,
            exec,
            note,
        });
    }
    Ok(out)
}

fn rows() -> Vec<Row> {
    parse(CONTRACTS).unwrap_or_else(|e| panic!("contracts/commands.txt: {e}"))
}

// ---------------------------------------------------------------- fixture

fn probe() -> EditorApp {
    EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into())
}

fn handled(a: &EditorApp, id: &str) -> bool {
    let r = a.sh.reg.resolve(id);
    a.handlers.contains_key(r) || WIRED.contains(&r)
}

struct Fx {
    app: EditorApp,
    walls: Vec<i64>,
    door: i64,
    curtain: i64,
}

/// Two levels, an 8x6 m ring of four walls on the first, a door in the first wall, a free
/// curtain wall east of the ring. Written
/// straight into the model: the editor undo stack starts empty.
fn fixture() -> Fx {
    let mut ed = Editor::new_in_memory().unwrap();
    let l1 = ed.level;
    let wt = ed.types[0].id;
    let m = &mut ed.model;
    m.add_level("L2", 3.0).unwrap();
    let c = [(0.0, 0.0), (8.0, 0.0), (8.0, 6.0), (0.0, 6.0)];
    let ws: Vec<WallSpec> = (0..4)
        .map(|k| WallSpec {
            a: c[k],
            b: c[(k + 1) % 4],
            level: l1,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .collect();
    let walls = m.add_walls(&ws).unwrap();
    let door = m
        .add_opening(
            walls[0],
            &OpeningSpec {
                along: 2.0,
                width: 0.9,
                sill: 0.0,
                height: 2.1,
                kind: "door".into(),
            },
        )
        .unwrap();
    let sec = MullionSection::Rect { w: 0.05, d: 0.15 };
    let curtain = m
        .add_curtain_wall(&CurtainSpec {
            level: l1,
            name: "CW".into(),
            a: (10.0, 0.0),
            b: (16.0, 0.0),
            bulge: 0.0,
            base_offset: 0.0,
            height: 3.0,
            u: GridLines::Spacing(1.5),
            v: GridLines::Spacing(1.0),
            interior: sec,
            border: sec,
            lines: Default::default(),
            top: vec![],
        })
        .unwrap();
    ed.reload().unwrap();
    let mut app = EditorApp::new(ed, None, "test".into());
    app.ed.set_mode(Mode::Select);
    Fx { app, walls, door, curtain }
}

/// Fixture element for a selection category; `None` when the fixture has none.
fn pick(fx: &Fx, cat: &str) -> Option<Sel> {
    match cat {
        "*" | "wall" => Some(Sel::Wall(fx.walls[1])),
        "opening" | "door" => Some(Sel::Opening(fx.door)),
        "curtain" => Some(Sel::Curtain(fx.curtain)),
        _ => None,
    }
}

fn data(m: &Model) -> Vec<TableRows> {
    ec::dump_except(m, SKIP)
}

// ---------------------------------------------------------------- raw keys

/// Catalogue keys of both languages and their first segments.
struct Keys {
    all: BTreeSet<String>,
    spaces: BTreeSet<String>,
}

fn keys(a: &EditorApp) -> Keys {
    let mut all = BTreeSet::new();
    for lang in [Lang::Ru, Lang::En] {
        all.extend(a.sh.cat.table(lang).keys().cloned());
    }
    let spaces = all
        .iter()
        .filter_map(|k| k.split('.').next())
        .map(str::to_string)
        .collect();
    Keys { all, spaces }
}

/// Tokens of `text` that are catalogue keys, or look like one in a known namespace
/// (a missing key falls back to the key itself).
fn raw_keys(text: &str, k: &Keys) -> Vec<String> {
    let mut out = vec![];
    for tok in text.split(|c: char| c.is_whitespace() || "«»\"'(),:;!?[]{}".contains(c)) {
        let tok = tok.trim_end_matches('.');
        let key_like = tok.contains('.')
            && tok.starts_with(|c: char| c.is_ascii_lowercase())
            && tok
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.');
        if !key_like {
            continue;
        }
        let ns = tok.split('.').next().unwrap_or("");
        if k.all.contains(tok) || k.spaces.contains(ns) {
            out.push(tok.to_string());
        }
    }
    out
}

fn on_screen(a: &EditorApp) -> Vec<String> {
    let mut v: Vec<String> = a.sh.toasts.live.iter().map(|t| t.text.clone()).collect();
    v.extend(a.ed.status.clone());
    if !a.sh.echo.is_empty() {
        v.push(a.sh.echo.clone());
    }
    v
}

// ---------------------------------------------------------------- generator + runner

#[derive(Debug, Clone)]
enum Expect {
    /// No handler: run returns false, a worded refusal, nothing changes.
    Unavailable,
    /// A selection command with nothing selected.
    Refuse,
    /// The contract row as written.
    Row(Undo, Active),
}

#[derive(Debug, Clone)]
struct Check {
    name: String,
    id: String,
    sel: Option<String>,
    expect: Expect,
}

/// Named checks for one row (`handled`: the command has a handler).
fn checks(r: &Row, handled: bool) -> Vec<Check> {
    let name = |case: &str| format!("contract[{}]/{case}", r.id);
    let mk = |case: &str, sel: Option<String>, expect| Check {
        name: name(case),
        id: r.id.clone(),
        sel,
        expect,
    };
    if r.exec == Exec::Skip {
        return vec![];
    }
    if !handled {
        return vec![mk("unavailable", None, Expect::Unavailable)];
    }
    if r.exec == Exec::Todo {
        return vec![];
    }
    match r.sel.first() {
        None => vec![mk("run", None, Expect::Row(r.undo, r.active))],
        Some(cat) => vec![
            mk("empty", None, Expect::Refuse),
            mk(
                &format!("sel:{cat}"),
                Some(cat.clone()),
                Expect::Row(r.undo, r.active),
            ),
        ],
    }
}

#[derive(Default)]
struct Report {
    /// Behaviour oracles (change, undo, redo, integrity, activity).
    behaviour: Vec<String>,
    /// Raw catalogue keys seen on screen.
    raw: Vec<String>,
    ran: usize,
    unavailable: usize,
    todo: usize,
    skip: usize,
}

/// Runs one check on a fresh fixture: (behaviour failures, raw-key failures).
fn exercise(c: &Check, k: &Keys) -> (Vec<String>, Vec<String>) {
    let mut bad = vec![];
    let mut raw = vec![];
    let n = &c.name;
    let mut fx = fixture();
    // no run of a contract may open a native picker: every pick cancels
    ddc_app::file_pick::script_picks(vec![]);
    // armed click-waiting state is thread-local: one row must not leak into the next
    ddc_app::clip_tools::reset();
    if let Some(cat) = &c.sel {
        match pick(&fx, cat) {
            Some(s) => fx.app.ed.selection = vec![s],
            None => return (vec![format!("{n}: no fixture element for '{cat}'")], raw),
        }
    }
    if fx.app.ed.can_undo() {
        return (vec![format!("{n}: fixture starts with an undo step")], raw);
    }
    let toasts0 = fx.app.sh.toasts.live.len();
    let before = data(&fx.app.ed.model);
    let ran = fx.app.run(&c.id);
    let after = data(&fx.app.ed.model);
    let changed = ec::hash_of(&after) != ec::hash_of(&before);
    let stepped = fx.app.ed.can_undo();
    let tool = fx.app.active_command().is_some();
    match fx.app.ed.model.check_integrity() {
        Ok(iss) if iss.is_empty() => {}
        Ok(iss) => bad.push(format!("{n}: integrity {iss:?}")),
        Err(e) => bad.push(format!("{n}: integrity check failed: {e}")),
    }
    for text in on_screen(&fx.app) {
        for key in raw_keys(&text, k) {
            raw.push(format!("{n}: raw key '{key}' in \"{text}\""));
        }
    }
    let unchanged = |what: &str, bad: &mut Vec<String>| {
        if changed {
            bad.push(format!("{n}: {what} changed the model{}", ec::diff(&after, &before)));
        }
        if stepped {
            bad.push(format!("{n}: {what} pushed an undo step"));
        }
    };
    let (undo, active) = match &c.expect {
        Expect::Unavailable => {
            if ran {
                bad.push(format!("{n}: no handler, yet run reported success"));
            }
            if fx.app.sh.toasts.live.len() <= toasts0 {
                bad.push(format!("{n}: refused without a word"));
            }
            unchanged("unavailable command", &mut bad);
            return (bad, raw);
        }
        Expect::Refuse => {
            unchanged("run with nothing selected", &mut bad);
            return (bad, raw);
        }
        Expect::Row(u, a) => (*u, *a),
    };
    if !ran {
        bad.push(format!("{n}: run returned false"));
    }
    match active {
        Active::Tool if !tool => bad.push(format!("{n}: no tool active after the command")),
        Active::Once if tool => bad.push(format!("{n}: a tool stays active ({:?})", fx.app.active_command())),
        _ => {}
    }
    let one = match undo {
        Undo::Zero => {
            unchanged("command", &mut bad);
            false
        }
        Undo::One => {
            if !stepped || !changed {
                bad.push(format!(
                    "{n}: want one undo step (stepped {stepped}, changed {changed})"
                ));
            }
            stepped
        }
        Undo::AtMostOne => {
            if !stepped && changed {
                bad.push(format!("{n}: changed the model without an undo step"));
            }
            stepped
        }
    };
    if one {
        fx.app.ed.undo();
        let back = data(&fx.app.ed.model);
        if ec::hash_of(&back) != ec::hash_of(&before) {
            bad.push(format!("{n}: undo does not restore{}", ec::diff(&back, &before)));
        }
        if fx.app.ed.can_undo() {
            bad.push(format!("{n}: more than one undo step"));
        }
        fx.app.ed.redo();
        let again = data(&fx.app.ed.model);
        if ec::hash_of(&again) != ec::hash_of(&after) {
            bad.push(format!("{n}: redo does not reproduce{}", ec::diff(&again, &after)));
        }
        if let Ok(iss) = fx.app.ed.model.check_integrity() {
            if !iss.is_empty() {
                bad.push(format!("{n}: integrity after undo/redo {iss:?}"));
            }
        }
    }
    (bad, raw)
}

/// Every generated check, once per test process (both oracle tests read it).
fn report() -> &'static Report {
    static R: OnceLock<Report> = OnceLock::new();
    R.get_or_init(|| {
        let a = probe();
        let k = keys(&a);
        let mut rep = Report::default();
        for r in rows() {
            let h = handled(&a, &r.id);
            match r.exec {
                Exec::Skip => rep.skip += 1,
                Exec::Todo if h => rep.todo += 1,
                Exec::Run if !h => rep
                    .behaviour
                    .push(format!("contract[{}]: row says run, no handler", r.id)),
                _ => {}
            }
            for c in checks(&r, h) {
                if matches!(c.expect, Expect::Unavailable) {
                    rep.unavailable += 1;
                } else {
                    rep.ran += 1;
                }
                match std::panic::catch_unwind(AssertUnwindSafe(|| exercise(&c, &k))) {
                    Ok((b, raw)) => {
                        rep.behaviour.extend(b);
                        rep.raw.extend(raw);
                    }
                    Err(_) => rep.behaviour.push(format!("{}: panicked", c.name)),
                }
            }
        }
        eprintln!(
            "contracts: {} scripted checks, {} unavailable commands, {} handlers not scripted yet, {} skipped",
            rep.ran, rep.unavailable, rep.todo, rep.skip
        );
        rep
    })
}

// ---------------------------------------------------------------- gate

/// Menu paths: (path, command) for the quick menu without and with a selection.
fn menu_paths(fx: &mut Fx) -> Vec<(String, String)> {
    let mut out = vec![];
    let cases: [(&str, Option<Sel>); 3] = [
        ("none", None),
        ("wall", Some(Sel::Wall(fx.walls[1]))),
        ("opening", Some(Sel::Opening(fx.door))),
    ];
    for (name, sel) in cases {
        fx.app.ed.selection = sel.into_iter().collect();
        let items =
            ddc_app::quick::menu::items(&fx.app.ed, &fx.app.sh.reg, Some("arch.wall"), true, 1, &|_| true);
        for id in items {
            if id != ddc_app::quick::menu::SEP {
                out.push((format!("menu ({name})"), id));
            }
        }
    }
    fx.app.ed.selection.clear();
    out
}

#[test]
fn every_palette_command_has_contract() {
    let rs = rows();
    let mut fx = fixture();
    let reg = &fx.app.sh.reg;
    let mut bad = vec![];
    let mut by_id: BTreeMap<&str, &Row> = BTreeMap::new();
    for r in &rs {
        if by_id.insert(&r.id, r).is_some() {
            bad.push(format!("line {}: second row for {}", r.line, r.id));
        }
        if reg.get(&r.id).is_none() {
            bad.push(format!("line {}: {} is not a registry command", r.line, r.id));
        }
    }
    let mut need: Vec<(String, String)> = vec![];
    for c in reg.all() {
        need.push(("palette".into(), c.id.clone()));
        if c.on("R") {
            let tab = c.tab.as_deref().unwrap_or("-");
            let g = ddc_ui::widgets::ribbon::group_name(c);
            need.push((format!("ribbon {tab}/{g}"), c.id.clone()));
        }
    }
    let cats: BTreeSet<String> = reg.all().iter().flat_map(|c| c.context.clone()).collect();
    for cat in &cats {
        for c in reg.contextual(cat) {
            need.push((format!("ribbon modify ({cat})"), c.id.clone()));
        }
    }
    let menu = menu_paths(&mut fx);
    let reg = &fx.app.sh.reg;
    for (path, id) in menu {
        if reg.get(&id).is_none() {
            bad.push(format!("{path}: {id} is not a registry command"));
        }
        need.push((path, id));
    }
    let mut missing = BTreeSet::new();
    for (path, id) in &need {
        if !by_id.contains_key(id.as_str()) {
            let h = if handled(&fx.app, id) { "handler" } else { "no handler" };
            missing.insert(format!("{path}: {id} has no contract row ({h})"));
        }
    }
    bad.extend(missing);
    for c in reg.all() {
        let Some(r) = by_id.get(c.id.as_str()) else {
            continue;
        };
        let want: BTreeSet<String> = std::iter::once("P".to_string())
            .chain(c.surfaces.iter().cloned())
            .collect();
        if r.routes != want {
            bad.push(format!(
                "line {}: {} routes {:?}, registry says {:?}",
                r.line, c.id, r.routes, want
            ));
        }
        if c.context.is_empty() != r.sel.is_empty() {
            bad.push(format!(
                "line {}: {} sel {:?}, registry context {:?}",
                r.line, c.id, r.sel, c.context
            ));
        }
        let any = c.context.iter().any(|k| k == "*");
        if let Some(s) = r.sel.iter().find(|s| !any && !c.context.contains(s)) {
            bad.push(format!("line {}: {} sel '{s}' not in its context", r.line, c.id));
        }
    }
    let n = |e: Exec| rs.iter().filter(|r| r.exec == e).count();
    eprintln!(
        "contracts: {} rows ({} run, {} todo, {} skip), {} entry routes",
        rs.len(),
        n(Exec::Run),
        n(Exec::Todo),
        n(Exec::Skip),
        need.len()
    );
    let todo = n(Exec::Todo);
    assert!(todo <= TODO_MAX, "{todo} todo rows, ceiling TODO_MAX = {TODO_MAX}");
    assert_eq!(todo, TODO_MAX, "{todo} todo rows: lower TODO_MAX to match");
    assert!(bad.is_empty(), "{} problems:\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn contract_rows_parse_and_generate_named_checks() {
    let src = "# c\n\
        a.x | P R | - | 0 | once | run\n\
        b.y | P C | wall | 1 | ? | run | n\n\
        c.z | P | - | ? | ? | todo\n\
        d.w | P | - | ? | ? | skip | dialog\n";
    let rs = parse(src).unwrap();
    assert_eq!(rs.len(), 4);
    assert_eq!(rs[1].sel, vec!["wall".to_string()]);
    assert_eq!(rs[1].line, 3);
    let names = |r: &Row, h| -> Vec<String> { checks(r, h).into_iter().map(|c| c.name).collect() };
    assert_eq!(names(&rs[0], true), ["contract[a.x]/run"]);
    assert_eq!(names(&rs[1], true), ["contract[b.y]/empty", "contract[b.y]/sel:wall"]);
    assert!(names(&rs[2], true).is_empty());
    assert_eq!(names(&rs[2], false), ["contract[c.z]/unavailable"]);
    assert!(names(&rs[3], false).is_empty(), "skip never runs");
    for bad in [
        "a | P | - | 2 | once | run",
        "a | P | - | 0 | idle | run",
        "a | P | - | 0 | once | go",
        "a | P | - | 0 | once | skip",
        "a | P | -",
    ] {
        assert!(parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn contract_refusals_change_nothing() {
    let rep = report();
    assert!(rep.ran > 0, "no scripted checks ran");
    assert!(
        rep.behaviour.is_empty(),
        "{} contract failures:\n{}",
        rep.behaviour.len(),
        rep.behaviour.join("\n")
    );
}

#[test]
fn no_raw_key_on_screen() {
    let k = keys(&probe());
    assert_eq!(raw_keys("cmd.arch.wall", &k), ["cmd.arch.wall"]);
    assert_eq!(raw_keys("\"cmd.no_such\" is not available yet.", &k), ["cmd.no_such"]);
    assert!(raw_keys("Wall 3.5 m long, e.g. here", &k).iter().all(|t| t != "3.5"));
    let rep = report();
    assert!(
        rep.raw.is_empty(),
        "{} raw keys on screen:\n{}",
        rep.raw.len(),
        rep.raw.join("\n")
    );
}

// ---------------------------------------------------------------- save → reopen

fn reopen(m: &Model, name: &str) -> Model {
    let p = ec::temp_file(name);
    m.snapshot_to(&p).unwrap();
    // the reopened connection holds the file open; it stays for the temp-folder cleanup
    Model::open(p.to_str().unwrap()).unwrap()
}

fn op(f: impl Fn(&mut Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static) -> OpFn {
    OpFn(Arc::new(f))
}

/// One random edit through the editor; a rejected op must change nothing.
fn step(ed: &mut Editor, rng: &mut Rng, ctx: &str) -> bool {
    let walls = ec::ids(&ed.model, "wall");
    let (lv, wt) = (ed.level, ed.types[0].id);
    let p = |rng: &mut Rng| (rng.f(-10.0, 10.0), rng.f(-10.0, 10.0));
    let f = match rng.below(5) {
        0 | 1 => {
            let (a, b) = (p(rng), p(rng));
            op(move |m| {
                m.add_wall(&WallSpec {
                    a,
                    b,
                    level: lv,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type: wt,
                    offset0: None,
                })
                .map(|_| ())
            })
        }
        2 => {
            let id = rng.pick(&walls).unwrap_or(9_999);
            let (a, b) = (p(rng), p(rng));
            op(move |m| m.set_wall_axis(id, a, b))
        }
        3 => {
            let id = rng.pick(&walls).unwrap_or(9_999);
            let along = rng.f(0.0, 6.0);
            op(move |m| {
                m.add_opening(
                    id,
                    &OpeningSpec {
                        along,
                        width: 0.9,
                        sill: 0.0,
                        height: 2.1,
                        kind: "door".into(),
                    },
                )
                .map(|_| ())
            })
        }
        _ => {
            let id = rng.pick(&walls).unwrap_or(9_999);
            op(move |m| m.delete_wall(id))
        }
    };
    let before = ec::dump(&ed.model);
    let ok = ed.run_op(f).is_ok();
    if !ok {
        ec::assert_same(&ec::dump(&ed.model), &before, &format!("{ctx}: rejected op"));
    }
    ec::assert_clean(&ed.model, ctx);
    ok
}

#[test]
fn random_ops_save_reopen_identical() {
    let seed0: u64 = std::env::var("DDC_E2E_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let n = if seed0 > 0 { 1 } else { ec::seqs(6) };
    for s in 0..n {
        let seed = if seed0 > 0 { seed0 } else { 0x6C0 + s };
        let mut rng = Rng::new(seed);
        let mut ed = Editor::new_in_memory().unwrap();
        let mut done = 0;
        for i in 0..12 {
            let ctx = format!("seed {seed} step {i}");
            if step(&mut ed, &mut rng, &ctx) {
                done += 1;
            }
            if rng.below(4) == 0 {
                ed.undo();
                if rng.below(2) == 0 {
                    ed.redo();
                }
            }
        }
        let ctx = format!("seed {seed} ({done} accepted)");
        let want = data(&ed.model);
        // undo all, redo all: the identical model
        let mut k = 0;
        while ed.can_undo() && k < 64 {
            ed.undo();
            k += 1;
        }
        for _ in 0..k {
            ed.redo();
        }
        ec::assert_same(&data(&ed.model), &want, &format!("{ctx}: undo all + redo all"));
        ec::assert_clean(&ed.model, &ctx);
        // save → reopen, twice, and the editor takes the reopened model
        let r1 = reopen(&ed.model, &format!("ri6_{seed}_a.ddc"));
        ec::assert_same(&data(&r1), &want, &format!("{ctx}: reopened"));
        assert_eq!(ec::journal_len(&r1), ec::journal_len(&ed.model), "{ctx}: journal");
        ec::assert_clean(&r1, &format!("{ctx}: reopened"));
        let r2 = reopen(&r1, &format!("ri6_{seed}_b.ddc"));
        ec::assert_same(&data(&r2), &want, &format!("{ctx}: reopened twice"));
        let mut ed2 = Editor::new_in_memory().unwrap();
        ed2.replace_model(r2).unwrap();
        ec::assert_same(&data(&ed2.model), &want, &format!("{ctx}: editor on reopened"));
    }
}
