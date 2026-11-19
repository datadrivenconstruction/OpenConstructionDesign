//! Host commands: openings and dormers from a sketch, roof by extrusion along a wall,
//! attaching wall tops, editing floor outline and shape. Each is one undo step.

use super::Rec;
use crate::editor::Sel;
use crate::shell::EditorApp;
use ddc_model::{Model, OpeningHost, RoofKind, RoofSpec, SketchHost, WallSide};

pub fn register(app: &mut EditorApp) {
    app.register_command("arch.dormer", |a| opening(a, OpeningHost::Roof(0), true));
    app.register_command("arch.roof_opening", |a| {
        opening(a, OpeningHost::Roof(0), false)
    });
    app.register_command("str.slab_opening", |a| {
        opening(a, OpeningHost::Slab(0), false)
    });
    app.register_command("arch.ceiling_opening", |a| {
        opening(a, OpeningHost::Ceiling(0), false)
    });
    app.register_command("arch.roof_extrusion", roof_extrusion);
    app.register_command("ctx.wall.attach", attach_top);
    app.register_command("ctx.wall.detach", detach_top);
    app.register_command("ctx.floor.sketch", |a| {
        if let Some(id) = slab_sel(a) {
            crate::sketch_mode::begin(&mut a.ed, SketchHost::Slab(id));
        }
    });
    app.register_command("ctx.floor.shape", |a| {
        if let Some(id) = slab_sel(a) {
            if let Err(e) = crate::shape_edit::begin(&mut a.ed, id) {
                a.ed.status = Some(e);
            }
        }
    });
}

fn opening(a: &mut EditorApp, h: OpeningHost, dormer: bool) {
    // a selected floor is the host directly; otherwise by outline position
    let sel = a.ed.selection.iter().find_map(|s| match s {
        Sel::Slab(i) => Some(*i),
        _ => None,
    });
    let h = match (h, sel) {
        (OpeningHost::Slab(_), Some(id)) => OpeningHost::Slab(id),
        _ => h,
    };
    crate::sketch_mode::begin_opening(&mut a.ed, h, dormer);
}

fn slab_sel(a: &mut EditorApp) -> Option<i64> {
    let id = a.ed.selection.iter().find_map(|s| match s {
        Sel::Slab(i) => Some(*i),
        _ => None,
    });
    if id.is_none() {
        a.ed.status = Some(a.sh.cat.t("host_shape.status.select_slab"));
    }
    id
}

fn walls_sel(a: &mut EditorApp) -> Vec<i64> {
    let v: Vec<i64> =
        a.ed.selection
            .iter()
            .filter_map(|s| match s {
                Sel::Wall(i) => Some(*i),
                _ => None,
            })
            .collect();
    if v.is_empty() {
        a.ed.status = Some(a.sh.cat.t("host_shape.status.select_walls"));
    }
    v
}

/// `ok` — catalog key for the success message.
fn apply(a: &mut EditorApp, r: Rec, ok: &str) {
    match a.ed.apply_ext(r) {
        Ok(()) => a.ed.status = Some(a.sh.cat.t(ok)),
        Err(e) => a.ed.status = Some(e.to_string()),
    }
}

/// Roll back the already written `done` operations when the next one fails.
fn rollback(m: &mut Model, done: usize, e: ddc_model::Error) -> ddc_model::Error {
    for _ in 0..done {
        let _ = m.undo();
    }
    e
}

/// Top of the selected walls: to the roof of the level above them, else to the next level up.
fn attach_top(a: &mut EditorApp) {
    let walls = walls_sel(a);
    if walls.is_empty() {
        return;
    }
    let level = a.ed.level;
    let mut lv = a.ed.levels();
    lv.sort_by(|x, y| x.2.total_cmp(&y.2));
    let above = lv
        .iter()
        .position(|l| l.0 == level)
        .and_then(|i| lv.get(i + 1))
        .map(|l| l.0);
    let none = a.sh.cat.t("host_shape.status.no_roof_or_level");
    let r = Rec::new("wall attach top", move |m| {
        let roofs: Vec<i64> = {
            let mut st = m
                .sql()
                .prepare("SELECT id FROM roof WHERE level=?1 ORDER BY id")?;
            let v = st.query_map([level], |r| r.get(0))?.flatten().collect();
            v
        };
        let mut n = 0;
        for &w in &walls {
            let hit = roofs.iter().any(|&r| m.attach_wall_top(w, r).is_ok());
            if hit {
                n += 1;
                continue;
            }
            let res = match above {
                Some(l) => m.attach_wall_to_level(w, l, 0.0),
                None => Err(ddc_model::Error::Invalid(none.clone())),
            };
            match res {
                Ok(()) => n += 1,
                Err(e) => return Err(rollback(m, n, e)),
            }
        }
        Ok(n)
    });
    apply(a, r, "host_shape.status.top_attached");
}

fn detach_top(a: &mut EditorApp) {
    let walls = walls_sel(a);
    if walls.is_empty() {
        return;
    }
    let r = Rec::new("wall detach top", move |m| {
        let mut n = 0;
        for &w in &walls {
            let roof: Option<i64> =
                m.sql()
                    .query_row("SELECT top_roof FROM wall WHERE id=?1", [w], |r| r.get(0))?;
            let res = if roof.is_some() {
                m.detach_wall_top(w)
            } else {
                m.detach_wall_from_slab(w, WallSide::Top)
            };
            match res {
                Ok(()) => n += 1,
                Err(e) => return Err(rollback(m, n, e)),
            }
        }
        Ok(n)
    });
    apply(a, r, "host_shape.status.top_detached");
}

/// 30° gable profile above the selected straight wall (plane = wall axis), extruded
/// leftwards to the far parallel wall (else by the wall length); elevation = wall top.
fn roof_extrusion(a: &mut EditorApp) {
    let Some(&wid) = walls_sel(a).first() else {
        return;
    };
    let Some(w) = a.ed.walls.iter().find(|w| w.id == wid && w.bulge == 0.0) else {
        a.ed.status = Some(a.sh.cat.t("host_shape.status.straight_wall"));
        return;
    };
    let base: f64 =
        a.ed.model
            .sql()
            .query_row("SELECT base_offset FROM wall WHERE id=?1", [wid], |r| {
                r.get(0)
            })
            .unwrap_or(0.0);
    // elevation above the level: wall bottom + height
    let (pa, pb, h) = (w.a, w.b, base + w.height);
    let d = pb - pa;
    let len = d.len();
    if len < 0.1 {
        return;
    }
    let n = ddc_geom::v2(-d.y, d.x) * (1.0 / len);
    let depth =
        a.ed.walls
            .iter()
            .filter(|o| o.id != wid && (o.b - o.a).cross(d).abs() < 1e-3 * len * (o.b - o.a).len())
            .map(|o| (o.a - pa).dot(n))
            .fold(0.0f64, f64::max);
    let depth = if depth > 0.1 { depth } else { len };
    let rise = 0.5 * len * 30f64.to_radians().tan();
    let spec = RoofSpec {
        level: a.ed.level,
        slab_type: 0,
        offset: h,
        overhang: 0.0,
        fascia: 0.2,
        kind: RoofKind::Extrusion {
            a: (pa.x, pa.y),
            b: (pb.x, pb.y),
            profile: vec![(0.0, 0.0), (0.5 * len, rise), (len, 0.0)],
            depth,
        },
    };
    // named once, in the language of the moment: a redo must not rename the type
    let roof_name = crate::tr("names.roof_slab", &[]);
    let r = Rec::new("roof extrusion", move |m| {
        let (t, k) = super::ensure_slab_type(m, &roof_name)?;
        m.add_roof(&RoofSpec {
            slab_type: t,
            ..spec.clone()
        })?;
        Ok(k + 1)
    });
    apply(a, r, "host_shape.status.roof_extruded");
}
