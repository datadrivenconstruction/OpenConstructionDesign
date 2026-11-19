// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! What the views draw as the tool preview: the running session's primitives, else
//! the wall draft of the old wall tool as bands with a temporary dimension, so 3D and section
//! show the rubber band the plan shows.
use super::Editor;
use crate::interact::{Interaction, Preview, Prim};
use ddc_geom::v3;

impl Editor {
    pub fn preview_prims(&self) -> Preview {
        let mut out = Interaction::preview(self);
        if !out.0.is_empty() {
            return out;
        }
        let Some(t) = self.current_type() else {
            return out;
        };
        let z = self.elevation;
        for (a, b) in self.draft_preview() {
            let len = (b - a).len();
            if len < 1e-9 {
                continue;
            }
            let s = self.strip_of(t.id, a, b, 0.0, self.height);
            let width: f64 = s.layers.iter().map(|l| l.thickness).sum();
            if width <= 0.0 {
                continue;
            }
            let (pa, pb) = (v3(a.x, a.y, z + s.base), v3(b.x, b.y, z + s.base));
            out.0.push(Prim::Band {
                axis: [pa, pb],
                bulge: 0.0,
                width,
                height: s.height,
                justify: (s.offset0 + width / 2.0) / width,
            });
            out.0.push(Prim::TempDim { a: pa, b: pb, value: len, editable: true });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use crate::editor::Editor;
    use crate::interact::{PlanAdapter, Prim, SectionAdapter, ViewAdapter};
    use crate::wall_tool::{Event, Mode};
    use ddc_geom::v2;

    #[test]
    fn wall_draft_is_a_band_every_view_draws() {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.set_mode(Mode::Polyline);
        ed.handle(Event::Move(v2(0.0, 0.0)));
        ed.handle(Event::Click(v2(0.0, 0.0)));
        ed.handle(Event::Move(v2(6.0, 0.0)));
        let p = ed.preview_prims();
        let band = p.0.iter().find(|x| matches!(x, Prim::Band { .. })).expect("draft band");
        let Prim::Band { axis, width, height, justify, .. } = band else { unreachable!() };
        assert!((axis[1].x - axis[0].x - 6.0).abs() < 1e-9);
        assert!(*width > 0.0 && (*height - ed.height).abs() < 1e-9 && justify.abs() < 1e-9);
        assert!(p.0.iter().any(|x| matches!(x, Prim::TempDim { value, .. } if (value - 6.0).abs() < 1e-9)));
        let plan = PlanAdapter { center: v2(3.0, 0.0), scale: 50.0, canvas: [0.0, 0.0], level: ed.level, z: ed.elevation };
        let sec = SectionAdapter::new(1, v2(3.0, -1.0), v2(3.0, 1.0), v2(0.0, 0.0), 50.0, [0.0, 0.0]).unwrap();
        for ad in [&plan as &dyn ViewAdapter, &sec] {
            assert!(!crate::interact::preview::draws(&p.0, ad).is_empty(), "{:?}", ad.kind());
        }
    }
}
