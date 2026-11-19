// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use crate::{LayerSpec, Target, WallSpec};

/// Test executor: `add_wall` adds one wall at `params.y`, `move_wall` moves `params.wall`; the
/// x offset comes from the seed only, so the replay is a pure function of the recipe.
fn lower(m: &mut Model, r: &Replay<'_>) -> Result<(), Error> {
    let p: serde_json::Value = serde_json::from_str(r.params)?;
    let x = (splitmix(r.seed) % 1000) as f64 / 100.0;
    let int = |k: &str| p[k].as_i64().ok_or_else(|| Error::Invalid(format!("no {k}")));
    match r.envelope {
        "add_wall" => {
            let y = p["y"].as_f64().unwrap_or(30.0);
            m.add_wall(&WallSpec {
                a: (x, y),
                b: (x + 4.0, y),
                level: int("level")?,
                base_offset: 0.0,
                height: 3.0,
                wall_type: int("type")?,
                offset0: None,
            })?;
        }
        "move_wall" => m.move_elements(&[Target::Wall(int("wall")?)], (x + 0.5, 0.0))?,
        e => return Err(Error::Invalid(format!("unknown envelope {e}"))),
    }
    Ok(())
}

fn splitmix(seed: u64) -> u64 {
    let mut z = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

struct Fx {
    m: Model,
    lv: i64,
    wt: i64,
    wall: i64,
    dir: PathBuf,
    base: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn fixture(tag: &str) -> Fx {
    let dir = std::env::temp_dir().join(format!("ddc-ai-opt-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let wt = m
        .add_wall_type(
            "w",
            &[LayerSpec {
                material: "brick".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    let wall = m
        .add_wall(&WallSpec {
            a: (0.0, 0.0),
            b: (5.0, 0.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .unwrap();
    let base = dir.join("base.db");
    m.snapshot_to(&base).unwrap();
    Fx {
        m,
        lv,
        wt,
        wall,
        dir,
        base,
    }
}

fn recipe(fx: &Fx, name: &str, envelope: &str, seed: u64, params: String) -> OptionRec {
    let (base_head, base_stamp) = fx.m.ai_option_head().unwrap();
    let mut r = OptionRec {
        id: 0,
        set: "Entrance".into(),
        name: name.into(),
        base_head,
        base_stamp,
        envelope: envelope.into(),
        seed,
        params,
        generator: None,
        metrics: None,
        note: None,
        why: Some("test".into()),
        status: OptionStatus::Draft,
    };
    r.id = fx.m.ai_option_add(&r).unwrap();
    fx.m.ai_option_get(r.id).unwrap().unwrap()
}

fn add_params(fx: &Fx, y: f64) -> String {
    format!(r#"{{"level":{},"type":{},"y":{y}}}"#, fx.lv, fx.wt)
}

fn walls(m: &Model) -> i64 {
    m.sql()
        .query_row("SELECT count(*) FROM wall", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn option_recipe_rematerialises_identical_diff() {
    let fx = fixture("replay");
    let add = recipe(&fx, "A", "add_wall", 7, add_params(&fx, 30.0));
    let mv = recipe(&fx, "B", "move_wall", 11, format!(r#"{{"wall":{}}}"#, fx.wall));
    let recs = [add.clone(), mv.clone()];
    let one = materialize_options(&fx.base, &recs, &fx.dir.join("one"), &mut lower).unwrap();
    let two = materialize_options(&fx.base, &recs, &fx.dir.join("two"), &mut lower).unwrap();
    assert_eq!(one.len(), 2);
    for (a, b) in one.iter().zip(&two) {
        assert!(!a.diff.rows.is_empty() && !a.diff.truncated);
        assert_eq!(a.mode, b.mode);
        assert_eq!(a.diff, b.diff, "same recipe on the same base must give the same rows");
    }
    // the stored recipe is the whole input: re-read and replayed again in the same folder
    let again = fx.m.ai_option_get(add.id).unwrap().unwrap();
    let three = materialize_options(&fx.base, &[again], &fx.dir.join("one"), &mut lower).unwrap();
    assert_eq!(three[0].diff, one[0].diff);
    // the seed is part of the recipe
    let other = OptionRec { seed: 8, ..add };
    let four = materialize_options(&fx.base, &[other], &fx.dir.join("four"), &mut lower).unwrap();
    assert_ne!(four[0].diff, one[0].diff);
}

#[test]
fn add_only_option_uses_alternatives() {
    let fx = fixture("alt");
    let a = recipe(&fx, "A", "add_wall", 1, add_params(&fx, 30.0));
    let b = recipe(&fx, "B", "add_wall", 2, add_params(&fx, 60.0));
    let out = materialize_options(&fx.base, &[a.clone(), b.clone()], &fx.dir, &mut lower).unwrap();
    assert!(out.iter().all(|c| c.mode == OptionMode::Alternative));
    assert_eq!(out[0].path, out[1].path, "one copy holds all add-only options");
    assert_ne!(out[0].alternative, out[1].alternative);
    assert!(!fx.dir.join(format!("opt-{}.db", a.id)).exists());
    assert!(!fx.dir.join(format!("opt-{}.db", b.id)).exists());
    let alt = Model::open_copy(out[0].path.to_str().unwrap()).unwrap();
    let sets = alt.option_sets().unwrap();
    assert_eq!(sets.len(), 1);
    assert_eq!(sets[0].name, "Entrance");
    assert_eq!(sets[0].options.len(), 2);
    assert!(sets[0].options.iter().all(|o| o.members >= 1));
    assert_eq!(walls(&alt), 3, "main wall shared, one wall per alternative");
    let in_options: i64 = alt
        .sql()
        .query_row("SELECT count(*) FROM option_member WHERE kind='wall'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(in_options, 2, "the shared wall stays in the main model");
}

#[test]
fn shared_edit_option_uses_branch() {
    let fx = fixture("branch");
    let mv = recipe(&fx, "Moved", "move_wall", 3, format!(r#"{{"wall":{}}}"#, fx.wall));
    let add = recipe(&fx, "Added", "add_wall", 4, add_params(&fx, 30.0));
    let out = materialize_options(&fx.base, &[mv.clone(), add], &fx.dir, &mut lower).unwrap();
    assert_eq!(out[0].mode, OptionMode::Branch);
    assert_eq!(out[0].path, fx.dir.join(format!("opt-{}.db", mv.id)));
    assert!(out[0].path.is_file());
    assert_eq!(out[0].alternative, None);
    assert!(out[0]
        .diff
        .rows
        .iter()
        .any(|r| r.table == "wall" && r.rowid == fx.wall && r.kind == RowKind::Changed));
    assert_eq!(out[1].mode, OptionMode::Alternative);
    assert_eq!(added_elements(&out[0].diff), None);
    let branch = Model::open_copy(out[0].path.to_str().unwrap()).unwrap();
    assert!(branch.option_sets().unwrap().is_empty(), "a branch holds no alternatives");
}

#[test]
fn option_never_touches_main_until_applied() {
    let mut fx = fixture("main");
    let head = fx.m.change_head();
    let top = fx.m.op_top().unwrap();
    let a = recipe(&fx, "A", "add_wall", 5, add_params(&fx, 30.0));
    let b = recipe(&fx, "B", "move_wall", 6, format!(r#"{{"wall":{}}}"#, fx.wall));
    materialize_options(&fx.base, &[a.clone(), b.clone()], &fx.dir, &mut lower).unwrap();
    fx.m.ai_option_set_metrics(a.id, r#"{"walls":2}"#).unwrap();
    assert!(fx.m.changes_since(head).is_empty(), "recipes and copies leave the model as is");
    assert_eq!(fx.m.op_top().unwrap(), top, "no undo step before Apply");
    assert_eq!(walls(&fx.m), 1);

    fx.m.ai_option_apply(a.id, &mut lower).unwrap();
    assert_eq!(walls(&fx.m), 2);
    assert_eq!(fx.m.op_top().unwrap(), top + 1, "Apply is one undo step");
    assert_eq!(
        fx.m.ai_option_get(a.id).unwrap().unwrap().status,
        OptionStatus::Accepted
    );
    let err = fx.m.ai_option_apply(b.id, &mut lower).unwrap_err();
    assert!(err.to_string().contains("stale"), "{err}");
    fx.m.ai_option_reject(b.id).unwrap();
    assert_eq!(
        fx.m.ai_option_get(b.id).unwrap().unwrap().status,
        OptionStatus::Rejected
    );
    fx.m.undo().unwrap();
    assert_eq!(walls(&fx.m), 1);
    assert_eq!(fx.m.ai_option_list(Some("Entrance")).unwrap().len(), 2);
}

#[test]
fn old_project_without_ai_option_opens() {
    let fx = fixture("old");
    let p = fx.dir.join("old.ddc");
    let ps = p.to_str().unwrap();
    {
        let mut m = Model::open(ps).unwrap();
        m.add_level("L1", 0.0).unwrap();
    }
    {
        // own connection: the model's authorizer turns DROP TABLE into a silent no-op
        let c = Connection::open(ps).unwrap();
        c.execute_batch("DROP INDEX ai_option_set_ix; DROP TABLE ai_option;")
            .unwrap();
        c.execute(
            "UPDATE meta SET value=?1 WHERE key='schema_version'",
            [crate::schema::VERSION - 1],
        )
        .unwrap();
    }
    {
        let m = Model::open(ps).unwrap();
        assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
        assert!(m.ai_option_list(None).unwrap().is_empty());
        let levels: i64 = m
            .sql()
            .query_row("SELECT count(*) FROM level", [], |r| r.get(0))
            .unwrap();
        assert_eq!(levels, 1, "upgrade keeps rows");
        let r = OptionRec {
            id: 0,
            set: "S".into(),
            name: "A".into(),
            base_head: 0,
            base_stamp: 0,
            envelope: "add_wall".into(),
            seed: u64::MAX,
            params: "{}".into(),
            generator: Some("grid".into()),
            metrics: None,
            note: None,
            why: None,
            status: OptionStatus::Draft,
        };
        m.ai_option_add(&r).unwrap();
    }
    let m = Model::open(ps).unwrap();
    let list = m.ai_option_list(None).unwrap();
    assert_eq!(list.len(), 1, "reopen is additive");
    assert_eq!(list[0].seed, u64::MAX, "the seed survives the i64 column");
}

#[test]
fn rejected_option_revivable() {
    let mut fx = fixture("revive");
    let a = recipe(&fx, "A", "add_wall", 3, add_params(&fx, 40.0));
    fx.m.ai_option_reject(a.id).unwrap();
    assert!(fx.m.ai_option_apply(a.id, &mut lower).is_err(), "a rejected option is not applied");
    assert!(fx.m.ai_option_revive(a.id).is_ok());
    assert!(fx.m.ai_option_revive(a.id).is_err(), "only a rejected option revives");
    let back = fx.m.ai_option_get(a.id).unwrap().unwrap();
    assert_eq!(back.status, OptionStatus::Draft);
    assert_eq!(back.envelope, a.envelope, "the recipe survives the rejection");

    merge_wall(&mut fx, a.id, Some("widest entrance"), 40.0).unwrap();
    merge_wall(&mut fx, a.id, None, 50.0).unwrap();
    let merged = fx.m.ai_option_get(a.id).unwrap().unwrap();
    assert_eq!(merged.status, OptionStatus::Merged);
    assert_eq!(merged.note.as_deref(), Some("widest entrance"), "history keeps why this one");
    assert!(fx.m.ai_option_reject(a.id).is_err(), "a merged option is not rejected");
}

/// The Apply of a merge card of `id`: one wall at `y`, the mark in the same journal entry.
fn merge_wall(fx: &mut Fx, id: i64, note: Option<&str>, y: f64) -> Result<(), Error> {
    let params = add_params(fx, y);
    let rp = Replay {
        envelope: "add_wall",
        seed: 1,
        params: &params,
    };
    fx.m.atomic_merge("merge_card", id, note, |m| lower(m, &rp))
}

fn merge_rows(m: &Model) -> i64 {
    m.sql()
        .query_row("SELECT count(*) FROM ai_option_merge", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn undo_merge_apply_reverts_merged_mark() {
    let mut fx = fixture("undomerge");
    let top = fx.m.op_top().unwrap();
    let a = recipe(&fx, "A", "add_wall", 5, add_params(&fx, 30.0));
    merge_wall(&mut fx, a.id, Some("widest entrance"), 30.0).unwrap();
    assert_eq!(fx.m.op_top().unwrap(), top + 1, "mark and elements are one undo step");
    assert_eq!(status(&fx, a.id), OptionStatus::Merged);
    fx.m.undo().unwrap();
    assert_eq!(walls(&fx.m), 1, "undo removes the merged elements");
    assert_eq!(fx.m.op_top().unwrap(), top);
    let back = fx.m.ai_option_get(a.id).unwrap().unwrap();
    assert_eq!(back.status, OptionStatus::Draft, "undo of the merge returns it to draft");
    assert_eq!(back.note, None, "and takes the note back");
    // a new operation reuses the undone seq; its stamp differs, so the option stays draft
    fx.m.add_level("L2", 3.0).unwrap();
    assert_eq!(fx.m.op_top().unwrap(), top + 1);
    assert_eq!(status(&fx, a.id), OptionStatus::Draft);
    assert!(fx.m.ai_option_reject(a.id).is_ok(), "an undone merge can be rejected");
}

#[test]
fn redo_merge_apply_restores_merged_mark() {
    let mut fx = fixture("redomerge");
    let top = fx.m.op_top().unwrap();
    let a = recipe(&fx, "A", "add_wall", 5, add_params(&fx, 30.0));
    merge_wall(&mut fx, a.id, Some("why"), 30.0).unwrap();
    fx.m.undo().unwrap();
    assert_eq!(status(&fx, a.id), OptionStatus::Draft);
    // redo re-runs the same Apply closure
    merge_wall(&mut fx, a.id, Some("why"), 30.0).unwrap();
    assert_eq!(walls(&fx.m), 2);
    assert_eq!(fx.m.op_top().unwrap(), top + 1, "redo is one undo step again");
    let rec = fx.m.ai_option_get(a.id).unwrap().unwrap();
    assert_eq!(rec.status, OptionStatus::Merged);
    assert_eq!(rec.note.as_deref(), Some("why"));
    fx.m.undo().unwrap();
    assert_eq!(status(&fx, a.id), OptionStatus::Draft);
}

#[test]
fn failed_merge_apply_leaves_option_unmarked() {
    let mut fx = fixture("failmerge");
    let top = fx.m.op_top().unwrap();
    let a = recipe(&fx, "A", "add_wall", 5, add_params(&fx, 30.0));
    let params = add_params(&fx, 30.0);
    let rp = Replay {
        envelope: "add_wall",
        seed: 1,
        params: &params,
    };
    // the first step writes a wall, the second fails: nothing of the Apply stays
    let err = fx.m.atomic_merge("merge_card", a.id, Some("x"), |m| {
        lower(m, &rp)?;
        Err::<(), _>(Error::Invalid("step failed".into()))
    });
    assert!(err.is_err());
    assert_eq!(walls(&fx.m), 1, "no element of a failed Apply stays");
    assert_eq!(fx.m.op_top().unwrap(), top, "no journal entry");
    assert_eq!(merge_rows(&fx.m), 0, "no merge mark");
    let rec = fx.m.ai_option_get(a.id).unwrap().unwrap();
    assert_eq!(rec.status, OptionStatus::Draft);
    assert_eq!(rec.note, None);
}

#[test]
fn undo_second_part_merge_keeps_first() {
    let mut fx = fixture("partmerge");
    let a = recipe(&fx, "A", "add_wall", 5, add_params(&fx, 30.0));
    merge_wall(&mut fx, a.id, Some("first"), 30.0).unwrap();
    merge_wall(&mut fx, a.id, Some("second"), 40.0).unwrap();
    assert_eq!(fx.m.ai_option_get(a.id).unwrap().unwrap().note.as_deref(), Some("second"));
    fx.m.undo().unwrap();
    let rec = fx.m.ai_option_get(a.id).unwrap().unwrap();
    assert_eq!(rec.status, OptionStatus::Merged, "the first part merge is still live");
    assert_eq!(rec.note.as_deref(), Some("first"));
    fx.m.undo().unwrap();
    assert_eq!(status(&fx, a.id), OptionStatus::Draft);
}

#[test]
fn deleted_option_drops_its_merge_marks() {
    let mut fx = fixture("delmerge");
    let a = recipe(&fx, "A", "add_wall", 5, add_params(&fx, 30.0));
    merge_wall(&mut fx, a.id, None, 30.0).unwrap();
    assert!(fx.m.ai_option_delete(a.id).unwrap());
    assert_eq!(merge_rows(&fx.m), 0, "a reused id starts unmarked");
}

fn status(fx: &Fx, id: i64) -> OptionStatus {
    fx.m.ai_option_get(id).unwrap().unwrap().status
}

#[test]
fn undo_accept_option_back_to_draft() {
    let mut fx = fixture("undoacc");
    let top = fx.m.op_top().unwrap();
    let a = recipe(&fx, "A", "add_wall", 5, add_params(&fx, 30.0));
    fx.m.ai_option_apply(a.id, &mut lower).unwrap();
    assert_eq!(status(&fx, a.id), OptionStatus::Accepted);
    fx.m.undo().unwrap();
    assert_eq!(walls(&fx.m), 1);
    assert_eq!(fx.m.op_top().unwrap(), top);
    assert_eq!(status(&fx, a.id), OptionStatus::Draft, "undo of Apply returns it to draft");
    // a new operation reuses the undone seq; its stamp differs, so the option stays draft
    fx.m.add_level("L2", 3.0).unwrap();
    assert_eq!(fx.m.op_top().unwrap(), top + 1);
    assert_eq!(status(&fx, a.id), OptionStatus::Draft);
}

#[test]
fn redo_accept_option_accepted_again() {
    let mut fx = fixture("redoacc");
    let top = fx.m.op_top().unwrap();
    let a = recipe(&fx, "A", "add_wall", 5, add_params(&fx, 30.0));
    fx.m.ai_option_apply(a.id, &mut lower).unwrap();
    fx.m.undo().unwrap();
    assert_eq!(status(&fx, a.id), OptionStatus::Draft);
    fx.m.ai_option_apply(a.id, &mut lower).unwrap();
    assert_eq!(walls(&fx.m), 2);
    assert_eq!(fx.m.op_top().unwrap(), top + 1, "redo is one undo step again");
    assert_eq!(status(&fx, a.id), OptionStatus::Accepted);
    fx.m.undo().unwrap();
    assert_eq!(status(&fx, a.id), OptionStatus::Draft);
}

#[test]
fn accepted_option_cannot_reapply() {
    let mut fx = fixture("reapply");
    let top = fx.m.op_top().unwrap();
    let a = recipe(&fx, "A", "add_wall", 5, add_params(&fx, 30.0));
    fx.m.ai_option_apply(a.id, &mut lower).unwrap();
    let err = fx.m.ai_option_apply(a.id, &mut lower).unwrap_err();
    assert!(err.to_string().contains("accepted"), "{err}");
    assert!(fx.m.ai_option_reject(a.id).is_err(), "an accepted option is not rejected");
    assert_eq!(walls(&fx.m), 2);
    assert_eq!(fx.m.op_top().unwrap(), top + 1);
    assert_eq!(status(&fx, a.id), OptionStatus::Accepted);
}

fn set_accepted(path: &Path, id: i64, seq: i64, stamp: i64) {
    Connection::open(path)
        .unwrap()
        .execute(
            "UPDATE ai_option SET accepted_seq=?2, accepted_stamp=?3 WHERE id=?1",
            params![id, seq, stamp],
        )
        .unwrap();
}

#[test]
fn store_without_op_table_lists() {
    let fx = fixture("storenoop");
    let path = fx.dir.join("recipes.db");
    let s = OptionStore::open(&path).unwrap();
    let mut r = recipe(&fx, "A", "add_wall", 5, add_params(&fx, 30.0));
    r.id = 0;
    let id = s.add_recipe(&r).unwrap();
    let listed = s.list(None).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].status, OptionStatus::Draft);
    // no journal here: a recorded Apply reads as accepted
    set_accepted(&path, id, 1, 1);
    assert_eq!(s.get(id).unwrap().unwrap().status, OptionStatus::Accepted);
}

#[test]
fn store_on_project_needs_live_record() {
    let mut fx = fixture("storeop");
    fx.m.add_level("L2", 3.0).unwrap();
    let (seq, stamp) = fx.m.ai_option_head().unwrap();
    let path = fx.dir.join("project.db");
    fx.m.snapshot_to(&path).unwrap();
    let s = OptionStore::open(&path).unwrap();
    let mut r = recipe(&fx, "A", "add_wall", 5, add_params(&fx, 30.0));
    r.id = 0;
    let id = s.add_recipe(&r).unwrap();
    // a mark naming no journal record (undone, or an older build) reads as draft
    set_accepted(&path, id, seq + 50, stamp);
    assert_eq!(s.get(id).unwrap().unwrap().status, OptionStatus::Draft);
    set_accepted(&path, id, seq, stamp);
    assert_eq!(s.get(id).unwrap().unwrap().status, OptionStatus::Accepted);
}
