//! Meaningful references to snap points: `"wall:12/mid"` names a feature of an element and
//! resolves to the exact 3D point, for scripts and the AI strip (INTERACTIONS §1).
//! Walls only for now: the tables the snap hits carry refs for.

use crate::editor::Editor;
use crate::snap_core::{Feat, SnapRef};
use ddc_geom::{v3, V3};
use ddc_model::Target;

/// `"<table>:<id>/<feat>"`, feat one of `mid`, `end0`, `end1`, `axis`, `side+`, `side-`.
pub fn parse(s: &str) -> Option<SnapRef> {
    let (head, feat) = s.split_once('/')?;
    let (table, id) = head.split_once(':')?;
    let id: i64 = id.trim().parse().ok()?;
    let target = match table.trim() {
        "wall" => Target::Wall(id),
        _ => return None,
    };
    let feat = match feat.trim() {
        "mid" => Feat::Mid,
        "end0" => Feat::End(0),
        "end1" => Feat::End(1),
        "axis" => Feat::Axis,
        "side+" => Feat::Side(1),
        "side-" => Feat::Side(-1),
        _ => return None,
    };
    Some(SnapRef { target, feat })
}

/// The exact point of a reference on the editor's current level, at mid-height of the wall.
pub fn resolve(ed: &Editor, r: &SnapRef) -> Option<V3> {
    let Target::Wall(id) = r.target else { return None };
    let w = ed.walls.iter().find(|w| w.id == id)?;
    let z0 = ed.levels().into_iter().find(|l| l.0 == ed.level).map_or(0.0, |l| l.2) + w.base_offset;
    let (a, b) = (w.a, w.b);
    let d = b - a;
    let len = d.len();
    let p = match r.feat {
        Feat::Mid | Feat::Axis => (a + b) * 0.5,
        Feat::End(0) => a,
        Feat::End(_) => b,
        Feat::Side(s) => {
            if len == 0.0 {
                return None;
            }
            let half = ed.type_by_id(w.wall_type).map_or(0.0, |t| t.thickness() / 2.0);
            let n = ddc_geom::v2(-d.y / len, d.x / len);
            (a + b) * 0.5 + n * (half * f64::from(s))
        }
        Feat::MassFace(_) | Feat::Line => return None,
    };
    Some(v3(p.x, p.y, z0 + w.height / 2.0))
}

pub fn resolve_text(ed: &Editor, s: &str) -> Option<V3> {
    resolve(ed, &parse(s)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_wall_mid_end_side() {
        let mut ed = Editor::new_in_memory().unwrap();
        crate::modify_tools::tests::walls(&mut ed, &[((0.0, 0.0), (6.0, 0.0))]);
        let id = ed.walls[0].id;
        let at = |f: &str| resolve_text(&ed, &format!("wall:{id}/{f}")).expect(f);
        let near = |p: V3, q: V3| (p - q).len() < 1e-9;
        assert!(near(at("mid"), v3(3.0, 0.0, 1.5)));
        assert!(near(at("end0"), v3(0.0, 0.0, 1.5)));
        assert!(near(at("end1"), v3(6.0, 0.0, 1.5)));
        let half = ed.type_by_id(ed.walls[0].wall_type).unwrap().thickness() / 2.0;
        assert!(near(at("side+"), v3(3.0, half, 1.5)) && near(at("side-"), v3(3.0, -half, 1.5)));
        assert!(resolve_text(&ed, "wall:9999/mid").is_none());
        assert!(parse("wall:12/mid").is_some_and(|r| r.target == Target::Wall(12) && r.feat == Feat::Mid));
        for bad in ["wall:12", "door:1/mid", "wall:x/mid", "wall:1/top"] {
            assert!(parse(bad).is_none(), "{bad}");
        }
    }
}
