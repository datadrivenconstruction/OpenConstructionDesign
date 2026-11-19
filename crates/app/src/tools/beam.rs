// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Beam (BM): click start/end, chained; Space flips direction; Z elevation relative to the level.

use super::{column, Active, Rec};
use ddc_geom::V2;
use ddc_model::{BeamJoin, BeamSpec, Just};

pub fn spec(a: &Active, s: V2, e: V2) -> BeamSpec {
    BeamSpec {
        level: a.level,
        member_type: 0,
        a: (s.x, s.y, a.opts.z),
        b: (e.x, e.y, a.opts.z),
        bulge: 0.0,
        yjust: Just::Center,
        zjust: Just::Pos,
        rotation: 0.0,
        start_ext: 0.0,
        end_ext: 0.0,
        join_start: BeamJoin::Auto,
        join_end: BeamJoin::Auto,
    }
}

/// Axis height the beam tool starts with for wall height `wall_h`: the picks use the same value.
pub(crate) fn tool_z(wall_h: f64) -> f64 {
    super::Opts::for_kind(super::Kind::Beam, wall_h).z
}

/// One record that adds a beam of member type `name` on each of `lines` at level `level`,
/// axis height `z` above it: one undo step.
pub(crate) fn pick_rec(level: i64, z: f64, name: String, lines: Vec<(V2, V2)>) -> Rec {
    Rec::new("beam", move |m| {
        let (t, n) = column::ensure_member(m, &name)?;
        let specs: Vec<BeamSpec> = lines
            .iter()
            .map(|&(s, e)| BeamSpec {
                level,
                member_type: t,
                a: (s.x, s.y, z),
                b: (e.x, e.y, z),
                bulge: 0.0,
                yjust: Just::Center,
                zjust: Just::Pos,
                rotation: 0.0,
                start_ext: 0.0,
                end_ext: 0.0,
                join_start: BeamJoin::Auto,
                join_end: BeamJoin::Auto,
            })
            .collect();
        if let Err(e) = m.add_beams(&specs) {
            // a type made for these beams goes too: no op is left without its undo step
            for _ in 0..n {
                m.undo()?;
            }
            return Err(e);
        }
        Ok(n + 1)
    })
}

pub fn rec(a: &Active, s: V2, e: V2) -> Result<Rec, String> {
    let name = a.type_name().to_string();
    let sp = spec(a, s, e);
    Ok(Rec::new("beam", move |m| {
        let (t, n) = column::ensure_member(m, &name)?;
        m.add_beam(&BeamSpec {
            member_type: t,
            ..sp.clone()
        })?;
        Ok(n + 1)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    use crate::tools::Kind;
    use ddc_geom::v2;
    use ddc_ui::i18n::Lang;

    #[test]
    fn beam_pick_z_matches_drawn_beam() {
        let mut e = Editor::new_in_memory().unwrap();
        e.height = 3.7;
        let a = Active::new(Kind::Beam, &e, Lang::En);
        let drawn = spec(&a, v2(0.0, 0.0), v2(4.0, 0.0));
        assert!((drawn.a.2 - tool_z(e.height)).abs() < 1e-12);
        assert!((drawn.b.2 - tool_z(e.height)).abs() < 1e-12);
    }
}
