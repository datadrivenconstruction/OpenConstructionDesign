use super::*;
use crate::ai_option::{materialize_options, OptionRec, OptionStatus, Replay};
use crate::{LayerSpec, WallSpec};
use std::fs;
use std::path::PathBuf;

/// Adds one wall; the x offset comes from the seed, so the replay is a pure function of the recipe.
fn lower(m: &mut Model, r: &Replay<'_>) -> Result<(), Error> {
    let p: serde_json::Value = serde_json::from_str(r.params)?;
    let int = |k: &str| p[k].as_i64().ok_or_else(|| Error::Invalid(format!("no {k}")));
    let x = (r.seed % 1000) as f64 / 10.0;
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
    Ok(())
}

struct Fx {
    m: Model,
    lv: i64,
    wt: i64,
    dir: PathBuf,
    base: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn layers() -> Vec<LayerSpec> {
    vec![
        LayerSpec {
            material: "brick".into(),
            thickness: 0.2,
        },
        LayerSpec {
            material: "insul".into(),
            thickness: 0.1,
        },
    ]
}

fn fixture(tag: &str) -> Fx {
    let dir = std::env::temp_dir().join(format!("ddc-opt-metrics-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let wt = m.add_wall_type("w", &layers()).unwrap();
    m.add_wall(&WallSpec {
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
        dir,
        base,
    }
}

fn params(fx: &Fx, y: f64) -> String {
    format!(r#"{{"level":{},"type":{},"y":{y}}}"#, fx.lv, fx.wt)
}

fn recipe(fx: &Fx, name: &str, seed: u64, y: f64) -> OptionRec {
    let (base_head, base_stamp) = fx.m.ai_option_head().unwrap();
    let mut r = OptionRec {
        id: 0,
        set: "Entrance".into(),
        name: name.into(),
        base_head,
        base_stamp,
        envelope: "add_wall".into(),
        seed,
        params: params(fx, y),
        generator: None,
        metrics: None,
        note: None,
        why: None,
        status: OptionStatus::Draft,
    };
    r.id = fx.m.ai_option_add(&r).unwrap();
    r
}

fn no_prices() -> BTreeMap<String, f64> {
    BTreeMap::new()
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn assert_cells_equal(a: &OptionMetrics, b: &OptionMetrics) {
    let (ca, cb) = (a.cells(), b.cells());
    assert_eq!(ca.len(), cb.len());
    for (x, y) in ca.iter().zip(&cb) {
        assert_eq!((&x.key, x.unit), (&y.key, y.unit));
        assert!(near(x.value, y.value), "{}: {} vs {}", x.key, x.value, y.value);
    }
}

/// The same recipe replayed on a plain copy of the base: the branch-copy reference.
fn branch_metrics(fx: &Fx, rec: &OptionRec, name: &str) -> OptionMetrics {
    let path = fx.dir.join(name);
    Model::backup_file(&fx.base, &path, &mut |_, _| true).unwrap();
    let mut m = Model::open_copy(path.to_str().unwrap()).unwrap();
    let rp = Replay {
        envelope: &rec.envelope,
        seed: rec.seed,
        params: &rec.params,
    };
    m.atomic("ref", |m| lower(m, &rp)).unwrap();
    m.option_metrics(None, &no_prices(), false).unwrap()
}

#[test]
fn totals_respect_option_filter() {
    let mut fx = fixture("filter");
    let (set, a) = fx.m.add_option_set("S", "A").unwrap();
    let b = fx.m.add_option(set, "B").unwrap();
    let w2 = fx
        .m
        .add_wall(&WallSpec {
            a: (0.0, 10.0),
            b: (4.0, 10.0),
            level: fx.lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: fx.wt,
            offset0: None,
        })
        .unwrap();
    fx.m.add_to_option(b, &[("wall".to_string(), w2)]).unwrap();
    let count = |o: Option<i64>| fx.m.level_totals_in(o).unwrap()[0].totals.walls;
    assert_eq!(count(None), 1, "primary option A has no extra wall");
    assert_eq!(count(Some(a)), 1);
    assert_eq!(count(Some(b)), 2);
    assert_eq!(fx.m.level_totals().unwrap()[0].totals.walls, 1);
    let brick = |o: Option<i64>| {
        let v = fx.m.material_totals_in(None, o).unwrap();
        v.iter().find(|q| q.material == "brick").unwrap().volume
    };
    assert!(near(brick(Some(b)) - brick(Some(a)), 4.0 * 3.0 * 0.2));
    assert!(near(brick(None), brick(Some(a))));
    assert!(near(fx.m.material_totals(None).unwrap()[0].volume, brick(None)));
}

#[test]
fn metrics_row_matches_branch_copy() {
    let fx = fixture("branch");
    let a = recipe(&fx, "A", 1, 30.0);
    let b = recipe(&fx, "B", 2, 60.0);
    let out = materialize_options(&fx.base, &[a.clone(), b.clone()], &fx.dir, &mut lower).unwrap();
    assert!(out.iter().all(|c| c.mode == OptionMode::Alternative));
    for (copy, rec) in out.iter().zip([&a, &b]) {
        let got = option_copy_metrics(copy, &no_prices()).unwrap();
        let want = branch_metrics(&fx, rec, &format!("ref-{}.db", rec.id));
        assert_eq!(got.totals.walls, 2, "shared wall plus the option's own");
        assert_cells_equal(&got, &want);
    }
    let json: serde_json::Value =
        serde_json::from_str(&option_copy_metrics(&out[0], &no_prices()).unwrap().to_json())
            .unwrap();
    assert_eq!(json["units"]["area"], "m2");
    assert!(json["cells"].as_array().unwrap().iter().all(|c| c["unit"].is_string()));
}

#[test]
fn cost_missing_prices_reported() {
    let fx = fixture("cost");
    let none = fx.m.option_metrics(None, &no_prices(), false).unwrap();
    assert!(!none.cost.priced, "no price table: quantities only");
    assert_eq!(none.cost.missing, vec!["brick".to_string(), "insul".to_string()]);
    assert!(near(none.cost.total, 0.0));

    fx.m.set_ai_policy(UNIT_COST_KEY, r#"{"brick": 100.0}"#).unwrap();
    let prices = fx.m.unit_prices().unwrap();
    let c = fx.m.option_metrics(None, &prices, false).unwrap().cost;
    assert!(c.priced);
    assert_eq!(c.missing, vec!["insul".to_string()]);
    let brick = c.lines.iter().find(|l| l.material == "brick").unwrap();
    assert!(near(brick.volume, 5.0 * 3.0 * 0.2));
    assert!(near(c.total, brick.volume * 100.0), "the unpriced line is not in the total");
    assert_eq!(c.lines.iter().find(|l| l.material == "insul").unwrap().cost, None);
}

#[test]
fn main_metrics_unchanged_by_option_metrics() {
    let fx = fixture("main");
    let before = fx.m.option_metrics(None, &no_prices(), false).unwrap();
    let head = fx.m.ai_option_head().unwrap();
    let a = recipe(&fx, "A", 1, 30.0);
    let out = materialize_options(&fx.base, &[a], &fx.dir, &mut lower).unwrap();
    let opt = option_copy_metrics(&out[0], &no_prices()).unwrap();
    assert_eq!(opt.totals.walls, 2);
    let after = fx.m.option_metrics(None, &no_prices(), false).unwrap();
    assert_eq!(before, after);
    assert_eq!(before.totals.walls, 1);
    assert_eq!(fx.m.ai_option_head().unwrap(), head);
}

#[test]
fn identical_recipes_give_identical_metrics() {
    let fx = fixture("same");
    let a = recipe(&fx, "A", 7, 30.0);
    let b = recipe(&fx, "A", 7, 30.0);
    let one = materialize_options(&fx.base, &[a], &fx.dir.join("one"), &mut lower).unwrap();
    let two = materialize_options(&fx.base, &[b], &fx.dir.join("two"), &mut lower).unwrap();
    let (x, y) = (
        option_copy_metrics(&one[0], &no_prices()).unwrap(),
        option_copy_metrics(&two[0], &no_prices()).unwrap(),
    );
    assert_eq!(x, y);
    assert_eq!(x.to_json(), y.to_json());
}
