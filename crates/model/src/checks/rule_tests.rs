// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use crate::{
    ColumnSpec, LayerSpec, MemberShape, OpeningSpec, PVal, ParamSpec, ParamType, PipeMaterial,
    RoomSpec, RunSpec, SystemKind, WallSpec,
};
use rusqlite::types::Value;

/// Contents of the model tables except the journal and settings: company rules are not geometry.
fn hash(m: &Model) -> u64 {
    use std::hash::{Hash, Hasher};
    let db = m.sql();
    let mut st = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap();
    let tables: Vec<String> = st
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|x| x.unwrap())
        .collect();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for t in tables.iter().filter(|t| {
        !matches!(t.as_str(), "op" | "join_cache")
            && !t.ends_with("_qty")
            && !t.starts_with("ai_")
            && !t.starts_with("sqlite_")
    }) {
        let mut st = db.prepare(&format!("SELECT * FROM \"{t}\"")).unwrap();
        let n = st.column_count();
        let mut rows: Vec<String> = st
            .query_map([], |r| {
                Ok((0..n)
                    .map(|i| format!("{:?}", r.get::<_, Value>(i).unwrap()))
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        rows.sort();
        t.hash(&mut h);
        rows.hash(&mut h);
    }
    h.finish()
}

struct Base {
    m: Model,
    level: i64,
    wall_type: i64,
}

fn base() -> Base {
    let mut m = Model::open(":memory:").unwrap();
    let level = m.add_level("L1", 0.0).unwrap();
    let layer = [LayerSpec {
        material: "Brick".into(),
        thickness: 0.2,
    }];
    let wall_type = m.add_wall_type("W200", &layer).unwrap();
    Base {
        m,
        level,
        wall_type,
    }
}

impl Base {
    fn wall(&mut self, a: (f64, f64), b: (f64, f64)) -> i64 {
        let spec = WallSpec {
            a,
            b,
            level: self.level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: self.wall_type,
            offset0: None,
        };
        self.m.add_wall(&spec).unwrap()
    }
    fn closed(&mut self) -> Vec<i64> {
        let p = [(0.0, 0.0), (10.0, 0.0), (10.0, 8.0), (0.0, 8.0)];
        (0..4).map(|i| self.wall(p[i], p[(i + 1) % 4])).collect()
    }
    fn door(&mut self, wall: i64, along: f64) -> i64 {
        let spec = OpeningSpec {
            along,
            width: 0.9,
            sill: 0.0,
            height: 2.1,
            kind: "door".into(),
        };
        self.m.add_opening(wall, &spec).unwrap()
    }
    fn room(&mut self, at: (f64, f64)) -> i64 {
        let spec = RoomSpec {
            level: self.level,
            name: "R".into(),
            number: "1".into(),
            point: at,
            height: 3.0,
        };
        self.m.add_room(&spec).unwrap()
    }
    fn run(&self, id: &str) -> Vec<Finding> {
        self.scoped(id, &CheckScope::All)
    }
    fn scoped(&self, id: &str, scope: &CheckScope) -> Vec<Finding> {
        let rule = rules().iter().find(|r| r.id() == id).unwrap();
        let mut out = vec![];
        rule.run(&self.m, scope, &mut out).unwrap();
        out
    }
    /// A fix removes the finding, and undo restores the model byte for byte.
    fn fix_and_undo(&mut self, id: &str) {
        let found = self.run(id);
        assert!(!found.is_empty(), "{id} found nothing");
        let before = hash(&self.m);
        let top = self.m.op_top().unwrap();
        self.m.apply_check_fix(&found[0].fix).unwrap();
        assert_eq!(
            self.m.op_top().unwrap(),
            top + 1,
            "one fix is one journal row"
        );
        assert_ne!(hash(&self.m), before);
        assert_eq!(
            self.run(id).len(),
            found.len() - 1,
            "{id}: the fix removes its finding"
        );
        self.m.undo().unwrap();
        assert_eq!(hash(&self.m), before, "{id}: undo restores the exact state");
    }
    fn company(&mut self, rule: J) {
        let list = load_pack(&J::Array(vec![rule]).to_string()).unwrap();
        self.m.save_rule(&list[0]).unwrap();
    }
}

fn door_rule(check: J) -> J {
    json!({"id": "c.r", "title": "T", "severity": "error", "scope": {"cats": ["door"]}, "check": check})
}

#[test]
fn a_sound_model_has_no_errors_or_warnings() {
    let mut b = base();
    let walls = b.closed();
    b.door(walls[0], 2.0);
    b.room((5.0, 4.0));
    let found = run_all(&b.m, None, &CheckScope::All, 1000).unwrap();
    let serious: Vec<_> = found
        .iter()
        .filter(|f| f.severity != Severity::Info)
        .collect();
    assert!(serious.is_empty(), "{serious:?}");
}

#[test]
fn the_same_model_gives_the_same_findings() {
    let mut b = base();
    let walls = b.closed();
    b.door(walls[0], 2.0);
    b.room((100.0, 100.0));
    let text = |m: &Model| format!("{:?}", run_all(m, None, &CheckScope::All, 1000).unwrap());
    assert_eq!(text(&b.m), text(&b.m));
}

#[test]
fn an_opening_outside_its_wall_moves_back_in() {
    let mut b = base();
    let walls = b.closed();
    let door = b.door(walls[0], 2.0);
    b.m.sql()
        .execute("UPDATE opening SET along=9.5 WHERE id=?1", [door])
        .unwrap();
    assert!(matches!(b.run("opening_outside")[0].fix, Fix::Edits(_)));
    b.fix_and_undo("opening_outside");
}

#[test]
fn an_opening_wider_than_the_wall_can_only_go() {
    let mut b = base();
    let wall = b.wall((0.0, 0.0), (2.0, 0.0));
    let door = b.door(wall, 0.5);
    b.m.sql()
        .execute("UPDATE opening SET width=5.0 WHERE id=?1", [door])
        .unwrap();
    assert!(matches!(b.run("opening_outside")[0].fix, Fix::Delete(_)));
    b.fix_and_undo("opening_outside");
}

#[test]
fn overlapping_openings_are_found_on_one_wall_only() {
    let mut b = base();
    let walls = b.closed();
    let (d1, d2) = (b.door(walls[0], 1.0), b.door(walls[0], 3.0));
    b.door(walls[1], 1.0);
    assert!(b.run("opening_overlap").is_empty());
    b.m.sql()
        .execute("UPDATE opening SET along=1.2 WHERE id=?1", [d2])
        .unwrap();
    let found = b.run("opening_overlap");
    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].elements,
        vec![("opening".to_string(), d1), ("opening".to_string(), d2)]
    );
    b.fix_and_undo("opening_overlap");
}

#[test]
fn a_window_above_a_door_is_no_overlap() {
    let mut b = base();
    let walls = b.closed();
    b.door(walls[0], 1.0);
    let w = OpeningSpec {
        along: 1.0,
        width: 0.9,
        sill: 2.2,
        height: 0.5,
        kind: "window".into(),
    };
    b.m.add_opening(walls[0], &w).unwrap();
    assert!(b.run("opening_overlap").is_empty());
}

#[test]
fn rooms_outside_and_rooms_open_are_told_apart() {
    let mut b = base();
    b.wall((0.0, 0.0), (10.0, 0.0));
    b.wall((10.0, 0.0), (10.0, 8.0));
    b.wall((0.0, 8.0), (0.0, 0.0));
    let open = b.room((5.0, 4.0));
    let far = b.room((100.0, 100.0));
    let unplaced = b.run("room_unplaced");
    assert_eq!(unplaced.len(), 1);
    assert_eq!(unplaced[0].elements[0].1, far);
    let not_closed = b.run("room_open");
    assert_eq!(not_closed.len(), 1);
    assert_eq!(not_closed[0].elements[0].1, open);
    assert!(matches!(not_closed[0].fix, Fix::None));
    b.fix_and_undo("room_unplaced");
}

#[test]
fn zero_sizes_are_found() {
    let mut b = base();
    b.closed();
    let room = b.room((5.0, 4.0));
    assert!(b.run("zero_size").is_empty());
    b.m.sql()
        .execute("UPDATE room_qty SET area=0 WHERE room=?1", [room])
        .unwrap();
    let found = b.run("zero_size");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].elements[0], ("room".to_string(), room));
}

#[test]
fn duplicates_keep_the_first_element() {
    let mut b = base();
    let mt =
        b.m.add_member_type("C", &MemberShape::Rect { b: 0.4, h: 0.4 }, "Concrete")
            .unwrap();
    let col = |at| ColumnSpec {
        level: b.level,
        top_level: None,
        member_type: mt,
        structural: true,
        at,
        base_offset: 0.0,
        top_offset: 3.0,
        rotation: 0.0,
        top_at: None,
    };
    let (a, c) = (col((1.0, 1.0)), col((5.0, 1.0)));
    let first = b.m.add_column(&a).unwrap();
    let second = b.m.add_column(&a).unwrap();
    b.m.add_column(&c).unwrap();
    let found = b.run("duplicates");
    assert_eq!(found.len(), 1);
    match &found[0].fix {
        Fix::Delete(list) => assert_eq!(list, &vec![("col".to_string(), second)]),
        other => panic!("{other:?}"),
    }
    assert!(found[0].elements.contains(&("col".to_string(), first)));
    b.fix_and_undo("duplicates");
}

#[test]
fn walls_along_one_line_overlap_only_when_they_share_a_stretch() {
    let mut b = base();
    b.wall((0.0, 0.0), (5.0, 0.0));
    b.wall((5.0, 0.0), (10.0, 0.0));
    assert!(
        b.run("duplicates").is_empty(),
        "walls meeting at a point are normal"
    );
    let inner = b.wall((7.0, 0.0), (6.0, 0.0));
    let found = b.run("duplicates");
    assert_eq!(found.len(), 1);
    assert!(found[0].elements.contains(&("wall".to_string(), inner)));
    assert!(matches!(found[0].fix, Fix::Delete(_)));
}

fn door_param(name: &str) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        ptype: ParamType::Text,
        per_type: false,
        unit: String::new(),
        group: String::new(),
        guid: None,
        categories: vec!["door".into()],
    }
}

#[test]
fn an_empty_parameter_is_one_finding_with_a_count() {
    let mut b = base();
    let walls = b.closed();
    let (d1, _) = (b.door(walls[0], 1.0), b.door(walls[0], 3.0));
    b.m.add_param(&door_param("Rating")).unwrap();
    let found = b.run("missing_params");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].args[0].1, "Rating");
    assert_eq!(found[0].args[2].1, "2");
    let set = Edit::ParamValue {
        param: "Rating".into(),
        kind: "opening".into(),
        id: d1,
        value: Some(PVal::Text("EI30".into())),
    };
    b.m.apply_edits(&[set]).unwrap();
    assert_eq!(b.run("missing_params")[0].args[2].1, "1");
}

#[test]
fn a_room_without_a_name_is_reported() {
    let mut b = base();
    b.closed();
    let room = b.room((5.0, 4.0));
    assert!(b.run("missing_params").is_empty());
    b.m.sql()
        .execute("UPDATE room SET name='' WHERE id=?1", [room])
        .unwrap();
    assert_eq!(b.run("missing_params")[0].msg_key, "ai.check.room_no_name");
}

#[test]
fn type_marks_are_filled_type_by_type() {
    let mut b = base();
    b.closed();
    assert_eq!(b.run("type_marks").len(), 1);
    assert_eq!(b.run("type_marks")[0].severity, Severity::Info);
    b.fix_and_undo("type_marks");

    let layer = [LayerSpec {
        material: "Brick".into(),
        thickness: 0.1,
    }];
    let other = b.m.add_wall_type("W100", &layer).unwrap();
    let spec = WallSpec {
        a: (20.0, 0.0),
        b: (25.0, 0.0),
        level: b.level,
        base_offset: 0.0,
        height: 3.0,
        wall_type: other,
        offset0: None,
    };
    b.m.add_wall(&spec).unwrap();
    let stale = b.run("type_marks");
    assert_eq!(stale.len(), 2);
    // both fixes were computed before the first: the second must not trip over the created parameter
    b.m.apply_check_fix(&stale[0].fix).unwrap();
    b.m.apply_check_fix(&stale[1].fix).unwrap();
    assert!(b.run("type_marks").is_empty());
}

#[test]
fn a_pipe_through_a_wall_is_a_clash_with_a_fix() {
    let mut b = base();
    let walls = b.closed();
    let sys = b.m.add_mep_system("CW", SystemKind::ColdWater).unwrap();
    let run = RunSpec::pipe(
        sys,
        b.level,
        PipeMaterial::Copper,
        15,
        vec![(5.0, -2.0, 1.0), (5.0, 2.0, 1.0)],
    )
    .unwrap();
    let pipe = b.m.add_mep_run(&run).unwrap();
    let found = b.run("clashes");
    assert_eq!(found.len(), 1);
    assert!(found[0].elements.contains(&("mep_run".to_string(), pipe)));
    assert!(found[0].elements.contains(&("wall".to_string(), walls[0])));
    assert!(matches!(found[0].fix, Fix::Clash(_)));
    let before = hash(&b.m);
    b.m.apply_check_fix(&found[0].fix).unwrap();
    assert_ne!(hash(&b.m), before);
    b.m.undo().unwrap();
    assert_eq!(hash(&b.m), before);
}

#[test]
fn a_finding_without_a_fix_is_refused_and_leaves_no_journal_row() {
    let mut b = base();
    let top = b.m.op_top().unwrap();
    assert!(b.m.apply_check_fix(&Fix::None).is_err());
    assert!(b.m.apply_check_fix(&Fix::Delete(vec![])).is_err());
    assert_eq!(b.m.op_top().unwrap(), top);
}

#[test]
fn company_range_rule_finds_and_fixes_the_door_width() {
    let mut b = base();
    let walls = b.closed();
    let (narrow, _) = (b.door(walls[0], 2.0), b.door(walls[0], 6.0));
    b.m.sql()
        .execute("UPDATE opening SET width=0.6 WHERE id=?1", [narrow])
        .unwrap();
    b.company(door_rule(
        json!({"kind": "param_range", "param": "width", "min": 0.8}),
    ));
    let found = b.run("company_rules");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].msg_key, "ai.check.rule.param_range");
    assert_eq!(found[0].severity, Severity::Error);
    assert_eq!(found[0].args[0].1, "c.r");
    assert_eq!(found[0].elements[0], ("opening".to_string(), narrow));
    b.fix_and_undo("company_rules");
}

#[test]
fn company_present_and_in_rules_read_project_parameters() {
    let mut b = base();
    let walls = b.closed();
    let (d1, d2) = (b.door(walls[0], 2.0), b.door(walls[0], 6.0));
    b.m.add_param(&door_param("Rating")).unwrap();
    let set = Edit::ParamValue {
        param: "Rating".into(),
        kind: "opening".into(),
        id: d1,
        value: Some(PVal::Text("EI30".into())),
    };
    b.m.apply_edits(&[set]).unwrap();

    b.company(door_rule(
        json!({"kind": "param_present", "param": "Rating"}),
    ));
    let found = b.run("company_rules");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].elements[0].1, d2);

    b.company(door_rule(
        json!({"kind": "param_in", "param": "Rating", "allowed": ["EI60"]}),
    ));
    let found = b.run("company_rules");
    assert_eq!(found.len(), 2);
    let wrong = found.iter().find(|f| f.elements[0].1 == d1).unwrap();
    assert_eq!(wrong.msg_key, "ai.check.rule.param_in");
    assert!(
        matches!(wrong.fix, Fix::Edits(_)),
        "a single allowed value can be written"
    );
}

#[test]
fn company_count_rule_sees_the_whole_model_only() {
    let mut b = base();
    let walls = b.closed();
    b.door(walls[0], 2.0);
    b.company(door_rule(json!({"kind": "count", "min": 2})));
    assert_eq!(b.run("company_rules")[0].msg_key, "ai.check.rule.count");
    let scope = CheckScope::Elements(vec![("wall".to_string(), walls[0])]);
    assert!(b.scoped("company_rules", &scope).is_empty());
}

#[test]
fn company_clearance_rule_measures_in_plan_and_per_level() {
    let mut b = base();
    let walls = b.closed();
    let mt =
        b.m.add_member_type("C", &MemberShape::Rect { b: 0.4, h: 0.4 }, "Concrete")
            .unwrap();
    let spec = |at| ColumnSpec {
        level: b.level,
        top_level: None,
        member_type: mt,
        structural: true,
        at,
        base_offset: 0.0,
        top_offset: 3.0,
        rotation: 0.0,
        top_at: None,
    };
    let (near, far) = (spec((5.0, 0.5)), spec((5.0, 4.0)));
    let near_id = b.m.add_column(&near).unwrap();
    b.m.add_column(&far).unwrap();
    b.company(json!({"id": "c.gap", "title": "Gap", "severity": "warning", "scope": {"cats": ["column"]}, "check": {"kind": "clearance", "other": ["wall"], "min_m": 1.0}}));
    let found = b.run("company_rules");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].msg_key, "ai.check.rule.clearance");
    assert_eq!(
        found[0].elements,
        vec![("col".to_string(), near_id), ("wall".to_string(), walls[0])]
    );
}

#[test]
fn a_rule_on_an_unknown_category_says_it_did_not_run() {
    let mut b = base();
    b.company(json!({"id": "c.x", "title": "X", "severity": "error", "scope": {"cats": ["spaceship"]}, "check": {"kind": "param_present", "param": "P"}}));
    let found = b.run("company_rules");
    assert_eq!(found[0].msg_key, "ai.check.rule.unsupported");
    assert_eq!(found[0].severity, Severity::Info);
}

#[test]
fn text_of_a_rule_cannot_change_the_query() {
    let mut b = base();
    let walls = b.closed();
    b.door(walls[0], 2.0);
    let evil = "x'; DROP TABLE wall; --";
    b.company(door_rule(
        json!({"kind": "param_in", "param": evil, "allowed": [evil]}),
    ));
    assert_eq!(b.run("company_rules").len(), 1);
    assert_eq!(
        b.m.sql()
            .query_row("SELECT count(*) FROM wall", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        4
    );
}

#[test]
fn the_scope_limits_findings() {
    let mut b = base();
    let walls = b.closed();
    let other = b.m.add_level("L2", 3.0).unwrap();
    let (d1, d2) = (b.door(walls[0], 1.0), b.door(walls[1], 1.0));
    b.m.sql()
        .execute("UPDATE opening SET along=9.5 WHERE id=?1", [d1])
        .unwrap();
    b.m.sql()
        .execute("UPDATE opening SET along=7.5 WHERE id=?1", [d2])
        .unwrap();
    assert_eq!(b.run("opening_outside").len(), 2);
    let only = CheckScope::Elements(vec![("opening".to_string(), d2)]);
    let found = b.scoped("opening_outside", &only);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].elements[0].1, d2);
    assert!(b
        .scoped("opening_outside", &CheckScope::Level(other))
        .is_empty());
    assert_eq!(
        b.scoped("opening_outside", &CheckScope::Level(b.level))
            .len(),
        2
    );
}

#[test]
fn too_many_findings_end_with_a_count_of_the_rest() {
    let mut b = base();
    b.m.sql()
        .execute_batch(&format!(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<{}) \
             INSERT INTO room(level,name,number,px,py,height) SELECT {},'R','1',i*100.0,0,3 FROM n",
            util::RULE_CAP + 7,
            b.level
        ))
        .unwrap();
    let found = b.run("room_unplaced");
    assert_eq!(found.len(), util::RULE_CAP + 1);
    let last = found.last().unwrap();
    assert_eq!(last.msg_key, "ai.check.more");
    assert_eq!(last.args[0].1, "7");
}

/// Rules that run at every plan step must remain set-based queries.
#[test]
#[ignore = "slow: 200k elements"]
fn light_rules_stay_quick_on_200k_elements() {
    let mut b = base();
    b.m.sql()
        .execute_batch(&format!(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<100000) \
             INSERT INTO wall(level,wtype,ax,ay,bx,by,base_offset,height) SELECT {l},{t},i*20.0,0,i*20.0+5,0,0,3 FROM n; \
             INSERT INTO opening(wall,along,width,sill,height,kind) SELECT id,1,0.9,0,2.1,'door' FROM wall;",
            l = b.level,
            t = b.wall_type
        ))
        .unwrap();
    let t = std::time::Instant::now();
    for r in rules().iter().filter(|r| !r.heavy()) {
        let mut out = vec![];
        r.run(&b.m, &CheckScope::All, &mut out).unwrap();
    }
    assert!(t.elapsed().as_secs() < 30, "{:?}", t.elapsed());
}

/// A model that trips most rules at once.
fn dirty() -> Base {
    let mut b = base();
    let walls = b.closed();
    let (d1, d2) = (b.door(walls[0], 1.0), b.door(walls[0], 4.0));
    let outside = b.door(walls[1], 2.0);
    b.wall((2.0, 0.0), (3.0, 0.0));
    let far = b.room((100.0, 100.0));
    let sys = b.m.add_mep_system("CW", SystemKind::ColdWater).unwrap();
    let run = RunSpec::pipe(
        sys,
        b.level,
        PipeMaterial::Copper,
        15,
        vec![(5.0, -2.0, 1.0), (5.0, 2.0, 1.0)],
    )
    .unwrap();
    b.m.add_mep_run(&run).unwrap();
    let db = b.m.sql();
    db.execute("UPDATE opening SET along=1.2 WHERE id=?1", [d2]).unwrap();
    db.execute("UPDATE opening SET along=7.9 WHERE id=?1", [outside]).unwrap();
    db.execute("UPDATE opening SET width=0.6 WHERE id=?1", [d1]).unwrap();
    db.execute("UPDATE room SET name='' WHERE id=?1", [far]).unwrap();
    db.execute("UPDATE wall SET bx=ax, by=ay WHERE id=?1", [walls[3]]).unwrap();
    db.execute("UPDATE wall SET height=0 WHERE id=?1", [walls[2]]).unwrap();
    b.company(door_rule(
        json!({"kind": "param_range", "param": "width", "min": 0.8}),
    ));
    b
}

#[test]
fn every_existing_rule_emits_position_and_measure() {
    let b = dirty();
    let found = run_all(&b.m, None, &CheckScope::All, 1000).unwrap();
    let fired: std::collections::BTreeSet<&str> = found.iter().map(|f| f.rule).collect();
    for id in [
        "opening_outside",
        "opening_overlap",
        "room_unplaced",
        "zero_size",
        "duplicates",
        "level_mismatch",
        "type_marks",
        "missing_params",
        "clashes",
        "company_rules",
    ] {
        assert!(fired.contains(id), "{id} did not fire: {fired:?}");
    }
    for f in found.iter().filter(|f| !f.elements.is_empty()) {
        let at = f.at.unwrap_or_else(|| panic!("{} has no position", f.rule));
        assert!(at.iter().all(|x| x.is_finite()), "{}: {at:?}", f.rule);
        let ms = f
            .measure
            .unwrap_or_else(|| panic!("{} has no measure", f.rule));
        assert!(ms.value.is_finite() && ms.limit.is_finite(), "{}", f.rule);
        assert!(
            ["m", "m2", "m3", "count", ""].contains(&ms.unit),
            "{}: unit {}",
            f.rule,
            ms.unit
        );
    }
}

#[test]
fn finding_has_point_and_measure() {
    let mut b = base();
    let walls = b.closed();
    let door = b.door(walls[1], 2.0);
    b.m.sql()
        .execute("UPDATE opening SET along=7.9 WHERE id=?1", [door])
        .unwrap();
    let found = run_all(&b.m, Some(&["opening_outside"]), &CheckScope::All, 10).unwrap();
    assert_eq!(found.len(), 1);
    let f = &found[0];
    // 7.9 + 0.9 = 8.8 m on an 8 m wall: 0.8 m over a limit of 0
    let ms = f.measure.unwrap();
    assert_eq!(ms.unit, "m");
    assert!((ms.value - 0.8).abs() < 1e-6 && ms.limit == 0.0, "{ms:?}");
    // the centre of the opening is clamped to the wall end (10, 8), mid-height 1.05 m
    let at = f.at.unwrap();
    assert!((at[0] - 10.0).abs() < 1e-6 && (at[1] - 8.0).abs() < 1e-6, "{at:?}");
    assert!((at[2] - 1.05).abs() < 1e-6, "{at:?}");
}

#[test]
fn fix_envelope_round_trips_through_serde() {
    let b = dirty();
    let found = run_all(&b.m, None, &CheckScope::All, 1000).unwrap();
    let mut seen = 0;
    for f in &found {
        let Some(env) = f.envelope() else { continue };
        let text = serde_json::to_string(&env).unwrap();
        let back: FixEnvelope = serde_json::from_str(&text).unwrap();
        assert_eq!(back, env, "{}", f.rule);
        let lowered = back.to_fix(&b.m).unwrap();
        assert_eq!(
            std::mem::discriminant(&lowered),
            std::mem::discriminant(&f.fix),
            "{}",
            f.rule
        );
        assert_eq!(lowered.envelope().unwrap(), env, "{}", f.rule);
        seen += 1;
    }
    assert!(seen >= 3, "only {seen} findings carried a fix");
    assert!(Fix::None.envelope().is_none());
    assert!(Fix::Delete(vec![]).envelope().is_none());
}

#[test]
fn clash_finding_fix_is_envelope() {
    let mut b = base();
    b.closed();
    let sys = b.m.add_mep_system("CW", SystemKind::ColdWater).unwrap();
    let run = RunSpec::pipe(
        sys,
        b.level,
        PipeMaterial::Copper,
        15,
        vec![(5.0, -2.0, 1.0), (5.0, 2.0, 1.0)],
    )
    .unwrap();
    b.m.add_mep_run(&run).unwrap();
    let found = run_all(&b.m, Some(&["clashes"]), &CheckScope::All, 10).unwrap();
    assert_eq!(found.len(), 1);
    let env = found[0].envelope().expect("a clash with a fix has an envelope");
    assert_eq!(env.ops.len(), 1);
    assert!(matches!(
        env.ops[0],
        FixOp::ClashOpening { .. } | FixOp::ClashSlabHole { .. } | FixOp::ClashReroute { .. }
    ));
    assert!(matches!(env.to_fix(&b.m).unwrap(), Fix::Clash(_)));
    let ms = found[0].measure.unwrap();
    assert_eq!((ms.unit, ms.limit), ("m3", 0.0));
    assert!(ms.value > 0.0);
    let text = serde_json::to_string(&env).unwrap();
    assert_eq!(serde_json::from_str::<FixEnvelope>(&text).unwrap(), env);
}

#[test]
fn dry_run_fix_never_writes_the_live_file() {
    let dir = std::env::temp_dir().join(format!("ddc-fix-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut b = base();
    let walls = b.closed();
    let door = b.door(walls[0], 2.0);
    b.m.sql()
        .execute("UPDATE opening SET along=9.5 WHERE id=?1", [door])
        .unwrap();
    let file = dir.join("live.db");
    b.m.snapshot_to(&file).unwrap();
    let live = Model::open(&file.to_string_lossy()).unwrap();
    let found = run_all(&live, Some(&["opening_outside"]), &CheckScope::All, 10).unwrap();
    let env = found[0].envelope().unwrap();
    let (h, top, head) = (hash(&live), live.op_top().unwrap(), live.change_head());
    let bytes = std::fs::read(&file).unwrap();
    // `live` is not `mut`: the dry run takes &self and cannot write the live model
    let dry = live.dry_run_fix(&env).unwrap();
    assert_eq!(dry.rows, 1);
    assert_eq!((hash(&live), live.op_top().unwrap(), live.change_head()), (h, top, head));
    assert_eq!(std::fs::read(&file).unwrap(), bytes, "live file bytes unchanged");
    drop(live);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fix_goes_through_dry_run() {
    let mut b = base();
    let walls = b.closed();
    let door = b.door(walls[0], 2.0);
    b.m.sql()
        .execute("UPDATE opening SET along=9.5 WHERE id=?1", [door])
        .unwrap();
    let found = run_all(&b.m, Some(&["opening_outside"]), &CheckScope::All, 10).unwrap();
    let env = found[0].envelope().unwrap();
    let (before, top) = (hash(&b.m), b.m.op_top().unwrap());

    let dry = b.m.dry_run_fix(&env).unwrap();
    assert_eq!(dry.rows, 1);
    assert!(dry.regressions.is_empty(), "{:?}", dry.regressions);
    assert_eq!(hash(&b.m), before, "a dry run leaves the model as it was");
    assert_eq!(b.m.op_top().unwrap(), top);

    b.m.apply_fix(&env).unwrap();
    assert_eq!(b.m.op_top().unwrap(), top + 1, "one fix is one journal row");
    assert_ne!(hash(&b.m), before);
    assert!(run_all(&b.m, Some(&["opening_outside"]), &CheckScope::All, 10)
        .unwrap()
        .is_empty());
    b.m.undo().unwrap();
    assert_eq!(hash(&b.m), before);

    // an envelope that cannot run is refused and leaves nothing behind
    let bad = FixEnvelope {
        v: 1,
        title_key: "ai.fix.delete".into(),
        ops: vec![FixOp::Delete {
            table: "no_such_table".into(),
            id: 1,
        }],
    };
    assert!(b.m.apply_fix(&bad).is_err());
    assert_eq!(b.m.op_top().unwrap(), top);
}

/// run_checks and the clash tool share one structure search: a column in a wall and walls
/// crossing are found by both with the same count; joined corners of a closed outline are not.
#[test]
fn structure_clashes_match_the_clash_tool() {
    let mut b = base();
    let walls = b.closed();
    let cross = b.wall((2.0, -1.0), (2.0, 1.0));
    let ct = b
        .m
        .add_member_type("C400", &MemberShape::Rect { b: 0.4, h: 0.4 }, "Concrete")
        .unwrap();
    let col = b
        .m
        .add_column(&ColumnSpec {
            level: b.level,
            top_level: None,
            member_type: ct,
            structural: true,
            at: (5.0, 0.0),
            base_offset: 0.0,
            top_offset: 3.0,
            rotation: 0.0,
            top_at: None,
        })
        .unwrap();
    let tool = b.m.clash_find(&crate::ClashParams::structure()).unwrap();
    let found = b.run("clashes");
    assert_eq!(found.len(), tool.len(), "{found:?} vs {tool:?}");
    assert_eq!(tool.len(), 2, "{tool:?}");
    let has = |k: &str, id: i64| found.iter().any(|f| f.elements.contains(&(k.to_string(), id)));
    assert!(has("col", col), "column in the wall: {found:?}");
    assert!(has("wall", cross), "crossing wall: {found:?}");
    for f in &found {
        assert!(f.elements.contains(&("wall".to_string(), walls[0])), "{f:?}");
    }
}

#[test]
fn a_cost_group_that_is_not_din_276_blocks_and_a_good_one_passes() {
    let mut b = base();
    let walls = b.closed();
    let (d1, d2) = (b.door(walls[0], 1.0), b.door(walls[0], 3.0));
    b.m.add_param(&door_param("Classification.DIN276")).unwrap();
    let set = |id: i64, v: &str| Edit::ParamValue {
        param: "Classification.DIN276".into(),
        kind: "opening".into(),
        id,
        value: Some(PVal::Text(v.into())),
    };
    b.m.apply_edits(&[set(d1, "KG 344"), set(d2, "3440")]).unwrap();
    let found = b.run("din276");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].severity, Severity::Error);
    assert_eq!(found[0].args[1].1, "3440");
    assert_eq!(found[0].elements.len(), 1);
    b.m.apply_edits(&[set(d2, "344")]).unwrap();
    assert!(b.run("din276").is_empty());
}
